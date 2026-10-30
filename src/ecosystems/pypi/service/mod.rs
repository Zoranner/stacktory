//! PyPI Simple API 只读适配器。
//!
//! 项目索引、文件下载和根索引分别由 `metadata`、`artifact` 和 `root` 承担；
//! 本模块只做路径路由、Accept 协商、错误码映射、审计和共享的来源查询。

mod artifact;
mod metadata;
mod root;

use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{ACCEPT, CONTENT_TYPE, VARY},
    },
    response::{IntoResponse, Response},
};
use thiserror::Error;
use tokio_util::io::ReaderStream;
use url::Url;

use crate::{
    PublicUrls, SourceConfig, SourcePool,
    audit::{AuditDetails, AuditError, AuditSink},
    upstream::{
        SourcePoolError,
        registry::{Adapter, AdapterFuture, AdapterRequest},
    },
};

use super::{
    accept::{AcceptError, PyPiRepresentation, negotiate},
    cache::{ArtifactFile, PyPiStorage, StorageError, base_url_fingerprint},
    index::PyPiIndexError,
    path::{PyPiPathError, parse_file_path, parse_project_path},
};

const METADATA_REVALIDATE_AFTER_SECS: u64 = 15 * 60;
/// 项目索引的显式读取上限。官方 PyPI 根索引约 47 MB，根索引走流式改写，
/// 不经过这个上限；单个项目索引超过该值时按来源失败处理。
pub(super) const MAX_PROJECT_INDEX_BYTES: usize = 16 * 1024 * 1024;
const HTML_ACCEPT: &str = "text/html, application/vnd.pypi.simple.v1+html;q=0.9";
const JSON_ACCEPT: &str = "application/vnd.pypi.simple.v1+json";

#[derive(Debug)]
pub struct PyPiAdapter {
    service: Arc<PyPiService>,
}

#[derive(Debug)]
pub(crate) struct PyPiService {
    pub(super) public_urls: PublicUrls,
    pub(super) storage: PyPiStorage,
    pub(super) audit: AuditSink,
}

#[derive(Debug, Error)]
pub enum PyPiError {
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error(transparent)]
    Path(#[from] PyPiPathError),
    #[error(transparent)]
    Index(#[from] PyPiIndexError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error("PyPI project `{0}` was not found")]
    MissingProject(String),
    #[error("PyPI file `{0}` is not bound to an indexed project")]
    MissingFile(String),
    #[error("all PyPI sources failed for `{project}`: {failures}")]
    AllSourcesFailed { project: String, failures: String },
}

impl PyPiAdapter {
    pub fn new(public_urls: impl Into<PublicUrls>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(PyPiService::new(public_urls, data_dir)),
        }
    }
}

impl PyPiService {
    pub fn new(public_urls: impl Into<PublicUrls>, data_dir: impl Into<PathBuf>) -> Self {
        let storage = PyPiStorage::new(data_dir);
        storage.cleanup_temporary_artifacts();
        let audit = AuditSink::new(storage.data_dir().to_path_buf());
        Self {
            public_urls: public_urls.into(),
            storage,
            audit,
        }
    }

    pub(crate) async fn handle_request(
        &self,
        entry_name: &str,
        request: AdapterRequest,
        pool: &SourcePool,
    ) -> Response {
        if request.method != Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let raw_path = request.raw_path.trim_start_matches('/');
        if raw_path == "simple/" {
            return match negotiate(&request.headers) {
                Ok(representation) => {
                    self.root_index(entry_name, representation, pool, &request.base_url)
                        .await
                }
                Err(AcceptError::NotAcceptable) => StatusCode::NOT_ACCEPTABLE.into_response(),
            };
        }
        if let Ok(project) = parse_project_path(raw_path) {
            return match negotiate(&request.headers) {
                Ok(representation) => {
                    self.project_index(
                        entry_name,
                        &project.project,
                        representation,
                        pool,
                        &request.base_url,
                    )
                    .await
                }
                Err(AcceptError::NotAcceptable) => StatusCode::NOT_ACCEPTABLE.into_response(),
            };
        }
        if let Ok(file) = parse_file_path(raw_path) {
            return self
                .file_response(entry_name, &file.project, &file.filename, pool)
                .await;
        }
        StatusCode::NOT_FOUND.into_response()
    }

    pub(super) fn error_response(&self, error: &PyPiError) -> Response {
        let status = match error {
            PyPiError::Path(_) => StatusCode::NOT_FOUND,
            PyPiError::MissingProject(_)
            | PyPiError::MissingFile(_)
            | PyPiError::Source(SourcePoolError::UnknownEntry(_))
            | PyPiError::Source(SourcePoolError::TerminalSourceError { status: 404, .. })
            | PyPiError::Source(SourcePoolError::AllSourcesFailed {
                status: Some(404), ..
            }) => StatusCode::NOT_FOUND,
            PyPiError::Storage(StorageError::IntegrityMismatch(_)) => StatusCode::BAD_GATEWAY,
            PyPiError::Storage(_) | PyPiError::Audit(_) => StatusCode::INTERNAL_SERVER_ERROR,
            PyPiError::Index(_) | PyPiError::Source(_) | PyPiError::AllSourcesFailed { .. } => {
                StatusCode::BAD_GATEWAY
            }
        };
        status.into_response()
    }
}

impl Adapter for PyPiAdapter {
    fn ecosystem(&self) -> &str {
        "pypi"
    }

