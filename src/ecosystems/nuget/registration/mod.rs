use std::{collections::BTreeMap, fs};

use axum::{
    body::Body,
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::{
    ecosystems::nuget::service_index::NugetAuditDetails,
    ecosystems::nuget::service_index::{NugetError, NugetService},
    storage::atomic::atomic_write,
    upstream::{FetchOperation, GroupRun, SourcePool, SourcePoolError},
};

use super::{
    group_fallback_error, group_fallback_source,
    path::{is_safe_segment, local_icon_url, safe_icon_url, safe_resource_url},
    service_index::ServiceIndexSnapshot,
    {metadata_is_stale, now_unix_secs},
};

mod cache;
mod protocol;

use cache::{
    binding_key, read_descriptor, read_if_present, registration_descriptor_path, registration_path,
};
pub(crate) use cache::{persist_binding, read_binding};
use protocol::{
    append_relative_path, local_package_url, local_registration_url, package_content_url,
    registration_relative_path, rewrite_registration, same_origin,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PackageBinding {
    pub(crate) selected_proxy: String,
    #[serde(default)]
    pub(crate) source_fingerprint: String,
    #[serde(default)]
    pub(crate) validated_at_unix_secs: u64,
    pub(crate) package_id: String,
    pub(crate) version: String,
    pub(crate) upstream_url: String,
    #[serde(default)]
    pub(crate) icon_url: Option<String>,
    pub(crate) package_hash: Option<String>,
    pub(crate) package_hash_algorithm: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RegistrationDescriptor {
    selected_proxy: String,
    #[serde(default)]
    source_fingerprint: String,
    upstream_url: String,
    #[serde(default)]
    public_base_url: String,
    #[serde(default)]
    validated_at_unix_secs: u64,
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
    base_url: &Url,
) -> Response {
    match serve_registration(service, entry_name, path, pool, base_url).await {
        Ok(body) => response(StatusCode::OK, "application/json", body),
        Err(error) => registration_error_response(&error),
    }
}

async fn serve_registration(
    service: &NugetService,
    entry_name: &str,
    path: &str,
    pool: &SourcePool,
    base_url: &Url,
) -> Result<Vec<u8>, NugetError> {
    let request = parse_registration_path(path)?;
    let snapshot = service.service_index(entry_name, pool, base_url).await?;
    let cache_path = registration_path(&service.data_dir, entry_name, &request.relative);
    let descriptor_path =
        registration_descriptor_path(&service.data_dir, entry_name, &request.relative);
    let cached = match (
        read_if_present(&cache_path)?,
        read_descriptor(&descriptor_path)?,
    ) {
        (Some(body), Some(descriptor))
            if !descriptor.public_base_url.is_empty()
                && descriptor.selected_proxy == snapshot.selected_proxy
                && descriptor.source_fingerprint == snapshot.source_fingerprint =>
        {
            Some((body, descriptor))
        }
        _ => None,
    };
    if let Some((mut body, descriptor)) = cached {
        if descriptor.public_base_url != base_url.as_str()
            && let Some(rebased) = crate::ecosystems::nuget::render::rebase_public_urls(
                &body,
                &descriptor.public_base_url,
                base_url.as_str(),
            )?
        {
            body = rebased;
        }
        let stale = metadata_is_stale(descriptor.validated_at_unix_secs);
        if !stale {
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
        match fetch_registration(
            service, entry_name, &request, &snapshot, pool, true, base_url,
        )
        .await
        {
            Ok(body) => return Ok(body),
            Err(error) if is_not_found(&error) => {
                let _ = fs::remove_file(&cache_path);
                let _ = fs::remove_file(&descriptor_path);
                service.record_object(
                    "nuget_registration_revalidate_failed",
                    NugetAuditDetails {
                        object_key: &request.relative,
                        entry_name,
                        selected_proxy: &descriptor.selected_proxy,
                        object_type: "registration",
                        cache_status: "failed",
                        failure_category: Some("not_found"),
                    },
                )?;
                return Err(error);
            }
            Err(_) => {
                service.record_object(
                    "nuget_registration_stale",
                    NugetAuditDetails {
                        object_key: &request.relative,
                        entry_name,
                        selected_proxy: &descriptor.selected_proxy,
                        object_type: "registration",
                        cache_status: "stale",
                        failure_category: Some("source_failed"),
                    },
                )?;
                return Ok(body);
            }
        }
    }

    fetch_registration(
        service, entry_name, &request, &snapshot, pool, false, base_url,
    )
    .await
}

async fn fetch_registration(
    service: &NugetService,
    entry_name: &str,
    request: &RegistrationPath,
    snapshot: &ServiceIndexSnapshot,
    pool: &SourcePool,
    revalidating: bool,
    base_url: &Url,
) -> Result<Vec<u8>, NugetError> {
    let preferred = snapshot.selected_proxy.clone();
    let mut run = GroupRun::new(pool, entry_name, Some(&preferred))?;
    let cache_path = registration_path(&service.data_dir, entry_name, &request.relative);
    let descriptor_path =
        registration_descriptor_path(&service.data_dir, entry_name, &request.relative);

    while let Some(member) = run.next_member() {
        let source_snapshot = if member.name == preferred {
            snapshot.clone()
        } else {
            match service
                .service_index_from_source(entry_name, &member.name, pool, base_url)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error)
                    if is_retryable(&error) || group_fallback_error(&error, pool, entry_name) =>
                {
                    run.record_failure(&member.name, &error.to_string(), is_not_found(&error));
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        let Some(registration_base) = source_snapshot.registration_url.clone() else {
            run.record_failure(&member.name, "no registration resource", false);
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
                run.record_failure(
                    &member.name,
                    &error.to_string(),
                    is_source_not_found(&error),
                );
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let mut body: Value = match serde_json::from_slice(&upstream.body) {
            Ok(body) => body,
            Err(error) => {
                run.record_failure(&member.name, &format!("invalid JSON: {error}"), false);
                continue;
            }
        };
        let mut bindings = BTreeMap::new();
        if let Err(error) = rewrite_registration(
            &mut body,
            request,
            &source_snapshot,
            entry_name,
            &registration_base,
            &mut bindings,
            base_url,
        ) {
            run.record_failure(&member.name, &error.to_string(), false);
            continue;
        }
        let body = serde_json::to_vec(&body)?;

        atomic_write(&cache_path, &body)?;
        atomic_write(
            &descriptor_path,
            &serde_json::to_vec(&RegistrationDescriptor {
                selected_proxy: source_snapshot.selected_proxy.clone(),
                source_fingerprint: source_snapshot.source_fingerprint.clone(),
                upstream_url: upstream_url.to_string(),
                public_base_url: base_url.to_string(),
                validated_at_unix_secs: now_unix_secs(),
            })?,
        )?;
        for binding in bindings.values() {
            persist_binding(&service.data_dir, entry_name, binding)?;
        }
        if source_snapshot.selected_proxy != preferred {
            service.publish_service_index(entry_name, &source_snapshot, base_url)?;
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
            if revalidating {
                "nuget_registration_revalidated"
            } else {
                "nuget_registration_published"
            },
            NugetAuditDetails {
                object_key: &request.relative,
                entry_name,
                selected_proxy: &source_snapshot.selected_proxy,
                object_type: "registration",
                cache_status: if revalidating {
                    "revalidated"
                } else {
                    "published"
                },
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
            failure_category: Some(if run.all_not_found() {
                "not_found"
            } else {
                "source_failed"
            }),
        },
    )?;
    Err(run.all_sources_failed(request.relative.clone()).into())
}

pub(crate) async fn binding_for_source(
    entry_name: &str,
    package_id: &str,
    version: &str,
    snapshot: &ServiceIndexSnapshot,
    pool: &SourcePool,
    base_url: &Url,
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
        entry_name,
        &registration_base,
        &mut bindings,
        base_url,
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

fn rewrite_object(
    object: &mut serde_json::Map<String, Value>,
    request: &RegistrationPath,
    snapshot: &ServiceIndexSnapshot,
    entry_name: &str,
    registration_base: &Url,
    bindings: &mut BTreeMap<String, PackageBinding>,
    base_url: &Url,
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
    let icon_identity = catalog_identity
        .as_ref()
        .map(|(package_id, version, _, _)| (package_id.clone(), version.clone()))
        .or_else(|| {
            let package_id = object.get("id").and_then(Value::as_str)?;
            let version = object.get("version").and_then(Value::as_str)?;
            Some((package_id.to_owned(), version.to_owned()))
        })
        .or_else(|| {
            request
                .leaf_version
                .clone()
                .map(|version| (request.package_id.clone(), version))
        });
    let mut icon_url = rewrite_icon_field(
        object,
        icon_identity.as_ref(),
        snapshot,
        entry_name,
        base_url,
    );

    if object.contains_key("catalogEntry") {
        match object.get_mut("catalogEntry") {
            Some(Value::String(_)) => {
                object.remove("catalogEntry");
            }
            Some(Value::Object(catalog)) => {
                catalog.remove("@id");
                if icon_url.is_none() {
                    icon_url = safe_icon_upstream(catalog, icon_identity.as_ref(), snapshot);
                }
            }
            _ => {}
        }
    }

    if let Some(content) = object.get("packageContent").cloned() {
        let content = content.as_str().ok_or_else(|| {
            NugetError::InvalidRegistration("packageContent is not a URL string".to_owned())
        })?;
        let (package_id, version, package_hash, package_hash_algorithm) = catalog_identity
            .clone()
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
                base_url,
                entry_name,
                &package_id,
                &version,
            )),
        );
        let binding = PackageBinding {
            selected_proxy: snapshot.selected_proxy.clone(),
            source_fingerprint: snapshot.source_fingerprint.clone(),
            validated_at_unix_secs: now_unix_secs(),
            package_id: package_id.to_owned(),
            version: version.to_owned(),
            upstream_url: upstream_url.to_string(),
            icon_url: icon_url.clone(),
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
                    entry_name,
                    registration_base,
                    bindings,
                    base_url,
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
                    *value = Value::String(local_registration_url(base_url, entry_name, &relative));
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
                entry_name,
                registration_base,
                bindings,
                base_url,
            )?;
        } else if value.is_array() {
            for item in value.as_array_mut().expect("checked array") {
                if item.is_object() {
                    rewrite_object(
                        item.as_object_mut().expect("checked object"),
                        request,
                        snapshot,
                        entry_name,
                        registration_base,
                        bindings,
                        base_url,
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

fn rewrite_icon_field(
    object: &mut serde_json::Map<String, Value>,
    identity: Option<&(String, String)>,
    snapshot: &ServiceIndexSnapshot,
    entry_name: &str,
    base_url: &Url,
) -> Option<String> {
    if !object.contains_key("iconUrl") {
        return None;
    }
    let upstream = safe_icon_upstream(object, identity, snapshot);
    match (upstream, identity) {
        (Some(upstream), Some((package_id, version))) => {
            object.insert(
                "iconUrl".to_owned(),
                Value::String(local_icon_url(base_url, entry_name, package_id, version)),
            );
            Some(upstream)
        }
        _ => {
            object.remove("iconUrl");
            None
        }
    }
}

fn safe_icon_upstream(
    object: &serde_json::Map<String, Value>,
    identity: Option<&(String, String)>,
    snapshot: &ServiceIndexSnapshot,
) -> Option<String> {
    let icon = object.get("iconUrl")?.as_str()?.to_owned();
    let (_, _) = identity?;
    let package_base = snapshot.package_base_url.as_ref()?;
    let parsed = Url::parse(&icon).ok()?;
    safe_icon_url(package_base, &parsed).then_some(icon)
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
        NugetError::IconNotFound(_) => StatusCode::NOT_FOUND,
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
