use url::Url;

use crate::sources::pool::is_safe_configured_url;

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

pub(crate) fn is_safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}
