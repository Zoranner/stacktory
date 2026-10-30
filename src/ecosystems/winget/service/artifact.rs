use std::{
    path::PathBuf,
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
use tokio_util::io::ReaderStream;
use url::Url;

use crate::upstream::{
    FetchOperation, GroupRun, SourcePool, SourcePoolError, source_error_is_not_found,
};

use super::{
    super::{
        WingetAuditDetails, WingetError,
        cache::{self, InstallerBinding},
        group_fallback_source,
        path::{installer_url_allowed, version_query},
    },
    WingetService,
};

pub(crate) struct InstallerFile {
    path: PathBuf,
    length: u64,
    content_type: Option<String>,
}

impl WingetService {
    pub(crate) async fn installer(
        &self,
        entry_name: &str,
        token: &str,
        base_url: &Url,
        pool: &SourcePool,
    ) -> Result<InstallerFile, WingetError> {
        let Some(binding) = cache::read_binding(&self.data_dir, entry_name, token)? else {
            return Err(WingetError::InstallerNotIndexed(token.to_owned()));
        };
        let mut run = GroupRun::new(pool, entry_name, Some(&binding.selected_proxy))?;
        while let Some(member) = run.next_member() {
            let Some(allowed_hosts) = self.installer_hosts(&member.name).cloned() else {
                let error = WingetError::InstallerPolicy(format!(
                    "source `{}` has no installer host allowlist",
                    member.name
                ));
                if run.is_group() {
                    run.record_failure(&member.name, &error.to_string(), false);
                    self.record_source_failure(
                        "winget_installer_source_failed",
                        &format!("installers/{token}"),
                        entry_name,
                        &member.name,
                        "installer",
                        &error,
                    )?;
                    continue;
                }
                return Err(error);
            };
            let current_fingerprint = pool.source_fingerprint(entry_name, &member.name);
            let binding_is_current = member.name == binding.selected_proxy
                && current_fingerprint.as_deref() == Some(binding.source_fingerprint.as_str());
            let (upstream_url, request_query, source_fingerprint) = if binding_is_current {
                let url = Url::parse(&binding.upstream_url).map_err(|_| {
                    WingetError::InvalidBinding("stored installer URL is invalid".to_owned())
                })?;
                (
                    url,
                    binding.request_query.clone(),
                    current_fingerprint.expect("checked current source fingerprint"),
                )
            } else {
                let query = binding
                    .request_query
                    .clone()
                    .or_else(|| Some(version_query(&binding.version)));
                let manifest = match self
                    .manifest_from_source(
                        entry_name,
                        &member,
                        &binding.package_id,
                        query.as_deref(),
                        base_url,
                        pool,
                    )
                    .await
                {
                    Ok(manifest) => manifest,
                    Err(WingetError::Source(error))
                        if group_fallback_source(&error, pool, entry_name) =>
                    {
                        run.record_failure(
                            &member.name,
                            &error.to_string(),
                            source_error_is_not_found(&error),
                        );
                        self.record_source_failure(
                            "winget_installer_source_failed",
                            &format!("installers/{token}"),
                            entry_name,
                            &member.name,
                            "installer",
                            &WingetError::Source(error),
                        )?;
                        continue;
                    }
                    Err(error) if run.is_group() => {
                        let not_found = matches!(error, WingetError::VersionNotFound { .. });
                        run.record_failure(&member.name, &error.to_string(), not_found);
                        self.record_source_failure(
                            "winget_installer_source_failed",
                            &format!("installers/{token}"),
                            entry_name,
                            &member.name,
                            "installer",
                            &error,
                        )?;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let Some(installer) = manifest
                    .installers
                    .into_iter()
                    .find(|installer| installer.installer_token == token)
                else {
                    let error = WingetError::InvalidManifest(
                        "installer identity is no longer present in the manifest".to_owned(),
                    );
                    if run.is_group() {
                        run.record_failure(&member.name, &error.to_string(), false);
                        self.record_source_failure(
                            "winget_installer_source_failed",
                            &format!("installers/{token}"),
                            entry_name,
                            &member.name,
                            "installer",
                            &error,
                        )?;
                        continue;
                    }
                    return Err(error);
                };
                let Some(fingerprint) = pool.source_fingerprint(entry_name, &member.name) else {
                    return Err(WingetError::InvalidManifest(
                        "source fingerprint is missing".to_owned(),
                    ));
                };
                (
                    Url::parse(&installer.upstream_url).map_err(|_| {
                        WingetError::InvalidManifest("fallback installer URL is invalid".to_owned())
                    })?,
                    installer.request_query,
                    fingerprint,
                )
            };
            if !installer_url_allowed(&upstream_url, &allowed_hosts, member.require_https) {
                let error = WingetError::InstallerPolicy(
                    upstream_url.host_str().unwrap_or_default().to_owned(),
                );
                if run.is_group() {
                    run.record_failure(&member.name, &error.to_string(), false);
                    self.record_source_failure(
                        "winget_installer_source_failed",
                        &format!("installers/{token}"),
                        entry_name,
                        &member.name,
                        "installer",
                        &error,
                    )?;
                    continue;
                }
                return Err(error);
            }
            let cache_path = cache::artifact_path(
                &self.data_dir,
                &member.name,
                entry_name,
                &binding.package_id,
                &binding.version,
                token,
            );
            if cache_path.exists() {
                if cache::verify_file_sha256(&cache_path, &binding.installer_sha256)? {
                    let _ = crate::storage::access::note_access(&cache_path);
                    let mut updated = binding.clone();
                    updated.selected_proxy = member.name.clone();
                    updated.source_fingerprint = source_fingerprint.clone();
                    updated.upstream_url = upstream_url.to_string();
                    updated.request_query = request_query.clone();
                    if updated.selected_proxy != binding.selected_proxy
                        || updated.source_fingerprint != binding.source_fingerprint
                    {
                        cache::write_binding(&self.data_dir, entry_name, &updated)?;
                    }
                    self.record_object(
                        "winget_installer_read",
                        WingetAuditDetails {
                            object_key: &format!("installers/{token}"),
                            entry_name,
                            selected_proxy: &member.name,
                            object_type: "installer",
                            cache_status: "hit",
                            failure_category: None,
                        },
                    )?;
                    return installer_file(cache_path, &updated);
                }
                cache::remove_if_present(&cache_path)?;
            }

            let temporary = cache::temporary_path(&self.data_dir)?;
            let downloaded = match pool
                .fetch_allowlisted_url_to_file(
                    &member.name,
                    upstream_url.clone(),
                    &allowed_hosts,
                    FetchOperation::Artifact,
                    temporary,
                )
                .await
            {
                Ok(downloaded) => downloaded,
                Err(error) if group_fallback_source(&error, pool, entry_name) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    self.record_source_failure(
                        "winget_installer_source_failed",
                        &format!("installers/{token}"),
                        entry_name,
                        &member.name,
                        "installer",
                        &WingetError::Source(error),
                    )?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if !cache::verify_file_sha256(&downloaded.path, &binding.installer_sha256)? {
                let _ = tokio::fs::remove_file(&downloaded.path).await;
                let error = WingetError::InstallerIntegrityFailure(format!(
                    "installer `{token}` SHA-256 does not match the manifest"
                ));
                if run.is_group() {
                    self.record_object(
                        "winget_installer_fallback",
                        WingetAuditDetails {
                            object_key: &format!("installers/{token}"),
                            entry_name,
                            selected_proxy: &member.name,
                            object_type: "installer",
                            cache_status: "fallback",
                            failure_category: Some("integrity_mismatch"),
                        },
                    )?;
                    run.record_failure(&member.name, &error.to_string(), false);
                    continue;
                }
                return Err(error);
            }
            cache::publish_verified_file(&downloaded.path, &cache_path)?;
            let mut updated = binding.clone();
            updated.selected_proxy = member.name.clone();
            updated.source_fingerprint = source_fingerprint;
            updated.upstream_url = upstream_url.to_string();
            updated.request_query = request_query;
            updated.content_type = downloaded.content_type;
            updated.validated_at_unix_secs = now_unix_secs();
            if let Err(error) = cache::write_binding(&self.data_dir, entry_name, &updated) {
                let _ = std::fs::remove_file(&cache_path);
                return Err(error);
            }
            self.record_object(
                if binding.selected_proxy == member.name {
                    "winget_installer_read"
                } else {
                    "winget_installer_fallback"
                },
                WingetAuditDetails {
                    object_key: &format!("installers/{token}"),
                    entry_name,
                    selected_proxy: &member.name,
                    object_type: "installer",
                    cache_status: if binding.selected_proxy == member.name {
                        "published"
                    } else {
                        "fallback"
                    },
                    failure_category: None,
                },
            )?;
            return installer_file(cache_path, &updated);
        }
        let failure_category = if run.all_not_found() {
            "not_found"
        } else {
            "source_failed"
        };
        let error: SourcePoolError = run.all_sources_failed(format!("installers/{token}"));
        self.record_object(
            "winget_installer_fetch_failed",
            WingetAuditDetails {
                object_key: &format!("installers/{token}"),
                entry_name,
                selected_proxy: "",
                object_type: "installer",
                cache_status: "failed",
                failure_category: Some(failure_category),
            },
        )?;
        Err(error.into())
    }
}

pub(crate) async fn installer_response(file: InstallerFile) -> Response {
    let handle = match tokio::fs::File::open(&file.path).await {
        Ok(handle) => handle,
        Err(error) => {
            return super::super::winget_error_response(&WingetError::Storage(error));
        }
    };
    let content_type = file
        .content_type
        .as_deref()
        .and_then(|value| axum::http::HeaderValue::from_str(value).ok())
        .unwrap_or_else(|| axum::http::HeaderValue::from_static("application/octet-stream"));
    let mut response =
        (StatusCode::OK, Body::from_stream(ReaderStream::new(handle))).into_response();
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    if let Ok(length) = axum::http::HeaderValue::from_str(&file.length.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
}

fn installer_file(path: PathBuf, binding: &InstallerBinding) -> Result<InstallerFile, WingetError> {
    Ok(InstallerFile {
        length: std::fs::metadata(&path)?.len(),
        path,
        content_type: binding.content_type.clone(),
    })
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
