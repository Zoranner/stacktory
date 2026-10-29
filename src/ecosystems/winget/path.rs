use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use url::{Url, form_urlencoded};

use super::WingetError;

pub(crate) const CONTRACT_VERSION: &str = "1.0.0";

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum WingetRoute {
    Information,
    ManifestSearch,
    PackageManifest { package_id: String },
    Installer { token: String },
}

pub(crate) fn parse_route(path: &str, raw_path: &str) -> Result<WingetRoute, WingetError> {
    let path = path.trim_start_matches('/');
    if !raw_path_is_safe(raw_path) {
        return Err(WingetError::InvalidPath(path.to_owned()));
    }
    if path == "information" {
        return Ok(WingetRoute::Information);
    }
    if path == "manifestSearch" {
        return Ok(WingetRoute::ManifestSearch);
    }
    if let Some(package_id) = path.strip_prefix("packageManifests/") {
        if !is_safe_package_id(package_id) {
            return Err(WingetError::InvalidPath(path.to_owned()));
        }
        return Ok(WingetRoute::PackageManifest {
            package_id: package_id.to_owned(),
        });
    }
    if let Some(rest) = path.strip_prefix("installers/") {
        let (token, extension) = match rest.split_once('.') {
            Some((token, extension)) => (token, Some(extension)),
            None => (rest, None),
        };
        let token = normalize_installer_token(token)
            .ok_or_else(|| WingetError::InvalidPath(path.to_owned()))?;
        if extension.is_some_and(|extension| !is_safe_installer_extension(extension)) {
            return Err(WingetError::InvalidPath(path.to_owned()));
        }
        return Ok(WingetRoute::Installer { token });
    }
    Err(WingetError::InvalidPath(path.to_owned()))
}

pub(crate) fn validate_query(query: Option<&str>) -> Result<Option<String>, WingetError> {
    let Some(query) = query else {
        return Ok(None);
    };
    if query.len() > 16 * 1024 || query.contains('#') || query.chars().any(char::is_control) {
        return Err(WingetError::InvalidRequest(
            "query is too long or contains control characters".to_owned(),
        ));
    }
    Ok(Some(query.to_owned()))
}

pub(crate) fn is_safe_package_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
        && !matches!(value, "." | "..")
}

pub(crate) fn is_safe_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
        && !matches!(value, "." | "..")
}

pub(crate) fn installer_identity(
    sha256: &str,
    installer_type: &str,
    architecture: &str,
    scope: &str,
    installer_locale: &str,
) -> String {
    format!(
        "{}\0{installer_type}\0{architecture}\0{scope}\0{installer_locale}",
        sha256.to_ascii_lowercase()
    )
}

pub(crate) fn installer_token(
    entry_name: &str,
    package_id: &str,
    version: &str,
    identity: &str,
) -> String {
    let canonical = format!("{entry_name}\0{package_id}\0{version}\0{identity}");
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

/// 安装包 URL 保留上游文件扩展名，供 `InstallerType: default` 判定安装器类型。
pub(crate) fn installer_extension(url: &Url) -> Option<String> {
    let file_name = url.path_segments()?.next_back()?;
    let (_, extension) = file_name.rsplit_once('.')?;
    if !is_safe_installer_extension(extension) {
        return None;
    }
    Some(extension.to_ascii_lowercase())
}

pub(crate) fn local_installer_url(
    root_url: &Url,
    entry_name: &str,
    token: &str,
    extension: Option<&str>,
) -> String {
    let mut url = root_url.clone();
    let base_path = url.path().trim_end_matches('/');
    let suffix = extension
        .map(|extension| format!(".{extension}"))
        .unwrap_or_default();
    url.set_path(&format!(
        "{base_path}/mirror/winget/{entry_name}/installers/{token}{suffix}"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

pub(crate) fn query_version(query: Option<&str>) -> Option<String> {
    query.and_then(|query| {
        form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key.eq_ignore_ascii_case("Version"))
            .map(|(_, value)| value.into_owned())
    })
}

pub(crate) fn version_query(version: &str) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("Version", version);
    serializer.finish()
}

pub(crate) fn installer_url_allowed(
    url: &Url,
    allowed_hosts: &BTreeSet<String>,
    require_https: bool,
) -> bool {
    crate::upstream::fetch::is_allowed_installer_url(url, allowed_hosts, require_https)
}

fn normalize_installer_token(value: &str) -> Option<String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

fn is_safe_installer_extension(value: &str) -> bool {
    (1..=10).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn raw_path_is_safe(raw_path: &str) -> bool {
    !raw_path.contains('\\')
        && !raw_path.contains('%')
        && !raw_path.chars().any(char::is_control)
        && !raw_path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
}
