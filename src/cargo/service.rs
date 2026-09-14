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
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::{
    SourcePool,
    audit::{AuditDetails, AuditSink},
    sources::registry::{Adapter, AdapterFuture, AdapterRequest},
    sources::{FetchOperation, SourcePoolError},
};

use super::{
    index::{
        CargoIndexError, download_template, parse_index_lines, parse_registry_config,
        render_download_url_for_source, rewrite_index_lines, sparse_path_for_request,
    },
    path::{CargoPath, CargoPathError, normalize_crate_name, validate_version},
};

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
            Err(error) => cargo_error_response(&error),
        }
    }

    async fn serve(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
    ) -> Result<(&'static str, Vec<u8>), CargoError> {
        if pool.proxy_members(entry_name).is_none() {
            return Err(SourcePoolError::UnknownEntry(entry_name.to_owned()).into());
        }
        if path == "config.json" {
            return Ok(("application/json", self.config(entry_name)?));
        }
        if let Ok((crate_name, _)) = sparse_path_for_request(path) {
            let body = self.index(entry_name, path, &crate_name, pool).await?;
            return Ok(("application/json", body.body));
        }
        if let Some(crate_path) = path.strip_prefix("crates/") {
            let (crate_name, version) = parse_crate_request(crate_path)?;
            let body = self
                .crate_file(entry_name, &crate_name, &version, pool)
                .await?;
            return Ok(("application/octet-stream", body));
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

        let request_key = index_path.to_owned();
        let mut failures = Vec::new();
        let mut saw_failure = false;
        let mut all_not_found = true;
        for member in pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?
        {
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
                            atomic_write(&cache_path, &contents)?;
                            atomic_write(
                                &sidecar_path,
                                format!("{}\n", response.source_name).as_bytes(),
                            )?;
                            self.record_cargo(
                                "cargo_index_published",
                                entry_name,
                                index_path,
                                CargoAuditDetails {
                                    selected_proxy: Some(&response.source_name),
                                    object_type: "index",
                                    cache_status: "published",
                                    failure_category: None,
                                },
                            );
                            return Ok(IndexSnapshot {
                                body: contents,
                                selected_proxy: response.source_name,
                            });
                        }
                        Err(error) => {
                            saw_failure = true;
                            all_not_found = false;
                            failures.push(format!("{}: {error}", member.name));
                        }
                    }
                }
                Err(error) if is_retryable(&error) => {
                    saw_failure = true;
                    all_not_found = false;
                    failures.push(format!("{}: {error}", member.name));
                }
                Err(error)
                    if terminal_status(&error) == Some(404) && is_group_entry(pool, entry_name) =>
                {
                    saw_failure = true;
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

    async fn crate_file(
        &self,
        entry_name: &str,
        crate_name: &str,
        version: &str,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, CargoError> {
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
        if let Some(contents) = read_if_present(&cache_path)? {
            if checksum(&contents).eq_ignore_ascii_case(&expected) {
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
                return Ok(contents);
            }
            fs::remove_file(&cache_path)?;
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
                Err(error) if is_retryable(&error) => {
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
                            failure_category: Some("source_unavailable"),
                        },
                    );
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) if terminal_status(&error) == Some(404) && is_group => {
                    saw_failure = true;
                    self.record_cargo(
                        "cargo_artifact_fallback",
                        entry_name,
                        &request_key,
                        CargoAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "fallback",
                            failure_category: Some("not_found"),
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
            match pool
                .fetch_configured_url(&member.name, download_url, FetchOperation::Artifact)
                .await
            {
                Ok(response) if checksum(&response.body).eq_ignore_ascii_case(&expected) => {
                    atomic_write(
                        &self.artifact_path(&member.name, crate_name, version),
                        &response.body,
                    )?;
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
                    return Ok(response.body);
                }
                Ok(_) => failures.push(format!(
                    "{}: checksum mismatch for {crate_name} {version}",
                    member.name
                )),
                Err(error) if terminal_status(&error) == Some(404) && is_group => {
                    saw_failure = true;
                    self.record_cargo(
                        "cargo_artifact_fallback",
                        entry_name,
                        &request_key,
                        CargoAuditDetails {
                            selected_proxy: Some(&member.name),
                            object_type: "artifact",
                            cache_status: "fallback",
                            failure_category: Some("not_found"),
                        },
                    );
                    failures.push(format!("{}: {error}", member.name));
                }
                Err(error) if is_source_failure(&error) => {
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
                }
                Err(error) => return Err(error.into()),
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
            eprintln!("Stacktory audit write failed: {error}");
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

fn is_retryable(error: &SourcePoolError) -> bool {
    matches!(error, SourcePoolError::AllSourcesFailed { .. })
}

fn is_source_failure(error: &SourcePoolError) -> bool {
    matches!(
        error,
        SourcePoolError::AllSourcesFailed { .. } | SourcePoolError::InvalidConfiguredUrl(_)
    )
}

fn terminal_status(error: &SourcePoolError) -> Option<u16> {
    match error {
        SourcePoolError::TerminalSourceError { status, .. } => Some(*status),
        SourcePoolError::AllSourcesFailed { status, .. } => *status,
        _ => None,
    }
}

fn is_group_entry(pool: &SourcePool, entry_name: &str) -> bool {
    pool.is_group(entry_name)
}

fn checksum(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
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
        .ok_or_else(|| io::Error::other("cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("cache path has no file name"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));
    fs::write(&temporary, contents)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}
