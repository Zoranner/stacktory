pub mod path;

use std::{
    collections::HashMap,
    fs, io,
    io::{Read, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
};
use tokio_util::io::ReaderStream;
use url::Url;

use crate::{
    DockerConfig, SourcePool,
    audit::{AuditDetails, AuditSink},
    fsutil::atomic_write,
    sources::registry::{Adapter, AdapterFuture, AdapterRegistry, AdapterRequest, RegistryError},
    sources::{FetchOperation, SourcePoolError, source_error_is_group_fallback},
};

use self::path::{DockerRequest, ManifestReference, parse_request};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct DockerAdapter {
    service: Arc<DockerService>,
}

#[derive(Debug)]
pub struct DockerService {
    _root_url: Url,
    data_dir: PathBuf,
    audit: AuditSink,
    blob_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
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
    register_adapter_with_proxy(registry, config, root_url, data_dir, None, None)
}

pub fn register_adapter_with_proxy(
    registry: &mut AdapterRegistry,
    config: DockerConfig,
    root_url: Url,
    data_dir: impl Into<PathBuf>,
    proxy_url: Option<&Url>,
    proxy_no_proxy: Option<&str>,
) -> Result<(), DockerRegistrationError> {
    registry.register(
        "docker",
        Arc::new(SourcePool::new_with_proxy(
            "docker",
            config.sources,
            proxy_url,
            proxy_no_proxy,
        )?),
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
            blob_locks: Mutex::new(HashMap::new()),
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

    async fn catalog(
        &self,
        entry_name: &str,
        query: Option<&str>,
        pool: &SourcePool,
    ) -> Result<Response, DockerError> {
        let query = validate_list_query(query)?;
        self.list_endpoint(
            entry_name,
            "v2/_catalog",
            "_catalog",
            query.as_deref(),
            "catalog",
            pool,
        )
        .await
    }

    async fn tags(
        &self,
        entry_name: &str,
        repository: &str,
        query: Option<&str>,
        pool: &SourcePool,
    ) -> Result<Response, DockerError> {
        let query = validate_list_query(query)?;
        self.list_endpoint(
            entry_name,
            &format!("v2/{repository}/tags/list"),
            repository,
            query.as_deref(),
            "tags",
            pool,
        )
        .await
    }

    async fn list_endpoint(
        &self,
        entry_name: &str,
        request_key: &str,
        namespace: &str,
        query: Option<&str>,
        object_type: &str,
        pool: &SourcePool,
    ) -> Result<Response, DockerError> {
        let result = self
            .query_result(entry_name, request_key, namespace, query, object_type, pool)
            .await?;
        Ok(json_response(
            result.body,
            result.content_type.as_deref(),
            result.link.as_deref(),
        ))
    }

    async fn query_result(
        &self,
        entry_name: &str,
        request_key: &str,
        namespace: &str,
        query: Option<&str>,
        object_type: &str,
        pool: &SourcePool,
    ) -> Result<QueryResult, DockerError> {
        let bound = read_query_metadata(
            &self.data_dir,
            entry_name,
            object_type,
            namespace,
            request_key,
            query,
        )?
        .filter(|metadata| {
            source_is_current(
                pool,
                entry_name,
                &metadata.selected_proxy,
                &metadata.source_fingerprint,
            )
        });
        if let Some(metadata) = &bound
            && let Some(body) = read_verified_artifact(
                &self.data_dir,
                &metadata.selected_proxy,
                object_type,
                &metadata.digest,
            )?
        {
            self.record(
                "docker_query_read",
                DockerAuditDetails {
                    entry_name,
                    object_key: &query_object_key(request_key, query),
                    selected_proxy: &metadata.selected_proxy,
                    object_type,
                    cache_status: "hit",
                    failure_category: None,
                },
            );
            return Ok(QueryResult {
                body,
                content_type: metadata.content_type.clone(),
                link: metadata.link.clone(),
            });
        }

        let candidates = candidate_sources(
            pool,
            entry_name,
            bound
                .as_ref()
                .map(|metadata| metadata.selected_proxy.as_str()),
        )?;
        let mut failures = Vec::new();
        let mut all_not_found = true;
        for source_name in candidates {
            match pool
                .fetch_proxy_registry_path_with_query(
                    &source_name,
                    request_key,
                    query,
                    Method::GET,
                    HeaderMap::new(),
                    FetchOperation::Metadata,
                )
                .await
            {
                Ok(upstream) => {
                    let validation = serde_json::from_slice::<Value>(&upstream.body)
                        .map_err(|_| {
                            DockerError::InvalidManifest(format!("{object_type} is not valid JSON"))
                        })
                        .and_then(|value| validate_list_response(&value, object_type));
                    if let Err(error) = validation {
                        if pool.is_group(entry_name) && is_fallback_search_error(&error) {
                            all_not_found = false;
                            failures.push(format!("{source_name}: {error}"));
                            continue;
                        }
                        return Err(error);
                    }
                    let digest = sha256_digest(&upstream.body);
                    let metadata = QueryMetadata {
                        namespace: namespace.to_owned(),
                        request_key: request_key.to_owned(),
                        query: query.map(str::to_owned),
                        object_type: object_type.to_owned(),
                        selected_proxy: source_name.clone(),
                        source_fingerprint: pool
                            .source_fingerprint(entry_name, &source_name)
                            .unwrap_or_default(),
                        digest: digest.clone(),
                        content_type: upstream.content_type.clone(),
                        link: upstream
                            .headers
                            .get(header::LINK)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned),
                    };
                    write_verified_artifact(
                        &self.data_dir,
                        &source_name,
                        object_type,
                        &digest,
                        &upstream.body,
                    )?;
                    write_query_metadata(&self.data_dir, entry_name, &metadata)?;
                    self.record(
                        "docker_query_read",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &query_object_key(request_key, query),
                            selected_proxy: &source_name,
                            object_type,
                            cache_status: "miss",
                            failure_category: None,
                        },
                    );
                    return Ok(QueryResult {
                        body: upstream.body,
                        content_type: metadata.content_type,
                        link: metadata.link,
                    });
                }
                Err(error) if pool.is_group(entry_name) && is_fallback_source_error(&error) => {
                    if !is_not_found_source_error(&error) {
                        all_not_found = false;
                    }
                    failures.push(format!("{source_name}: {error}"));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(DockerError::Source(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key: request_key.to_owned(),
            failures: failures.join("; "),
            status: (all_not_found && !failures.is_empty()).then_some(404),
        }))
    }

    async fn search(
        &self,
        entry_name: &str,
        query: Option<&str>,
        pool: &SourcePool,
    ) -> Result<Response, DockerError> {
        let query = parse_search_query(query)?;
        let candidates = candidate_sources(pool, entry_name, None)?;
        let mut failures = Vec::new();
        for source_name in candidates {
            match self.search_source(&source_name, &query, pool).await {
                Ok(results) => {
                    let body = serde_json::to_vec(&json!({
                        "query": query.term,
                        "results": results,
                    }))?;
                    return Ok(json_response(body, Some("application/json"), None));
                }
                Err(error) if pool.is_group(entry_name) && is_fallback_search_error(&error) => {
                    failures.push(format!("{source_name}: {error}"));
                }
                Err(error) => return Err(error),
            }
        }
        Err(DockerError::Source(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key: "v2/_catalog".to_owned(),
            failures: failures.join("; "),
            status: None,
        }))
    }

    async fn search_source(
        &self,
        source_name: &str,
        query: &SearchQuery,
        pool: &SourcePool,
    ) -> Result<Vec<Value>, DockerError> {
        let mut last = None;
        let mut results = Vec::new();
        let needle = query.term.to_ascii_lowercase();
        for _ in 0..100 {
            let page_query = catalog_page_query(last.as_deref());
            let upstream = pool
                .fetch_proxy_registry_path_with_query(
                    source_name,
                    "v2/_catalog",
                    Some(&page_query),
                    Method::GET,
                    HeaderMap::new(),
                    FetchOperation::Metadata,
                )
                .await?;
            let value: Value =
                serde_json::from_slice(&upstream.body).map_err(|_| DockerError::InvalidCatalog)?;
            validate_list_response(&value, "catalog")?;
            for repository in value["repositories"]
                .as_array()
                .expect("validated catalog repositories")
            {
                let repository = repository.as_str().ok_or(DockerError::InvalidCatalog)?;
                if repository.to_ascii_lowercase().contains(&needle) {
                    results.push(json!({ "name": repository }));
                    if results.len() >= query.limit {
                        return Ok(results);
                    }
                }
            }
            let Some(next_last) = next_catalog_last(&upstream.headers) else {
                return Ok(results);
            };
            if last.as_deref() == Some(next_last.as_str()) {
                return Err(DockerError::InvalidCatalog);
            }
            last = Some(next_last);
        }
        Err(DockerError::InvalidCatalog)
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
        .filter(|metadata| {
            source_is_current(
                pool,
                entry_name,
                &metadata.selected_proxy,
                &metadata.source_fingerprint,
            )
        });
        if let Some(metadata) = &bound
            && let Some(body) = read_verified_artifact(
                &self.data_dir,
                &metadata.selected_proxy,
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
                if let Some(cached) = read_cached_manifest(&self.data_dir, &source_name, digest)? {
                    let source_fingerprint = pool
                        .source_fingerprint(entry_name, &source_name)
                        .unwrap_or_default();
                    let metadata = ManifestMetadata {
                        repository: repository.to_owned(),
                        reference: digest.clone(),
                        digest: digest.clone(),
                        selected_proxy: source_name,
                        source_fingerprint,
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
            let source_fingerprint = pool
                .source_fingerprint(entry_name, &source_name)
                .unwrap_or_default();
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
                    ManifestSource {
                        name: &source_name,
                        fingerprint: &source_fingerprint,
                    },
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
        let path = crate::layout::metadata_root(&self.data_dir, "docker")
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
                count: None,
            },
        ) {
            crate::logging::error(format!("audit write failed: {error}"));
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
        let blob_lock = self.blob_lock(repository, digest).await;
        let _blob_guard = blob_lock.lock().await;
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
        let cached = if source_is_member(pool, entry_name, &metadata.selected_proxy) {
            read_verified_artifact_path(
                &self.data_dir,
                &metadata.selected_proxy,
                &metadata.object_type,
                digest,
            )
            .await?
        } else {
            None
        };
        if let Some((path, length)) = cached {
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
            return blob_file_response(
                &path,
                length,
                &metadata.content_type,
                digest,
                request_headers
                    .get(header::RANGE)
                    .and_then(|value| value.to_str().ok()),
                head,
            )
            .await;
        }
        let headers = HeaderMap::new();
        let requested_range = request_headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok());
        let request_key = format!("v2/{repository}/blobs/{digest}");
        let mut failures = Vec::new();
        let mut all_not_found = true;
        for source_name in candidates {
            let temporary_path = self.temporary_blob_path();
            match pool
                .fetch_proxy_registry_path_to_file(
                    &source_name,
                    &request_key,
                    Method::GET,
                    headers.clone(),
                    FetchOperation::Artifact,
                    temporary_path,
                )
                .await
            {
                Ok(upstream) => {
                    if upstream.sha256 != digest {
                        let error = DockerError::DigestMismatch {
                            expected: digest.to_owned(),
                            actual: upstream.sha256,
                        };
                        let _ = fs::remove_file(&upstream.path);
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
                    let path = publish_verified_file(
                        &self.data_dir,
                        &source_name,
                        &metadata.object_type,
                        digest,
                        &upstream.path,
                    )
                    .await?;
                    let mut selected_metadata = metadata.clone();
                    selected_metadata.selected_proxy = source_name.clone();
                    write_blob_metadata(
                        &self.data_dir,
                        entry_name,
                        repository,
                        &selected_metadata,
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
                    return blob_file_response(
                        &path,
                        upstream.length,
                        &metadata.content_type,
                        digest,
                        requested_range,
                        head,
                    )
                    .await;
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

    async fn blob_lock(&self, repository: &str, digest: &str) -> Arc<Mutex<()>> {
        let mut locks = self.blob_locks.lock().await;
        locks
            .entry(format!("{repository}\0{digest}"))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn temporary_blob_path(&self) -> PathBuf {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.data_dir
            .join("tmp")
            .join(format!("docker-{}-{counter}.part", std::process::id()))
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

fn query_object_key(request_key: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => format!("{request_key}?{query}"),
        _ => request_key.to_owned(),
    }
}

#[derive(Debug)]
struct SearchQuery {
    term: String,
    limit: usize,
}

fn validate_list_query(query: Option<&str>) -> Result<Option<String>, DockerError> {
    let Some(query) = query else {
        return Ok(None);
    };
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "n" => {
                let limit = value
                    .parse::<usize>()
                    .map_err(|_| DockerError::InvalidSearchQuery)?;
                if !(1..=1000).contains(&limit) {
                    return Err(DockerError::InvalidSearchQuery);
                }
            }
            "last" if !value.is_empty() => {}
            _ => return Err(DockerError::InvalidSearchQuery),
        }
    }
    Ok(Some(query.to_owned()))
}

fn parse_search_query(query: Option<&str>) -> Result<SearchQuery, DockerError> {
    let query = query.ok_or(DockerError::InvalidSearchQuery)?;
    let mut term = None;
    let mut limit = 25;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "q" if term.is_none() && !value.is_empty() => {
                if value.chars().any(char::is_control) {
                    return Err(DockerError::InvalidSearchQuery);
                }
                term = Some(value.into_owned());
            }
            "n" if limit == 25 => {
                limit = value
                    .parse::<usize>()
                    .map_err(|_| DockerError::InvalidSearchQuery)?;
                if !(1..=100).contains(&limit) {
                    return Err(DockerError::InvalidSearchQuery);
                }
            }
            _ => return Err(DockerError::InvalidSearchQuery),
        }
    }
    Ok(SearchQuery {
        term: term.ok_or(DockerError::InvalidSearchQuery)?,
        limit,
    })
}

fn catalog_page_query(last: Option<&str>) -> String {
    let mut query = url::form_urlencoded::Serializer::new("n=1000".to_owned());
    if let Some(last) = last {
        query.append_pair("last", last);
    }
    query.finish()
}

fn next_catalog_last(headers: &HeaderMap) -> Option<String> {
    let link = headers.get(header::LINK)?.to_str().ok()?;
    let target = link
        .split(',')
        .find(|value| value.contains("rel=\"next\""))?
        .trim();
    let target = target.strip_prefix('<')?.split_once('>')?.0;
    let query = target.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == "last").then(|| value.into_owned()))
}

fn validate_list_response(value: &Value, object_type: &str) -> Result<(), DockerError> {
    match object_type {
        "catalog" => {
            let repositories = value
                .get("repositories")
                .and_then(Value::as_array)
                .ok_or(DockerError::InvalidCatalog)?;
            if repositories.iter().all(|value| value.as_str().is_some()) {
                Ok(())
            } else {
                Err(DockerError::InvalidCatalog)
            }
        }
        "tags" => {
            let name = value.get("name").and_then(Value::as_str);
            let tags = value.get("tags").and_then(Value::as_array);
            if name.is_some()
                && tags.is_some_and(|tags| tags.iter().all(|value| value.as_str().is_some()))
            {
                Ok(())
            } else {
                Err(DockerError::InvalidTags)
            }
        }
        _ => Err(DockerError::InvalidManifest(
            "unknown Docker query response type".to_owned(),
        )),
    }
}

fn json_response(body: Vec<u8>, content_type: Option<&str>, link: Option<&str>) -> Response {
    let mut response = (StatusCode::OK, Body::from(body)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(content_type.unwrap_or("application/json"))
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/json")),
    );
    if let Some(link) = link
        && let Ok(value) = header::HeaderValue::from_str(link)
    {
        response.headers_mut().insert(header::LINK, value);
    }
    response
}

fn validate_manifest(
    repository: &str,
    reference: &ManifestReference,
    source: ManifestSource<'_>,
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
        selected_proxy: source.name.to_owned(),
        source_fingerprint: source.fingerprint.to_owned(),
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
    let root = crate::layout::metadata_root(data_dir, "docker");
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
    let path = crate::layout::metadata_root(data_dir, "docker")
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

fn read_query_metadata(
    data_dir: &Path,
    entry_name: &str,
    object_type: &str,
    namespace: &str,
    request_key: &str,
    query: Option<&str>,
) -> Result<Option<QueryMetadata>, DockerError> {
    let path = query_metadata_path(
        data_dir,
        entry_name,
        object_type,
        namespace,
        request_key,
        query,
    );
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(DockerError::Serialization)
}

fn write_query_metadata(
    data_dir: &Path,
    entry_name: &str,
    metadata: &QueryMetadata,
) -> Result<(), DockerError> {
    let path = query_metadata_path(
        data_dir,
        entry_name,
        &metadata.object_type,
        &metadata.namespace,
        &metadata.request_key,
        metadata.query.as_deref(),
    );
    atomic_write(&path, &serde_json::to_vec(metadata)?).map_err(DockerError::Storage)
}

fn query_metadata_path(
    data_dir: &Path,
    entry_name: &str,
    object_type: &str,
    namespace: &str,
    request_key: &str,
    query: Option<&str>,
) -> PathBuf {
    let identity = format!(
        "docker\0{entry_name}\0{object_type}\0{namespace}\0{request_key}\0{}",
        query.unwrap_or_default()
    );
    crate::layout::metadata_root(data_dir, "docker")
        .join(entry_name)
        .join("queries")
        .join(format!("{}.json", sha256_hex(identity.as_bytes())))
}

fn read_cached_manifest(
    data_dir: &Path,
    selected_proxy: &str,
    digest: &str,
) -> Result<Option<CachedManifest>, DockerError> {
    for object_type in ["manifest", "image-index"] {
        if let Some(body) = read_verified_artifact(data_dir, selected_proxy, object_type, digest)? {
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
    crate::layout::metadata_root(data_dir, "docker")
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
    object_type: &str,
    digest: &str,
) -> PathBuf {
    crate::layout::artifact_objects_root(data_dir, "docker", selected_proxy)
        .join(object_type)
        .join(metadata_file_key(digest))
}

// Manifest and query objects are small enough to keep in memory; blob objects
// use the file-based helpers below.
fn read_verified_artifact(
    data_dir: &Path,
    selected_proxy: &str,
    object_type: &str,
    digest: &str,
) -> Result<Option<Vec<u8>>, DockerError> {
    let path = artifact_path(data_dir, selected_proxy, object_type, digest);
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    if sha256_digest(&contents) == digest {
        let _ = crate::retention::note_access(&path);
        Ok(Some(contents))
    } else {
        fs::remove_file(path)?;
        Ok(None)
    }
}

fn write_verified_artifact(
    data_dir: &Path,
    selected_proxy: &str,
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
        &artifact_path(data_dir, selected_proxy, object_type, digest),
        contents,
    )
    .map_err(DockerError::Storage)
}

async fn read_verified_artifact_path(
    data_dir: &Path,
    selected_proxy: &str,
    object_type: &str,
    digest: &str,
) -> Result<Option<(PathBuf, u64)>, DockerError> {
    let path = artifact_path(data_dir, selected_proxy, object_type, digest);
    let digest = digest.to_owned();
    tokio::task::spawn_blocking(move || {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(DockerError::Storage(error)),
        };
        let mut file = fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if format!("sha256:{:x}", hasher.finalize()) == digest {
            let _ = crate::retention::note_access(&path);
            Ok(Some((path, metadata.len())))
        } else {
            fs::remove_file(&path)?;
            Ok(None)
        }
    })
    .await
    .map_err(|error| DockerError::Storage(io::Error::other(error)))?
}

async fn publish_verified_file(
    data_dir: &Path,
    selected_proxy: &str,
    object_type: &str,
    digest: &str,
    temporary: &Path,
) -> Result<PathBuf, DockerError> {
    let destination = artifact_path(data_dir, selected_proxy, object_type, digest);
    let temporary = temporary.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let result = (|| -> Result<(), io::Error> {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&temporary, &destination)
        })();
        match result {
            Ok(()) => Ok(destination),
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(DockerError::Storage(error))
            }
        }
    })
    .await
    .map_err(|error| DockerError::Storage(io::Error::other(error)))?
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

async fn blob_file_response(
    path: &Path,
    full_length: u64,
    content_type: &str,
    digest: &str,
    requested_range: Option<&str>,
    head: bool,
) -> Result<Response, DockerError> {
    let (status, start, response_length, content_range) = if let Some(range) = requested_range {
        let (start, end) =
            parse_range(range, full_length).ok_or(DockerError::InvalidRange(full_length))?;
        (
            StatusCode::PARTIAL_CONTENT,
            Some(start),
            end - start + 1,
            Some(format!("bytes {start}-{end}/{full_length}")),
        )
    } else {
        (StatusCode::OK, None, full_length, None)
    };
    let body = if head {
        Body::empty()
    } else {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(DockerError::Storage)?;
        if let Some(start) = start {
            let mut file = file;
            file.seek(SeekFrom::Start(start))
                .await
                .map_err(DockerError::Storage)?;
            Body::from_stream(ReaderStream::new(file.take(response_length)))
        } else {
            Body::from_stream(ReaderStream::new(file))
        }
    };
    let mut response = (status, body).into_response();
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

fn parse_range(value: &str, length: u64) -> Option<(u64, u64)> {
    if length == 0 || !value.starts_with("bytes=") || value[6..].contains(',') {
        return None;
    }
    let (start, end) = value[6..].split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some((length.saturating_sub(suffix), length - 1));
    }
    let start = start.parse::<u64>().ok()?;
    if start >= length {
        return None;
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().ok()?.min(length - 1)
    };
    (start <= end).then_some((start, end))
}
