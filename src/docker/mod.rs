pub mod path;

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::{
    DockerConfig, SourcePool,
    audit::{AuditDetails, AuditSink},
    sources::registry::{Adapter, AdapterFuture, AdapterRegistry, AdapterRequest, RegistryError},
    sources::{FetchOperation, SourcePoolError},
};

use self::path::{DockerRequest, ManifestReference, parse_request};

#[derive(Debug)]
pub struct DockerAdapter {
    service: Arc<DockerService>,
}

#[derive(Debug)]
pub struct DockerService {
    _root_url: Url,
    data_dir: PathBuf,
    audit: AuditSink,
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
    #[error("Docker Range header is invalid for an object of {0} bytes")]
    InvalidRange(usize),
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
    registry.register(
        "docker",
        Arc::new(SourcePool::new("docker", config.sources)?),
        Arc::new(DockerAdapter::new(root_url, data_dir)),
    )?;
    Ok(())
}

impl DockerService {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        Self {
            _root_url: root_url,
            audit: AuditSink::new(data_dir.clone()),
            data_dir,
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

    async fn manifest(
        &self,
        entry_name: &str,
        repository: &str,
        reference: &ManifestReference,
        request_headers: &HeaderMap,
        pool: &SourcePool,
        head: bool,
    ) -> Result<Response, DockerError> {
        let mut headers = HeaderMap::new();
        if let Some(accept) = request_headers.get(header::ACCEPT) {
            headers.insert(header::ACCEPT, accept.clone());
        }
        let accept = request_headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bound = read_manifest_metadata(
            &self.data_dir,
            entry_name,
            repository,
            reference.as_str(),
            accept.as_deref(),
        )?
        .filter(|metadata| source_is_member(pool, entry_name, &metadata.selected_proxy));
        if let Some(metadata) = &bound
            && let Some(body) = read_verified_artifact(
                &self.data_dir,
                &metadata.selected_proxy,
                repository,
                manifest_object_type(metadata.content_type.as_deref()),
                &metadata.digest,
            )?
        {
            self.record(
                "docker_manifest_read",
                DockerAuditDetails {
                    entry_name,
                    object_key: &manifest_object_key(repository, reference),
                    selected_proxy: &metadata.selected_proxy,
                    object_type: manifest_object_type(metadata.content_type.as_deref()),
                    cache_status: "hit",
                    failure_category: None,
                },
            );
            return Ok(manifest_response(
                body,
                metadata.content_type.as_deref(),
                &metadata.digest,
                head,
            ));
        }

        if let ManifestReference::Digest(digest) = reference {
            for source_name in candidate_sources(pool, entry_name, None)? {
                if let Some(cached) =
                    read_cached_manifest(&self.data_dir, &source_name, repository, digest)?
                {
                    let metadata = ManifestMetadata {
                        repository: repository.to_owned(),
                        reference: digest.clone(),
                        digest: digest.clone(),
                        selected_proxy: source_name,
                        content_type: cached.content_type,
                        accept: accept.clone(),
                    };
                    self.write_manifest_metadata(entry_name, &metadata, &cached.body)?;
                    self.record(
                        "docker_manifest_read",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &manifest_object_key(repository, reference),
                            selected_proxy: &metadata.selected_proxy,
                            object_type: manifest_object_type(metadata.content_type.as_deref()),
                            cache_status: "hit",
                            failure_category: None,
                        },
                    );
                    return Ok(manifest_response(
                        cached.body,
                        metadata.content_type.as_deref(),
                        digest,
                        head,
                    ));
                }
            }
        }

