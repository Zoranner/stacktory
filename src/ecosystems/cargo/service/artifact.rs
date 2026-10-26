use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{
        StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use tokio_util::io::ReaderStream;

use crate::{
    SourcePool,
    storage::atomic::atomic_write,
    upstream::{FetchOperation, GroupRun, SourcePoolError, source_error_is_group_fallback},
};

use super::super::index::{
    CargoIndexError, parse_index_lines, parse_registry_config, render_download_url_for_source,
};
use super::super::path::{normalize_crate_name, sparse_index_path};
use super::{
    CargoAuditDetails, CargoError, CargoService, CrateFile, cargo_error_response, terminal_status,
};

impl CargoService {
    pub(super) async fn crate_file(
        &self,
        entry_name: &str,
        crate_name: &str,
        version: &str,
        pool: &SourcePool,
    ) -> Result<CrateFile, CargoError> {
        let index_path = sparse_index_path(crate_name)?;
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
            let _ = crate::storage::access::note_access(&cache_path);
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
        let mut run = GroupRun::new(pool, entry_name, Some(&snapshot.selected_proxy))?;
        let is_group = run.is_group();
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
        while let Some(member) = run.next_member() {
            let config = match pool
                .fetch(&member.name, "config.json", FetchOperation::Metadata)
                .await
            {
                Ok(response) => match parse_registry_config(&response.body) {
                    Ok(config) => config,
                    Err(error) => {
                        run.record_failure(&member.name, &error.to_string(), false);
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
                        continue;
                    }
                },
                Err(error) if is_group && source_error_is_group_fallback(&error) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        terminal_status(&error) == Some(404),
                    );
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
                    run.record_failure(&member.name, &error.to_string(), false);
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
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        terminal_status(&error) == Some(404),
                    );
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
                    run.record_failure(
                        &member.name,
                        &format!("checksum mismatch for {crate_name} {version}"),
                        false,
                    );
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
                failure_category: Some(if run.has_failures() && run.all_not_found() {
                    "not_found"
                } else {
                    "source_failed"
                }),
            },
        );
        Err(run.all_sources_failed(request_key).into())
    }

    fn artifact_path(&self, source_name: &str, crate_name: &str, version: &str) -> PathBuf {
        crate::storage::layout::artifact_objects_root(&self.data_dir, "cargo", source_name)
            .join("crates")
            .join(crate_name)
            .join(format!("{version}.crate"))
    }
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

pub(super) fn cleanup_temporary_artifacts(data_dir: &Path) {
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

pub(super) async fn crate_file_response(file: CrateFile) -> Response {
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
