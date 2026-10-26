pub mod path;

mod blob;
mod cache;
mod manifest;
mod query;

use cache::*;
use manifest::*;

use std::{
    fs, io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    body::Body,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::{
    DockerConfig, SourcePool,
    audit::{AuditDetails, AuditSink},
    storage::locks::ShardedLocks,
    upstream::registry::{Adapter, AdapterFuture, AdapterRegistry, AdapterRequest, RegistryError},
    upstream::{
        ArtifactBudget, FetchOperation, FetchTimeouts, GroupRun, SourcePoolError,
        source_error_is_group_fallback, source_error_is_not_found,
    },
};

use self::path::{DockerRequest, ManifestReference, parse_request};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
const MANIFEST_ACCEPT: &str = "application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.v2+json, \
application/vnd.oci.image.manifest.v1+json";

#[derive(Debug)]
pub struct DockerAdapter {
    service: Arc<DockerService>,
}

#[derive(Debug)]
pub struct DockerService {
    _root_url: Url,
    data_dir: PathBuf,
    audit: AuditSink,
    blob_locks: ShardedLocks,
}

#[derive(Debug, Error)]
pub enum DockerRegistrationError {
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

#[derive(Debug, Error)]
pub enum DockerError {
    #[error(transparent)]
    Path(#[from] path::DockerPathError),
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error("Docker manifest digest header is missing or invalid")]
    MissingDigest,
    #[error("Docker manifest digest mismatch: expected `{expected}`, got `{actual}`")]
    DigestMismatch { expected: String, actual: String },
    #[error("Docker blob `{0}` has no descriptor metadata")]
    UnknownBlob(String),
    #[error("Docker manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("Docker catalog response is invalid")]
    InvalidCatalog,
    #[error("Docker tag list response is invalid")]
    InvalidTags,
    #[error("Docker search query is invalid")]
    InvalidSearchQuery,
    #[error("Docker Range header is invalid for an object of {0} bytes")]
    InvalidRange(u64),
    #[error("Docker metadata storage failed: {0}")]
    Storage(#[from] io::Error),
    #[error("Docker metadata serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManifestMetadata {
    repository: String,
    reference: String,
    digest: String,
    selected_proxy: String,
    #[serde(default)]
    source_fingerprint: String,
    content_type: Option<String>,
    #[serde(default)]
    accept: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BlobMetadata {
    repository: String,
    digest: String,
    object_type: String,
    content_type: String,
    selected_proxy: String,
}

struct DockerAuditDetails<'a> {
    entry_name: &'a str,
    object_key: &'a str,
    selected_proxy: &'a str,
    object_type: &'a str,
    cache_status: &'a str,
    failure_category: Option<&'a str>,
}

struct CachedManifest {
    body: Vec<u8>,
    content_type: Option<String>,
}

struct ManifestSource<'a> {
    name: &'a str,
    fingerprint: &'a str,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct QueryMetadata {
    namespace: String,
    request_key: String,
    query: Option<String>,
    object_type: String,
    selected_proxy: String,
    #[serde(default)]
    source_fingerprint: String,
    digest: String,
    content_type: Option<String>,
    link: Option<String>,
}

#[derive(Debug)]
struct QueryResult {
    body: Vec<u8>,
    content_type: Option<String>,
    link: Option<String>,
}

impl DockerAdapter {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(DockerService::new(root_url, data_dir)),
        }
    }

    pub fn service(&self) -> &DockerService {
        &self.service
    }
}

pub fn register_adapter(
    registry: &mut AdapterRegistry,
    config: DockerConfig,
    root_url: Url,
    data_dir: impl Into<PathBuf>,
) -> Result<(), DockerRegistrationError> {
    let artifact_budget = ArtifactBudget::default();
    register_adapter_with_options(
        registry,
        config,
        root_url,
        data_dir,
        DockerRegistrationOptions {
            proxy_url: None,
            proxy_no_proxy: None,
            fetch_timeouts: FetchTimeouts::default(),
            artifact_budget: &artifact_budget,
        },
    )
}

pub fn register_adapter_with_proxy(
    registry: &mut AdapterRegistry,
    config: DockerConfig,
    root_url: Url,
    data_dir: impl Into<PathBuf>,
    proxy_url: Option<&Url>,
    proxy_no_proxy: Option<&str>,
) -> Result<(), DockerRegistrationError> {
    let artifact_budget = ArtifactBudget::default();
    register_adapter_with_options(
        registry,
        config,
        root_url,
        data_dir,
        DockerRegistrationOptions {
            proxy_url,
            proxy_no_proxy,
            fetch_timeouts: FetchTimeouts::default(),
            artifact_budget: &artifact_budget,
        },
    )
}

#[derive(Clone, Copy, Debug)]
pub struct DockerRegistrationOptions<'a> {
    pub proxy_url: Option<&'a Url>,
    pub proxy_no_proxy: Option<&'a str>,
    pub fetch_timeouts: FetchTimeouts,
    pub artifact_budget: &'a ArtifactBudget,
}

pub fn register_adapter_with_options(
    registry: &mut AdapterRegistry,
    config: DockerConfig,
    root_url: Url,
    data_dir: impl Into<PathBuf>,
    options: DockerRegistrationOptions<'_>,
) -> Result<(), DockerRegistrationError> {
    let DockerRegistrationOptions {
        proxy_url,
        proxy_no_proxy,
        fetch_timeouts,
        artifact_budget,
    } = options;
    registry.register(
        "docker",
        Arc::new(
            SourcePool::new_with_proxy("docker", config.sources, proxy_url, proxy_no_proxy)?
                .with_artifact_budget(artifact_budget.clone())
                .with_fetch_timeouts(fetch_timeouts),
        ),
        Arc::new(DockerAdapter::new(root_url, data_dir)),
    )?;
    Ok(())
}

