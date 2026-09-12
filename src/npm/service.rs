use std::{io, path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{Method, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use thiserror::Error;
use url::Url;

use crate::{
    SourcePool,
    audit::{AuditDetails, AuditSink},
    sources::{
        FetchOperation, SourcePoolError,
        pool::is_safe_configured_url_for_source,
        registry::{Adapter, AdapterFuture, AdapterRequest},
    },
};

use super::{
    metadata::{NpmArtifactDescriptor, NpmMetadataError},
    path::{parse_artifact_path, parse_metadata_path},
    storage::{IntegrityExpectation, NpmCacheKey, NpmStorage, StorageError},
};

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
                crate::upm::path::parse_metadata_path(path)
                    .map(|parsed| parsed.package)
                    .map_err(|error| error.to_string())
            },
            is_valid_package: |package| crate::upm::path::validate_package_name(package).is_ok(),
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
    root_url: Url,
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
}

impl NpmAdapter {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self::new_with_flavor(root_url, data_dir, RegistryFlavor::npm())
    }

    pub(crate) fn new_with_flavor(
        root_url: Url,
        data_dir: impl Into<PathBuf>,
        flavor: RegistryFlavor,
    ) -> Self {
        Self {
            service: Arc::new(NpmService::new_with_flavor(root_url, data_dir, flavor)),
        }
    }

    pub fn service(&self) -> &NpmService {
        &self.service
    }
}

