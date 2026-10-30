use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{
        HeaderMap, Method, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::io::ReaderStream;
use url::Url;

use crate::{
    PublicUrls, SourcePool,
    audit::{AuditDetails, AuditSink},
    upstream::{
        FetchOperation, GroupRun, SourcePoolError,
        pool::is_safe_configured_url_for_source,
        registry::{Adapter, AdapterFuture, AdapterRequest},
        source_error_is_group_fallback, source_error_is_not_found, source_failure_category,
    },
};

use super::{
    cache::{
        ArtifactFile, IntegrityExpectation, NpmCacheKey, NpmStorage, StorageError, StoredMetadata,
    },
    metadata::{NpmArtifactDescriptor, NpmMetadataError},
    path::{parse_metadata_path, parse_tarball_path},
};

mod metadata;
mod search;
mod tarball;

const METADATA_REVALIDATE_AFTER_SECS: u64 = 15 * 60;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RegistryFlavor {
    pub(crate) ecosystem: &'static str,
    pub(crate) route: &'static str,
    pub(crate) parse_package_path: fn(&str) -> Result<String, String>,
    pub(crate) is_valid_package: fn(&str) -> bool,
    pub(crate) upstream_package_path: fn(&str) -> String,
}

impl RegistryFlavor {
    pub(crate) const fn npm() -> Self {
        Self {
            ecosystem: "npm",
            route: "npm",
            parse_package_path: |path| {
                parse_metadata_path(path)
                    .map(|parsed| parsed.package)
                    .map_err(|error| error.to_string())
            },
            is_valid_package: |package| super::path::validate_package_name(package).is_ok(),
            upstream_package_path: |package| package.replacen('/', "%2F", 1),
        }
    }

    pub(crate) const fn upm() -> Self {
        Self {
            ecosystem: "upm",
            route: "upm",
            parse_package_path: |path| {
                crate::ecosystems::upm::path::parse_metadata_path(path)
                    .map(|parsed| parsed.package)
                    .map_err(|error| error.to_string())
            },
            is_valid_package: |package| {
                crate::ecosystems::upm::path::validate_package_name(package).is_ok()
            },
            upstream_package_path: |package| package.to_owned(),
        }
    }
}

#[derive(Debug)]
pub struct NpmAdapter {
    service: Arc<NpmService>,
}

#[derive(Debug)]
pub struct NpmService {
    flavor: RegistryFlavor,
    public_urls: PublicUrls,
    storage: NpmStorage,
    audit: AuditSink,
}

#[derive(Debug, Error)]
pub enum NpmError {
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error(transparent)]
    Metadata(#[from] NpmMetadataError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("npm artifact `{0}` is not bound to metadata")]
    MissingArtifact(String),
    #[error("all npm sources failed for `{package}`: {failures}")]
    AllSourcesFailed { package: String, failures: String },
    #[error("npm version `{package}@{version}` was not found")]
    MissingVersion { package: String, version: String },
    #[error("npm search query is invalid: {0}")]
    InvalidSearchQuery(String),
    #[error("npm search response is invalid")]
    InvalidSearchResponse,
}

impl NpmAdapter {
    pub fn new(public_urls: impl Into<PublicUrls>, data_dir: impl Into<PathBuf>) -> Self {
        Self::new_with_flavor(public_urls, data_dir, RegistryFlavor::npm())
    }

    pub(crate) fn new_with_flavor(
        public_urls: impl Into<PublicUrls>,
        data_dir: impl Into<PathBuf>,
        flavor: RegistryFlavor,
    ) -> Self {
        Self {
            service: Arc::new(NpmService::new_with_flavor(public_urls, data_dir, flavor)),
        }
    }

    pub fn service(&self) -> &NpmService {
        &self.service
    }
}

impl NpmService {
    pub fn new(public_urls: impl Into<PublicUrls>, data_dir: impl Into<PathBuf>) -> Self {
        Self::new_with_flavor(public_urls, data_dir, RegistryFlavor::npm())
    }

    pub(crate) fn new_with_flavor(
        public_urls: impl Into<PublicUrls>,
        data_dir: impl Into<PathBuf>,
        flavor: RegistryFlavor,
    ) -> Self {
        let data_dir = data_dir.into();
        let storage = NpmStorage::new_for_ecosystem(data_dir.clone(), flavor.ecosystem);
        storage.cleanup_temporary_artifacts();
        Self {
            flavor,
            public_urls: public_urls.into(),
            storage,
            audit: AuditSink::new(data_dir),
        }
    }

    pub fn public_urls(&self) -> &PublicUrls {
        &self.public_urls
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
        let path = request.raw_path.trim_start_matches('/');
        if path == "-/ping" {
            return match self
                .ping(entry_name, request.query.as_deref(), &request.headers, pool)
                .await
            {
                Ok(body) => response(StatusCode::OK, "application/json", body),
                Err(error) => npm_error_response(&error),
            };
        }
        if path == "-/v1/search" {
            return match self
                .search(entry_name, request.query.as_deref(), &request.headers, pool)
                .await
            {
                Ok(body) => response(StatusCode::OK, "application/json", body),
                Err(error) => npm_error_response(&error),
            };
        }
        if let Ok(tarball) = parse_tarball_path(path) {
            return match self
                .standard_tarball(entry_name, &tarball, pool, &request.base_url)
                .await
            {
                Ok(file) => artifact_response(file).await,
                Err(error) => npm_error_response(&error),
            };
        }
        if let Ok(package) = parse_dist_tags_path_for_flavor(self.flavor, path) {
            return match self
                .dist_tags(entry_name, &package, pool, &request.base_url)
                .await
            {
                Ok(body) => response(StatusCode::OK, "application/json", body),
                Err(error) => npm_error_response(&error),
            };
        }
        if let Ok((package, version)) = parse_version_path_for_flavor(self.flavor, path) {
            return match self
                .version(entry_name, &package, &version, pool, &request.base_url)
                .await
            {
                Ok(body) => response(StatusCode::OK, "application/json", body),
                Err(error) => npm_error_response(&error),
            };
        }
        let package = match (self.flavor.parse_package_path)(path) {
            Ok(package) => package,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        match self
            .metadata(entry_name, &package, pool, &request.base_url)
            .await
        {
            Ok(body) => response(StatusCode::OK, "application/json", body),
            Err(error) => npm_error_response(&error),
        }
    }

    async fn version(
        &self,
        entry_name: &str,
        package: &str,
        version: &str,
        pool: &SourcePool,
        base_url: &Url,
    ) -> Result<Vec<u8>, NpmError> {
        let metadata = self.metadata(entry_name, package, pool, base_url).await?;
        let body: Value = serde_json::from_slice(&metadata)
            .map_err(|error| NpmError::Metadata(NpmMetadataError::InvalidJson(error)))?;
        let version_body = body
            .get("versions")
            .and_then(Value::as_object)
            .and_then(|versions| versions.get(version))
            .cloned()
            .ok_or_else(|| NpmError::MissingVersion {
                package: package.to_owned(),
                version: version.to_owned(),
            })?;
        serde_json::to_vec(&version_body)
            .map_err(|error| NpmError::Metadata(NpmMetadataError::InvalidJson(error)))
    }

    async fn dist_tags(
        &self,
        entry_name: &str,
        package: &str,
        pool: &SourcePool,
        base_url: &Url,
    ) -> Result<Vec<u8>, NpmError> {
        let metadata = self.metadata(entry_name, package, pool, base_url).await?;
        let body: Value = serde_json::from_slice(&metadata)
            .map_err(|error| NpmError::Metadata(NpmMetadataError::InvalidJson(error)))?;
        let tags = body
            .get("dist-tags")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        serde_json::to_vec(&tags)
            .map_err(|error| NpmError::Metadata(NpmMetadataError::InvalidJson(error)))
    }

    async fn ping(
        &self,
        entry_name: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, NpmError> {
        let query = validate_read_query(query)?;
        let mut run = GroupRun::new(pool, entry_name, None)?;
        while let Some(member) = run.next_member() {
            match pool
                .fetch_proxy_registry_path_with_query(
                    &member.name,
                    "-/ping",
                    query.as_deref(),
                    Method::GET,
                    upstream_headers(headers),
                    FetchOperation::Metadata,
                )
                .await
            {
                Ok(response) => {
                    self.record(
                        &self.event("ping_read"),
                        entry_name,
                        "-/ping",
                        Some(&response.source_name),
                        "miss",
                        None,
                    );
                    return Ok(response.body);
                }
                Err(error) if run.is_group() && source_error_is_group_fallback(&error) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(NpmError::AllSourcesFailed {
            package: "-/ping".to_owned(),
            failures: run.failure_summary(),
        })
    }

    fn event(&self, suffix: &str) -> String {
        format!("{}_{}", self.flavor.ecosystem, suffix)
    }

    fn record(
        &self,
        event: &str,
        entry_name: &str,
        object_key: &str,
        selected_proxy: Option<&str>,
        cache_status: &str,
        failure_category: Option<&str>,
    ) {
        if let Err(error) = self.audit.record_detailed(
            event,
            object_key,
            if failure_category.is_some() {
                "failure"
            } else {
                "success"
            },
            AuditDetails {
                ecosystem: Some(self.flavor.ecosystem),
                entry_name: Some(entry_name),
                object_key: Some(object_key),
                selected_proxy,
                object_type: Some(if cache_status == "hit" || cache_status == "published" {
                    if event.contains("metadata") {
                        "metadata"
                    } else {
                        "artifact"
                    }
                } else {
                    "unknown"
                }),
                cache_status: Some(cache_status),
                failure_category,
                count: None,
            },
        ) {
            crate::logging::error(format!("audit write failed: {error}"));
        }
    }

    fn record_metadata_source_failure(
        &self,
        entry_name: &str,
        package: &str,
        source_name: &str,
        failure_category: &str,
    ) {
        self.record(
            &self.event("metadata_source_failed"),
            entry_name,
            package,
            Some(source_name),
            "failed",
            Some(failure_category),
        );
    }
}

impl Adapter for NpmAdapter {
    fn ecosystem(&self) -> &str {
        "npm"
    }

    fn public_urls(&self) -> &PublicUrls {
        self.service.public_urls()
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

fn expectation(descriptor: &NpmArtifactDescriptor) -> IntegrityExpectation {
    IntegrityExpectation {
        integrity: descriptor.integrity.clone(),
        shasum: descriptor.shasum.clone(),
    }
}

fn descriptors_match_source(
    descriptors: &[NpmArtifactDescriptor],
    source: &crate::SourceConfig,
) -> bool {
    let Some(base_url) = source.base_url.as_ref() else {
        return false;
    };
    descriptors.iter().all(|descriptor| {
        Url::parse(&descriptor.upstream_tarball).is_ok_and(|url| {
            is_safe_configured_url_for_source(base_url, source.require_https, &url)
        })
    })
}

fn is_allowed_source(pool: &SourcePool, entry_name: &str, source_name: &str) -> bool {
    pool.proxy_members(entry_name)
        .is_some_and(|members| members.iter().any(|member| member.name == source_name))
}

fn is_not_found_npm_error(error: &NpmError) -> bool {
    matches!(
        error,
        NpmError::Source(source_error) if source_error_is_not_found(source_error)
    )
}

fn metadata_is_stale(validated_at_unix_secs: u64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    now.saturating_sub(validated_at_unix_secs) >= METADATA_REVALIDATE_AFTER_SECS
}

fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn is_retryable_npm(error: &NpmError, pool: &SourcePool, entry_name: &str) -> bool {
    matches!(
        error,
        NpmError::Metadata(_)
            | NpmError::MissingArtifact(_)
            | NpmError::Storage(StorageError::IntegrityMismatch(_))
    ) || matches!(
        error,
        NpmError::Source(source_error)
            if pool.is_group(entry_name) && source_error_is_group_fallback(source_error)
    )
}

fn parse_dist_tags_path_for_flavor(flavor: RegistryFlavor, path: &str) -> Result<String, NpmError> {
    let package_path = path
        .strip_prefix("-/package/")
        .and_then(|path| path.strip_suffix("/dist-tags"))
        .ok_or_else(|| NpmError::MissingArtifact(path.to_owned()))?;
    (flavor.parse_package_path)(package_path)
        .map_err(|_| NpmError::MissingArtifact(package_path.to_owned()))
}

fn parse_version_path_for_flavor(
    flavor: RegistryFlavor,
    path: &str,
) -> Result<(String, String), NpmError> {
    let (package_path, version) = path
        .rsplit_once('/')
        .ok_or_else(|| NpmError::MissingArtifact(path.to_owned()))?;
    if semver::Version::parse(version).is_err() {
        return Err(NpmError::MissingArtifact(path.to_owned()));
    }
    let package = (flavor.parse_package_path)(package_path)
        .map_err(|_| NpmError::MissingArtifact(package_path.to_owned()))?;
    Ok((package, version.to_owned()))
}

fn upstream_headers(headers: &HeaderMap) -> HeaderMap {
    let mut selected = HeaderMap::new();
    if let Some(value) = headers.get("accept") {
        selected.insert("accept", value.clone());
    }
    selected
}

fn validate_read_query(query: Option<&str>) -> Result<Option<String>, NpmError> {
    let Some(query) = query else {
        return Ok(None);
    };
    validate_query_encoding(query)?;
    Ok(Some(query.to_owned()))
}

fn validate_search_query(query: Option<&str>) -> Result<Option<String>, NpmError> {
    let query = validate_read_query(query)?;
    if let Some(query_value) = query.as_deref() {
        for (key, value) in url::form_urlencoded::parse(query_value.as_bytes()) {
            if !matches!(
                key.as_ref(),
                "text" | "size" | "from" | "quality" | "popularity" | "maintenance"
            ) {
                return Err(NpmError::InvalidSearchQuery(key.into_owned()));
            }
            if matches!(key.as_ref(), "size" | "from") {
                let value = value.into_owned();
                value
                    .parse::<u32>()
                    .map_err(|_| NpmError::InvalidSearchQuery(value.clone()))?;
                continue;
            }
            if matches!(key.as_ref(), "quality" | "popularity" | "maintenance") {
                let value = value.into_owned();
                let score = value
                    .parse::<f64>()
                    .map_err(|_| NpmError::InvalidSearchQuery(value.clone()))?;
                if !(0.0..=1.0).contains(&score) {
                    return Err(NpmError::InvalidSearchQuery(value));
                }
            }
        }
    }
    Ok(query)
}

fn validate_query_encoding(query: &str) -> Result<(), NpmError> {
    let bytes = query.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(NpmError::InvalidSearchQuery(query.to_owned()));
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    if query.contains('#') || query.chars().any(char::is_control) {
        return Err(NpmError::InvalidSearchQuery(query.to_owned()));
    }
    Ok(())
}

fn npm_error_response(error: &NpmError) -> Response {
    let status = match error {
        NpmError::MissingArtifact(_)
        | NpmError::MissingVersion { .. }
        | NpmError::Metadata(NpmMetadataError::Path(_)) => StatusCode::NOT_FOUND,
        NpmError::InvalidSearchQuery(_) => StatusCode::BAD_REQUEST,
        NpmError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        NpmError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        NpmError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) if *status == 404 => StatusCode::NOT_FOUND,
        NpmError::Source(_)
        | NpmError::Metadata(_)
        | NpmError::InvalidSearchResponse
        | NpmError::AllSourcesFailed { .. } => StatusCode::BAD_GATEWAY,
        NpmError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    (status, [(CONTENT_TYPE, content_type)], Body::from(body)).into_response()
}

async fn artifact_response(file: ArtifactFile) -> Response {
    let handle = match tokio::fs::File::open(&file.path).await {
        Ok(handle) => handle,
        Err(error) => return npm_error_response(&NpmError::Storage(StorageError::Io(error))),
    };
    let mut response =
        (StatusCode::OK, Body::from_stream(ReaderStream::new(handle))).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(length) = axum::http::HeaderValue::from_str(&file.length.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
}
