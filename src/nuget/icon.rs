use std::{
    fs, io,
    path::{Path, PathBuf},
};

use axum::{
    body::Body,
    http::{
        StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    fsutil::atomic_write,
    nuget::{
        path::{flatcontainer_icon_url, is_safe_segment},
        registration::{PackageBinding, binding_for_source, binding_path, persist_binding},
        service_index::{
            NugetAuditDetails, NugetError, NugetService, ServiceIndexSnapshot, nuget_error_response,
        },
    },
    sources::{FetchOperation, SourcePool},
};

const MAX_ICON_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
struct IconCacheMetadata {
    content_type: String,
    source_fingerprint: String,
}

struct IconLocation {
    selected_proxy: String,
    source_fingerprint: String,
    icon_url: Url,
}

pub(crate) async fn serve(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
) -> Response {
    match serve_icon(service, entry_name, path, pool).await {
        Ok((body, content_type)) => icon_response(body, &content_type),
        Err(error) => nuget_error_response(&error),
    }
}

async fn serve_icon(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
) -> Result<(Vec<u8>, String), NugetError> {
    let (package_id, version) = parse_icon_path(path)?;
    let initial_snapshot = service.service_index(entry_name, pool).await?;
    let location = resolve_icon(
        service,
        entry_name,
        &package_id,
        &version,
        &initial_snapshot,
        pool,
    )
    .await?;
    let cache_path = icon_cache_path(
        &service.data_dir,
        &location.selected_proxy,
        &package_id,
        &version,
    );
    let metadata_path = icon_cache_metadata_path(&cache_path);
    if let Some(metadata) = read_json::<IconCacheMetadata>(&metadata_path)?
        && metadata.source_fingerprint == location.source_fingerprint
        && metadata
            .content_type
            .to_ascii_lowercase()
            .starts_with("image/")
        && cache_path.exists()
    {
        let _ = crate::retention::note_access(&cache_path);
        let _ = crate::retention::note_access(&metadata_path);
        service.record_object(
            "nuget_icon_read",
            NugetAuditDetails {
                object_key: &format!("icon/{package_id}/{version}"),
                entry_name,
                selected_proxy: &location.selected_proxy,
                object_type: "icon",
                cache_status: "hit",
                failure_category: None,
            },
        )?;
        return Ok((fs::read(cache_path)?, metadata.content_type));
    }

    if cache_path.exists() {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(&metadata_path);
    }

    let upstream = pool
        .fetch_discovered_url_following_safe_redirects(
            &location.selected_proxy,
            location.icon_url,
            FetchOperation::Artifact,
        )
        .await?;
    let content_type = upstream
        .content_type
        .filter(|value| value.to_ascii_lowercase().starts_with("image/"))
        .ok_or_else(|| {
            NugetError::InvalidIcon("upstream did not return an image content type".to_owned())
        })?;
    if upstream.body.is_empty() || upstream.body.len() > MAX_ICON_BYTES {
        return Err(NugetError::InvalidIcon(
            "upstream icon size is invalid".to_owned(),
        ));
    }
    atomic_write(&cache_path, &upstream.body)?;
    atomic_write(
        &metadata_path,
        &serde_json::to_vec(&IconCacheMetadata {
            content_type: content_type.clone(),
            source_fingerprint: location.source_fingerprint,
        })?,
    )?;
    service.record_object(
        "nuget_icon_published",
        NugetAuditDetails {
            object_key: &format!("icon/{package_id}/{version}"),
            entry_name,
            selected_proxy: &location.selected_proxy,
            object_type: "icon",
            cache_status: "published",
            failure_category: None,
        },
    )?;
    Ok((upstream.body, content_type))
}

async fn resolve_icon(
    service: &NugetService,
    entry_name: &str,
    package_id: &str,
    version: &str,
    snapshot: &ServiceIndexSnapshot,
    pool: &SourcePool,
) -> Result<IconLocation, NugetError> {
    let stored: Option<PackageBinding> = read_json(&binding_path(
        &service.data_dir,
        entry_name,
        package_id,
        version,
    ))?;
    if let Some(binding) = stored
        && binding.package_id.eq_ignore_ascii_case(package_id)
        && binding.version.eq_ignore_ascii_case(version)
        && pool
            .source_fingerprint(entry_name, &binding.selected_proxy)
            .is_some_and(|fingerprint| fingerprint == binding.source_fingerprint)
        && let Some(icon_url) = binding
            .icon_url
            .as_deref()
            .and_then(|value| Url::parse(value).ok())
    {
        return Ok(IconLocation {
            selected_proxy: binding.selected_proxy,
            source_fingerprint: binding.source_fingerprint,
            icon_url,
        });
    }

    if let Ok(binding) =
        binding_for_source(service, entry_name, package_id, version, snapshot, pool).await
    {
        persist_binding(&service.data_dir, entry_name, &binding)?;
        if let Some(icon_url) = binding
            .icon_url
            .as_deref()
            .and_then(|value| Url::parse(value).ok())
        {
            return Ok(IconLocation {
                selected_proxy: binding.selected_proxy,
                source_fingerprint: binding.source_fingerprint,
                icon_url,
            });
        }
    }

    let package_base = snapshot
        .package_base_url
        .as_ref()
        .ok_or_else(|| NugetError::IconNotFound(format!("{package_id}/{version}")))?;
    Ok(IconLocation {
        selected_proxy: snapshot.selected_proxy.clone(),
        source_fingerprint: snapshot.source_fingerprint.clone(),
        icon_url: flatcontainer_icon_url(package_base, package_id, version),
    })
}

fn parse_icon_path(path: &str) -> Result<(String, String), NugetError> {
    let relative = path
        .strip_prefix("icon/")
        .ok_or_else(|| NugetError::InvalidIcon("icon path is invalid".to_owned()))?;
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || !is_safe_segment(parts[0])
        || !is_safe_segment(parts[1])
        || matches!(parts[0], "." | "..")
        || matches!(parts[1], "." | "..")
    {
        return Err(NugetError::InvalidIcon(
            "icon path contains unsafe segments".to_owned(),
        ));
    }
    Ok((parts[0].to_ascii_lowercase(), parts[1].to_ascii_lowercase()))
}

fn icon_cache_path(
    data_dir: &Path,
    selected_proxy: &str,
    package_id: &str,
    version: &str,
) -> PathBuf {
    crate::layout::artifact_objects_root(data_dir, "nuget", selected_proxy)
        .join("icon")
        .join(package_id)
        .join(version)
        .join("icon")
}

fn icon_cache_metadata_path(path: &Path) -> PathBuf {
    path.with_file_name("icon.meta.json")
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, NugetError> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(NugetError::Serialization)
}

fn icon_response(body: Vec<u8>, content_type: &str) -> Response {
    let length = body.len();
    let mut response = (StatusCode::OK, Body::from(body)).into_response();
    let content_type = axum::http::HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    if let Ok(length) = axum::http::HeaderValue::from_str(&length.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
}
