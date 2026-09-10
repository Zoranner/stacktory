use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::{
    SourcePool,
    audit::{AuditDetails, AuditSink},
    sources::{
        FetchOperation, SourcePoolError,
        registry::{Adapter, AdapterFuture},
    },
};

use super::path::{VscodePath, VscodePathError};

#[derive(Debug)]
pub struct VscodeAdapter {
    service: Arc<VscodeService>,
}

#[derive(Debug)]
pub struct VscodeService {
    root_url: Url,
    data_dir: PathBuf,
    audit: AuditSink,
}

#[derive(Debug, Error)]
pub enum VscodeError {
    #[error(transparent)]
    Path(#[from] VscodePathError),
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error("VS Code metadata JSON is invalid: {0}")]
    MetadataJson(#[from] serde_json::Error),
    #[error("VS Code metadata is invalid: {0}")]
    InvalidMetadata(String),
    #[error("VS Code storage I/O failed: {0}")]
    Storage(#[from] io::Error),
}

impl VscodeAdapter {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(VscodeService::new(root_url, data_dir)),
        }
    }

    pub fn service(&self) -> &VscodeService {
        &self.service
    }
}

impl VscodeService {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        Self {
            root_url,
            audit: AuditSink::new(data_dir.clone()),
            data_dir,
        }
    }

    pub async fn handle_request(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
    ) -> Response {
        match self.serve(entry_name, path, pool).await {
            Ok((content_type, body)) => response(StatusCode::OK, content_type, body),
            Err(error) => vscode_error_response(&error),
        }
    }

    async fn serve(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
    ) -> Result<(&'static str, Vec<u8>), VscodeError> {
        if pool.proxy_members(entry_name).is_none() {
            return Err(SourcePoolError::UnknownEntry(entry_name.to_owned()).into());
        }

        match VscodePath::parse(path)? {
            metadata @ VscodePath::Metadata { .. } => self
                .metadata(entry_name, metadata, pool)
                .await
                .map(|body| ("application/json", body)),
            file @ VscodePath::File { .. } => self
                .artifact(entry_name, file, pool)
                .await
                .map(|body| ("application/zip", body)),
        }
    }

    async fn metadata(
        &self,
        entry_name: &str,
        path: VscodePath,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, VscodeError> {
        let cache_path = self.metadata_path(entry_name, &path);
        let sidecar_path = self.metadata_sidecar_path(entry_name, &path);
        if let Some(contents) = read_if_present(&cache_path)? {
            if validate_cached_metadata(&contents).is_ok() {
                let selected_proxy = read_source_name(&sidecar_path)?;
                self.record_vscode(
                    "vscode_metadata_read",
                    entry_name,
                    &path.request_key(),
                    VscodeAuditDetails {
                        selected_proxy: selected_proxy.as_deref(),
                        object_type: "metadata",
                        cache_status: "hit",
                        failure_category: None,
                    },
                );
                return Ok(contents);
            }
            fs::remove_file(&cache_path)?;
            let _ = fs::remove_file(&sidecar_path);
            self.record_vscode(
                "vscode_metadata_cache_invalid",
                entry_name,
                &path.request_key(),
                VscodeAuditDetails {
                    selected_proxy: None,
                    object_type: "metadata",
                    cache_status: "miss",
                    failure_category: Some("invalid_cache"),
                },
            );
        }

        let request_key = path.request_key();
        let response = match pool
            .fetch(entry_name, &request_key, FetchOperation::Get)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                self.record_vscode(
                    "vscode_metadata_fetch_failed",
                    entry_name,
                    &request_key,
                    VscodeAuditDetails {
                        selected_proxy: None,
                        object_type: "metadata",
                        cache_status: "failed",
                        failure_category: Some("source_failed"),
                    },
                );
                return Err(error.into());
            }
        };
        let contents = rewrite_metadata(&response.body, &self.root_url, entry_name, &path)?;
        atomic_write(&cache_path, &contents)?;
        atomic_write(
            &sidecar_path,
            format!("{}\n", response.source_name).as_bytes(),
        )?;
        self.record_vscode(
            "vscode_metadata_published",
            entry_name,
            &request_key,
            VscodeAuditDetails {
                selected_proxy: Some(&response.source_name),
                object_type: "metadata",
                cache_status: "published",
                failure_category: None,
            },
        );
        Ok(contents)
    }

    async fn artifact(
        &self,
        entry_name: &str,
        path: VscodePath,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, VscodeError> {
        let publisher = path.publisher().to_owned();
        let extension = path.extension().to_owned();
        let version = path
            .version()
            .ok_or_else(|| VscodePathError::Invalid(path.request_key()))?
            .to_owned();
        let filename = path
            .filename()
            .ok_or_else(|| VscodePathError::Invalid(path.request_key()))?
            .to_owned();
        let metadata_path = path.exact_metadata();
        let metadata = self
            .metadata(entry_name, metadata_path.clone(), pool)
            .await?;
        let expected_checksum = metadata_checksum(&metadata)?;
        let cache_path =
            self.artifact_path(entry_name, &publisher, &extension, &version, &filename);
        if let Some(contents) = read_if_present(&cache_path)? {
            if checksum(&contents).eq_ignore_ascii_case(&expected_checksum) {
                self.record_vscode(
                    "vscode_artifact_read",
                    entry_name,
                    &path.request_key(),
                    VscodeAuditDetails {
                        selected_proxy: read_source_name(
                            &self.metadata_sidecar_path(entry_name, &metadata_path),
                        )?
                        .as_deref(),
                        object_type: "artifact",
                        cache_status: "hit",
                        failure_category: None,
                    },
                );
                return Ok(contents);
            }
            fs::remove_file(&cache_path)?;
            self.record_vscode(
                "vscode_artifact_cache_invalid",
                entry_name,
                &path.request_key(),
                VscodeAuditDetails {
                    selected_proxy: None,
                    object_type: "artifact",
                    cache_status: "miss",
                    failure_category: Some("integrity_mismatch"),
                },
            );
        }

        let mut members = pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
        if let Some(selected_proxy) = read_source_sidecar(
            &self.metadata_sidecar_path(entry_name, &metadata_path),
            &members,
        )? && let Some(position) = members
            .iter()
            .position(|member| member.name == selected_proxy)
        {
            let selected = members.remove(position);
            members.insert(0, selected);
        }

        let request_key = format!("api/{publisher}/{extension}/{version}/file/{filename}");
        let is_group = pool.is_group(entry_name);
        let mut failures = Vec::new();
        let mut saw_failure = false;
        let mut all_not_found = true;
        self.record_vscode(
            "vscode_artifact_read",
            entry_name,
            &request_key,
            VscodeAuditDetails {
                selected_proxy: read_source_name(
                    &self.metadata_sidecar_path(entry_name, &metadata_path),
                )?
                .as_deref(),
                object_type: "artifact",
                cache_status: "miss",
                failure_category: None,
            },
        );
        for member in members {
            match pool
                .fetch(&member.name, &request_key, FetchOperation::Get)
                .await
            {
                Ok(response) if checksum(&response.body) == expected_checksum => {
                    atomic_write(&cache_path, &response.body)?;
                    atomic_write(
                        &self.metadata_sidecar_path(entry_name, &metadata_path),
                        format!("{}\n", member.name).as_bytes(),
                    )?;
                    self.record_vscode(
                        "vscode_artifact_published",
                        entry_name,
                        &request_key,
                        VscodeAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "published",
                            failure_category: None,
                        },
                    );
                    return Ok(response.body);
                }
                Ok(_) => {
                    saw_failure = true;
                    all_not_found = false;
                    failures.push(format!("{}: VSIX SHA-256 mismatch", member.name));
                    self.record_vscode(
                        "vscode_artifact_fallback",
                        entry_name,
                        &request_key,
                        VscodeAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "fallback",
                            failure_category: Some("integrity_mismatch"),
                        },
                    );
                }
                Err(error) if is_retryable(&error) => {
                    saw_failure = true;
                    all_not_found = false;
                    failures.push(format!("{}: {error}", member.name));
                    self.record_vscode(
                        "vscode_artifact_fallback",
                        entry_name,
                        &request_key,
                        VscodeAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "fallback",
                            failure_category: Some("source_unavailable"),
                        },
                    );
                }
                Err(error) if terminal_status(&error) == Some(404) && is_group => {
                    saw_failure = true;
                    failures.push(format!("{}: {error}", member.name));
                    self.record_vscode(
                        "vscode_artifact_fallback",
                        entry_name,
                        &request_key,
                        VscodeAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "fallback",
                            failure_category: Some("not_found"),
                        },
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }

        self.record_vscode(
            "vscode_artifact_fetch_failed",
            entry_name,
            &request_key,
            VscodeAuditDetails {
                selected_proxy: None,
                object_type: "artifact",
                cache_status: "failed",
                failure_category: Some(if saw_failure && all_not_found {
                    "not_found"
                } else {
                    "source_failed"
                }),
            },
        );
        Err(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key,
            failures: failures.join("; "),
            status: (is_group && saw_failure && all_not_found).then_some(404),
        }
        .into())
    }

    fn metadata_path(&self, entry_name: &str, path: &VscodePath) -> PathBuf {
        self.data_dir
            .join("vscode")
            .join("metadata")
            .join(entry_name)
            .join(path.metadata_key())
    }

    fn metadata_sidecar_path(&self, entry_name: &str, path: &VscodePath) -> PathBuf {
        let path = self.metadata_path(entry_name, path);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("VS Code metadata path must have a file name");
        path.with_file_name(format!("{file_name}.source"))
    }

    fn artifact_path(
        &self,
        entry_name: &str,
        publisher: &str,
        extension: &str,
        version: &str,
        filename: &str,
    ) -> PathBuf {
        self.data_dir
            .join("vscode")
            .join("artifacts")
            .join(entry_name)
            .join(publisher)
            .join(extension)
            .join(version)
            .join(filename)
    }

    fn record_vscode(
        &self,
        event: &str,
        entry_name: &str,
        object_key: &str,
        details: VscodeAuditDetails<'_>,
    ) {
        if let Err(error) = self.audit.record_detailed(
            event,
            object_key,
            if details.failure_category.is_some() {
                "failure"
            } else {
                "success"
            },
            AuditDetails {
                ecosystem: Some("vscode"),
                entry_name: Some(entry_name),
                object_key: Some(object_key),
                selected_proxy: details.selected_proxy,
                object_type: Some(details.object_type),
                cache_status: Some(details.cache_status),
                failure_category: details.failure_category,
            },
        ) {
            eprintln!("Stacktory audit write failed: {error}");
        }
    }
}

