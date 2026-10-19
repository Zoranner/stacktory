use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    http::{Method, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use url::Url;

use crate::{
    SourcePool,
    audit::{AuditDetails, AuditError, AuditSink},
    fsutil::atomic_write,
    sources::{
        FetchOperation, SourcePoolError,
        registry::{Adapter, AdapterFuture, AdapterRequest},
    },
};

use super::path::{safe_resource_url, validate_service_index_url};

const REGISTRATIONS_BASE_URL: &str = "RegistrationsBaseUrl";
const PACKAGE_BASE_ADDRESS: &str = "PackageBaseAddress";
const SEARCH_QUERY_SERVICE: &str = "SearchQueryService";
const VULNERABILITY_INFO: &str = "VulnerabilityInfo/6.7.0";

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
    #[error("NuGet package version index is invalid: {0}")]
    InvalidPackageIndex(String),
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
        let members = pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
        let cache_path = self.service_index_path(entry_name);
        let descriptor_path = self.service_index_descriptor_path(entry_name);
        if let (Some(body), Some(descriptor)) = (
            read_if_present(&cache_path)?,
            read_descriptor(&descriptor_path)?,
        ) && is_valid_cached_service_index(&body, &descriptor)
            && pool.proxy_members(entry_name).is_some_and(|configured| {
                configured
                    .iter()
                    .any(|source| source.name == descriptor.selected_proxy)
            })
        {
            self.record(
                "nuget_service_index_read",
                entry_name,
                &descriptor.selected_proxy,
                "hit",
                None,
            )?;
            return Ok(ServiceIndexSnapshot {
                body,
                selected_proxy: descriptor.selected_proxy,
                registration_url: parse_optional_url(descriptor.registration_url.clone())?,
                registration_urls: parse_url_list(
                    descriptor.registration_urls,
                    descriptor.registration_url.clone(),
                )?,
                package_base_url: parse_optional_url(descriptor.package_base_url)?,
                search_url: parse_optional_url(descriptor.search_url)?,
                vulnerability_url: parse_optional_url(descriptor.vulnerability_url)?,
            });
        }

        let mut failures = Vec::new();
        let mut all_not_found = true;
        for member in members {
            let base_url = member
                .base_url
                .as_ref()
                .expect("validated NuGet proxy must have a base URL");
            if !validate_service_index_url(base_url, member.require_https) {
                failures.push(format!("{}: invalid service index URL", member.name));
                all_not_found = false;
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
                Err(error) if is_retryable(&error) => {
                    failures.push(format!("{}: {error}", member.name));
                    all_not_found = false;
                    continue;
                }
                Err(error) if is_group_not_found(pool, entry_name, &error) => {
                    failures.push(format!("{}: {error}", member.name));
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
                    atomic_write(&cache_path, &rewritten.body)?;
                    atomic_write(&descriptor_path, &serde_json::to_vec(&descriptor)?)?;
                    self.record(
                        "nuget_service_index_published",
                        entry_name,
                        &upstream.source_name,
                        "published",
                        None,
                    )?;
                    return Ok(ServiceIndexSnapshot {
                        body: rewritten.body,
                        selected_proxy: upstream.source_name,
                        registration_url: rewritten.registration_url,
                        registration_urls: rewritten.registration_urls,
                        package_base_url: rewritten.package_base_url,
                        search_url: rewritten.search_url,
                        vulnerability_url: rewritten.vulnerability_url,
                    });
                }
                Err(error) => {
                    failures.push(format!("{}: {error}", member.name));
                    all_not_found = false;
                }
            }
        }

        self.record(
            "nuget_service_index_fetch_failed",
            entry_name,
            "",
            "failed",
            Some(if all_not_found {
                "not_found"
            } else {
                "source_failed"
            }),
        )?;
        Err(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key: "v3/index.json".to_owned(),
            failures: failures.join("; "),
            status: (pool.is_group(entry_name) && all_not_found).then_some(404),
        }
        .into())
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
            registration_url: rewritten.registration_url,
            registration_urls: rewritten.registration_urls,
            package_base_url: rewritten.package_base_url,
            search_url: rewritten.search_url,
            vulnerability_url: rewritten.vulnerability_url,
        })
    }

    pub(crate) fn publish_service_index(
        &self,
        entry_name: &str,
        snapshot: &ServiceIndexSnapshot,
    ) -> Result<(), NugetError> {
        let descriptor = ServiceIndexDescriptor {
            selected_proxy: snapshot.selected_proxy.clone(),
            registration_url: snapshot.registration_url.as_ref().map(Url::to_string),
            registration_urls: snapshot
                .registration_urls
                .iter()
                .map(Url::to_string)
                .collect(),
            package_base_url: snapshot.package_base_url.as_ref().map(Url::to_string),
            search_url: snapshot.search_url.as_ref().map(Url::to_string),
            vulnerability_url: snapshot.vulnerability_url.as_ref().map(Url::to_string),
        };
        atomic_write(&self.service_index_path(entry_name), &snapshot.body)?;
        atomic_write(
            &self.service_index_descriptor_path(entry_name),
            &serde_json::to_vec(&descriptor)?,
        )?;
        Ok(())
    }

    pub(crate) fn service_index_path(&self, entry_name: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, "nuget")
            .join(entry_name)
            .join("service-index.json")
    }

    pub(crate) fn service_index_descriptor_path(&self, entry_name: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, "nuget")
            .join(entry_name)
            .join("service-index.source")
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