impl DockerService {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        let temporary_root = data_dir.join("tmp");
        let _ = fs::create_dir_all(&temporary_root);
        if let Ok(entries) = fs::read_dir(&temporary_root) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().ends_with(".part") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Self {
            _root_url: root_url,
            audit: AuditSink::new(data_dir.clone()),
            data_dir,
            blob_locks: ShardedLocks::default(),
        }
    }

    async fn handle_request(
        &self,
        entry_name: &str,
        request: AdapterRequest,
        pool: &SourcePool,
    ) -> Response {
        if !matches!(request.method, Method::GET | Method::HEAD) {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let head = request.method == Method::HEAD;
        match parse_request(&request.raw_path) {
            Ok(DockerRequest::Probe) => (
                StatusCode::OK,
                [(
                    header::HeaderName::from_static("docker-distribution-api-version"),
                    "registry/2.0",
                )],
                Body::empty(),
            )
                .into_response(),
            Ok(DockerRequest::Catalog) => match self
                .catalog(entry_name, request.query.as_deref(), pool)
                .await
            {
                Ok(response) => response,
                Err(error) => docker_error_response(&error),
            },
            Ok(DockerRequest::Tags { repository }) => {
                match self
                    .tags(entry_name, &repository, request.query.as_deref(), pool)
                    .await
                {
                    Ok(response) => response,
                    Err(error) => docker_error_response(&error),
                }
            }
            Ok(DockerRequest::Search) => {
                match self
                    .search(entry_name, request.query.as_deref(), pool)
                    .await
                {
                    Ok(response) => response,
                    Err(error) => docker_error_response(&error),
                }
            }
            Ok(DockerRequest::Manifest {
                repository,
                reference,
            }) => match self
                .manifest(
                    entry_name,
                    &repository,
                    &reference,
                    &request.headers,
                    pool,
                    head,
                )
                .await
            {
                Ok(response) => response,
                Err(error) => docker_error_response(&error),
            },
            Ok(DockerRequest::Blob { repository, digest }) => match self
                .blob(
                    entry_name,
                    &repository,
                    &digest,
                    &request.headers,
                    pool,
                    head,
                )
                .await
            {
                Ok(response) => response,
                Err(error) => docker_error_response(&error),
            },
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        }
    }

    fn record(&self, event: &str, details: DockerAuditDetails<'_>) {
        if let Err(error) = self.audit.record_detailed(
            event,
            details.object_key,
            if details.failure_category.is_some() {
                "failure"
            } else {
                "success"
            },
            AuditDetails {
                ecosystem: Some("docker"),
                entry_name: Some(details.entry_name),
                object_key: Some(details.object_key),
                selected_proxy: Some(details.selected_proxy),
                object_type: Some(details.object_type),
                cache_status: Some(details.cache_status),
                failure_category: details.failure_category,
                count: None,
            },
        ) {
            crate::logging::error(format!("audit write failed: {error}"));
        }
    }
}

impl ManifestReference {
    fn as_str(&self) -> &str {
        match self {
            Self::Tag(value) | Self::Digest(value) => value,
        }
    }
}

impl Adapter for DockerAdapter {
    fn ecosystem(&self) -> &str {
        "docker"
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

fn docker_error_response(error: &DockerError) -> Response {
    let status = match error {
        DockerError::Path(_) => StatusCode::NOT_FOUND,
        DockerError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        DockerError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        DockerError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        DockerError::Source(_)
        | DockerError::MissingDigest
        | DockerError::DigestMismatch { .. }
        | DockerError::InvalidManifest(_) => StatusCode::BAD_GATEWAY,
        DockerError::InvalidCatalog | DockerError::InvalidTags => StatusCode::BAD_GATEWAY,
        DockerError::InvalidSearchQuery => StatusCode::BAD_REQUEST,
        DockerError::UnknownBlob(_) => StatusCode::NOT_FOUND,
        DockerError::InvalidRange(_) => StatusCode::RANGE_NOT_SATISFIABLE,
        DockerError::Storage(_) | DockerError::Serialization(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let mut response = status.into_response();
    if let DockerError::InvalidRange(length) = error {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            header::HeaderValue::from_str(&format!("bytes */{length}"))
                .expect("content range is valid"),
        );
    }
    response
}

fn sha256_digest(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}

fn sha256_hex(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn source_is_member(pool: &SourcePool, entry_name: &str, source_name: &str) -> bool {
    pool.proxy_members(entry_name)
        .is_some_and(|members| members.iter().any(|member| member.name == source_name))
}

fn source_is_current(
    pool: &SourcePool,
    entry_name: &str,
    source_name: &str,
    source_fingerprint: &str,
) -> bool {
    pool.source_fingerprint(entry_name, source_name)
        .is_some_and(|fingerprint| fingerprint == source_fingerprint)
}

fn is_fallback_source_error(error: &SourcePoolError) -> bool {
    source_error_is_group_fallback(error)
}

fn is_fallback_search_error(error: &DockerError) -> bool {
    match error {
        DockerError::InvalidCatalog => true,
        DockerError::InvalidTags => true,
        DockerError::InvalidManifest(_) => true,
        DockerError::Source(source) => is_fallback_source_error(source),
        _ => false,
    }
}

fn is_fallback_validation_error(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::MissingDigest
            | DockerError::DigestMismatch { .. }
            | DockerError::InvalidManifest(_)
    )
}

fn failure_category(error: &DockerError) -> &'static str {
    match error {
        DockerError::MissingDigest => "missing_digest",
        DockerError::DigestMismatch { .. } => "digest_mismatch",
        _ => "protocol_invalid",
    }
}
