use std::net::IpAddr;

use url::Url;

use super::model::ConfigError;

pub(super) fn validate_public_url(url: &Url) -> Result<(), ConfigError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ConfigError::PublicUrlUnsupportedScheme);
    }
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::InvalidPublicUrl);
    }
    if url.scheme() != "https" && !is_localhost(url) {
        return Err(ConfigError::PublicUrlMustUseHttps);
    }
    Ok(())
}

pub(super) fn is_valid_docker_registry_public_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.port() != Some(0)
}

pub(super) fn is_valid_channel(channel: &str) -> bool {
    !channel.is_empty()
        && channel != "."
        && channel != ".."
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

pub(super) fn is_valid_source_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(super) fn is_valid_source_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn is_localhost(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}
