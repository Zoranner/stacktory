use std::{fs, io, path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{Method, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::{
    SourcePool,
    audit::{AuditDetails, AuditError, AuditSink},
    upstream::{
        FetchOperation, GroupRun, SourcePoolError,
        registry::{Adapter, AdapterFuture, AdapterRequest},
    },
};

use super::group_fallback_source;
use super::path::validate_service_index_url;
use super::{metadata_is_stale, now_unix_secs};

mod cache;
mod protocol;

use cache::snapshot_from_descriptor;
use protocol::rewrite_service_index;

#[derive(Debug)]
pub struct NugetAdapter {
    service: Arc<NugetService>,
}

#[derive(Debug)]
pub struct NugetService {
    pub(crate) root_url: Url,
    pub(crate) data_dir: PathBuf,
    pub(crate) audit: AuditSink,
}

#[derive(Debug, Error)]
pub enum NugetError {
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error("NuGet service index is invalid: {0}")]
    InvalidServiceIndex(String),
    #[error("NuGet registration is invalid: {0}")]
    InvalidRegistration(String),
    #[error("NuGet registration path is invalid: {0}")]
    InvalidRegistrationPath(String),
    #[error("NuGet package content is invalid: {0}")]
    InvalidPackage(String),
    #[error("NuGet icon is invalid: {0}")]
    InvalidIcon(String),
    #[error("NuGet icon `{0}` was not found")]
    IconNotFound(String),
    #[error("NuGet package version index is invalid: {0}")]
    InvalidPackageIndex(String),
    #[error("NuGet search request is invalid: {0}")]
    InvalidSearchRequest(String),
    #[error("NuGet search response is invalid: {0}")]
    InvalidSearch(String),
    #[error("NuGet vulnerability index is invalid: {0}")]
    InvalidVulnerability(String),
    #[error("NuGet vulnerability path is invalid: {0}")]
    InvalidVulnerabilityPath(String),
    #[error("NuGet package content integrity check failed: {0}")]
    PackageIntegrityFailure(String),
    #[error("NuGet storage I/O failed: {0}")]
    Storage(#[from] io::Error),
    #[error("NuGet JSON serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("NuGet audit storage failed: {0}")]
    Audit(#[from] AuditError),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ServiceIndexDescriptor {
    pub(crate) selected_proxy: String,
    #[serde(default)]
    pub(crate) source_fingerprint: String,
    #[serde(default)]
    pub(crate) public_base_url: String,
    #[serde(default)]
    pub(crate) validated_at_unix_secs: u64,
    pub(crate) registration_url: Option<String>,
    #[serde(default)]
    pub(crate) registration_urls: Vec<String>,
    pub(crate) package_base_url: Option<String>,
    #[serde(default)]
    pub(crate) search_url: Option<String>,
    #[serde(default)]
    pub(crate) vulnerability_url: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ServiceIndexSnapshot {
    pub(crate) body: Vec<u8>,
    pub(crate) selected_proxy: String,
    pub(crate) source_fingerprint: String,
    pub(crate) registration_url: Option<Url>,
    pub(crate) registration_urls: Vec<Url>,
    pub(crate) package_base_url: Option<Url>,
    pub(crate) search_url: Option<Url>,
    pub(crate) vulnerability_url: Option<Url>,
}

pub(crate) struct NugetAuditDetails<'a> {
    pub(crate) object_key: &'a str,
    pub(crate) entry_name: &'a str,
    pub(crate) selected_proxy: &'a str,
    pub(crate) object_type: &'a str,
    pub(crate) cache_status: &'a str,
    pub(crate) failure_category: Option<&'a str>,
}

struct RewrittenServiceIndex {
    body: Vec<u8>,
    registration_url: Option<Url>,
    registration_urls: Vec<Url>,
    package_base_url: Option<Url>,
    search_url: Option<Url>,
    vulnerability_url: Option<Url>,
}

impl NugetAdapter {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(NugetService::new(root_url, data_dir)),
        }
    }

    pub fn service(&self) -> &NugetService {
        &self.service
    }
}

impl NugetService {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        let temporary_root = data_dir.join("tmp");
        let _ = fs::create_dir_all(&temporary_root);
        if let Ok(entries) = fs::read_dir(&temporary_root) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("nuget-") && name.ends_with(".part") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Self {
            root_url,
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
        if request.method != Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let path = request.path.trim_start_matches('/');
        if path.starts_with("registration/") {
            return super::registration::serve(self, entry_name, path, pool).await;
        }
        if path.starts_with("package/") {
            return super::package::serve(self, entry_name, path, pool).await;
        }
        if path.starts_with("icon/") {
            return super::icon::serve(self, entry_name, path, pool).await;
        }
        if path == "search/query" {
            return super::search::serve(
                self,
                entry_name,
                request.query.as_deref().unwrap_or_default(),
                pool,
            )
            .await;
        }
        if path.starts_with("vulnerability/") {
            return super::vulnerability::serve(self, entry_name, path, pool).await;
        }
        if path != "v3/index.json" {
            return StatusCode::NOT_FOUND.into_response();
        }
        match self.service_index(entry_name, pool).await {
            Ok(snapshot) => response(StatusCode::OK, "application/json", snapshot.body),
            Err(error) => nuget_error_response(&error),
        }
    }

    pub(crate) async fn service_index(
        &self,
        entry_name: &str,
        pool: &SourcePool,
    ) -> Result<ServiceIndexSnapshot, NugetError> {
        let cached = self.read_cached_service_index(entry_name, pool)?;
        if let Some((mut body, mut descriptor)) = cached {
            if descriptor.public_base_url != self.root_url.as_str() {
                if let Some(rebased) = crate::ecosystems::nuget::render::rebase_public_urls(
                    &body,
                    &descriptor.public_base_url,
                    self.root_url.as_str(),
                )? {
                    self.write_service_index_body(entry_name, &rebased)?;
                    body = rebased;
                }
                descriptor.public_base_url = self.root_url.to_string();
                self.write_service_index_descriptor(entry_name, &descriptor)?;
            }
            let stale = metadata_is_stale(descriptor.validated_at_unix_secs);
            let snapshot = snapshot_from_descriptor(body, descriptor.clone())?;
            if !stale {
                self.record(
                    "nuget_service_index_read",
                    entry_name,
                    &descriptor.selected_proxy,
                    "hit",
                    None,
                )?;
                return Ok(snapshot);
            }
            match self
                .fetch_service_index(entry_name, pool, Some(&descriptor.selected_proxy), true)
                .await
            {
                Ok(snapshot) => return Ok(snapshot),
                Err(error) if is_not_found(&error) => {
                    self.remove_service_index_cache(entry_name);
                    self.record(
                        "nuget_service_index_revalidate_failed",
                        entry_name,
                        &descriptor.selected_proxy,
                        "failed",
                        Some("not_found"),
                    )?;
                    return Err(error);
                }
                Err(_) => {
                    self.record(
                        "nuget_service_index_stale",
                        entry_name,
                        &descriptor.selected_proxy,
                        "stale",
                        Some("source_failed"),
                    )?;
                    return Ok(snapshot);
                }
            }
        }

        self.fetch_service_index(entry_name, pool, None, false)
            .await
    }

    async fn fetch_service_index(
        &self,
        entry_name: &str,
        pool: &SourcePool,
        preferred_source: Option<&str>,
        revalidating: bool,
    ) -> Result<ServiceIndexSnapshot, NugetError> {
        let mut run = GroupRun::new(pool, entry_name, preferred_source)?;
        while let Some(member) = run.next_member() {
            let base_url = member
                .base_url
                .as_ref()
                .expect("validated NuGet proxy must have a base URL");
            if !validate_service_index_url(base_url, member.require_https) {
                run.record_failure(&member.name, "invalid service index URL", false);
                continue;
            }
            let upstream = match pool
                .fetch_configured_url_following_safe_redirects(
                    &member.name,
                    base_url.clone(),
                    FetchOperation::Metadata,
                )
                .await
            {
                Ok(response) => response,
                Err(error) if group_fallback_source(&error, pool, entry_name) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        is_group_not_found(pool, entry_name, &error),
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match rewrite_service_index(
                &upstream.body,
                &self.root_url,
                entry_name,
                base_url,
                member.require_https,
            ) {
                Ok(rewritten) => {
                    let descriptor = ServiceIndexDescriptor {
                        selected_proxy: upstream.source_name.clone(),
                        source_fingerprint: member.fingerprint(),
                        public_base_url: self.root_url.to_string(),
                        validated_at_unix_secs: now_unix_secs(),
                        registration_url: rewritten.registration_url.as_ref().map(Url::to_string),
                        registration_urls: rewritten
                            .registration_urls
                            .iter()
                            .map(Url::to_string)
                            .collect(),
                        package_base_url: rewritten.package_base_url.as_ref().map(Url::to_string),
                        search_url: rewritten.search_url.as_ref().map(Url::to_string),
                        vulnerability_url: rewritten.vulnerability_url.as_ref().map(Url::to_string),
                    };
                    self.write_service_index_cache(entry_name, &rewritten.body, &descriptor)?;
                    self.record(
                        if revalidating {
                            "nuget_service_index_revalidated"
                        } else {
                            "nuget_service_index_published"
                        },
                        entry_name,
                        &upstream.source_name,
                        if revalidating {
                            "revalidated"
                        } else {
                            "published"
                        },
                        None,
                    )?;
                    return Ok(ServiceIndexSnapshot {
                        body: rewritten.body,
                        selected_proxy: upstream.source_name,
                        source_fingerprint: descriptor.source_fingerprint,
                        registration_url: rewritten.registration_url,
                        registration_urls: rewritten.registration_urls,
                        package_base_url: rewritten.package_base_url,
                        search_url: rewritten.search_url,
                        vulnerability_url: rewritten.vulnerability_url,
                    });
                }
                Err(error) => {
                    run.record_failure(&member.name, &error.to_string(), false);
                }
            }
        }

        self.record(
            "nuget_service_index_fetch_failed",
            entry_name,
            "",
            "failed",
            Some(if run.all_not_found() {
                "not_found"
            } else {
                "source_failed"
            }),
        )?;
        Err(run.all_sources_failed("v3/index.json").into())
    }

    pub(crate) async fn service_index_from_source(
        &self,
        entry_name: &str,
        source_name: &str,
        pool: &SourcePool,
    ) -> Result<ServiceIndexSnapshot, NugetError> {
        let source = pool
            .proxy_members(entry_name)
            .and_then(|members| {
                members
                    .into_iter()
                    .find(|member| member.name == source_name)
            })
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        let base_url = source
            .base_url
            .as_ref()
            .expect("validated NuGet proxy must have a base URL");
        if !validate_service_index_url(base_url, source.require_https) {
            return Err(NugetError::InvalidServiceIndex(
                "configured service index URL is invalid".to_owned(),
            ));
        }
        let upstream = pool
            .fetch_configured_url_following_safe_redirects(
                source_name,
                base_url.clone(),
                FetchOperation::Metadata,
            )
            .await?;
        let rewritten = rewrite_service_index(
            &upstream.body,
            &self.root_url,
            entry_name,
            base_url,
            source.require_https,
        )?;
        Ok(ServiceIndexSnapshot {
            body: rewritten.body,
            selected_proxy: source_name.to_owned(),
            source_fingerprint: source.fingerprint(),
            registration_url: rewritten.registration_url,
            registration_urls: rewritten.registration_urls,
            package_base_url: rewritten.package_base_url,
            search_url: rewritten.search_url,
            vulnerability_url: rewritten.vulnerability_url,
        })
    }

    fn record(
        &self,
        event: &str,
        entry_name: &str,
        selected_proxy: &str,
        cache_status: &str,
        failure_category: Option<&str>,
    ) -> Result<(), NugetError> {
        self.record_object(
            event,
            NugetAuditDetails {
                object_key: "v3/index.json",
                entry_name,
                selected_proxy,
                object_type: "service-index",
                cache_status,
                failure_category,
            },
        )
    }

    pub(crate) fn record_object(
        &self,
        event: &str,
        details: NugetAuditDetails<'_>,
    ) -> Result<(), NugetError> {
        self.audit.record_detailed(
            event,
            details.object_key,
            if details.failure_category.is_some() {
                "failure"
            } else {
                "success"
            },
            AuditDetails {
                ecosystem: Some("nuget"),
                entry_name: Some(details.entry_name),
                object_key: Some(details.object_key),
                selected_proxy: (!details.selected_proxy.is_empty())
                    .then_some(details.selected_proxy),
                object_type: Some(details.object_type),
                cache_status: Some(details.cache_status),
                failure_category: details.failure_category,
                count: None,
            },
        )?;
        Ok(())
    }
}

impl Adapter for NugetAdapter {
    fn ecosystem(&self) -> &str {
        "nuget"
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

fn is_group_not_found(pool: &SourcePool, entry_name: &str, error: &SourcePoolError) -> bool {
    pool.is_group(entry_name)
        && matches!(
            error,
            SourcePoolError::TerminalSourceError { status: 404, .. }
                | SourcePoolError::AllSourcesFailed {
                    status: Some(404),
                    ..
                }
        )
}

fn is_not_found(error: &NugetError) -> bool {
    matches!(
        error,
        NugetError::Source(SourcePoolError::TerminalSourceError { status: 404, .. })
            | NugetError::Source(SourcePoolError::AllSourcesFailed {
                status: Some(404),
                ..
            })
    )
}

pub(crate) fn nuget_error_response(error: &NugetError) -> Response {
    let status = match error {
        NugetError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        NugetError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        NugetError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        NugetError::InvalidVulnerabilityPath(_) => StatusCode::NOT_FOUND,
        NugetError::IconNotFound(_) => StatusCode::NOT_FOUND,
        NugetError::InvalidSearchRequest(_) => StatusCode::BAD_REQUEST,
        NugetError::Source(_)
        | NugetError::InvalidServiceIndex(_)
        | NugetError::InvalidRegistration(_)
        | NugetError::InvalidRegistrationPath(_)
        | NugetError::InvalidPackage(_)
        | NugetError::InvalidIcon(_)
        | NugetError::InvalidPackageIndex(_)
        | NugetError::InvalidSearch(_)
        | NugetError::InvalidVulnerability(_)
        | NugetError::PackageIntegrityFailure(_) => StatusCode::BAD_GATEWAY,
        NugetError::Storage(_) | NugetError::Serialization(_) | NugetError::Audit(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, [(CONTENT_TYPE, "application/json")], Body::empty()).into_response()
}

fn response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response {
    let mut response = (status, Body::from(body)).into_response();
    let content_type = axum::http::HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response
}
