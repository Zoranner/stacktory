use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{
        StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::io::ReaderStream;
use url::Url;

use crate::{
    SourcePool,
    audit::{AuditDetails, AuditSink},
    fsutil::atomic_write,
    sources::registry::{Adapter, AdapterFuture, AdapterRequest},
    sources::{FetchOperation, SourcePoolError, source_error_is_group_fallback},
};

use super::{
    index::{
        CargoIndexError, download_template, parse_index_lines, parse_registry_config,
        render_download_url_for_source, rewrite_index_lines, sparse_path_for_request,
    },
    path::{CargoPath, CargoPathError, normalize_crate_name, validate_version},
};

const INDEX_REVALIDATE_AFTER_SECS: u64 = 15 * 60;

#[derive(Debug)]
pub struct CargoAdapter {
    service: Arc<CargoService>,
}

#[derive(Debug)]
pub struct CargoService {
    root_url: Url,
    data_dir: PathBuf,
    audit: AuditSink,
}

#[derive(Debug)]
struct IndexSnapshot {
    body: Vec<u8>,
    selected_proxy: String,
}

struct FetchedIndex {
    body: Vec<u8>,
    source_name: String,
}

struct CrateFile {
    path: PathBuf,
    length: u64,
}

enum CargoPayload {
    Bytes(&'static str, Vec<u8>),
    File(CrateFile),
}

#[derive(Debug, Error)]
pub enum CargoError {
    #[error(transparent)]
    Path(#[from] CargoPathError),
    #[error(transparent)]
    Index(#[from] CargoIndexError),
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error("cargo crate `{name}` version `{version}` was not found in the index")]
    MissingVersion { name: String, version: String },
    #[error("cargo storage I/O failed: {0}")]
    Storage(#[from] io::Error),
}

impl CargoAdapter {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(CargoService::new(root_url, data_dir)),
        }
    }

    pub fn service(&self) -> &CargoService {
        &self.service
    }
}

impl CargoService {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        cleanup_temporary_artifacts(&data_dir);
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
            Ok(CargoPayload::Bytes(content_type, body)) => {
                response(StatusCode::OK, content_type, body)
            }
            Ok(CargoPayload::File(file)) => crate_file_response(file).await,
            Err(error) => cargo_error_response(&error),
        }
    }

    async fn serve(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
    ) -> Result<CargoPayload, CargoError> {
        if pool.proxy_members(entry_name).is_none() {
            return Err(SourcePoolError::UnknownEntry(entry_name.to_owned()).into());
        }
        if path == "config.json" {
            return Ok(CargoPayload::Bytes(
                "application/json",
                self.config(entry_name)?,
            ));
        }
        if let Ok((crate_name, _)) = sparse_path_for_request(path) {
            let body = self.index(entry_name, path, &crate_name, pool).await?;
            return Ok(CargoPayload::Bytes("application/json", body.body));
        }
        if let Some(crate_path) = path.strip_prefix("crates/") {
            let (crate_name, version) = parse_crate_request(crate_path)?;
            let file = self
                .crate_file(entry_name, &crate_name, &version, pool)
                .await?;
            return Ok(CargoPayload::File(file));
        }
        Err(CargoPathError::Invalid(path.to_owned()).into())
    }

    fn config(&self, entry_name: &str) -> Result<Vec<u8>, CargoError> {
        let body = json!({
            "dl": download_template(&self.root_url, entry_name),
        });
        Ok(serde_json::to_vec(&body).expect("static cargo config is serializable"))
    }

    async fn index(
        &self,
        entry_name: &str,
        index_path: &str,
        crate_name: &str,
        pool: &SourcePool,
    ) -> Result<IndexSnapshot, CargoError> {
        let cache_path = self.metadata_path(entry_name, index_path);
        let sidecar_path = self.metadata_sidecar_path(entry_name, index_path);
        if let Some(contents) = read_if_present(&cache_path)? {
            let selected_proxy = read_if_present(&sidecar_path)?
                .and_then(|contents| String::from_utf8(contents).ok())
                .map(|contents| contents.trim().to_owned())
                .filter(|source_name| {
                    pool.proxy_members(entry_name).is_some_and(|members| {
                        members.iter().any(|member| member.name == *source_name)
                    })
                });
            let valid_cache = std::str::from_utf8(&contents)
                .ok()
                .and_then(|contents| parse_index_lines(contents).ok())
                .is_some_and(|records| {
                    records.iter().any(|record| {
                        normalize_crate_name(&record.name).is_ok_and(|name| name == crate_name)
                    })
                });
            if let Some(selected_proxy) = selected_proxy.filter(|_| valid_cache) {
                if index_is_stale(&cache_path) {
                    match self
                        .fetch_index(
                            entry_name,
                            index_path,
                            crate_name,
                            pool,
                            Some(&selected_proxy),
                        )
                        .await
                    {
                        Ok(fetched) if fetched.body == contents => {
                            let _ = crate::retention::note_access(&cache_path);
                            self.record_cargo(
                                "cargo_index_revalidated",
                                entry_name,
                                index_path,
                                CargoAuditDetails {
                                    selected_proxy: Some(&selected_proxy),
                                    object_type: "index",
                                    cache_status: "hit",
                                    failure_category: None,
                                },
                            );
                        }
                        Ok(fetched) => {
                            self.publish_index_cache(
                                &cache_path,
                                &sidecar_path,
                                entry_name,
                                index_path,
                                &fetched,
                                "revalidated",
                            )?;
                            return Ok(IndexSnapshot {
                                body: fetched.body,
                                selected_proxy: fetched.source_name,
                            });
                        }
                        Err(error) if is_not_found_cargo_error(&error) => {
                            fs::remove_file(&cache_path)?;
                            let _ = fs::remove_file(&sidecar_path);
                            self.record_cargo(
                                "cargo_index_stale",
                                entry_name,
                                index_path,
                                CargoAuditDetails {
                                    selected_proxy: Some(&selected_proxy),
                                    object_type: "index",
                                    cache_status: "failed",
                                    failure_category: Some("not_found"),
                                },
                            );
                            return Err(error);
                        }
                        Err(_) => {
                            self.record_cargo(
                                "cargo_index_stale",
                                entry_name,
                                index_path,
                                CargoAuditDetails {
                                    selected_proxy: Some(&selected_proxy),
                                    object_type: "index",
                                    cache_status: "stale",
                                    failure_category: Some("source_failed"),
                                },
                            );
                            return Ok(IndexSnapshot {
                                body: contents,
                                selected_proxy,
                            });
                        }
                    }
                }
                self.record_cargo(
                    "cargo_index_read",
                    entry_name,
                    index_path,
                    CargoAuditDetails {
                        selected_proxy: Some(&selected_proxy),
                        object_type: "index",
                        cache_status: "hit",
                        failure_category: None,
                    },
                );
                return Ok(IndexSnapshot {
                    body: contents,
                    selected_proxy,
                });
            }
            fs::remove_file(&cache_path)?;
            let _ = fs::remove_file(&sidecar_path);
            self.record_cargo(
                "cargo_index_cache_invalid",
                entry_name,
                index_path,
                CargoAuditDetails {
                    selected_proxy: None,
                    object_type: "index",
                    cache_status: "miss",
                    failure_category: Some("invalid_cache"),
                },
            );
        }

        let fetched = self
            .fetch_index(entry_name, index_path, crate_name, pool, None)
            .await?;
        self.publish_index_cache(
            &cache_path,
            &sidecar_path,
            entry_name,
            index_path,
            &fetched,
            "published",
        )?;
        Ok(IndexSnapshot {
            body: fetched.body,
            selected_proxy: fetched.source_name,
        })
    }

    async fn fetch_index(
        &self,
        entry_name: &str,
        index_path: &str,
        crate_name: &str,
        pool: &SourcePool,
        preferred_source: Option<&str>,
    ) -> Result<FetchedIndex, CargoError> {
        let request_key = index_path.to_owned();
        let mut members = pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
        if let Some(preferred_source) = preferred_source
            && let Some(index) = members
                .iter()
                .position(|member| member.name == preferred_source)
        {
            let preferred = members.remove(index);
            members.insert(0, preferred);
        }
        let mut failures = Vec::new();
        let mut saw_failure = false;
        let mut all_not_found = true;
        for member in members {
            match pool
                .fetch(&member.name, &request_key, FetchOperation::Metadata)
                .await
            {
                Ok(response) => {
                    let source = std::str::from_utf8(&response.body).map_err(|_| {
                        CargoIndexError::InvalidRecord {
                            line: 0,
                            error: "index is not UTF-8".to_owned(),
                        }
                    });
                    match source
                        .and_then(|source| rewrite_index_lines(source, &self.root_url, entry_name))
                    {
                        Ok(contents) => {
                            let records =
                                parse_index_lines(std::str::from_utf8(&contents).map_err(
                                    |_| CargoIndexError::InvalidRecord {
                                        line: 0,
                                        error: "index is not UTF-8".to_owned(),
                                    },
                                )?)?;
                            if !records.iter().any(|record| {
                                normalize_crate_name(&record.name)
                                    .is_ok_and(|name| name == crate_name)
                            }) {
                                saw_failure = true;
                                all_not_found = false;
                                failures.push(format!(
                                    "{}: index does not contain the requested crate",
                                    member.name
                                ));
                                continue;
                            }
                            return Ok(FetchedIndex {
                                body: contents,
                                source_name: response.source_name,
                            });
                        }
                        Err(error) => {
                            saw_failure = true;
                            all_not_found = false;
                            failures.push(format!("{}: {error}", member.name));
                        }
                    }
                }
                Err(error)
                    if is_group_entry(pool, entry_name)
                        && source_error_is_group_fallback(&error) =>
                {
                    saw_failure = true;
                    if terminal_status(&error) != Some(404) {
                        all_not_found = false;
                    }
                    failures.push(format!("{}: {error}", member.name));
                }
                Err(error) => return Err(error.into()),
            }
        }

        self.record_cargo(
            "cargo_index_fetch_failed",
            entry_name,
            index_path,
            CargoAuditDetails {
                selected_proxy: None,
                object_type: "index",
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
            status: (is_group_entry(pool, entry_name) && saw_failure && all_not_found)
                .then_some(404),
        }
        .into())
    }

    fn publish_index_cache(
        &self,
        cache_path: &Path,
        sidecar_path: &Path,
        entry_name: &str,
        index_path: &str,
        fetched: &FetchedIndex,
        cache_status: &str,
    ) -> Result<(), CargoError> {
        atomic_write(cache_path, &fetched.body)?;
        atomic_write(
            sidecar_path,
            format!("{}\n", fetched.source_name).as_bytes(),
        )?;
        self.record_cargo(
            &format!("cargo_index_{cache_status}"),
            entry_name,
            index_path,
            CargoAuditDetails {
                selected_proxy: Some(&fetched.source_name),
                object_type: "index",
                cache_status,
                failure_category: None,
            },
        );
        Ok(())
    }

    async fn crate_file(
        &self,
        entry_name: &str,
        crate_name: &str,
        version: &str,
        pool: &SourcePool,
    ) -> Result<CrateFile, CargoError> {
        let index_path = super::path::sparse_index_path(crate_name)?;
        let snapshot = self
            .index(entry_name, &index_path, crate_name, pool)
            .await?;
        let records = parse_index_lines(std::str::from_utf8(&snapshot.body).map_err(|_| {
            CargoIndexError::InvalidRecord {
                line: 0,
                error: "index is not UTF-8".to_owned(),
            }
        })?)?;
        let (download_name, expected) = records
            .iter()
            .find(|record| {
                normalize_crate_name(&record.name).is_ok_and(|name| name == crate_name)
                    && record.version == version
            })
            .map(|record| (record.name.clone(), record.checksum.clone()))
            .ok_or_else(|| CargoError::MissingVersion {
                name: crate_name.to_owned(),
                version: version.to_owned(),
            })?;

        let cache_path = self.artifact_path(&snapshot.selected_proxy, crate_name, version);
        if let Some(file) = cached_crate_file(&cache_path, &expected)? {
            let _ = crate::retention::note_access(&cache_path);
            self.record_cargo(
                "cargo_artifact_read",
                entry_name,
                &format!("crates/{crate_name}/{version}.crate"),
                CargoAuditDetails {
                    selected_proxy: Some(&snapshot.selected_proxy),
                    object_type: "artifact",
                    cache_status: "hit",
                    failure_category: None,
                },
            );
            return Ok(file);
        }

        let request_key = format!("crates/{crate_name}/{version}.crate");
        let mut failures = Vec::new();
        let mut members = pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
        if let Some(position) = members
            .iter()
            .position(|member| member.name == snapshot.selected_proxy)
        {
            let selected = members.remove(position);
            members.insert(0, selected);
        }
        let is_group = is_group_entry(pool, entry_name);
        let mut saw_failure = false;
        let mut all_not_found = true;
        self.record_cargo(
            "cargo_artifact_read",
            entry_name,
            &request_key,
            CargoAuditDetails {
                selected_proxy: Some(&snapshot.selected_proxy),
                object_type: "artifact",
                cache_status: "miss",
                failure_category: None,
            },
        );
        for member in members {
            let config = match pool
                .fetch(&member.name, "config.json", FetchOperation::Metadata)
                .await
            {
                Ok(response) => match parse_registry_config(&response.body) {
                    Ok(config) => config,
                    Err(error) => {
                        saw_failure = true;
                        all_not_found = false;
                        self.record_cargo(
                            "cargo_artifact_fallback",
                            entry_name,
                            &request_key,
                            CargoAuditDetails {
                                selected_proxy: Some(&member.name),
                                object_type: "artifact",
                                cache_status: "fallback",
                                failure_category: Some("invalid_config"),
                            },
                        );
                        failures.push(format!("{}: {error}", member.name));
                        continue;
                    }
                },
                Err(error) if is_group && source_error_is_group_fallback(&error) => {
                    saw_failure = true;
                    if terminal_status(&error) != Some(404) {
                        all_not_found = false;
                    }
                    self.record_cargo(
                        "cargo_artifact_fallback",
                        entry_name,
                        &request_key,
                        CargoAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "fallback",
                            failure_category: Some(if terminal_status(&error) == Some(404) {
                                "not_found"
                            } else {
                                "source_unavailable"
                            }),
                        },
                    );
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let download_url = match render_download_url_for_source(
                &config.dl,
                &download_name,
                version,
                &expected,
                member
                    .base_url
                    .as_ref()
                    .expect("validated proxy source must have a base URL"),
                member.require_https,
            ) {
                Ok(url) => url,
                Err(error) => {
                    saw_failure = true;
                    all_not_found = false;
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
            };
            let temporary_path = temporary_artifact_path(&self.data_dir)?;
            let fetched = match pool
                .fetch_configured_url_following_safe_redirects_to_file(
                    &member.name,
                    download_url,
                    FetchOperation::Artifact,
                    temporary_path,
                )
                .await
            {
                Ok(file) => file,
                Err(error)
                    if is_group
                        && (source_error_is_group_fallback(&error)
                            || matches!(error, SourcePoolError::InvalidConfiguredUrl(_))) =>
                {
                    saw_failure = true;
                    if terminal_status(&error) != Some(404) {
                        all_not_found = false;
                    }
                    self.record_cargo(
                        "cargo_artifact_fallback",
                        entry_name,
                        &request_key,
                        CargoAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "fallback",
                            failure_category: Some(if terminal_status(&error) == Some(404) {
                                "not_found"
                            } else {
                                "source_unavailable"
                            }),
                        },
                    );
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match checksum_file(&fetched.path) {
                Ok(actual) if actual.eq_ignore_ascii_case(&expected) => {
                    let destination = self.artifact_path(&member.name, crate_name, version);
                    publish_crate_file(&fetched.path, &destination)?;
                    atomic_write(
                        &self.metadata_sidecar_path(entry_name, &index_path),
                        format!("{}\n", member.name).as_bytes(),
                    )?;
                    self.record_cargo(
                        "cargo_artifact_published",
                        entry_name,
                        &request_key,
                        CargoAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "published",
                            failure_category: None,
                        },
                    );
                    return crate_file_at(&destination);
                }
                Ok(_) => {
                    let _ = fs::remove_file(&fetched.path);
                    saw_failure = true;
                    all_not_found = false;
                    failures.push(format!(
                        "{}: checksum mismatch for {crate_name} {version}",
                        member.name
                    ));
                    self.record_cargo(
                        "cargo_artifact_fallback",
                        entry_name,
                        &request_key,
                        CargoAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "fallback",
                            failure_category: Some("integrity_mismatch"),
                        },
                    );
                }
                Err(error) => {
                    let _ = fs::remove_file(&fetched.path);
                    return Err(error.into());
                }
            }
        }

        self.record_cargo(
            "cargo_artifact_fetch_failed",
            entry_name,
            &request_key,
            CargoAuditDetails {
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

    fn metadata_path(&self, entry_name: &str, object_key: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, "cargo")
            .join(entry_name)
            .join("index")
            .join(object_key)
    }

    fn metadata_sidecar_path(&self, entry_name: &str, object_key: &str) -> PathBuf {
        let path = self.metadata_path(entry_name, object_key);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("metadata path must have a file name");
        path.with_file_name(format!("{file_name}.source"))
    }

    fn artifact_path(&self, source_name: &str, crate_name: &str, version: &str) -> PathBuf {
        crate::layout::artifact_objects_root(&self.data_dir, "cargo", source_name)
            .join("crates")
            .join(crate_name)
            .join(format!("{version}.crate"))
    }

    fn record_cargo(
        &self,
        event: &str,
        entry_name: &str,
        object_key: &str,
        details: CargoAuditDetails<'_>,
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
                ecosystem: Some("cargo"),
                entry_name: Some(entry_name),
                object_key: Some(object_key),
                selected_proxy: details.selected_proxy,
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

struct CargoAuditDetails<'a> {
    selected_proxy: Option<&'a str>,
    object_type: &'a str,
    cache_status: &'a str,
    failure_category: Option<&'a str>,
}

impl Adapter for CargoAdapter {
    fn ecosystem(&self) -> &str {
        "cargo"
    }

    fn handle(
        &self,
        source_name: String,
        request: AdapterRequest,
        pool: Arc<SourcePool>,
    ) -> AdapterFuture {
        let service = self.service.clone();
        Box::pin(async move {
            if request.method != axum::http::Method::GET {
                return axum::http::StatusCode::METHOD_NOT_ALLOWED.into_response();
            }
            service
                .handle_request(&source_name, &request.path, &pool)
                .await
        })
    }
}

fn parse_crate_request(path: &str) -> Result<(String, String), CargoPathError> {
    let path = CargoPath::parse(path)?;
    let mut parts = path.as_str().split('/');
    let crate_name = normalize_crate_name(
        parts
            .next()
            .ok_or_else(|| CargoPathError::Invalid(path.to_string()))?,
    )?;
    let version_file = parts
        .next()
        .ok_or_else(|| CargoPathError::Invalid(path.to_string()))?;
    if parts.next().is_some() {
        return Err(CargoPathError::Invalid(path.to_string()));
    }
    let version = version_file
        .strip_suffix(".crate")
        .ok_or_else(|| CargoPathError::Invalid(path.to_string()))?;
    validate_version(version)?;
    Ok((crate_name, version.to_owned()))
}

fn cargo_error_response(error: &CargoError) -> Response {
    let status = match error {
        CargoError::Path(_)
        | CargoError::MissingVersion { .. }
        | CargoError::Index(CargoIndexError::Path(_)) => StatusCode::NOT_FOUND,
        CargoError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        CargoError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        CargoError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        CargoError::Source(_) | CargoError::Index(_) => StatusCode::BAD_GATEWAY,
        CargoError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    (status, [(CONTENT_TYPE, content_type)], Body::from(body)).into_response()
}

fn terminal_status(error: &SourcePoolError) -> Option<u16> {
    match error {
        SourcePoolError::TerminalSourceError { status, .. } => Some(*status),
        SourcePoolError::AllSourcesFailed { status, .. } => *status,
        _ => None,
    }
}

fn is_not_found_cargo_error(error: &CargoError) -> bool {
    matches!(
        error,
        CargoError::Source(SourcePoolError::TerminalSourceError { status: 404, .. })
            | CargoError::Source(SourcePoolError::AllSourcesFailed {
                status: Some(404),
                ..
            })
    )
}

fn index_is_stale(path: &Path) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= Duration::from_secs(INDEX_REVALIDATE_AFTER_SECS))
}

fn is_group_entry(pool: &SourcePool, entry_name: &str) -> bool {
    pool.is_group(entry_name)
}

fn checksum_file(path: &Path) -> Result<String, io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn cached_crate_file(path: &Path, expected_checksum: &str) -> Result<Option<CrateFile>, io::Error> {
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if checksum_file(path)?.eq_ignore_ascii_case(expected_checksum) {
        return Ok(Some(CrateFile {
            path: path.to_path_buf(),
            length,
        }));
    }
    fs::remove_file(path)?;
    Ok(None)
}

fn publish_crate_file(temporary_path: &Path, destination: &Path) -> Result<(), io::Error> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("cargo artifact cache has no parent directory"))?;
    if let Err(error) =
        fs::create_dir_all(parent).and_then(|()| fs::rename(temporary_path, destination))
    {
        let _ = fs::remove_file(temporary_path);
        return Err(error);
    }
    Ok(())
}

fn crate_file_at(path: &Path) -> Result<CrateFile, CargoError> {
    Ok(CrateFile {
        path: path.to_path_buf(),
        length: fs::metadata(path)?.len(),
    })
}

fn temporary_artifact_path(data_dir: &Path) -> Result<PathBuf, io::Error> {
    let directory = data_dir.join("tmp");
    fs::create_dir_all(&directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(directory.join(format!("cargo-{}-{nonce}.part", std::process::id())))
}

fn cleanup_temporary_artifacts(data_dir: &Path) {
    let directory = data_dir.join("tmp");
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("cargo-") && name.ends_with(".part") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

async fn crate_file_response(file: CrateFile) -> Response {
    let handle = match tokio::fs::File::open(&file.path).await {
        Ok(handle) => handle,
        Err(error) => return cargo_error_response(&CargoError::Storage(error)),
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
