use std::collections::BTreeSet;

use serde_json::{Map, Value};
use url::Url;

use super::{
    WingetError,
    path::{
        installer_extension, installer_identity, installer_token, installer_url_allowed,
        is_safe_package_id, is_safe_version, local_installer_url, query_version,
    },
};

#[derive(Clone, Debug)]
pub(crate) struct ManifestInstaller {
    pub(crate) package_id: String,
    pub(crate) version: String,
    pub(crate) installer_token: String,
    pub(crate) installer_sha256: String,
    pub(crate) installer_type: String,
    pub(crate) architecture: String,
    pub(crate) scope: String,
    pub(crate) installer_locale: String,
    pub(crate) installer_index: usize,
    pub(crate) request_query: Option<String>,
    pub(crate) upstream_url: String,
}

#[derive(Debug)]
pub(crate) struct RewrittenManifest {
    pub(crate) body: Vec<u8>,
    pub(crate) installers: Vec<ManifestInstaller>,
}

struct RewriteContext<'a> {
    entry_name: &'a str,
    package_id: &'a str,
    request_query: Option<&'a str>,
    allowed_hosts: &'a BTreeSet<String>,
    require_https: bool,
    base_url: &'a Url,
}

pub(crate) fn rewrite_manifest(
    source: &[u8],
    entry_name: &str,
    package_id: &str,
    request_query: Option<&str>,
    allowed_hosts: &BTreeSet<String>,
    require_https: bool,
    base_url: &Url,
) -> Result<RewrittenManifest, WingetError> {
    let mut value: Value = serde_json::from_slice(source)?;
    let data = value
        .get_mut("Data")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| WingetError::InvalidManifest("Data is missing".to_owned()))?;
    let upstream_id = data
        .get("PackageIdentifier")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WingetError::InvalidManifest("PackageIdentifier is missing".to_owned()))?;
    if !upstream_id.eq_ignore_ascii_case(package_id) {
        return Err(WingetError::InvalidManifest(
            "PackageIdentifier does not match the request".to_owned(),
        ));
    }
    if !is_safe_package_id(package_id) {
        return Err(WingetError::InvalidPath(package_id.to_owned()));
    }
    let versions = data
        .get_mut("Versions")
        .and_then(Value::as_array_mut)
        .filter(|versions| !versions.is_empty())
        .ok_or_else(|| WingetError::InvalidManifest("Versions is missing".to_owned()))?;
    let requested_version = query_version(request_query);
    let context = RewriteContext {
        entry_name,
        package_id,
        request_query,
        allowed_hosts,
        require_https,
        base_url,
    };
    let mut installers_out = Vec::new();

    match requested_version.as_deref() {
        Some(requested) => {
            let position = versions
                .iter()
                .position(|version| {
                    version
                        .get("PackageVersion")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value.trim().eq_ignore_ascii_case(requested))
                })
                .ok_or_else(|| WingetError::VersionNotFound {
                    package_id: package_id.to_owned(),
                    version: requested.to_owned(),
                })?;
            rewrite_version(&mut versions[position], &context, &mut installers_out)?;
            let selected = versions.remove(position);
            versions.clear();
            versions.push(selected);
        }
        None => {
            for version in versions.iter_mut() {
                rewrite_version(version, &context, &mut installers_out)?;
            }
        }
    }

    Ok(RewrittenManifest {
        body: serde_json::to_vec(&value)?,
        installers: installers_out,
    })
}

fn rewrite_version(
    version: &mut Value,
    context: &RewriteContext<'_>,
    installers_out: &mut Vec<ManifestInstaller>,
) -> Result<(), WingetError> {
    let version_object = version
        .as_object_mut()
        .ok_or_else(|| WingetError::InvalidManifest("version entry is not an object".to_owned()))?;
    let version_value = required_string(version_object, "PackageVersion")?
        .trim()
        .to_owned();
    if !is_safe_version(&version_value) {
        return Err(WingetError::InvalidManifest(
            "PackageVersion is not path safe".to_owned(),
        ));
    }
    let installers = version_object
        .get_mut("Installers")
        .and_then(Value::as_array_mut)
        .filter(|installers| !installers.is_empty())
        .ok_or_else(|| {
            WingetError::InvalidManifest(format!(
                "Installers is missing for version `{version_value}`"
            ))
        })?;
    for (installer_index, installer) in installers.iter_mut().enumerate() {
        let installer_object = installer.as_object_mut().ok_or_else(|| {
            WingetError::InvalidManifest("installer entry is not an object".to_owned())
        })?;
        let installer_url = required_string(installer_object, "InstallerUrl")?;
        let installer_url = Url::parse(installer_url.trim()).map_err(|_| {
            WingetError::InvalidManifest("InstallerUrl is not an absolute URL".to_owned())
        })?;
        if !installer_url_allowed(&installer_url, context.allowed_hosts, context.require_https) {
            return Err(WingetError::InstallerPolicy(
                installer_url.host_str().unwrap_or_default().to_owned(),
            ));
        }
        let installer_sha256 = required_string(installer_object, "InstallerSha256")?
            .trim()
            .to_owned();
        if installer_sha256.len() != 64
            || !installer_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(WingetError::InvalidManifest(
                "InstallerSha256 is not a SHA-256 hex digest".to_owned(),
            ));
        }
        let installer_type = required_string(installer_object, "InstallerType")?
            .trim()
            .to_owned();
        if installer_type.is_empty() {
            return Err(WingetError::InvalidManifest(
                "InstallerType is empty".to_owned(),
            ));
        }
        let architecture = required_string(installer_object, "Architecture")?
            .trim()
            .to_owned();
        if architecture.is_empty() {
            return Err(WingetError::InvalidManifest(
                "Architecture is empty".to_owned(),
            ));
        }
        let scope = optional_string(installer_object, "Scope")?;
        let scope = scope.trim().to_owned();
        let installer_locale = optional_string(installer_object, "InstallerLocale")?;
        let installer_locale = installer_locale.trim().to_owned();
        let identity = installer_identity(
            &installer_sha256,
            &installer_type,
            &architecture,
            &scope,
            &installer_locale,
        );
        let token = installer_token(
            context.entry_name,
            context.package_id,
            &version_value,
            &identity,
        );
        let extension = installer_extension(&installer_url);
        installer_object.insert(
            "InstallerUrl".to_owned(),
            Value::String(local_installer_url(
                context.base_url,
                context.entry_name,
                &token,
                extension.as_deref(),
            )),
        );
        installers_out.push(ManifestInstaller {
            package_id: context.package_id.to_owned(),
            version: version_value.clone(),
            installer_token: token,
            installer_sha256,
            installer_type,
            architecture,
            scope,
            installer_locale,
            installer_index,
            request_query: context.request_query.map(str::to_owned),
            upstream_url: installer_url.to_string(),
        });
    }
    Ok(())
}

fn required_string<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a str, WingetError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| WingetError::InvalidManifest(format!("{name} is missing")))
}

fn optional_string(object: &Map<String, Value>, name: &str) -> Result<String, WingetError> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(WingetError::InvalidManifest(format!(
            "{name} is not a string"
        ))),
    }
}