fn rewrite_service_index(
    source: &[u8],
    root_url: &Url,
    entry_name: &str,
    base_url: &Url,
    require_https: bool,
) -> Result<RewrittenServiceIndex, NugetError> {
    let mut index: Value = serde_json::from_slice(source)?;
    let resources = index
        .get_mut("resources")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| NugetError::InvalidServiceIndex("resources is missing".to_owned()))?;
    let mut rewritten = Vec::with_capacity(resources.len());
    let mut registration_resource: Option<(u8, Url, String)> = None;
    let mut registration_urls = Vec::new();
    let mut package_base_url = None;
    let mut search_resource: Option<(u8, Url)> = None;
    let mut vulnerability_url = None;

    for resource in resources.drain(..) {
        let Some(object) = resource.as_object() else {
            return Err(NugetError::InvalidServiceIndex(
                "resource is not an object".to_owned(),
            ));
        };
        let Some(resource_type) = object.get("@type").and_then(Value::as_str) else {
            continue;
        };
        let Some(upstream_url) = object.get("@id").and_then(Value::as_str) else {
            if is_supported_resource(resource_type) {
                return Err(NugetError::InvalidServiceIndex(
                    "supported resource has no @id".to_owned(),
                ));
            }
            continue;
        };
        let Some(upstream_url) = Url::parse(upstream_url).ok() else {
            if is_supported_resource(resource_type) {
                return Err(NugetError::InvalidServiceIndex(
                    "supported resource @id is not a URL".to_owned(),
                ));
            }
            continue;
        };
        if resource_type.starts_with(REGISTRATIONS_BASE_URL) {
            if !safe_resource_url(base_url, require_https, &upstream_url) {
                return Err(NugetError::InvalidServiceIndex(
                    "registration resource URL is outside the configured source".to_owned(),
                ));
            }
            registration_urls.push(upstream_url.clone());
            let rank = registration_resource_rank(resource_type);
            if rank.is_some_and(|rank| {
                registration_resource
                    .as_ref()
                    .is_none_or(|(current_rank, _, _)| rank > *current_rank)
            }) {
                registration_resource = Some((
                    rank.expect("checked registration rank"),
                    upstream_url,
                    resource_type.to_owned(),
                ));
            }
        } else if resource_type.starts_with(PACKAGE_BASE_ADDRESS) {
            if package_base_url.is_some() {
                continue;
            }
            if !safe_resource_url(base_url, require_https, &upstream_url) {
                return Err(NugetError::InvalidServiceIndex(
                    "package resource URL is outside the configured source".to_owned(),
                ));
            }
            package_base_url = Some(upstream_url);
            rewritten.push(rewrite_resource(
                object,
                local_resource_url(root_url, entry_name, "package/"),
            ));
        } else if resource_type.starts_with(SEARCH_QUERY_SERVICE) {
            if !safe_resource_url(base_url, require_https, &upstream_url) {
                return Err(NugetError::InvalidServiceIndex(
                    "search resource URL is unsafe".to_owned(),
                ));
            }
            let rank = search_resource_rank(resource_type);
            if rank.is_some_and(|rank| {
                search_resource
                    .as_ref()
                    .is_none_or(|(current_rank, _)| rank > *current_rank)
            }) {
                search_resource = Some((rank.expect("checked search resource"), upstream_url));
            }
        } else if resource_type == VULNERABILITY_INFO {
            if vulnerability_url.is_some() {
                continue;
            }
            if !safe_resource_url(base_url, require_https, &upstream_url) {
                return Err(NugetError::InvalidServiceIndex(
                    "vulnerability resource URL is unsafe".to_owned(),
                ));
            }
            vulnerability_url = Some(upstream_url);
            rewritten.push(rewrite_resource(
                object,
                local_resource_url(root_url, entry_name, "vulnerability/index.json"),
            ));
        }
    }
    let registration_url = if let Some((_, url, resource_type)) = registration_resource {
        rewritten.push(rewrite_resource_with_type(
            &Map::new(),
            local_resource_url(root_url, entry_name, "registration/"),
            Some(&resource_type),
        ));
        Some(url)
    } else {
        None
    };
    if registration_url.is_none() || package_base_url.is_none() {
        return Err(NugetError::InvalidServiceIndex(
            "service index lacks a restore resource".to_owned(),
        ));
    }
    let search_url = search_resource.map(|(_, url)| url);
    if search_url.is_some() {
        rewritten.push(rewrite_resource_with_type(
            &Map::new(),
            local_resource_url(root_url, entry_name, "search/query"),
            Some("SearchQueryService/Versioned"),
        ));
    }
    *resources = rewritten;
    Ok(RewrittenServiceIndex {
        body: serde_json::to_vec(&index)?,
        registration_url,
        registration_urls,
        package_base_url,
        search_url,
        vulnerability_url,
    })
}