    fn public_urls(&self) -> &PublicUrls {
        &self.service.public_urls
    }

    fn handle(
        &self,
        source_name: String,
        request: AdapterRequest,
        pool: Arc<SourcePool>,
    ) -> AdapterFuture {
        let service = self.service.clone();
        Box::pin(async move { service.handle_request(&source_name, request, &pool).await })
    }
}
impl PyPiService {
    pub(super) fn record(
        &self,
        event: &str,
        entry_name: &str,
        object_key: &str,
        selected_proxy: Option<&str>,
        cache_status: &str,
        failure_category: Option<&str>,
    ) -> Result<(), PyPiError> {
        let object_type = if event.contains("artifact") {
            "artifact"
        } else {
            "metadata"
        };
        self.audit.record_detailed(
            event,
            object_key,
            if failure_category.is_some() {
                "failure"
            } else {
                "success"
            },
            AuditDetails {
                ecosystem: Some("pypi"),
                entry_name: Some(entry_name),
                object_key: Some(object_key),
                selected_proxy,
                object_type: Some(object_type),
                cache_status: Some(cache_status),
                failure_category,
                count: None,
            },
        )?;
        Ok(())
    }

    pub(super) fn record_metadata_source_failure(
        &self,
        entry_name: &str,
        project: &str,
        source_name: &str,
        failure_category: &str,
    ) -> Result<(), PyPiError> {
        self.record(
            "pypi_metadata_source_failed",
            entry_name,
            project,
            Some(source_name),
            "failed",
            Some(failure_category),
        )
    }

    fn event(&self, suffix: &str) -> String {
        format!("pypi_{suffix}")
    }
}

pub(super) fn representation_response(
    representation: PyPiRepresentation,
    body: Vec<u8>,
) -> Response {
    let mut response = (
        StatusCode::OK,
        [(CONTENT_TYPE, representation.content_type())],
        Body::from(body),
    )
        .into_response();
    response
        .headers_mut()
        .insert(VARY, HeaderValue::from_static("Accept"));
    response
}

pub(super) async fn artifact_response(file: ArtifactFile) -> Response {
    let handle = match tokio::fs::File::open(&file.path).await {
        Ok(handle) => handle,
        Err(error) => {
            crate::logging::error(format!(
                "PyPI artifact `{}` cannot be opened: {error}",
                file.path.display()
            ));
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let mut response =
        (StatusCode::OK, Body::from_stream(ReaderStream::new(handle))).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(length) = HeaderValue::from_str(&file.length.to_string()) {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_LENGTH, length);
    }
    response
}

pub(super) fn html_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(HTML_ACCEPT));
    headers
}

pub(super) fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(JSON_ACCEPT));
    headers
}

pub(super) fn is_html_content_type(content_type: Option<&str>) -> bool {
    content_type.is_none_or(|content_type| {
        content_type.to_ascii_lowercase().contains("text/html")
            || content_type
                .to_ascii_lowercase()
                .contains("application/vnd.pypi.simple.v1+html")
    })
}

pub(super) fn is_json_content_type(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|content_type| {
        content_type
            .to_ascii_lowercase()
            .contains("application/vnd.pypi.simple.v1+json")
    })
}

pub(super) fn metadata_is_stale(validated_at_unix_secs: u64) -> bool {
    now_unix_secs().saturating_sub(validated_at_unix_secs) >= METADATA_REVALIDATE_AFTER_SECS
}

pub(super) fn source_member(
    pool: &SourcePool,
    entry_name: &str,
    source_name: &str,
) -> Option<SourceConfig> {
    pool.proxy_members(entry_name)?
        .into_iter()
        .find(|member| member.name == source_name)
}

pub(super) fn source_fingerprint_matches(
    pool: &SourcePool,
    entry_name: &str,
    source_name: &str,
    fingerprint: &str,
) -> bool {
    source_member(pool, entry_name, source_name)
        .is_some_and(|member| member.fingerprint() == fingerprint)
}

pub(super) fn upstream_project_url(base_url: &Url, project: &str) -> Url {
    let mut url = base_url.clone();
    let base_path = base_url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/simple/{project}/"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

pub(super) fn upstream_simple_url(base_url: &Url) -> Url {
    let mut url = base_url.clone();
    let base_path = base_url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/simple/"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

pub(super) fn internal_root_prefix(base_url: &Url, entry_name: &str) -> String {
    let base_path = base_url.path().trim_end_matches('/');
    format!("{base_path}/mirror/pypi/{entry_name}/simple/")
}

pub(super) fn internal_file_url(
    base_url: &Url,
    entry_name: &str,
    project: &str,
    filename: &str,
    fragment: Option<&str>,
) -> String {
    let mut url = base_url.clone();
    let base_path = base_url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/mirror/pypi/{entry_name}/files/{project}/{filename}"
    ));
    url.set_query(None);
    url.set_fragment(fragment);
    url.to_string()
}

pub(super) fn hash_fragment(hashes: &std::collections::BTreeMap<String, String>) -> Option<String> {
    let parts = ["sha512", "sha384", "sha256"]
        .iter()
        .filter_map(|algorithm| {
            hashes
                .get(*algorithm)
                .map(|digest| format!("{algorithm}={digest}"))
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("&"))
}

pub(super) fn base_url_fingerprint_for(base_url: &Url) -> String {
    base_url_fingerprint(base_url)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
