use std::{fs, path::PathBuf};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    ecosystems::nuget::{
        group_fallback_error, group_fallback_source,
        path::flatcontainer_icon_url,
        registration::{PackageBinding, binding_for_source, binding_path, persist_binding},
        service_index::{NugetAuditDetails, NugetError, NugetService, ServiceIndexSnapshot},
        {metadata_is_stale, now_unix_secs},
    },
    storage::atomic::atomic_write,
    upstream::{FetchOperation, GroupRun, SourcePool, SourcePoolError},
};

mod cache;
mod protocol;

use cache::{
    cache_is_valid, package_cache_metadata_path, package_cache_path, package_file,
    package_index_descriptor_path, package_index_path, publish_verified_file, read_if_present,
    read_json, read_package_index_descriptor, temporary_package_path, verify_package_file,
};
use protocol::{
    append_path, package_file_response, parse_package_index_path, parse_package_path, response,
    valid_package_index,
};

#[derive(Debug, Deserialize, Serialize)]
struct PackageCacheMetadata {
    local_sha256: String,
    upstream_verified: bool,
    #[serde(default)]
    source_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PackageIndexDescriptor {
    selected_proxy: String,
    #[serde(default)]
    source_fingerprint: String,
    #[serde(default)]
    validated_at_unix_secs: u64,
}

struct PackageFile {
    path: PathBuf,
    length: u64,
}

pub(crate) async fn serve(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
    base_url: &Url,
) -> Response {
    if path.ends_with("/index.json") {
        return match serve_package_index(service, entry_name, path, pool, base_url).await {
            Ok(body) => response(StatusCode::OK, "application/json", body),
            Err(error) => package_error_response(&error),
        };
    }
    match serve_package(service, entry_name, path, pool, base_url).await {
        Ok(file) => package_file_response(file).await,
        Err(error) => package_error_response(&error),
    }
}

async fn serve_package_index(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
    base_url: &Url,
) -> Result<Vec<u8>, NugetError> {
    let package_id = parse_package_index_path(path)?;
    let initial_snapshot = service.service_index(entry_name, pool, base_url).await?;
    let cache_path = package_index_path(&service.data_dir, entry_name, &package_id);
    let descriptor_path = package_index_descriptor_path(&service.data_dir, entry_name, &package_id);
    let cached = match (
        read_if_present(&cache_path)?,
        read_package_index_descriptor(&descriptor_path)?,
    ) {
        (Some(body), Some(descriptor))
            if descriptor.selected_proxy == initial_snapshot.selected_proxy
                && descriptor.source_fingerprint == initial_snapshot.source_fingerprint
                && valid_package_index(&body) =>
        {
            Some((body, descriptor))
        }
        _ => None,
    };
    if let Some((body, descriptor)) = cached {
        if !metadata_is_stale(descriptor.validated_at_unix_secs) {
            service.record_object(
                "nuget_package_index_read",
                NugetAuditDetails {
                    object_key: &format!("package/{package_id}/index.json"),
                    entry_name,
                    selected_proxy: &descriptor.selected_proxy,
                    object_type: "package-index",
                    cache_status: "hit",
                    failure_category: None,
                },
            )?;
            return Ok(body);
        }
        match fetch_package_index(
            service,
            entry_name,
            &package_id,
            &initial_snapshot,
            pool,
            true,
            base_url,
        )
        .await
        {
            Ok(body) => return Ok(body),
            Err(error) if is_not_found(&error) => {
                let _ = fs::remove_file(&cache_path);
                let _ = fs::remove_file(&descriptor_path);
                service.record_object(
                    "nuget_package_index_revalidate_failed",
                    NugetAuditDetails {
                        object_key: &format!("package/{package_id}/index.json"),
                        entry_name,
                        selected_proxy: &descriptor.selected_proxy,
                        object_type: "package-index",
                        cache_status: "failed",
                        failure_category: Some("not_found"),
                    },
                )?;
                return Err(error);
            }
            Err(_) => {
                service.record_object(
                    "nuget_package_index_stale",
                    NugetAuditDetails {
                        object_key: &format!("package/{package_id}/index.json"),
                        entry_name,
                        selected_proxy: &descriptor.selected_proxy,
                        object_type: "package-index",
                        cache_status: "stale",
                        failure_category: Some("source_failed"),
                    },
                )?;
                return Ok(body);
            }
        }
    }

    fetch_package_index(
        service,
        entry_name,
        &package_id,
        &initial_snapshot,
        pool,
        false,
        base_url,
    )
    .await
}

async fn fetch_package_index(
    service: &NugetService,
    entry_name: &str,
    package_id: &str,
    initial_snapshot: &ServiceIndexSnapshot,
    pool: &SourcePool,
    revalidating: bool,
    base_url: &Url,
) -> Result<Vec<u8>, NugetError> {
    let preferred = initial_snapshot.selected_proxy.clone();
    let mut run = GroupRun::new(pool, entry_name, Some(&preferred))?;
    let is_group = run.is_group();
    let cache_path = package_index_path(&service.data_dir, entry_name, package_id);
    let descriptor_path = package_index_descriptor_path(&service.data_dir, entry_name, package_id);

    while let Some(member) = run.next_member() {
        let snapshot = if member.name == preferred {
            initial_snapshot.clone()
        } else {
            match service
                .service_index_from_source(entry_name, &member.name, pool, base_url)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error)
                    if is_group
                        && (is_retryable(&error)
                            || group_fallback_error(&error, pool, entry_name)) =>
                {
                    run.record_failure(&member.name, &error.to_string(), is_not_found(&error));
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        let Some(package_base) = snapshot.package_base_url.clone() else {
            run.record_failure(&member.name, "package resource is missing", false);
            continue;
        };
        let upstream_url = append_path(&package_base, &format!("{package_id}/index.json"));
        let upstream = match pool
            .fetch_discovered_url_following_safe_redirects(
                &snapshot.selected_proxy,
                upstream_url,
                FetchOperation::Metadata,
            )
            .await
        {
            Ok(response) => response,
            Err(error) if group_fallback_source(&error, pool, entry_name) => {
                run.record_failure(
                    &member.name,
                    &error.to_string(),
                    is_source_not_found(&error),
                );
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if !valid_package_index(&upstream.body) {
            run.record_failure(&member.name, "invalid package version index", false);
            continue;
        }
        atomic_write(&cache_path, &upstream.body)?;
        atomic_write(
            &descriptor_path,
            &serde_json::to_vec(&PackageIndexDescriptor {
                selected_proxy: snapshot.selected_proxy.clone(),
                source_fingerprint: snapshot.source_fingerprint.clone(),
                validated_at_unix_secs: now_unix_secs(),
            })?,
        )?;
        if snapshot.selected_proxy != preferred {
            service.publish_service_index(entry_name, &snapshot, base_url)?;
            service.record_object(
                "nuget_package_index_fallback",
                NugetAuditDetails {
                    object_key: &format!("package/{package_id}/index.json"),
                    entry_name,
                    selected_proxy: &snapshot.selected_proxy,
                    object_type: "package-index",
                    cache_status: "fallback",
                    failure_category: Some("source_unavailable_or_invalid"),
                },
            )?;
        }
        service.record_object(
            if revalidating {
                "nuget_package_index_revalidated"
            } else {
                "nuget_package_index_published"
            },
            NugetAuditDetails {
                object_key: &format!("package/{package_id}/index.json"),
                entry_name,
                selected_proxy: &snapshot.selected_proxy,
                object_type: "package-index",
                cache_status: if revalidating {
                    "revalidated"
                } else {
                    "published"
                },
                failure_category: None,
            },
        )?;
        return Ok(upstream.body);
    }

    service.record_object(
        "nuget_package_index_fetch_failed",
        NugetAuditDetails {
            object_key: &format!("package/{package_id}/index.json"),
            entry_name,
            selected_proxy: "",
            object_type: "package-index",
            cache_status: "failed",
            failure_category: Some(if run.all_not_found() {
                "not_found"
            } else {
                "source_failed"
            }),
        },
    )?;
    Err(run
        .all_sources_failed(format!("package/{package_id}/index.json"))
        .into())
}

async fn serve_package(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
    base_url: &Url,
) -> Result<PackageFile, NugetError> {
    let (package_id, version) = parse_package_path(path)?;
    let binding_path = binding_path(&service.data_dir, entry_name, &package_id, &version);
    let initial_snapshot = service.service_index(entry_name, pool, base_url).await?;
    let stored_binding: Option<PackageBinding> = read_json(&binding_path)?;
    if let Some(binding) = &stored_binding
        && (!binding.package_id.eq_ignore_ascii_case(&package_id)
            || !binding.version.eq_ignore_ascii_case(&version))
    {
        return Err(NugetError::InvalidPackage(
            "stored package binding does not match the request".to_owned(),
        ));
    }
    let binding_is_current = stored_binding.as_ref().is_some_and(|binding| {
        pool.source_fingerprint(entry_name, &binding.selected_proxy)
            .is_some_and(|fingerprint| fingerprint == binding.source_fingerprint)
            && !metadata_is_stale(binding.validated_at_unix_secs)
    });
    let preferred = stored_binding
        .as_ref()
        .map(|binding| binding.selected_proxy.clone())
        .unwrap_or_else(|| initial_snapshot.selected_proxy.clone());
    let mut run = GroupRun::new(pool, entry_name, Some(&preferred))?;
    let is_group = run.is_group();
    while let Some(member) = run.next_member() {
        let use_stored_binding = binding_is_current
            && stored_binding
                .as_ref()
                .is_some_and(|binding| member.name == binding.selected_proxy);
        let (binding, fallback_snapshot) = if use_stored_binding {
            (
                stored_binding.clone().expect("checked stored binding"),
                None,
            )
        } else {
            let snapshot = if member.name == initial_snapshot.selected_proxy {
                initial_snapshot.clone()
            } else {
                match service
                    .service_index_from_source(entry_name, &member.name, pool, base_url)
                    .await
                {
                    Ok(snapshot) => snapshot,
                    Err(error)
                        if is_group
                            && (is_retryable(&error)
                                || group_fallback_error(&error, pool, entry_name)) =>
                    {
                        run.record_failure(&member.name, &error.to_string(), is_not_found(&error));
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };
            let binding = if stored_binding.is_some() {
                match binding_for_source(
                    entry_name,
                    &package_id,
                    &version,
                    &snapshot,
                    pool,
                    base_url,
                )
                .await
                {
                    Ok(binding) => binding,
                    Err(_) => match binding_for_package_index(
                        service,
                        entry_name,
                        &package_id,
                        &version,
                        &snapshot,
                        pool,
                    )
                    .await
                    {
                        Ok(binding) => binding,
                        Err(error)
                            if is_group
                                && (is_retryable(&error)
                                    || group_fallback_error(&error, pool, entry_name)) =>
                        {
                            run.record_failure(
                                &member.name,
                                &error.to_string(),
                                is_not_found(&error),
                            );
                            continue;
                        }
                        Err(error) => return Err(error),
                    },
                }
            } else {
                match binding_for_package_index(
                    service,
                    entry_name,
                    &package_id,
                    &version,
                    &snapshot,
                    pool,
                )
                .await
                {
                    Ok(binding) => binding,
                    Err(error)
                        if is_group
                            && (is_retryable(&error)
                                || group_fallback_error(&error, pool, entry_name)) =>
                    {
                        run.record_failure(&member.name, &error.to_string(), is_not_found(&error));
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };
            (binding, Some(snapshot))
        };

        let cache_path = package_cache_path(
            &service.data_dir,
            &binding.selected_proxy,
            &binding.package_id,
            &binding.version,
        );
        if cache_path.exists() && cache_is_valid(&cache_path, &binding)? {
            let _ = crate::storage::access::note_access(&cache_path);
            let _ = crate::storage::access::note_access(&package_cache_metadata_path(&cache_path));
            service.record_package(
                entry_name,
                &package_id,
                &version,
                &binding.selected_proxy,
                "hit",
                None,
            )?;
            return package_file(&cache_path);
        }
        if cache_path.exists() {
            let _ = fs::remove_file(&cache_path);
            let _ = fs::remove_file(package_cache_metadata_path(&cache_path));
        }

        let upstream_url = Url::parse(&binding.upstream_url)
            .map_err(|_| NugetError::InvalidPackage("stored package URL is invalid".to_owned()))?;
        let temporary_path = temporary_package_path(&service.data_dir)?;
        let upstream = match pool
            .fetch_discovered_url_following_safe_redirects_to_file(
                &binding.selected_proxy,
                upstream_url,
                FetchOperation::Artifact,
                temporary_path,
            )
            .await
        {
            Ok(file) => file,
            Err(error) if group_fallback_source(&error, pool, entry_name) => {
                run.record_failure(
                    &member.name,
                    &error.to_string(),
                    is_source_not_found(&error),
                );
                service.record_object(
                    "nuget_package_fallback",
                    NugetAuditDetails {
                        object_key: &format!("package/{package_id}/{version}"),
                        entry_name,
                        selected_proxy: &member.name,
                        object_type: "package-content",
                        cache_status: "fallback",
                        failure_category: Some(if is_source_not_found(&error) {
                            "not_found"
                        } else {
                            "source_unavailable"
                        }),
                    },
                )?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let upstream_verified = match verify_package_file(&upstream.path, &binding) {
            Ok(verified) => verified,
            Err(error @ NugetError::PackageIntegrityFailure(_)) if is_group => {
                let _ = fs::remove_file(&upstream.path);
                run.record_failure(&member.name, &error.to_string(), false);
                service.record_object(
                    "nuget_package_fallback",
                    NugetAuditDetails {
                        object_key: &format!("package/{package_id}/{version}"),
                        entry_name,
                        selected_proxy: &member.name,
                        object_type: "package-content",
                        cache_status: "fallback",
                        failure_category: Some("integrity_mismatch"),
                    },
                )?;
                continue;
            }
            Err(error) => {
                let _ = fs::remove_file(&upstream.path);
                return Err(error);
            }
        };
        publish_verified_file(&cache_path, &upstream.path, &binding, upstream_verified)?;
        if binding.selected_proxy != preferred {
            persist_binding(&service.data_dir, entry_name, &binding)?;
            if let Some(snapshot) = fallback_snapshot {
                service.publish_service_index(entry_name, &snapshot, base_url)?;
            }
        }
        service.record_package(
            entry_name,
            &package_id,
            &version,
            &binding.selected_proxy,
            "published",
            None,
        )?;
        return package_file(&cache_path);
    }

    service.record_object(
        "nuget_package_fetch_failed",
        NugetAuditDetails {
            object_key: &format!("package/{package_id}/{version}"),
            entry_name,
            selected_proxy: "",
            object_type: "package-content",
            cache_status: "failed",
            failure_category: Some(if run.all_not_found() {
                "not_found"
            } else {
                "source_failed"
            }),
        },
    )?;
    Err(run
        .all_sources_failed(format!("package/{package_id}/{version}"))
        .into())
}

async fn binding_for_package_index(
    _service: &NugetService,
    _entry_name: &str,
    package_id: &str,
    version: &str,
    snapshot: &crate::ecosystems::nuget::service_index::ServiceIndexSnapshot,
    pool: &SourcePool,
) -> Result<PackageBinding, NugetError> {
    let package_base = snapshot.package_base_url.as_ref().ok_or_else(|| {
        NugetError::InvalidPackageIndex("service index has no package content resource".to_owned())
    })?;
    let index_url = append_path(package_base, &format!("{package_id}/index.json"));
    let response = pool
        .fetch_discovered_url_following_safe_redirects(
            &snapshot.selected_proxy,
            index_url,
            FetchOperation::Metadata,
        )
        .await?;
    let index: serde_json::Value = serde_json::from_slice(&response.body)?;
    let versions = index
        .get("versions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| NugetError::InvalidPackageIndex("versions is missing".to_owned()))?;
    if !versions.iter().any(|candidate| {
        candidate
            .as_str()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(version))
    }) {
        return Err(NugetError::InvalidPackage(
            "package version is not listed by the package index".to_owned(),
        ));
    }
    let mut upstream_url = package_base.clone();
    let package_id = package_id.to_ascii_lowercase();
    let version = version.to_ascii_lowercase();
    let base_path = upstream_url.path().trim_end_matches('/');
    upstream_url.set_path(&format!(
        "{base_path}/{package_id}/{version}/{package_id}.{version}.nupkg"
    ));
    let icon_url = flatcontainer_icon_url(package_base, &package_id, &version).to_string();
    Ok(PackageBinding {
        selected_proxy: snapshot.selected_proxy.clone(),
        source_fingerprint: snapshot.source_fingerprint.clone(),
        validated_at_unix_secs: now_unix_secs(),
        package_id,
        version,
        upstream_url: upstream_url.to_string(),
        icon_url: Some(icon_url),
        package_hash: None,
        package_hash_algorithm: None,
    })
}

fn package_error_response(error: &NugetError) -> Response {
    let status = match error {
        NugetError::Source(SourcePoolError::UnknownEntry(_))
        | NugetError::InvalidPackage(_)
        | NugetError::IconNotFound(_) => StatusCode::NOT_FOUND,
        NugetError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        NugetError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        NugetError::Source(_) | NugetError::InvalidServiceIndex(_) => StatusCode::BAD_GATEWAY,
        NugetError::InvalidRegistration(_) => StatusCode::BAD_GATEWAY,
        NugetError::InvalidRegistrationPath(_) => StatusCode::NOT_FOUND,
        NugetError::InvalidVulnerability(_) => StatusCode::BAD_GATEWAY,
        NugetError::InvalidVulnerabilityPath(_) => StatusCode::NOT_FOUND,
        NugetError::InvalidPackageIndex(_) => StatusCode::BAD_GATEWAY,
        NugetError::InvalidSearchRequest(_) => StatusCode::BAD_REQUEST,
        NugetError::InvalidSearch(_) => StatusCode::BAD_GATEWAY,
        NugetError::InvalidIcon(_) => StatusCode::BAD_GATEWAY,
        NugetError::PackageIntegrityFailure(_) => StatusCode::BAD_GATEWAY,
        NugetError::Storage(_) | NugetError::Serialization(_) | NugetError::Audit(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    status.into_response()
}

fn is_retryable(error: &NugetError) -> bool {
    matches!(
        error,
        NugetError::InvalidRegistration(_)
            | NugetError::InvalidServiceIndex(_)
            | NugetError::InvalidPackageIndex(_)
            | NugetError::InvalidPackage(_)
            | NugetError::PackageIntegrityFailure(_)
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

fn is_source_not_found(error: &SourcePoolError) -> bool {
    matches!(
        error,
        SourcePoolError::TerminalSourceError { status: 404, .. }
            | SourcePoolError::AllSourcesFailed {
                status: Some(404),
                ..
            }
    )
}

impl NugetService {
    pub(crate) fn record_package(
        &self,
        entry_name: &str,
        package_id: &str,
        version: &str,
        selected_proxy: &str,
        cache_status: &str,
        failure_category: Option<&str>,
    ) -> Result<(), NugetError> {
        let object_key = format!("package/{package_id}/{version}");
        self.record_object(
            "nuget_package_content",
            NugetAuditDetails {
                object_key: &object_key,
                entry_name,
                selected_proxy,
                object_type: "package-content",
                cache_status,
                failure_category,
            },
        )
    }
}