        let candidates = candidate_sources(
            pool,
            entry_name,
            bound.as_ref().map(|value| value.selected_proxy.as_str()),
        )?;
        let request_key = format!("v2/{repository}/manifests/{}", reference.as_str());
        let mut failures = Vec::new();
        let mut all_not_found = true;
        for source_name in candidates {
            match pool
                .fetch_proxy_registry_path(
                    &source_name,
                    &request_key,
                    Method::GET,
                    headers.clone(),
                    FetchOperation::Metadata,
                )
                .await
            {
                Ok(upstream) => match validate_manifest(
                    repository,
                    reference,
                    &source_name,
                    upstream.content_type.as_deref(),
                    &upstream.headers,
                    &upstream.body,
                    accept.as_deref(),
                ) {
                    Ok(metadata) => {
                        let object_type = manifest_object_type(metadata.content_type.as_deref());
                        write_verified_artifact(
                            &self.data_dir,
                            &metadata.selected_proxy,
                            repository,
                            object_type,
                            &metadata.digest,
                            &upstream.body,
                        )?;
                        self.write_manifest_metadata(entry_name, &metadata, &upstream.body)?;
                        self.record(
                            "docker_manifest_read",
                            DockerAuditDetails {
                                entry_name,
                                object_key: &manifest_object_key(repository, reference),
                                selected_proxy: &metadata.selected_proxy,
                                object_type,
                                cache_status: "miss",
                                failure_category: None,
                            },
                        );
                        return Ok(manifest_response(
                            upstream.body,
                            metadata.content_type.as_deref(),
                            &metadata.digest,
                            head,
                        ));
                    }
                    Err(error)
                        if pool.is_group(entry_name) && is_fallback_validation_error(&error) =>
                    {
                        all_not_found = false;
                        let category = failure_category(&error);
                        self.record(
                            "docker_manifest_fetch_failed",
                            DockerAuditDetails {
                                entry_name,
                                object_key: &manifest_object_key(repository, reference),
                                selected_proxy: &source_name,
                                object_type: "manifest",
                                cache_status: "miss",
                                failure_category: Some(category),
                            },
                        );
                        failures.push(format!("{source_name}: {error}"));
                    }
                    Err(error) => {
                        self.record(
                            "docker_manifest_fetch_failed",
                            DockerAuditDetails {
                                entry_name,
                                object_key: &manifest_object_key(repository, reference),
                                selected_proxy: &source_name,
                                object_type: "manifest",
                                cache_status: "miss",
                                failure_category: Some(failure_category(&error)),
                            },
                        );
                        return Err(error);
                    }
                },
                Err(error) if pool.is_group(entry_name) && is_fallback_source_error(&error) => {
                    if !is_not_found_source_error(&error) {
                        all_not_found = false;
                    }
                    self.record(
                        "docker_manifest_fetch_failed",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &manifest_object_key(repository, reference),
                            selected_proxy: &source_name,
                            object_type: "manifest",
                            cache_status: "miss",
                            failure_category: Some(if is_not_found_source_error(&error) {
                                "not_found"
                            } else {
                                "source_failed"
                            }),
                        },
                    );
                    failures.push(format!("{source_name}: {error}"));
                }
                Err(error) => {
                    self.record(
                        "docker_manifest_fetch_failed",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &manifest_object_key(repository, reference),
                            selected_proxy: &source_name,
                            object_type: "manifest",
                            cache_status: "miss",
                            failure_category: Some(if is_not_found_source_error(&error) {
                                "not_found"
                            } else {
                                "source_failed"
                            }),
                        },
                    );
                    return Err(error.into());
                }
            }
        }
        Err(DockerError::Source(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key,
            failures: failures.join("; "),
            status: (all_not_found && !failures.is_empty()).then_some(404),
        }))
    }

    fn write_manifest_metadata(
        &self,
        entry_name: &str,
        metadata: &ManifestMetadata,
        body: &[u8],
    ) -> Result<(), DockerError> {
        let path = self
            .data_dir
            .join("docker")
            .join("metadata")
            .join(entry_name)
            .join(&metadata.repository)
            .join("manifests")
            .join(format!(
                "{}-{}.json",
                metadata_file_key(&metadata.reference),
                representation_file_key(metadata.accept.as_deref())
            ));
        atomic_write(&path, &serde_json::to_vec(&metadata)?).map_err(DockerError::Storage)?;
        write_blob_descriptors(
            &self.data_dir,
            entry_name,
            &metadata.repository,
            &metadata.selected_proxy,
            body,
        )?;
        Ok(())
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
            },
        ) {
            eprintln!("Stacktory audit write failed: {error}");
        }
    }

    async fn blob(
        &self,
        entry_name: &str,
        repository: &str,
        digest: &str,
        request_headers: &HeaderMap,
        pool: &SourcePool,
        head: bool,
    ) -> Result<Response, DockerError> {
        let entry_metadata = read_blob_metadata(&self.data_dir, entry_name, repository, digest)?;
        let candidates = candidate_sources(
            pool,
            entry_name,
            entry_metadata
                .as_ref()
                .map(|metadata| metadata.selected_proxy.as_str()),
        )?;
        let metadata = if let Some(metadata) = entry_metadata {
            metadata
        } else {
            let metadata = candidates
                .iter()
                .find_map(|source_name| {
                    match read_shared_blob_metadata(&self.data_dir, source_name, repository, digest)
                    {
                        Ok(Some(metadata)) => Some(Ok(metadata)),
                        Ok(None) => None,
                        Err(error) => Some(Err(error)),
                    }
                })
                .transpose()?
                .ok_or_else(|| DockerError::UnknownBlob(digest.to_owned()))?;
            write_blob_metadata(&self.data_dir, entry_name, repository, &metadata)?;
            metadata
        };
        if source_is_member(pool, entry_name, &metadata.selected_proxy)
            && let Some(body) = read_verified_artifact(
                &self.data_dir,
                &metadata.selected_proxy,
                repository,
                &metadata.object_type,
                digest,
            )?
        {
            self.record(
                "docker_blob_read",
                DockerAuditDetails {
                    entry_name,
                    object_key: &format!("{repository}/blobs/{digest}"),
                    selected_proxy: &metadata.selected_proxy,
                    object_type: &metadata.object_type,
                    cache_status: "hit",
                    failure_category: None,
                },
            );
            return blob_response(
                body,
                &metadata.content_type,
                digest,
                request_headers
                    .get(header::RANGE)
                    .and_then(|value| value.to_str().ok()),
                head,
            );
        }
        let headers = HeaderMap::new();
        let requested_range = request_headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok());
        let request_key = format!("v2/{repository}/blobs/{digest}");
        let mut failures = Vec::new();
        let mut all_not_found = true;
        for source_name in candidates {
            match pool
                .fetch_proxy_registry_path(
                    &source_name,
                    &request_key,
                    Method::GET,
                    headers.clone(),
                    FetchOperation::Artifact,
                )
                .await
            {
                Ok(upstream) => {
                    let actual_digest = sha256_digest(&upstream.body);
                    if actual_digest != digest {
                        let error = DockerError::DigestMismatch {
                            expected: digest.to_owned(),
                            actual: actual_digest,
                        };
                        if pool.is_group(entry_name) {
                            self.record(
                                "docker_blob_fetch_failed",
                                DockerAuditDetails {
                                    entry_name,
                                    object_key: &format!("{repository}/blobs/{digest}"),
                                    selected_proxy: &source_name,
                                    object_type: &metadata.object_type,
                                    cache_status: "miss",
                                    failure_category: Some("digest_mismatch"),
                                },
                            );
                            failures.push(format!("{source_name}: {error}"));
                            all_not_found = false;
                            continue;
                        }
                        self.record(
                            "docker_blob_fetch_failed",
                            DockerAuditDetails {
                                entry_name,
                                object_key: &format!("{repository}/blobs/{digest}"),
                                selected_proxy: &source_name,
                                object_type: &metadata.object_type,
                                cache_status: "miss",
                                failure_category: Some("digest_mismatch"),
                            },
                        );
                        return Err(error);
                    }
                    let mut selected_metadata = metadata.clone();
                    selected_metadata.selected_proxy = source_name.clone();
                    write_blob_metadata(
                        &self.data_dir,
                        entry_name,
                        repository,
                        &selected_metadata,
                    )?;
                    write_verified_artifact(
                        &self.data_dir,
                        &source_name,
                        repository,
                        &metadata.object_type,
                        digest,
                        &upstream.body,
                    )?;
                    self.record(
                        "docker_blob_read",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &format!("{repository}/blobs/{digest}"),
                            selected_proxy: &source_name,
                            object_type: &metadata.object_type,
                            cache_status: "miss",
                            failure_category: None,
                        },
                    );
                    return blob_response(
                        upstream.body,
                        &metadata.content_type,
                        digest,
                        requested_range,
                        head,
                    );
                }
                Err(error) if pool.is_group(entry_name) && is_fallback_source_error(&error) => {
                    if !is_not_found_source_error(&error) {
                        all_not_found = false;
                    }
                    self.record(
                        "docker_blob_fetch_failed",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &format!("{repository}/blobs/{digest}"),
                            selected_proxy: &source_name,
                            object_type: &metadata.object_type,
                            cache_status: "miss",
                            failure_category: Some(if is_not_found_source_error(&error) {
                                "not_found"
                            } else {
                                "source_failed"
                            }),
                        },
                    );
                    failures.push(format!("{source_name}: {error}"));
                }
                Err(error) => {
                    self.record(
                        "docker_blob_fetch_failed",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &format!("{repository}/blobs/{digest}"),
                            selected_proxy: &source_name,
                            object_type: &metadata.object_type,
                            cache_status: "miss",
                            failure_category: Some(if is_not_found_source_error(&error) {
                                "not_found"
                            } else {
                                "source_failed"
                            }),
                        },
                    );
                    return Err(error.into());
                }
            }
        }
        Err(DockerError::Source(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key,
            failures: failures.join("; "),
            status: (all_not_found && !failures.is_empty()).then_some(404),
        }))
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

