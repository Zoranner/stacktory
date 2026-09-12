use axum::{
    body::Body,
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use url::Url;

use crate::{
    nuget::service_index::{NugetAuditDetails, NugetError, NugetService},
    sources::{FetchOperation, SourcePool, SourcePoolError},
};

pub(crate) async fn serve(
    service: &NugetService,
    entry_name: &str,
    query: &str,
    pool: &SourcePool,
) -> Response {
    match search(service, entry_name, query, pool).await {
        Ok(body) => response(StatusCode::OK, "application/json", body),
        Err(error) => error_response(&error),
    }
}

async fn search(
    service: &NugetService,
    entry_name: &str,
    query: &str,
    pool: &SourcePool,
) -> Result<Vec<u8>, NugetError> {
    if query.len() > 16 * 1024 || query.chars().any(char::is_control) {
        return Err(NugetError::InvalidSearch(
            "search query is too long or contains control characters".to_owned(),
        ));
    }
    let initial = service.service_index(entry_name, pool).await?;
    let preferred = initial.selected_proxy.clone();
    let mut members = pool
        .proxy_members(entry_name)
        .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
    if let Some(position) = members.iter().position(|member| member.name == preferred) {
        let selected = members.remove(position);
        members.insert(0, selected);
    }
    let is_group = pool.is_group(entry_name);
    let mut failures = Vec::new();

    for member in members {
        let snapshot = if member.name == preferred {
            if initial.search_url.is_some() {
                initial.clone()
            } else {
                match service
                    .service_index_from_source(entry_name, &member.name, pool)
                    .await
                {
                    Ok(snapshot) => snapshot,
                    Err(error) if is_group && is_retryable(&error) => {
                        failures.push(format!("{}: {error}", member.name));
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
        } else {
            match service
                .service_index_from_source(entry_name, &member.name, pool)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) if is_group && is_retryable(&error) => {
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        let Some(search_base) = snapshot.search_url.clone() else {
            failures.push(format!("{}: search resource is missing", member.name));
            continue;
        };
        let mut search_url = search_base;
        search_url.set_query((!query.is_empty()).then_some(query));
        let upstream = match pool
            .fetch_discovered_url_with_query_following_safe_redirects(
                &snapshot.selected_proxy,
                search_url,
                FetchOperation::Metadata,
            )
            .await
        {
            Ok(response) => response,
            Err(error) if is_group && is_source_retryable(&error) => {
                failures.push(format!("{}: {error}", member.name));
                continue;
            }
            Err(error) if is_group && is_source_not_found(&error) => {
                failures.push(format!("{}: {error}", member.name));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let mut body: Value = serde_json::from_slice(&upstream.body)?;
        rewrite_registration_links(
            &mut body,
            snapshot.registration_url.as_ref(),
            service,
            entry_name,
        )?;
        let body = serde_json::to_vec(&body)?;
        service.record_object(
            "nuget_search_query",
            NugetAuditDetails {
                object_key: "search/query",
                entry_name,
                selected_proxy: &snapshot.selected_proxy,
                object_type: "search",
                cache_status: "published",
                failure_category: None,
            },
        )?;
        return Ok(body);
    }

    service.record_object(
        "nuget_search_fetch_failed",
        NugetAuditDetails {
            object_key: "search/query",
            entry_name,
            selected_proxy: "",
            object_type: "search",
            cache_status: "failed",
            failure_category: Some("source_failed"),
        },
    )?;
    Err(SourcePoolError::AllSourcesFailed {
        entry_name: entry_name.to_owned(),
        request_key: "search/query".to_owned(),
        failures: failures.join("; "),
        status: None,
    }
    .into())
}

fn rewrite_registration_links(
    value: &mut Value,
    registration_base: Option<&Url>,
    service: &NugetService,
    entry_name: &str,
) -> Result<(), NugetError> {
    let Some(registration_base) = registration_base else {
        return Ok(());
    };
    let object = value.as_object_mut().ok_or_else(|| {
        NugetError::InvalidSearch("search response is not a JSON object".to_owned())
    })?;
    for (key, value) in object.iter_mut() {
        if key == "@base"
            && let Some(upstream) = value.as_str()
        {
            let upstream = Url::parse(upstream).map_err(|_| {
                NugetError::InvalidSearch("search context base URL is invalid".to_owned())
            })?;
            if !same_origin(registration_base, &upstream) {
                return Err(NugetError::InvalidSearch(
                    "search context base URL is outside the discovered resource".to_owned(),
                ));
            }
            *value = Value::String(local_registration_base_url(&service.root_url, entry_name));
            continue;
        }
        if key == "registration"
            && let Some(upstream) = value.as_str()
        {
            let upstream = Url::parse(upstream).map_err(|_| {
                NugetError::InvalidSearch("search registration URL is invalid".to_owned())
            })?;
            let relative = registration_relative_path(registration_base, &upstream)?;
            *value = Value::String(local_registration_url(
                &service.root_url,
                entry_name,
                &relative,
            ));
            continue;
        }
        if key == "@id"
            && let Some(upstream) = value.as_str()
        {
            let upstream = Url::parse(upstream).map_err(|_| {
                NugetError::InvalidSearch("search resource ID is invalid".to_owned())
            })?;
            let relative = registration_relative_path(registration_base, &upstream)?;
            *value = Value::String(local_registration_url(
                &service.root_url,
                entry_name,
                &relative,
            ));
            continue;
        }
        if key.ends_with("Url") {
            *value = Value::Null;
            continue;
        }
        if value.is_object() {
            rewrite_registration_links(value, Some(registration_base), service, entry_name)?;
        } else if value.is_array() {
            for item in value.as_array_mut().expect("checked array") {
                if item.is_object() || item.is_array() {
                    rewrite_registration_links(item, Some(registration_base), service, entry_name)?;
                }
            }
        }
    }
    Ok(())
}

fn registration_relative_path(base_url: &Url, candidate: &Url) -> Result<String, NugetError> {
    if base_url.scheme() != candidate.scheme()
        || base_url.host_str() != candidate.host_str()
        || base_url.port_or_known_default() != candidate.port_or_known_default()
    {
        return Err(NugetError::InvalidSearch(
            "search registration URL is outside the discovered resource".to_owned(),
        ));
    }
    let base_path = base_url.path().trim_end_matches('/');
    let relative = candidate
        .path()
        .strip_prefix(&format!("{base_path}/"))
        .ok_or_else(|| {
            NugetError::InvalidSearch(
                "search registration URL is outside the discovered resource".to_owned(),
            )
        })?;
    if relative.is_empty()
        || relative.contains('%')
        || relative.contains('\\')
        || relative
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(NugetError::InvalidSearch(
            "search registration path is unsafe".to_owned(),
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

fn local_registration_base_url(root_url: &Url, entry_name: &str) -> String {
    local_registration_url(root_url, entry_name, "")
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_retryable(error: &NugetError) -> bool {
    matches!(
        error,
        NugetError::Source(SourcePoolError::AllSourcesFailed { .. })
            | NugetError::InvalidSearch(_)
            | NugetError::InvalidServiceIndex(_)
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

fn response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response {
    let mut response = (status, Body::from(body)).into_response();
    let content_type = axum::http::HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response
}

fn error_response(error: &NugetError) -> Response {
    let status = match error {
        NugetError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        NugetError::InvalidSearch(_) => StatusCode::BAD_REQUEST,
        NugetError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        NugetError::Source(_)
        | NugetError::InvalidServiceIndex(_)
        | NugetError::InvalidRegistration(_)
        | NugetError::InvalidRegistrationPath(_)
        | NugetError::InvalidPackage(_)
        | NugetError::InvalidPackageIndex(_)
        | NugetError::PackageIntegrityFailure(_) => StatusCode::BAD_GATEWAY,
        NugetError::Storage(_) | NugetError::Serialization(_) | NugetError::Audit(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    status.into_response()
}
