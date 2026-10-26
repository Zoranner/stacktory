use serde_json::{Map, Value};
use url::Url;

use super::{NugetError, RewrittenServiceIndex};
use crate::ecosystems::nuget::path::safe_resource_url;

const REGISTRATIONS_BASE_URL: &str = "RegistrationsBaseUrl";
const PACKAGE_BASE_ADDRESS: &str = "PackageBaseAddress";
const SEARCH_QUERY_SERVICE: &str = "SearchQueryService";
const VULNERABILITY_INFO: &str = "VulnerabilityInfo/6.7.0";

pub(super) fn rewrite_service_index(
    source: &[u8],
    root_url: &Url,
    entry_name: &str,
    base_url: &Url,
    require_https: bool,
) -> Result<RewrittenServiceIndex, NugetError> {
    let mut index: Value = serde_json::from_slice(source)?;
    let resources = index
        .get_mut("resources")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| NugetError::InvalidServiceIndex("resources is missing".to_owned()))?;
    let mut rewritten = Vec::with_capacity(resources.len());
    let mut registration_resource: Option<(u8, Url, String)> = None;
    let mut registration_urls = Vec::new();
    let mut package_base_url = None;
    let mut search_resource: Option<(u8, Url)> = None;
    let mut vulnerability_url = None;
    for resource in resources.drain(..) {
        let Some(object) = resource.as_object() else {
            return Err(NugetError::InvalidServiceIndex(
                "resource is not an object".to_owned(),
            ));
        };
        let Some(resource_type) = object.get("@type").and_then(Value::as_str) else {
            continue;
        };
        let Some(upstream_url) = object.get("@id").and_then(Value::as_str) else {
            if is_supported_resource(resource_type) {
                return Err(NugetError::InvalidServiceIndex(
                    "supported resource has no @id".to_owned(),
                ));
            }
            continue;
        };
        let Some(upstream_url) = Url::parse(upstream_url).ok() else {
            if is_supported_resource(resource_type) {
                return Err(NugetError::InvalidServiceIndex(
                    "supported resource @id is not a URL".to_owned(),
                ));
            }
            continue;
        };
        if resource_type.starts_with(REGISTRATIONS_BASE_URL) {
            if !safe_resource_url(base_url, require_https, &upstream_url) {
                return Err(NugetError::InvalidServiceIndex(
                    "registration resource URL is outside the configured source".to_owned(),
                ));
            }
            registration_urls.push(upstream_url.clone());
            let rank = registration_resource_rank(resource_type);
            if rank.is_some_and(|rank| {
                registration_resource
                    .as_ref()
                    .is_none_or(|(current_rank, _, _)| rank > *current_rank)
            }) {
                registration_resource = Some((
                    rank.expect("checked registration rank"),
                    upstream_url,
                    resource_type.to_owned(),
                ));
            }
        } else if resource_type.starts_with(PACKAGE_BASE_ADDRESS) {
            if package_base_url.is_some() {
                continue;
            }
            if !safe_resource_url(base_url, require_https, &upstream_url) {
                return Err(NugetError::InvalidServiceIndex(
                    "package resource URL is outside the configured source".to_owned(),
                ));
            }
            package_base_url = Some(upstream_url);
            rewritten.push(rewrite_resource(
                object,
                local_resource_url(root_url, entry_name, "package/"),
            ));
        } else if resource_type.starts_with(SEARCH_QUERY_SERVICE) {
            if !safe_resource_url(base_url, require_https, &upstream_url) {
                return Err(NugetError::InvalidServiceIndex(
                    "search resource URL is unsafe".to_owned(),
                ));
            }
            let rank = search_resource_rank(resource_type);
            if rank.is_some_and(|rank| {
                search_resource
                    .as_ref()
                    .is_none_or(|(current_rank, _)| rank > *current_rank)
            }) {
                search_resource = Some((rank.expect("checked search resource"), upstream_url));
            }
        } else if resource_type == VULNERABILITY_INFO {
            if vulnerability_url.is_some() {
                continue;
            }
            if !safe_resource_url(base_url, require_https, &upstream_url) {
                return Err(NugetError::InvalidServiceIndex(
                    "vulnerability resource URL is unsafe".to_owned(),
                ));
            }
            vulnerability_url = Some(upstream_url);
            rewritten.push(rewrite_resource(
                object,
                local_resource_url(root_url, entry_name, "vulnerability/index.json"),
            ));
        }
    }
    let registration_url = if let Some((_, url, resource_type)) = registration_resource {
        rewritten.push(rewrite_resource_with_type(
            &Map::new(),
            local_resource_url(root_url, entry_name, "registration/"),
            Some(&resource_type),
        ));
        Some(url)
    } else {
        None
    };
    if registration_url.is_none() || package_base_url.is_none() {
        return Err(NugetError::InvalidServiceIndex(
            "service index lacks a restore resource".to_owned(),
        ));
    }
    let search_url = search_resource.map(|(_, url)| url);
    if search_url.is_some() {
        rewritten.push(rewrite_resource_with_type(
            &Map::new(),
            local_resource_url(root_url, entry_name, "search/query"),
            Some("SearchQueryService/Versioned"),
        ));
    }
    *resources = rewritten;
    Ok(RewrittenServiceIndex {
        body: serde_json::to_vec(&index)?,
        registration_url,
        registration_urls,
        package_base_url,
        search_url,
        vulnerability_url,
    })
}

fn search_resource_rank(resource_type: &str) -> Option<u8> {
    match resource_type {
        "SearchQueryService/3.5.0" => Some(3),
        "SearchQueryService/3.0.0-rc" => Some(2),
        "SearchQueryService/3.0.0-beta" => Some(1),
        "SearchQueryService" => Some(0),
        _ if resource_type.starts_with("SearchQueryService/") => Some(1),
        _ => None,
    }
}

fn registration_resource_rank(resource_type: &str) -> Option<u8> {
    match resource_type {
        "RegistrationsBaseUrl/Versioned" => Some(5),
        "RegistrationsBaseUrl/3.6.0" => Some(4),
        "RegistrationsBaseUrl/3.4.0" => Some(3),
        "RegistrationsBaseUrl/3.0.0-rc" => Some(2),
        "RegistrationsBaseUrl/3.0.0-beta" => Some(1),
        "RegistrationsBaseUrl" => Some(0),
        _ if resource_type.starts_with("RegistrationsBaseUrl/") => Some(0),
        _ => None,
    }
}

fn is_supported_resource(resource_type: &str) -> bool {
    resource_type.starts_with(REGISTRATIONS_BASE_URL)
        || resource_type.starts_with(PACKAGE_BASE_ADDRESS)
}

fn rewrite_resource(resource: &Map<String, Value>, local_url: Url) -> Value {
    rewrite_resource_with_type(resource, local_url, None)
}

fn rewrite_resource_with_type(
    resource: &Map<String, Value>,
    local_url: Url,
    resource_type: Option<&str>,
) -> Value {
    let mut resource = resource.clone();
    resource.insert("@id".to_owned(), Value::String(local_url.to_string()));
    resource.remove("comment");
    if let Some(resource_type) = resource_type {
        resource.insert("@type".to_owned(), Value::String(resource_type.to_owned()));
    }
    Value::Object(resource)
}

fn local_resource_url(root_url: &Url, entry_name: &str, resource: &str) -> Url {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/mirror/nuget/{entry_name}/{resource}"));
    url.set_query(None);
    url.set_fragment(None);
    url
}