fn search_resource_rank(resource_type: &str) -> Option<u8> {
    match resource_type {
        "SearchQueryService/3.5.0" => Some(3),
        "SearchQueryService/3.0.0-rc" => Some(2),
        "SearchQueryService/3.0.0-beta" => Some(1),
        "SearchQueryService" => Some(0),
        _ if resource_type.starts_with("SearchQueryService/") => Some(1),
        _ => None,
    }
}

fn registration_resource_rank(resource_type: &str) -> Option<u8> {
    match resource_type {
        "RegistrationsBaseUrl/Versioned" => Some(5),
        "RegistrationsBaseUrl/3.6.0" => Some(4),
        "RegistrationsBaseUrl/3.4.0" => Some(3),
        "RegistrationsBaseUrl/3.0.0-rc" => Some(2),
        "RegistrationsBaseUrl/3.0.0-beta" => Some(1),
        "RegistrationsBaseUrl" => Some(0),
        _ if resource_type.starts_with("RegistrationsBaseUrl/") => Some(0),
        _ => None,
    }
}

fn is_supported_resource(resource_type: &str) -> bool {
    resource_type.starts_with(REGISTRATIONS_BASE_URL)
        || resource_type.starts_with(PACKAGE_BASE_ADDRESS)
}

fn rewrite_resource(resource: &Map<String, Value>, local_url: Url) -> Value {
    rewrite_resource_with_type(resource, local_url, None)
}

fn rewrite_resource_with_type(
    resource: &Map<String, Value>,
    local_url: Url,
    resource_type: Option<&str>,
) -> Value {
    let mut resource = resource.clone();
    resource.insert("@id".to_owned(), Value::String(local_url.to_string()));
    resource.remove("comment");
    if let Some(resource_type) = resource_type {
        resource.insert("@type".to_owned(), Value::String(resource_type.to_owned()));
    }
    Value::Object(resource)
}

fn local_resource_url(root_url: &Url, entry_name: &str, resource: &str) -> Url {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/mirror/nuget/{entry_name}/{resource}"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn read_descriptor(path: &Path) -> Result<Option<ServiceIndexDescriptor>, NugetError> {
    let Some(contents) = read_if_present(path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(NugetError::Serialization)
}

fn parse_optional_url(value: Option<String>) -> Result<Option<Url>, NugetError> {
    value
        .map(|value| {
            Url::parse(&value)
                .map_err(|_| NugetError::InvalidServiceIndex("stored URL is invalid".to_owned()))
        })
        .transpose()
}

fn parse_url_list(values: Vec<String>, fallback: Option<String>) -> Result<Vec<Url>, NugetError> {
    let values = if values.is_empty() {
        fallback.into_iter().collect()
    } else {
        values
    };
    values
        .into_iter()
        .map(|value| {
            Url::parse(&value)
                .map_err(|_| NugetError::InvalidServiceIndex("stored URL is invalid".to_owned()))
        })
        .collect()
}

fn is_valid_cached_service_index(body: &[u8], descriptor: &ServiceIndexDescriptor) -> bool {
    descriptor.registration_url.is_some()
        && descriptor.package_base_url.is_some()
        && serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|index| index.get("resources").and_then(Value::as_array).cloned())
            .is_some_and(|resources| {
                resources.iter().any(|resource| {
                    resource
                        .get("@type")
                        .and_then(Value::as_str)
                        .is_some_and(|resource_type| {
                            resource_type.starts_with(REGISTRATIONS_BASE_URL)
                        })
                }) && resources.iter().any(|resource| {
                    resource
                        .get("@type")
                        .and_then(Value::as_str)
                        .is_some_and(|resource_type| {
                            resource_type.starts_with(PACKAGE_BASE_ADDRESS)
                        })
                })
            })
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_retryable(error: &SourcePoolError) -> bool {
    matches!(error, SourcePoolError::AllSourcesFailed { .. })
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

fn nuget_error_response(error: &NugetError) -> Response {
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
        NugetError::Source(_)
        | NugetError::InvalidServiceIndex(_)
        | NugetError::InvalidRegistration(_)
        | NugetError::InvalidRegistrationPath(_)
        | NugetError::InvalidPackage(_)
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