impl Adapter for VscodeAdapter {
    fn ecosystem(&self) -> &str {
        "vscode"
    }

    fn handle(&self, source_name: String, path: String, pool: Arc<SourcePool>) -> AdapterFuture {
        let service = self.service.clone();
        Box::pin(async move { service.handle_request(&source_name, &path, &pool).await })
    }
}

fn rewrite_metadata(
    body: &[u8],
    root_url: &Url,
    entry_name: &str,
    path: &VscodePath,
) -> Result<Vec<u8>, VscodeError> {
    let mut metadata: Value = serde_json::from_slice(body)?;
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata must be a JSON object".to_owned()))?;
    let publisher = path.publisher();
    let extension = path.extension();
    if object.get("namespace").and_then(Value::as_str) != Some(publisher)
        || object.get("name").and_then(Value::as_str) != Some(extension)
    {
        return Err(VscodeError::InvalidMetadata(
            "metadata namespace/name does not match the request".to_owned(),
        ));
    }

    let version = object
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata version is missing".to_owned()))?
        .to_owned();
    if path.version().is_some_and(|requested| requested != version) {
        return Err(VscodeError::InvalidMetadata(
            "metadata version does not match the request".to_owned(),
        ));
    }
    semver::Version::parse(&version)
        .map_err(|_| VscodeError::InvalidMetadata("metadata version is invalid".to_owned()))?;

    let files = object
        .get_mut("files")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata files are missing".to_owned()))?;
    let download = files
        .get("download")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VscodeError::InvalidMetadata("metadata download URL is missing".to_owned())
        })?;
    let expected_filename = format!("{publisher}.{extension}-{version}.vsix");
    validate_download_url(download, &expected_filename)?;
    let checksum = files
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata SHA-256 is missing".to_owned()))?;
    validate_checksum(checksum)?;

    files.insert(
        "download".to_owned(),
        Value::String(local_download_url(
            root_url,
            entry_name,
            publisher,
            extension,
            &version,
            &expected_filename,
        )),
    );
    serde_json::to_vec(&metadata).map_err(VscodeError::MetadataJson)
}

