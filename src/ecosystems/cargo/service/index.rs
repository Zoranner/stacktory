use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

use url::Url;

use crate::{
    SourcePool,
    storage::atomic::atomic_write,
    upstream::{FetchOperation, GroupRun, source_error_is_group_fallback},
};

use super::super::index::{CargoIndexError, parse_index_lines, rewrite_index_lines};
use super::{CargoAuditDetails, CargoError, CargoService, FetchedIndex, IndexSnapshot};
use super::{is_not_found_cargo_error, terminal_status};

const INDEX_REVALIDATE_AFTER_SECS: u64 = 15 * 60;

impl CargoService {
    pub(super) async fn index(
        &self,
        entry_name: &str,
        index_path: &str,
        crate_name: &str,
        pool: &SourcePool,
        base_url: &Url,
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
                        super::super::path::normalize_crate_name(&record.name)
                            .is_ok_and(|name| name == crate_name)
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
                            base_url,
                        )
                        .await
                    {
                        Ok(fetched) if fetched.body == contents => {
                            let _ = crate::storage::access::note_access(&cache_path);
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
            .fetch_index(entry_name, index_path, crate_name, pool, None, base_url)
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
        base_url: &Url,
    ) -> Result<FetchedIndex, CargoError> {
        let request_key = index_path.to_owned();
        let mut run = GroupRun::new(pool, entry_name, preferred_source)?;
        while let Some(member) = run.next_member() {
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
                        .and_then(|source| rewrite_index_lines(source, base_url, entry_name))
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
                                super::super::path::normalize_crate_name(&record.name)
                                    .is_ok_and(|name| name == crate_name)
                            }) {
                                run.record_failure(
                                    &member.name,
                                    "index does not contain the requested crate",
                                    false,
                                );
                                continue;
                            }
                            return Ok(FetchedIndex {
                                body: contents,
                                source_name: response.source_name,
                            });
                        }
                        Err(error) => {
                            run.record_failure(&member.name, &error.to_string(), false);
                        }
                    }
                }
                Err(error) if run.is_group() && source_error_is_group_fallback(&error) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        terminal_status(&error) == Some(404),
                    );
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
                failure_category: Some(if run.has_failures() && run.all_not_found() {
                    "not_found"
                } else {
                    "source_failed"
                }),
            },
        );
        Err(run.all_sources_failed(request_key).into())
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
}

fn index_is_stale(path: &Path) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= Duration::from_secs(INDEX_REVALIDATE_AFTER_SECS))
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