fn validate_manifest(
    repository: &str,
    reference: &ManifestReference,
    selected_proxy: &str,
    content_type: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
    accept: Option<&str>,
) -> Result<ManifestMetadata, DockerError> {
    let manifest: Value = serde_json::from_slice(body)
        .map_err(|_| DockerError::InvalidManifest("body is not valid JSON".to_owned()))?;
    if !manifest.is_object() {
        return Err(DockerError::InvalidManifest(
            "body is not a JSON object".to_owned(),
        ));
    }
    if let Some(config) = manifest.get("config") {
        validate_blob_descriptor(config, "config")?;
    }
    if let Some(layers) = manifest.get("layers") {
        let layers = layers
            .as_array()
            .ok_or_else(|| DockerError::InvalidManifest("layers is not an array".to_owned()))?;
        for layer in layers {
            validate_blob_descriptor(layer, "layer")?;
        }
    }
    let actual_digest = sha256_digest(body);
    let expected_digest = match reference {
        ManifestReference::Tag(_) => headers
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok())
            .filter(|digest| is_sha256_digest(digest))
            .map(str::to_owned)
            .ok_or(DockerError::MissingDigest)?,
        ManifestReference::Digest(digest) => digest.clone(),
    };
    if expected_digest != actual_digest {
        return Err(DockerError::DigestMismatch {
            expected: expected_digest,
            actual: actual_digest,
        });
    }
    if let Some(header_digest) = headers
        .get("docker-content-digest")
        .and_then(|value| value.to_str().ok())
        .filter(|digest| is_sha256_digest(digest))
        && header_digest != actual_digest
    {
        return Err(DockerError::DigestMismatch {
            expected: header_digest.to_owned(),
            actual: actual_digest,
        });
    }
    let content_type = content_type
        .map(str::to_owned)
        .or_else(|| manifest_media_type(&manifest));
    Ok(ManifestMetadata {
        repository: repository.to_owned(),
        reference: reference.as_str().to_owned(),
        digest: actual_digest,
        selected_proxy: selected_proxy.to_owned(),
        content_type,
        accept: accept.map(str::to_owned),
    })
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == "sha256:".len() + 64
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_blob_descriptor(value: &Value, object_type: &str) -> Result<(), DockerError> {
    let digest = value
        .get("digest")
        .and_then(Value::as_str)
        .filter(|digest| is_sha256_digest(digest))
        .ok_or_else(|| DockerError::InvalidManifest(format!("{object_type} digest is invalid")))?;
    let has_media_type = value
        .get("mediaType")
        .and_then(Value::as_str)
        .is_some_and(|media_type| !media_type.is_empty());
    if !has_media_type {
        return Err(DockerError::InvalidManifest(format!(
            "{object_type} mediaType is missing for {digest}"
        )));
    }
    Ok(())
}