fn validate_cached_metadata(body: &[u8]) -> Result<(), VscodeError> {
    let metadata: Value = serde_json::from_slice(body)?;
    let files = metadata
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata files are missing".to_owned()))?;
    let download = files
        .get("download")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VscodeError::InvalidMetadata("metadata download URL is missing".to_owned())
        })?;
    let checksum = files
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata SHA-256 is missing".to_owned()))?;
    Url::parse(download)
        .map_err(|_| VscodeError::InvalidMetadata("cached download URL is invalid".to_owned()))?;
    validate_checksum(checksum)
}

fn validate_download_url(value: &str, expected_filename: &str) -> Result<(), VscodeError> {
    let url = Url::parse(value)
        .map_err(|_| VscodeError::InvalidMetadata("metadata download URL is invalid".to_owned()))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().ends_with(&format!("/file/{expected_filename}"))
    {
        return Err(VscodeError::InvalidMetadata(
            "metadata download URL is outside the Open VSX-compatible file path".to_owned(),
        ));
    }
    Ok(())
}

fn validate_checksum(value: &str) -> Result<(), VscodeError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(VscodeError::InvalidMetadata(
            "metadata SHA-256 must be 64 hexadecimal characters".to_owned(),
        ))
    }
}

fn metadata_checksum(body: &[u8]) -> Result<String, VscodeError> {
    let metadata: Value = serde_json::from_slice(body)?;
    let checksum = metadata
        .get("files")
        .and_then(Value::as_object)
        .and_then(|files| files.get("sha256"))
        .and_then(Value::as_str)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata SHA-256 is missing".to_owned()))?;
    validate_checksum(checksum)?;
    Ok(checksum.to_ascii_lowercase())
}

