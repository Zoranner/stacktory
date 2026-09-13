use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use url::Url;

use crate::{
    nuget::{
        registration::{PackageBinding, binding_for_source, binding_path, persist_binding},
        service_index::{NugetAuditDetails, NugetError, NugetService},
    },
    sources::{FetchOperation, SourcePool, SourcePoolError},
};

#[derive(Debug, Deserialize, Serialize)]
struct PackageCacheMetadata {
    local_sha256: String,
    upstream_verified: bool,
}

pub(crate) async fn serve(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
) -> Response {
    if path.ends_with("/index.json") {
        return match serve_package_index(service, entry_name, path, pool).await {
            Ok(body) => response(StatusCode::OK, "application/json", body),
            Err(error) => package_error_response(&error),
        };
    }
    match serve_package(service, entry_name, path, pool).await {
        Ok(body) => response(StatusCode::OK, "application/octet-stream", body),
        Err(error) => package_error_response(&error),
    }
}

async fn serve_package_index(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
) -> Result<Vec<u8>, NugetError> {
    let package_id = parse_package_index_path(path)?;
    let initial_snapshot = service.service_index(entry_name, pool).await?;
    let cache_path = package_index_path(&service.data_dir, entry_name, &package_id);
    let descriptor_path = package_index_descriptor_path(&service.data_dir, entry_name, &package_id);
    let preferred = initial_snapshot.selected_proxy.clone();
    let mut members = pool
        .proxy_members(entry_name)
        .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
    if let Some(position) = members.iter().position(|member| member.name == preferred) {
        let selected = members.remove(position);
        members.insert(0, selected);
    }
    let is_group = pool.is_group(entry_name);
    let mut failures = Vec::new();
    let mut all_not_found = true;

    for member in members {
        let snapshot = if member.name == preferred {
            initial_snapshot.clone()
        } else {
            match service
                .service_index_from_source(entry_name, &member.name, pool)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) if is_group && is_retryable(&error) => {
                    all_not_found = false;
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) if is_group && is_not_found(&error) => {
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        let Some(package_base) = snapshot.package_base_url.clone() else {
            all_not_found = false;
            failures.push(format!("{}: package resource is missing", member.name));
            continue;
        };
        if let (Some(body), Some(selected_proxy)) = (
            read_if_present(&cache_path)?,
            read_selected_proxy(&descriptor_path)?,
        ) && selected_proxy == snapshot.selected_proxy
            && valid_package_index(&body)
        {
            service.record_object(
                "nuget_package_index_read",
                NugetAuditDetails {
                    object_key: &format!("package/{package_id}/index.json"),
                    entry_name,
                    selected_proxy: &selected_proxy,
                    object_type: "package-index",
                    cache_status: "hit",
                    failure_category: None,
                },
            )?;
            return Ok(body);
        }
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
            Err(error) if is_group && is_source_retryable(&error) => {
                all_not_found = false;
                failures.push(format!("{}: {error}", member.name));
                continue;
            }
            Err(error) if is_group && is_source_not_found(&error) => {
                failures.push(format!("{}: {error}", member.name));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if !valid_package_index(&upstream.body) {
            all_not_found = false;
            failures.push(format!("{}: invalid package version index", member.name));
            continue;
        }
        atomic_write(&cache_path, &upstream.body)?;
        atomic_write(
            &descriptor_path,
            format!("{}\n", snapshot.selected_proxy).as_bytes(),
        )?;
        if snapshot.selected_proxy != preferred {
            service.publish_service_index(entry_name, &snapshot)?;
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
            "nuget_package_index_published",
            NugetAuditDetails {
                object_key: &format!("package/{package_id}/index.json"),
                entry_name,
                selected_proxy: &snapshot.selected_proxy,
                object_type: "package-index",
                cache_status: "published",
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
            failure_category: Some(if all_not_found {
                "not_found"
            } else {
                "source_failed"
            }),
        },
    )?;
    Err(SourcePoolError::AllSourcesFailed {
        entry_name: entry_name.to_owned(),
        request_key: format!("package/{package_id}/index.json"),
        failures: failures.join("; "),
        status: (is_group && all_not_found).then_some(404),
    }
    .into())
}

async fn serve_package(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
) -> Result<Vec<u8>, NugetError> {
    let (package_id, version) = parse_package_path(path)?;
    let binding_path = binding_path(&service.data_dir, entry_name, &package_id, &version);
    let initial_snapshot = service.service_index(entry_name, pool).await?;
    let stored_binding: Option<PackageBinding> = read_json(&binding_path)?;
    if let Some(binding) = &stored_binding
        && (!binding.package_id.eq_ignore_ascii_case(&package_id)
            || !binding.version.eq_ignore_ascii_case(&version))
    {
        return Err(NugetError::InvalidPackage(
            "stored package binding does not match the request".to_owned(),
        ));
    }
    let mut members = pool
        .proxy_members(entry_name)
        .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
    let preferred = stored_binding
        .as_ref()
        .map(|binding| binding.selected_proxy.clone())
        .unwrap_or_else(|| initial_snapshot.selected_proxy.clone());
    if let Some(position) = members.iter().position(|member| member.name == preferred) {
        let selected = members.remove(position);
        members.insert(0, selected);
    }

    let is_group = pool.is_group(entry_name);
    let mut failures = Vec::new();
    let mut all_not_found = true;
    for member in members {
        let (binding, fallback_snapshot) = if stored_binding
            .as_ref()
            .is_some_and(|binding| member.name == binding.selected_proxy)
        {
            (
                stored_binding.clone().expect("checked stored binding"),
                None,
            )
        } else {
            let snapshot = if member.name == initial_snapshot.selected_proxy {
                initial_snapshot.clone()
            } else {
                match service
                    .service_index_from_source(entry_name, &member.name, pool)
                    .await
                {
                    Ok(snapshot) => snapshot,
                    Err(error) if is_group && is_retryable(&error) => {
                        all_not_found = false;
                        failures.push(format!("{}: {error}", member.name));
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };
            let binding = if stored_binding.is_some() {
                match binding_for_source(
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
                        Err(error) if is_group && is_retryable(&error) => {
                            all_not_found = false;
                            failures.push(format!("{}: {error}", member.name));
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
                    Err(error) if is_group && is_retryable(&error) => {
                        all_not_found = false;
                        failures.push(format!("{}: {error}", member.name));
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
        if let Some(bytes) = read_if_present(&cache_path)? {
            if cache_is_valid(&bytes, &binding, &cache_path)? {
                let _ = crate::retention::note_access(&cache_path);
                service.record_package(
                    entry_name,
                    &package_id,
                    &version,
                    &binding.selected_proxy,
                    "hit",
                    None,
                )?;
                return Ok(bytes);
            }
            let _ = fs::remove_file(&cache_path);
            let _ = fs::remove_file(package_cache_metadata_path(&cache_path));
        }

        let upstream_url = Url::parse(&binding.upstream_url)
            .map_err(|_| NugetError::InvalidPackage("stored package URL is invalid".to_owned()))?;
        let upstream = match pool
            .fetch_discovered_url_following_safe_redirects(
                &binding.selected_proxy,
                upstream_url,
                FetchOperation::Artifact,
            )
            .await
        {
            Ok(response) => response,
            Err(error) if is_group && is_source_retryable(&error) => {
                all_not_found = false;
                failures.push(format!("{}: {error}", member.name));
                service.record_object(
                    "nuget_package_fallback",
                    NugetAuditDetails {
                        object_key: &format!("package/{package_id}/{version}"),
                        entry_name,
                        selected_proxy: &member.name,
                        object_type: "package-content",
                        cache_status: "fallback",
                        failure_category: Some("source_unavailable"),
                    },
                )?;
                continue;
            }
            Err(error) if is_group && is_source_not_found(&error) => {
                failures.push(format!("{}: {error}", member.name));
                service.record_object(
                    "nuget_package_fallback",
                    NugetAuditDetails {
                        object_key: &format!("package/{package_id}/{version}"),
                        entry_name,
                        selected_proxy: &member.name,
                        object_type: "package-content",
                        cache_status: "fallback",
                        failure_category: Some("not_found"),
                    },
                )?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let upstream_verified = match verify_package(&upstream.body, &binding) {
            Ok(verified) => verified,
            Err(error @ NugetError::PackageIntegrityFailure(_)) if is_group => {
                all_not_found = false;
                failures.push(format!("{}: {error}", member.name));
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
            Err(error) => return Err(error),
        };
        publish_verified(&cache_path, &upstream.body, &binding, upstream_verified)?;
        if binding.selected_proxy != preferred {
            persist_binding(&service.data_dir, entry_name, &binding)?;
            if let Some(snapshot) = fallback_snapshot {
                service.publish_service_index(entry_name, &snapshot)?;
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
        return Ok(upstream.body);
    }

    service.record_object(
        "nuget_package_fetch_failed",
        NugetAuditDetails {
            object_key: &format!("package/{package_id}/{version}"),
            entry_name,
            selected_proxy: "",
            object_type: "package-content",
            cache_status: "failed",
            failure_category: Some(if all_not_found {
                "not_found"
            } else {
                "source_failed"
            }),
        },
    )?;
    Err(SourcePoolError::AllSourcesFailed {
        entry_name: entry_name.to_owned(),
        request_key: format!("package/{package_id}/{version}"),
        failures: failures.join("; "),
        status: (is_group && all_not_found).then_some(404),
    }
    .into())
}

fn parse_package_path(path: &str) -> Result<(String, String), NugetError> {
    let relative = path
        .strip_prefix("package/")
        .ok_or_else(|| NugetError::InvalidPackage("package path is invalid".to_owned()))?;
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[..2]
            .iter()
            .any(|part| !is_safe_segment(part) || matches!(*part, "." | ".."))
    {
        return Err(NugetError::InvalidPackage(
            "package path contains unsafe segments".to_owned(),
        ));
    }
    let package_id = parts[0].to_owned();
    let version = parts[1].to_owned();
    let expected_file = format!(
        "{}.{}.nupkg",
        package_id.to_ascii_lowercase(),
        version.to_ascii_lowercase()
    );
    if !parts[2].eq_ignore_ascii_case(&expected_file) {
        return Err(NugetError::InvalidPackage(
            "package filename does not match ID and version".to_owned(),
        ));
    }
    Ok((package_id, version))
}

fn parse_package_index_path(path: &str) -> Result<String, NugetError> {
    let relative = path
        .strip_prefix("package/")
        .ok_or_else(|| NugetError::InvalidPackage("package path is invalid".to_owned()))?;
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || !is_safe_segment(parts[0])
        || matches!(parts[0], "." | "..")
        || parts[1] != "index.json"
    {
        return Err(NugetError::InvalidPackage(
            "package version index path is invalid".to_owned(),
        ));
    }
    Ok(parts[0].to_ascii_lowercase())
}

async fn binding_for_package_index(
    _service: &NugetService,
    _entry_name: &str,
    package_id: &str,
    version: &str,
    snapshot: &crate::nuget::service_index::ServiceIndexSnapshot,
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
    Ok(PackageBinding {
        selected_proxy: snapshot.selected_proxy.clone(),
        package_id,
        version,
        upstream_url: upstream_url.to_string(),
        package_hash: None,
        package_hash_algorithm: None,
    })
}

fn valid_package_index(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("versions")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .is_some_and(|versions| {
            !versions.is_empty()
                && versions
                    .iter()
                    .all(|version| version.as_str().is_some_and(is_safe_segment))
        })
}

fn append_path(base_url: &Url, relative: &str) -> Url {
    let mut url = base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/{relative}"));
    url
}

fn verify_package(bytes: &[u8], binding: &PackageBinding) -> Result<bool, NugetError> {
    let Some(expected) = binding.package_hash.as_deref() else {
        validate_nupkg(bytes)?;
        return Ok(false);
    };
    let Some(algorithm) = binding.package_hash_algorithm.as_deref() else {
        return Err(NugetError::InvalidPackage(
            "package hash algorithm is missing".to_owned(),
        ));
    };
    let expected = STANDARD
        .decode(expected)
        .map_err(|_| NugetError::InvalidPackage("package hash is not valid base64".to_owned()))?;
    let matches = match algorithm.to_ascii_uppercase().as_str() {
        "SHA512" => Sha512::digest(bytes).as_slice() == expected.as_slice(),
        "SHA256" => Sha256::digest(bytes).as_slice() == expected.as_slice(),
        _ => {
            return Err(NugetError::InvalidPackage(format!(
                "unsupported package hash algorithm `{algorithm}`"
            )));
        }
    };
    if !matches {
        return Err(NugetError::PackageIntegrityFailure(
            "package content digest does not match registration".to_owned(),
        ));
    }
    Ok(true)
}

fn validate_nupkg(bytes: &[u8]) -> Result<(), NugetError> {
    if bytes.len() < 22 || (!bytes.starts_with(b"PK\x03\x04") && !bytes.starts_with(b"PK\x05\x06"))
    {
        return Err(NugetError::PackageIntegrityFailure(
            "package content is not a ZIP archive".to_owned(),
        ));
    }
    let search_start = bytes.len().saturating_sub(65_557);
    let eocd = bytes[search_start..]
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .map(|offset| search_start + offset)
        .ok_or_else(|| {
            NugetError::PackageIntegrityFailure(
                "package content has no ZIP end-of-central-directory record".to_owned(),
            )
        })?;
    let comment_length = u16::from_le_bytes([bytes[eocd + 20], bytes[eocd + 21]]) as usize;
    if eocd + 22 + comment_length > bytes.len() {
        return Err(NugetError::PackageIntegrityFailure(
            "package ZIP end record is truncated".to_owned(),
        ));
    }
    Ok(())
}

fn cache_is_valid(
    bytes: &[u8],
    binding: &PackageBinding,
    cache_path: &Path,
) -> Result<bool, NugetError> {
    match verify_package(bytes, binding) {
        Ok(true) => Ok(true),
        Ok(false) => {
            let Some(metadata) =
                read_json::<PackageCacheMetadata>(&package_cache_metadata_path(cache_path))?
            else {
                return Ok(false);
            };
            Ok(metadata.local_sha256 == format!("{:x}", Sha256::digest(bytes)))
        }
        Err(_) => Ok(false),
    }
}

fn publish_verified(
    path: &Path,
    bytes: &[u8],
    binding: &PackageBinding,
    upstream_verified: bool,
) -> Result<(), NugetError> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("NuGet package cache has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("NuGet package cache has no file name"))?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos()
    ));
    fs::write(&temporary, bytes)?;
    let verified = fs::read(&temporary)?;
    if let Err(error) = verify_package(&verified, binding) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        io::Error::other(error)
    })?;
    atomic_write(
        &package_cache_metadata_path(path),
        &serde_json::to_vec(&PackageCacheMetadata {
            local_sha256: format!("{:x}", Sha256::digest(bytes)),
            upstream_verified,
        })?,
    )?;
    Ok(())
}

fn package_cache_path(
    data_dir: &Path,
    selected_proxy: &str,
    package_id: &str,
    version: &str,
) -> PathBuf {
    data_dir
        .join("nuget")
        .join("artifacts")
        .join(selected_proxy)
        .join("objects")
        .join("package")
        .join(package_id.to_ascii_lowercase())
        .join(version.to_ascii_lowercase())
        .join(format!(
            "{}.{}.nupkg",
            package_id.to_ascii_lowercase(),
            version.to_ascii_lowercase()
        ))
}

fn package_index_path(data_dir: &Path, entry_name: &str, package_id: &str) -> PathBuf {
    data_dir
        .join("nuget")
        .join("metadata")
        .join(entry_name)
        .join("package-index")
        .join(format!("{package_id}.json"))
}

fn package_index_descriptor_path(data_dir: &Path, entry_name: &str, package_id: &str) -> PathBuf {
    package_index_path(data_dir, entry_name, package_id)
        .with_file_name(format!("{package_id}.source"))
}

fn read_selected_proxy(path: &Path) -> Result<Option<String>, NugetError> {
    Ok(read_if_present(path)?
        .and_then(|contents| String::from_utf8(contents).ok())
        .map(|contents| contents.trim().to_owned())
        .filter(|proxy| !proxy.is_empty()))
}

fn package_cache_metadata_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.meta.json",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ))
}

fn is_safe_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('%')
        && !value.contains('\\')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, NugetError> {
    let Some(contents) = read_if_present(path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(NugetError::Serialization)
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), NugetError> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("NuGet package metadata has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("NuGet package metadata has no file name"))?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos()
    ));
    fs::write(&temporary, contents)?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        NugetError::Storage(error)
    })
}

fn response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response {
    let mut response = (status, Body::from(body)).into_response();
    let content_type = axum::http::HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response
}

fn package_error_response(error: &NugetError) -> Response {
    let status = match error {
        NugetError::Source(SourcePoolError::UnknownEntry(_)) | NugetError::InvalidPackage(_) => {
            StatusCode::NOT_FOUND
        }
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
        NugetError::InvalidSearch(_) => StatusCode::BAD_GATEWAY,
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
        NugetError::Source(SourcePoolError::AllSourcesFailed { .. })
            | NugetError::InvalidRegistration(_)
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

fn is_source_retryable(error: &SourcePoolError) -> bool {
    matches!(error, SourcePoolError::AllSourcesFailed { .. })
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