fn manifest_media_type(value: &Value) -> Option<String> {
    value
        .get("mediaType")
        .and_then(Value::as_str)
        .filter(|media_type| !media_type.is_empty())
        .map(str::to_owned)
}

fn metadata_file_key(reference: &str) -> String {
    reference.replace(':', "-")
}

fn representation_file_key(accept: Option<&str>) -> String {
    let value = accept.unwrap_or("default");
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn write_blob_descriptors(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    selected_proxy: &str,
    body: &[u8],
) -> Result<(), DockerError> {
    let manifest: Value = serde_json::from_slice(body).map_err(DockerError::Serialization)?;
    if let Some(config) = manifest.get("config") {
        write_blob_descriptor(
            data_dir,
            entry_name,
            repository,
            selected_proxy,
            config,
            "config",
        )?;
    }
    if let Some(layers) = manifest.get("layers").and_then(Value::as_array) {
        for layer in layers {
            write_blob_descriptor(
                data_dir,
                entry_name,
                repository,
                selected_proxy,
                layer,
                "layer",
            )?;
        }
    }
    Ok(())
}

fn write_blob_descriptor(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    selected_proxy: &str,
    descriptor: &Value,
    object_type: &str,
) -> Result<(), DockerError> {
    let digest = descriptor
        .get("digest")
        .and_then(Value::as_str)
        .filter(|value| is_sha256_digest(value))
        .ok_or_else(|| DockerError::UnknownBlob(object_type.to_owned()))?;
    let content_type = descriptor
        .get("mediaType")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DockerError::UnknownBlob(digest.to_owned()))?;
    let metadata = BlobMetadata {
        repository: repository.to_owned(),
        digest: digest.to_owned(),
        object_type: object_type.to_owned(),
        content_type: content_type.to_owned(),
        selected_proxy: selected_proxy.to_owned(),
    };
    write_blob_metadata(data_dir, entry_name, repository, &metadata)
}

