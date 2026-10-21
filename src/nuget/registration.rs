use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use axum::{
    body::Body,
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::{
    fsutil::atomic_write,
    nuget::service_index::NugetAuditDetails,
    nuget::service_index::{NugetError, NugetService},
    sources::{FetchOperation, SourcePool, SourcePoolError},
};

use super::{
    group_fallback_error, group_fallback_source,
    path::{is_safe_segment, safe_resource_url},
    service_index::ServiceIndexSnapshot,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PackageBinding {
    pub(crate) selected_proxy: String,
    pub(crate) package_id: String,
    pub(crate) version: String,
    pub(crate) upstream_url: String,
    pub(crate) package_hash: Option<String>,
    pub(crate) package_hash_algorithm: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RegistrationDescriptor {
    selected_proxy: String,
    upstream_url: String,
    #[serde(default)]
    public_base_url: String,
}

#[derive(Debug)]
struct RegistrationPath {
    package_id: String,
    relative: String,
    leaf_version: Option<String>,
}

pub(crate) async fn serve(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
) -> Response {
    match serve_registration(service, entry_name, path, pool).await {
        Ok(body) => response(StatusCode::OK, "application/json", body),
        Err(error) => registration_error_response(&error),
    }
}

async fn serve_registration(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
) -> Result<Vec<u8>, NugetError> {
    let request = parse_registration_path(path)?;
    let snapshot = service.service_index(entry_name, pool).await?;
    let cache_path = registration_path(&service.data_dir, entry_name, &request.relative);
    let descriptor_path =
        registration_descriptor_path(&service.data_dir, entry_name, &request.relative);

    if let (Some(mut body), Some(mut descriptor)) = (
        read_if_present(&cache_path)?,
        read_descriptor(&descriptor_path)?,
    ) && !descriptor.public_base_url.is_empty()
        && descriptor.selected_proxy == snapshot.selected_proxy
        && pool.proxy_members(entry_name).is_some_and(|members| {
            members
                .iter()
                .any(|member| member.name == descriptor.selected_proxy)
        })
    {
        if descriptor.public_base_url != service.root_url.as_str() {
            if let Some(rebased) = crate::nuget::render::rebase_public_urls(
                &body,
                &descriptor.public_base_url,
                service.root_url.as_str(),
            )? {
                atomic_write(&cache_path, &rebased)?;
                body = rebased;
            }
            descriptor.public_base_url = service.root_url.to_string();
            atomic_write(&descriptor_path, &serde_json::to_vec(&descriptor)?)?;
        }
        service.record_object(
            "nuget_registration_read",
            NugetAuditDetails {
                object_key: &request.relative,
                entry_name,
                selected_proxy: &descriptor.selected_proxy,
                object_type: "registration",
                cache_status: "hit",
                failure_category: None,
            },
        )?;
        return Ok(body);
    }

    let preferred = snapshot.selected_proxy.clone();
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
        let source_snapshot = if member.name == preferred {
            snapshot.clone()
        } else {
            match service
                .service_index_from_source(entry_name, &member.name, pool)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error)
                    if is_retryable(&error) || group_fallback_error(&error, pool, entry_name) =>
                {
                    if !is_not_found(&error) {
                        all_not_found = false;
                    }
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        let Some(registration_base) = source_snapshot.registration_url.clone() else {
            all_not_found = false;
            failures.push(format!("{}: no registration resource", member.name));
            continue;
        };
        let upstream_url = append_relative_path(&registration_base, &request.relative)?;
        let upstream = match pool
            .fetch_discovered_url_following_safe_redirects(
                &member.name,
                upstream_url.clone(),
                FetchOperation::Metadata,
            )
            .await
        {
            Ok(response) => response,
            Err(error) if group_fallback_source(&error, pool, entry_name) => {
                if !is_source_not_found(&error) {
                    all_not_found = false;
                }
                failures.push(format!("{}: {error}", member.name));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let mut body: Value = match serde_json::from_slice(&upstream.body) {
            Ok(body) => body,
            Err(error) => {
                all_not_found = false;
                failures.push(format!("{}: invalid JSON: {error}", member.name));
                continue;
            }
        };
        let mut bindings = BTreeMap::new();
        if let Err(error) = rewrite_registration(
            &mut body,
            &request,
            &source_snapshot,
            service,
            entry_name,
            &registration_base,
            &mut bindings,
        ) {
            all_not_found = false;
            failures.push(format!("{}: {error}", member.name));
            continue;
        }
        let body = serde_json::to_vec(&body)?;

        atomic_write(&cache_path, &body)?;
        atomic_write(
            &descriptor_path,
            &serde_json::to_vec(&RegistrationDescriptor {
                selected_proxy: source_snapshot.selected_proxy.clone(),
                upstream_url: upstream_url.to_string(),
                public_base_url: service.root_url.to_string(),
            })?,
        )?;
        for binding in bindings.values() {
            persist_binding(&service.data_dir, entry_name, binding)?;
        }
        if source_snapshot.selected_proxy != preferred {
            service.publish_service_index(entry_name, &source_snapshot)?;
            service.record_object(
                "nuget_registration_fallback",
                NugetAuditDetails {
                    object_key: &request.relative,
                    entry_name,
                    selected_proxy: &source_snapshot.selected_proxy,
                    object_type: "registration",
                    cache_status: "fallback",
                    failure_category: Some("source_unavailable_or_invalid"),
                },
            )?;
        }
        service.record_object(
            "nuget_registration_published",
            NugetAuditDetails {
                object_key: &request.relative,
                entry_name,
                selected_proxy: &source_snapshot.selected_proxy,
                object_type: "registration",
                cache_status: "published",
                failure_category: None,
            },
        )?;
        return Ok(body);
    }

    service.record_object(
        "nuget_registration_fetch_failed",
        NugetAuditDetails {
            object_key: &request.relative,
            entry_name,
            selected_proxy: "",
            object_type: "registration",
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
        request_key: request.relative,
        failures: failures.join("; "),
        status: (is_group && all_not_found).then_some(404),
    }
    .into())
}

pub(crate) async fn binding_for_source(
    service: &NugetService,
    entry_name: &str,
    package_id: &str,
    version: &str,
    snapshot: &ServiceIndexSnapshot,
    pool: &SourcePool,
) -> Result<PackageBinding, NugetError> {
    let registration_base = snapshot.registration_url.clone().ok_or_else(|| {
        NugetError::InvalidRegistration("service index has no registration resource".to_owned())
    })?;
    let relative = format!(
        "{}/{}.json",
        package_id.to_ascii_lowercase(),
        version.to_ascii_lowercase()
    );
    let upstream_url = append_relative_path(&registration_base, &relative)?;
    let upstream = pool
        .fetch_discovered_url_following_safe_redirects(
            &snapshot.selected_proxy,
            upstream_url,
            FetchOperation::Metadata,
        )
        .await?;
    let mut body: Value = serde_json::from_slice(&upstream.body)?;
    let request = RegistrationPath {
        package_id: package_id.to_owned(),
        relative,
        leaf_version: Some(version.to_owned()),
    };
    let mut bindings = BTreeMap::new();
    rewrite_registration(
        &mut body,
        &request,
        snapshot,
        service,
        entry_name,
        &registration_base,
        &mut bindings,
    )?;
    let binding = bindings
        .remove(&binding_key(package_id, version))
        .ok_or_else(|| {
            NugetError::InvalidRegistration(
                "registration leaf has no matching package content".to_owned(),
            )
        })?;
    Ok(binding)
}

fn parse_registration_path(path: &str) -> Result<RegistrationPath, NugetError> {
    let relative = path.strip_prefix("registration/").ok_or_else(|| {
        NugetError::InvalidRegistrationPath("registration path is invalid".to_owned())
    })?;
    let segments = relative.split('/').collect::<Vec<_>>();
    if segments.len() < 2
        || segments.iter().any(|segment| {
            !is_safe_segment(segment) || segment.contains('%') || matches!(*segment, "." | "..")
        })
        || !segments
            .last()
            .is_some_and(|segment| segment.ends_with(".json"))
    {
        return Err(NugetError::InvalidRegistrationPath(
            "registration path contains unsafe segments".to_owned(),
        ));
    }
    let package_id = segments[0].to_owned();
    let leaf_version = segments
        .last()
        .and_then(|segment| segment.strip_suffix(".json"))
        .filter(|version| *version != "index")
        .map(str::to_owned);
    Ok(RegistrationPath {
        package_id,
        relative: relative.to_owned(),
        leaf_version,
    })
}

fn rewrite_registration(
    value: &mut Value,
    request: &RegistrationPath,
    snapshot: &ServiceIndexSnapshot,
    service: &NugetService,
    entry_name: &str,
    registration_base: &Url,
    bindings: &mut BTreeMap<String, PackageBinding>,
) -> Result<(), NugetError> {
    let object = value.as_object_mut().ok_or_else(|| {
        NugetError::InvalidRegistration("registration response is not an object".to_owned())
    })?;
    rewrite_object(
        object,
        request,
        snapshot,
        service,
        entry_name,
        registration_base,
        bindings,
    )
}

fn rewrite_object(
    object: &mut serde_json::Map<String, Value>,
    request: &RegistrationPath,
    snapshot: &ServiceIndexSnapshot,
    service: &NugetService,
    entry_name: &str,
    registration_base: &Url,
    bindings: &mut BTreeMap<String, PackageBinding>,
) -> Result<(), NugetError> {
    let catalog_identity = object
        .get("catalogEntry")
        .and_then(Value::as_object)
        .and_then(|catalog| {
            let package_id = catalog.get("id").and_then(Value::as_str);
            let version = catalog.get("version").and_then(Value::as_str);
            package_id.zip(version).map(|(id, version)| {
                (
                    id.to_owned(),
                    version.to_owned(),
                    catalog
                        .get("packageHash")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    catalog
                        .get("packageHashAlgorithm")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
            })
        });

    if object.contains_key("catalogEntry") {
        match object.get_mut("catalogEntry") {
            Some(Value::String(_)) => {
                object.remove("catalogEntry");
            }
            Some(Value::Object(catalog)) => {
                catalog.remove("@id");
            }
            _ => {}
        }
    }

    if let Some(content) = object.get("packageContent").cloned() {
        let content = content.as_str().ok_or_else(|| {
            NugetError::InvalidRegistration("packageContent is not a URL string".to_owned())
        })?;
        let (package_id, version, package_hash, package_hash_algorithm) = catalog_identity
            .or_else(|| {
                let id = object.get("id").and_then(Value::as_str)?;
                let version = object.get("version").and_then(Value::as_str)?;
                Some((
                    id.to_owned(),
                    version.to_owned(),
                    object
                        .get("packageHash")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    object
                        .get("packageHashAlgorithm")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                ))
            })
            .or_else(|| {
                request
                    .leaf_version
                    .clone()
                    .map(|version| (request.package_id.clone(), version, None, None))
            })
            .ok_or_else(|| {
                NugetError::InvalidRegistration(
                    "packageContent has no package ID/version proof".to_owned(),
                )
            })?;
        validate_package_identity(
            &request.package_id,
            &package_id,
            request.leaf_version.as_deref(),
            &version,
        )?;
        let upstream_url = Url::parse(content).map_err(|_| {
            NugetError::InvalidRegistration("packageContent URL is invalid".to_owned())
        })?;
        let package_base = snapshot.package_base_url.as_ref().ok_or_else(|| {
            NugetError::InvalidRegistration(
                "service index has no package content resource".to_owned(),
            )
        })?;
        let expected_url = package_content_url(package_base, &package_id, &version)?;
        if !safe_resource_url(package_base, false, &upstream_url)
            || !same_origin(&expected_url, &upstream_url)
            || !expected_url
                .path()
                .eq_ignore_ascii_case(upstream_url.path())
        {
            return Err(NugetError::InvalidRegistration(
                "packageContent does not belong to the catalog package".to_owned(),
            ));
        }
        object.insert(
            "packageContent".to_owned(),
            Value::String(local_package_url(
                &service.root_url,
                entry_name,
                &package_id,
                &version,
            )),
        );
        let binding = PackageBinding {
            selected_proxy: snapshot.selected_proxy.clone(),
            package_id: package_id.to_owned(),
            version: version.to_owned(),
            upstream_url: upstream_url.to_string(),
            package_hash,
            package_hash_algorithm,
        };
        bindings.insert(binding_key(&package_id, &version), binding);
    }

    let page_like = object.contains_key("lower") || object.contains_key("upper");
    let mut dropped_ids = Vec::new();
    for (key, value) in object.iter_mut() {
        if key == "@context" {
            continue;
        }
        if key == "packageContent" || key == "catalogEntry" {
            if key == "catalogEntry"
                && let Value::Object(catalog) = value
            {
                rewrite_object(
                    catalog,
                    request,
                    snapshot,
                    service,
                    entry_name,
                    registration_base,
                    bindings,
                )?;
            }
            continue;
        }
        if matches!(key.as_str(), "@id" | "parent" | "registration")
            && let Some(url) = value.as_str()
        {
            let parsed = match Url::parse(url) {
                Ok(parsed) => parsed,
                Err(_) if key == "@id" => {
                    dropped_ids.push(key.clone());
                    continue;
                }
                Err(_) => {
                    return Err(NugetError::InvalidRegistration(
                        "registration URL is invalid".to_owned(),
                    ));
                }
            };
            match registration_relative_path(registration_base, &parsed) {
                Ok(relative) => {
                    *value = Value::String(local_registration_url(
                        &service.root_url,
                        entry_name,
                        &relative,
                    ));
                }
                Err(error) => {
                    if key != "@id" || page_like {
                        return Err(error);
                    }
                    dropped_ids.push(key.clone());
                }
            }
            continue;
        }
        if value.is_object() {
            rewrite_object(
                value.as_object_mut().expect("checked object"),
                request,
                snapshot,
                service,
                entry_name,
                registration_base,
                bindings,
            )?;
        } else if value.is_array() {
            for item in value.as_array_mut().expect("checked array") {
                if item.is_object() {
                    rewrite_object(
                        item.as_object_mut().expect("checked object"),
                        request,
                        snapshot,
                        service,
                        entry_name,
                        registration_base,
                        bindings,
                    )?;
                }
            }
        }
    }
    for key in dropped_ids {
        object.remove(&key);
    }
    Ok(())
}

fn validate_package_identity(
    requested_id: &str,
    actual_id: &str,
    requested_version: Option<&str>,
    version: &str,
) -> Result<(), NugetError> {
    if !actual_id.eq_ignore_ascii_case(requested_id)
        || requested_version.is_some_and(|requested| !version.eq_ignore_ascii_case(requested))
        || !is_safe_segment(actual_id)
        || !is_safe_segment(version)
    {
        return Err(NugetError::InvalidRegistration(
            "catalog package identity does not match the request".to_owned(),
        ));
    }
    Ok(())
}

fn package_content_url(base_url: &Url, package_id: &str, version: &str) -> Result<Url, NugetError> {
    let mut url = base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/{}/{}/{}.{}.nupkg",
        package_id.to_ascii_lowercase(),
        version.to_ascii_lowercase(),
        package_id.to_ascii_lowercase(),
        version.to_ascii_lowercase()
    ));
    Ok(url)
}

fn append_relative_path(base_url: &Url, relative: &str) -> Result<Url, NugetError> {
    let mut url = base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/{relative}"));
    Ok(url)
}

fn registration_relative_path(base_url: &Url, candidate: &Url) -> Result<String, NugetError> {
    if !same_origin(base_url, candidate) {
        return Err(NugetError::InvalidRegistration(
            "registration URL is outside the discovered resource".to_owned(),
        ));
    }
    let base_path = base_url.path().trim_end_matches('/');
    let relative = candidate
        .path()
        .strip_prefix(&format!("{base_path}/"))
        .ok_or_else(|| {
            NugetError::InvalidRegistration(
                "registration URL is outside the discovered resource".to_owned(),
            )
        })?;
    if relative.is_empty()
        || relative.contains('%')
        || relative.contains('\\')
        || relative.split('/').any(|segment| !is_safe_segment(segment))
    {
        return Err(NugetError::InvalidRegistration(
            "registration URL contains unsafe path".to_owned(),
        ));
    }
    Ok(relative.to_owned())
}

fn local_registration_url(root_url: &Url, entry_name: &str, relative: &str) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/mirror/nuget/{entry_name}/registration/{relative}"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn local_package_url(root_url: &Url, entry_name: &str, package_id: &str, version: &str) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    let package_id = package_id.to_ascii_lowercase();
    let version = version.to_ascii_lowercase();
    url.set_path(&format!(
        "{base_path}/mirror/nuget/{entry_name}/package/{package_id}/{version}/{package_id}.{version}.nupkg"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn registration_path(data_dir: &Path, entry_name: &str, relative: &str) -> PathBuf {
    crate::layout::metadata_root(data_dir, "nuget")
        .join(entry_name)
        .join("registration")
        .join(relative)
}

fn registration_descriptor_path(data_dir: &Path, entry_name: &str, relative: &str) -> PathBuf {
    let path = registration_path(data_dir, entry_name, relative);
    path.with_file_name(format!(
        "{}.source",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ))
}

pub(crate) fn binding_path(
    data_dir: &Path,
    entry_name: &str,
    package_id: &str,
    version: &str,
) -> PathBuf {
    crate::layout::metadata_root(data_dir, "nuget")
        .join(entry_name)
        .join("bindings")
        .join(package_id.to_ascii_lowercase())
        .join(format!("{}.json", version.to_ascii_lowercase()))
}

pub(crate) fn persist_binding(
    data_dir: &Path,
    entry_name: &str,
    binding: &PackageBinding,
) -> Result<(), NugetError> {
    atomic_write(
        &binding_path(data_dir, entry_name, &binding.package_id, &binding.version),
        &serde_json::to_vec(binding)?,
    )?;
    Ok(())
}

fn binding_key(package_id: &str, version: &str) -> String {
    format!(
        "{}\0{}",
        package_id.to_ascii_lowercase(),
        version.to_ascii_lowercase()
    )
}

fn read_descriptor(path: &Path) -> Result<Option<RegistrationDescriptor>, NugetError> {
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

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response {
    let mut response = (status, Body::from(body)).into_response();
    let content_type = axum::http::HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response
}

fn is_retryable(error: &NugetError) -> bool {
    matches!(
        error,
        NugetError::InvalidRegistration(_) | NugetError::InvalidServiceIndex(_)
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

fn registration_error_response(error: &NugetError) -> Response {
    let status = match error {
        NugetError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        NugetError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        NugetError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        NugetError::InvalidRegistration(_) => StatusCode::BAD_GATEWAY,
        NugetError::InvalidRegistrationPath(_) => StatusCode::NOT_FOUND,
        NugetError::InvalidVulnerability(_) => StatusCode::BAD_GATEWAY,
        NugetError::InvalidVulnerabilityPath(_) => StatusCode::NOT_FOUND,
        NugetError::Source(_) | NugetError::InvalidServiceIndex(_) => StatusCode::BAD_GATEWAY,
        NugetError::InvalidPackage(_) => StatusCode::NOT_FOUND,
        NugetError::InvalidPackageIndex(_) => StatusCode::BAD_GATEWAY,
        NugetError::InvalidSearch(_) => StatusCode::BAD_GATEWAY,
        NugetError::PackageIntegrityFailure(_) => StatusCode::BAD_GATEWAY,
        NugetError::Storage(_) | NugetError::Serialization(_) | NugetError::Audit(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    status.into_response()
}