impl NpmService {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self::new_with_flavor(root_url, data_dir, RegistryFlavor::npm())
    }

    pub(crate) fn new_with_flavor(
        root_url: Url,
        data_dir: impl Into<PathBuf>,
        flavor: RegistryFlavor,
    ) -> Self {
        let data_dir = data_dir.into();
        Self {
            flavor,
            root_url,
            storage: NpmStorage::new_for_ecosystem(data_dir.clone(), flavor.ecosystem),
            audit: AuditSink::new(data_dir),
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
        let path = request.raw_path.trim_start_matches('/');
        if let Some(artifact_key) = path.strip_prefix("tarballs/") {
            let artifact_key = match parse_artifact_path(artifact_key) {
                Ok(path) => path.artifact_key,
                Err(_) => return StatusCode::NOT_FOUND.into_response(),
            };
            return match self.tarball(entry_name, &artifact_key, pool).await {
                Ok(body) => response(StatusCode::OK, "application/octet-stream", body),
                Err(error) => npm_error_response(&error),
            };
        }
        let package = match (self.flavor.parse_package_path)(path) {
            Ok(package) => package,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        match self.metadata(entry_name, &package, pool).await {
            Ok(body) => response(StatusCode::OK, "application/json", body),
            Err(error) => npm_error_response(&error),
        }
    }

    pub async fn metadata(
        &self,
        entry_name: &str,
        package: &str,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, NpmError> {
        if let Some(snapshot) = self.storage.read_metadata(entry_name, package)?
            && is_allowed_source(pool, entry_name, &snapshot.key.selected_proxy)
        {
            self.record(
                &self.event("metadata_read"),
                entry_name,
                package,
                Some(&snapshot.key.selected_proxy),
                "hit",
                None,
            );
            return Ok(snapshot.body);
        }

        let request_key = (self.flavor.upstream_package_path)(package);
        let members = pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
        let mut failures = Vec::new();
        let mut all_not_found = true;
        for member in members {
            let response = match pool
                .fetch_proxy_path(&member.name, &request_key, FetchOperation::Metadata)
                .await
            {
                Ok(response) => response,
                Err(error) if is_retryable_source(&error) => {
                    all_not_found = false;
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) if is_not_found(&error) && pool.is_group(entry_name) => {
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let metadata = match crate::npm::metadata::parse_compatible_metadata(
                &response.body,
                package,
                self.flavor.is_valid_package,
            ) {
                Ok(metadata) => metadata,
                Err(error) => {
                    all_not_found = false;
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
            };
            let rewritten = match crate::npm::metadata::rewrite_compatible_metadata(
                &metadata,
                &self.root_url,
                self.flavor.ecosystem,
                self.flavor.route,
                entry_name,
                &response.source_name,
                self.flavor.is_valid_package,
            ) {
                Ok(rewritten) => rewritten,
                Err(error) => {
                    all_not_found = false;
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
            };
            if !descriptors_match_source(&rewritten.descriptors, &member) {
                all_not_found = false;
                failures.push(format!(
                    "{}: metadata tarball URL is outside the configured source",
                    member.name
                ));
                continue;
            }
            let body = serde_json::to_vec(&rewritten.body)
                .map_err(|error| StorageError::Io(io::Error::other(error)))?;
            let key = NpmCacheKey {
                ecosystem: self.flavor.ecosystem.to_owned(),
                entry: entry_name.to_owned(),
                protocol_variant: "npm-compatible-registry".to_owned(),
                package: package.to_owned(),
                version: "catalog".to_owned(),
                representation: "metadata".to_owned(),
                selected_proxy: response.source_name.clone(),
            };
            self.storage
                .publish_metadata(&key, &body, &rewritten.descriptors)?;
            self.record(
                &self.event("metadata_published"),
                entry_name,
                package,
                Some(&response.source_name),
                "published",
                None,
            );
            return Ok(body);
        }

        self.record(
            &self.event("metadata_fetch_failed"),
            entry_name,
            package,
            None,
            "failed",
            Some(if all_not_found {
                "not_found"
            } else {
                "source_failed"
            }),
        );
        if all_not_found {
            return Err(NpmError::Source(SourcePoolError::AllSourcesFailed {
                entry_name: entry_name.to_owned(),
                request_key,
                failures: failures.join("; "),
                status: Some(404),
            }));
        }
        Err(NpmError::AllSourcesFailed {
            package: package.to_owned(),
            failures: failures.join("; "),
        })
    }

    pub async fn tarball(
        &self,
        entry_name: &str,
        artifact_key: &str,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, NpmError> {
        let descriptor = self
            .storage
            .read_descriptor(entry_name, artifact_key)?
            .ok_or_else(|| NpmError::MissingArtifact(artifact_key.to_owned()))?;
        if !is_allowed_source(pool, entry_name, &descriptor.selected_proxy) {
            return Err(NpmError::MissingArtifact(artifact_key.to_owned()));
        }

        let mut members = pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
        if let Some(index) = members
            .iter()
            .position(|member| member.name == descriptor.selected_proxy)
        {
            let selected = members.remove(index);
            members.insert(0, selected);
        }
        let mut failures = Vec::new();
        for member in members {
            let candidate = if member.name == descriptor.selected_proxy {
                descriptor.clone()
            } else {
                match self
                    .descriptor_from_source(
                        entry_name,
                        &descriptor.package,
                        &descriptor.version,
                        &member,
                        pool,
                    )
                    .await
                {
                    Ok(candidate) => candidate,
                    Err(error) if is_retryable_npm(&error, pool, entry_name) => {
                        failures.push(format!("{}: {error}", member.name));
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };
            let key = NpmCacheKey {
                ecosystem: self.flavor.ecosystem.to_owned(),
                entry: entry_name.to_owned(),
                protocol_variant: "npm-compatible-registry".to_owned(),
                package: candidate.package.clone(),
                version: candidate.version.clone(),
                representation: "tarball".to_owned(),
                selected_proxy: member.name.clone(),
            };
            let expectation = expectation(&candidate);
            if let Some(contents) =
                self.storage
                    .read_verified_artifact(&key, artifact_key, &expectation)?
            {
                self.record(
                    &self.event("artifact_read"),
                    entry_name,
                    artifact_key,
                    Some(&member.name),
                    "hit",
                    None,
                );
                return Ok(contents);
            }
            let upstream_url = Url::parse(&candidate.upstream_tarball).map_err(|_| {
                NpmError::Metadata(NpmMetadataError::InvalidTarballUrl(
                    candidate.upstream_tarball.clone(),
                ))
            })?;
            let response = match pool
                .fetch_configured_url(&member.name, upstream_url, FetchOperation::Artifact)
                .await
            {
                Ok(response) => response,
                Err(error) if is_retryable_source(&error) => {
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) if is_not_found(&error) && pool.is_group(entry_name) => {
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match self.storage.publish_verified_artifact(
                &key,
                artifact_key,
                &expectation,
                &response.body,
            ) {
                Ok(_) => {
                    self.record(
                        &self.event("artifact_published"),
                        entry_name,
                        artifact_key,
                        Some(&member.name),
                        "published",
                        None,
                    );
                    return Ok(response.body);
                }
                Err(StorageError::IntegrityMismatch(_)) => {
                    failures.push(format!("{}: integrity mismatch", member.name));
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }

        self.record(
            &self.event("artifact_fetch_failed"),
            entry_name,
            artifact_key,
            None,
            "failed",
            Some("source_failed"),
        );
        Err(NpmError::AllSourcesFailed {
            package: descriptor.package,
            failures: failures.join("; "),
        })
    }

    async fn descriptor_from_source(
        &self,
        entry_name: &str,
        package: &str,
        version: &str,
        member: &crate::SourceConfig,
        pool: &SourcePool,
    ) -> Result<NpmArtifactDescriptor, NpmError> {
        let response = pool
            .fetch_proxy_path(
                &member.name,
                &(self.flavor.upstream_package_path)(package),
                FetchOperation::Metadata,
            )
            .await?;
        let metadata = crate::npm::metadata::parse_compatible_metadata(
            &response.body,
            package,
            self.flavor.is_valid_package,
        )?;
        let rewritten = crate::npm::metadata::rewrite_compatible_metadata(
            &metadata,
            &self.root_url,
            self.flavor.ecosystem,
            self.flavor.route,
            entry_name,
            &member.name,
            self.flavor.is_valid_package,
        )?;
        if !descriptors_match_source(&rewritten.descriptors, member) {
            return Err(NpmError::Metadata(NpmMetadataError::InvalidTarballUrl(
                package.to_owned(),
            )));
        }
        rewritten
            .descriptors
            .into_iter()
            .find(|descriptor| descriptor.version == version)
            .ok_or_else(|| NpmError::MissingArtifact(version.to_owned()))
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
            },
        ) {
            eprintln!("Stacktory audit write failed: {error}");
        }
    }
}

impl Adapter for NpmAdapter {
    fn ecosystem(&self) -> &str {
        "npm"
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

fn is_retryable_source(error: &SourcePoolError) -> bool {
    matches!(error, SourcePoolError::AllSourcesFailed { .. })
}

fn is_not_found(error: &SourcePoolError) -> bool {
    matches!(
        error,
        SourcePoolError::TerminalSourceError { status: 404, .. }
            | SourcePoolError::AllSourcesFailed {
                status: Some(404),
                ..
            }
    )
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
            if is_retryable_source(source_error)
                || (pool.is_group(entry_name) && is_not_found(source_error))
    )
}

fn npm_error_response(error: &NpmError) -> Response {
    let status = match error {
        NpmError::MissingArtifact(_) | NpmError::Metadata(NpmMetadataError::Path(_)) => {
            StatusCode::NOT_FOUND
        }
        NpmError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        NpmError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        NpmError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) if *status == 404 => StatusCode::NOT_FOUND,
        NpmError::Source(_) | NpmError::Metadata(_) | NpmError::AllSourcesFailed { .. } => {
            StatusCode::BAD_GATEWAY
        }
        NpmError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    (status, [(CONTENT_TYPE, content_type)], Body::from(body)).into_response()
}