fn write_blob_metadata(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    metadata: &BlobMetadata,
) -> Result<(), DockerError> {
    let path = blob_metadata_path(data_dir, entry_name, repository, &metadata.digest);
    atomic_write(&path, &serde_json::to_vec(metadata)?).map_err(DockerError::Storage)
}

fn read_blob_metadata(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    digest: &str,
) -> Result<Option<BlobMetadata>, DockerError> {
    let path = blob_metadata_path(data_dir, entry_name, repository, digest);
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(DockerError::Serialization)
}

fn read_shared_blob_metadata(
    data_dir: &Path,
    selected_proxy: &str,
    repository: &str,
    digest: &str,
) -> Result<Option<BlobMetadata>, DockerError> {
    let root = data_dir.join("docker").join("metadata");
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DockerError::Storage(error)),
    };
    for entry in entries {
        let entry = entry.map_err(DockerError::Storage)?;
        let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(metadata) = read_blob_metadata(data_dir, &entry_name, repository, digest)?
            && metadata.selected_proxy == selected_proxy
        {
            return Ok(Some(metadata));
        }
    }
    Ok(None)
}

fn read_manifest_metadata(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    reference: &str,
    accept: Option<&str>,
) -> Result<Option<ManifestMetadata>, DockerError> {
    let path = data_dir
        .join("docker")
        .join("metadata")
        .join(entry_name)
        .join(repository)
        .join("manifests")
        .join(format!(
            "{}-{}.json",
            metadata_file_key(reference),
            representation_file_key(accept)
        ));
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(DockerError::Serialization)
}

fn read_cached_manifest(
    data_dir: &Path,
    selected_proxy: &str,
    repository: &str,
    digest: &str,
) -> Result<Option<CachedManifest>, DockerError> {
    for object_type in ["manifest", "image-index"] {
        if let Some(body) =
            read_verified_artifact(data_dir, selected_proxy, repository, object_type, digest)?
        {
            let content_type = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("mediaType")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            return Ok(Some(CachedManifest { body, content_type }));
        }
    }
    Ok(None)
}