fn local_download_url(
    root_url: &Url,
    entry_name: &str,
    publisher: &str,
    extension: &str,
    version: &str,
    filename: &str,
) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/mirror/vscode/{entry_name}/api/{publisher}/{extension}/{version}/file/{filename}"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn vscode_error_response(error: &VscodeError) -> Response {
    let status = match error {
        VscodeError::Path(_) => StatusCode::NOT_FOUND,
        VscodeError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        VscodeError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        VscodeError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        VscodeError::Source(_) | VscodeError::MetadataJson(_) | VscodeError::InvalidMetadata(_) => {
            StatusCode::BAD_GATEWAY
        }
        VscodeError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    (status, [(CONTENT_TYPE, content_type)], Body::from(body)).into_response()
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("VS Code cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("VS Code cache path has no file name"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));
    fs::write(&temporary, contents)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(error)
        }
    }
}

fn read_source_sidecar(
    path: &Path,
    members: &[crate::SourceConfig],
) -> Result<Option<String>, io::Error> {
    let Some(source_name) = read_source_name(path)? else {
        return Ok(None);
    };
    Ok((members.iter().any(|member| member.name == source_name)).then_some(source_name))
}

fn read_source_name(path: &Path) -> Result<Option<String>, io::Error> {
    let Some(contents) = read_if_present(path)? else {
        return Ok(None);
    };
    let source_name = String::from_utf8_lossy(&contents).trim().to_owned();
    Ok((!source_name.is_empty()).then_some(source_name))
}

fn checksum(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn is_retryable(error: &SourcePoolError) -> bool {
    matches!(error, SourcePoolError::AllSourcesFailed { .. })
}

fn terminal_status(error: &SourcePoolError) -> Option<u16> {
    match error {
        SourcePoolError::TerminalSourceError { status, .. } => Some(*status),
        SourcePoolError::AllSourcesFailed { status, .. } => *status,
        _ => None,
    }
}

struct VscodeAuditDetails<'a> {
    selected_proxy: Option<&'a str>,
    object_type: &'a str,
    cache_status: &'a str,
    failure_category: Option<&'a str>,
}
