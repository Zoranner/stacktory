use url::Url;

use crate::upstream::{pool::is_safe_configured_url, same_origin};

pub(crate) fn validate_service_index_url(url: &Url, require_https: bool) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path().ends_with("/v3/index.json")
        && (!require_https || url.scheme() == "https")
}

pub(crate) fn safe_resource_url(base_url: &Url, require_https: bool, candidate: &Url) -> bool {
    if !matches!(candidate.scheme(), "http" | "https")
        || candidate.host_str().is_none()
        || !candidate.username().is_empty()
        || candidate.password().is_some()
        || candidate.query().is_some()
        || candidate.fragment().is_some()
        || (require_https && candidate.scheme() != "https")
        || candidate.path().contains('\\')
        || candidate.path().contains('%')
        || candidate.path().chars().any(char::is_control)
        || candidate
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return false;
    }
    if base_url.scheme() != candidate.scheme()
        || base_url.host_str() != candidate.host_str()
        || base_url.port_or_known_default() != candidate.port_or_known_default()
    {
        return is_safe_configured_url(candidate);
    }
    true
}

pub(crate) fn safe_icon_url(package_base: &Url, candidate: &Url) -> bool {
    same_origin(package_base, candidate) && safe_resource_url(package_base, false, candidate)
}

pub(crate) fn flatcontainer_icon_url(package_base: &Url, package_id: &str, version: &str) -> Url {
    let mut url = package_base.clone();
    let base_path = url.path().trim_end_matches('/');
    let package_id = package_id.to_ascii_lowercase();
    let version = version.to_ascii_lowercase();
    url.set_path(&format!("{base_path}/{package_id}/{version}/icon"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

pub(crate) fn local_icon_url(
    root_url: &Url,
    entry_name: &str,
    package_id: &str,
    version: &str,
) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    let package_id = package_id.to_ascii_lowercase();
    let version = version.to_ascii_lowercase();
    url.set_path(&format!(
        "{base_path}/mirror/nuget/{entry_name}/icon/{package_id}/{version}"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

pub(crate) fn is_safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}