fn blob_metadata_path(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    digest: &str,
) -> PathBuf {
    data_dir
        .join("docker")
        .join("metadata")
        .join(entry_name)
        .join(repository)
        .join("blobs")
        .join(format!("{}.json", metadata_file_key(digest)))
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn artifact_path(
    data_dir: &Path,
    selected_proxy: &str,
    repository: &str,
    object_type: &str,
    digest: &str,
) -> PathBuf {
    data_dir
        .join("docker")
        .join("artifacts")
        .join(selected_proxy)
        .join(repository)
        .join(object_type)
        .join(metadata_file_key(digest))
}

fn read_verified_artifact(
    data_dir: &Path,
    selected_proxy: &str,
    repository: &str,
    object_type: &str,
    digest: &str,
) -> Result<Option<Vec<u8>>, DockerError> {
    let path = artifact_path(data_dir, selected_proxy, repository, object_type, digest);
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    if sha256_digest(&contents) == digest {
        Ok(Some(contents))
    } else {
        fs::remove_file(path)?;
        Ok(None)
    }
}

fn write_verified_artifact(
    data_dir: &Path,
    selected_proxy: &str,
    repository: &str,
    object_type: &str,
    digest: &str,
    contents: &[u8],
) -> Result<(), DockerError> {
    let actual_digest = sha256_digest(contents);
    if actual_digest != digest {
        return Err(DockerError::DigestMismatch {
            expected: digest.to_owned(),
            actual: actual_digest,
        });
    }
    atomic_write(
        &artifact_path(data_dir, selected_proxy, repository, object_type, digest),
        contents,
    )
    .map_err(DockerError::Storage)
}

fn source_is_member(pool: &SourcePool, entry_name: &str, source_name: &str) -> bool {
    pool.proxy_members(entry_name)
        .is_some_and(|members| members.iter().any(|member| member.name == source_name))
}

fn candidate_sources(
    pool: &SourcePool,
    entry_name: &str,
    bound_source: Option<&str>,
) -> Result<Vec<String>, DockerError> {
    let members = pool
        .proxy_members(entry_name)
        .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
    let mut candidates = Vec::with_capacity(members.len());
    if let Some(bound_source) = bound_source
        && members.iter().any(|member| member.name == bound_source)
    {
        candidates.push(bound_source.to_owned());
    }
    for member in members {
        if !candidates.iter().any(|candidate| candidate == &member.name) {
            candidates.push(member.name);
        }
    }
    Ok(candidates)
}

fn is_fallback_source_error(error: &SourcePoolError) -> bool {
    matches!(
        error,
        SourcePoolError::AllSourcesFailed { .. }
            | SourcePoolError::TerminalSourceError { status: 404, .. }
    )
}

fn is_not_found_source_error(error: &SourcePoolError) -> bool {
    matches!(
        error,
        SourcePoolError::TerminalSourceError { status: 404, .. }
            | SourcePoolError::AllSourcesFailed {
                status: Some(404),
                ..
            }
    )
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

fn manifest_object_key(repository: &str, reference: &ManifestReference) -> String {
    format!("{repository}/manifests/{}", reference.as_str())
}

fn manifest_object_type(content_type: Option<&str>) -> &'static str {
    let media_type = content_type
        .and_then(|value| value.split(';').next())
        .unwrap_or_default()
        .trim();
    if media_type.contains("manifest.list") || media_type.contains("image.index") {
        "image-index"
    } else {
        "manifest"
    }
}

fn manifest_response(
    body: Vec<u8>,
    content_type: Option<&str>,
    digest: &str,
    head: bool,
) -> Response {
    let length = body.len();
    let response_body = if head { Vec::new() } else { body };
    let mut response = (StatusCode::OK, Body::from(response_body)).into_response();
    if let Some(content_type) = content_type
        && let Ok(value) = header::HeaderValue::from_str(content_type)
    {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_str(&length.to_string()).expect("content length is valid"),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("docker-content-digest"),
        header::HeaderValue::from_str(digest).expect("SHA-256 digest is valid"),
    );
    response
}

fn blob_response(
    body: Vec<u8>,
    content_type: &str,
    digest: &str,
    requested_range: Option<&str>,
    head: bool,
) -> Result<Response, DockerError> {
    let full_length = body.len();
    let (status, response_body, content_range) = if let Some(range) = requested_range {
        let (start, end) =
            parse_range(range, full_length).ok_or(DockerError::InvalidRange(full_length))?;
        (
            StatusCode::PARTIAL_CONTENT,
            body[start..=end].to_vec(),
            Some(format!("bytes {start}-{end}/{full_length}")),
        )
    } else {
        (StatusCode::OK, body, None)
    };
    let response_length = response_body.len();
    let response_body = if head { Vec::new() } else { response_body };
    let mut response = (status, Body::from(response_body)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(content_type)
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("accept-ranges"),
        header::HeaderValue::from_static("bytes"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_str(&response_length.to_string())
            .expect("content length is valid"),
    );
    if let Some(content_range) = content_range {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            header::HeaderValue::from_str(&content_range).expect("content range is valid"),
        );
    }
    response.headers_mut().insert(
        header::HeaderName::from_static("docker-content-digest"),
        header::HeaderValue::from_str(digest).expect("SHA-256 digest is valid"),
    );
    Ok(response)
}

fn parse_range(value: &str, length: usize) -> Option<(usize, usize)> {
    if length == 0 || !value.starts_with("bytes=") || value[6..].contains(',') {
        return None;
    }
    let (start, end) = value[6..].split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<usize>().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some((length.saturating_sub(suffix), length - 1));
    }
    let start = start.parse::<usize>().ok()?;
    if start >= length {
        return None;
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<usize>().ok()?.min(length - 1)
    };
    (start <= end).then_some((start, end))
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Docker metadata path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Docker metadata path has no file name"))?;
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
