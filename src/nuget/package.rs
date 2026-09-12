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
        registration::{PackageBinding, binding_path},
        service_index::{NugetError, NugetService},
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
    match serve_package(service, entry_name, path, pool).await {
        Ok(body) => response(StatusCode::OK, "application/octet-stream", body),
        Err(error) => package_error_response(&error),
    }
}

async fn serve_package(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
) -> Result<Vec<u8>, NugetError> {
    let (package_id, version) = parse_package_path(path)?;
    let binding_path = binding_path(&service.data_dir, entry_name, &package_id, &version);
    let binding: PackageBinding = read_json(&binding_path)?.ok_or_else(|| {
        NugetError::InvalidPackage("package is not referenced by registration".to_owned())
    })?;
    if !binding.package_id.eq_ignore_ascii_case(&package_id)
        || !binding.version.eq_ignore_ascii_case(&version)
    {
        return Err(NugetError::InvalidPackage(
            "stored package binding does not match the request".to_owned(),
        ));
    }
    if !pool.proxy_members(entry_name).is_some_and(|members| {
        members
            .iter()
            .any(|member| member.name == binding.selected_proxy)
    }) {
        return Err(NugetError::InvalidPackage(
            "stored package binding uses an unavailable source".to_owned(),
        ));
    }

    let cache_path = package_cache_path(
        &service.data_dir,
        &binding.selected_proxy,
        &binding.package_id,
        &binding.version,
    );
    if let Some(bytes) = read_if_present(&cache_path)? {
        if cache_is_valid(&bytes, &binding, &cache_path)? {
            service.record_package(
                entry_name,
                &package_id,
                &version,
                &binding.selected_proxy,
                "hit",
                None,
            );
            return Ok(bytes);
        }
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(package_cache_metadata_path(&cache_path));
    }

    let upstream_url = Url::parse(&binding.upstream_url)
        .map_err(|_| NugetError::InvalidPackage("stored package URL is invalid".to_owned()))?;
    let upstream = pool
        .fetch_discovered_url(
            &binding.selected_proxy,
            upstream_url,
            FetchOperation::Artifact,
        )
        .await?;
    let upstream_verified = verify_package(&upstream.body, &binding)?;
    publish_verified(&cache_path, &upstream.body, &binding, upstream_verified)?;
    service.record_package(
        entry_name,
        &package_id,
        &version,
        &binding.selected_proxy,
        "published",
        None,
    );
    Ok(upstream.body)
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

fn verify_package(bytes: &[u8], binding: &PackageBinding) -> Result<bool, NugetError> {
    let Some(expected) = binding.package_hash.as_deref() else {
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
    let temporary = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
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
        NugetError::PackageIntegrityFailure(_) => StatusCode::BAD_GATEWAY,
        NugetError::Storage(_) | NugetError::Serialization(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
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
    ) {
        if let Err(error) = self.audit.record_detailed(
            "nuget_package_content",
            &format!("package/{package_id}/{version}"),
            if failure_category.is_some() {
                "failure"
            } else {
                "success"
            },
            crate::audit::AuditDetails {
                ecosystem: Some("nuget"),
                entry_name: Some(entry_name),
                object_key: Some(&format!("package/{package_id}/{version}")),
                selected_proxy: Some(selected_proxy),
                object_type: Some("package-content"),
                cache_status: Some(cache_status),
                failure_category,
            },
        ) {
            eprintln!("Stacktory audit write failed: {error}");
        }
    }
}
