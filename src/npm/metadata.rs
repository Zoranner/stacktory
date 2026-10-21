use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use super::path::{NpmPathError, validate_package_name};

#[derive(Clone, Debug)]
pub struct NpmMetadata {
    pub body: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NpmArtifactDescriptor {
    pub artifact_key: String,
    pub package: String,
    pub version: String,
    pub selected_proxy: String,
    #[serde(default)]
    pub source_fingerprint: String,
    pub upstream_tarball: String,
    pub integrity: Option<String>,
    pub shasum: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RewrittenMetadata {
    pub body: Value,
    pub descriptors: Vec<NpmArtifactDescriptor>,
}

#[derive(Debug, Error)]
pub enum NpmMetadataError {
    #[error("npm metadata is invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("npm metadata package `{actual}` does not match requested package `{expected}`")]
    PackageMismatch { expected: String, actual: String },
    #[error("npm metadata has no versions object")]
    MissingVersions,
    #[error("npm version `{0}` is invalid")]
    InvalidVersion(String),
    #[error("npm version `{0}` is not a JSON object")]
    InvalidVersionObject(String),
    #[error("npm version `{0}` has no dist object")]
    MissingDist(String),
    #[error("npm version `{0}` has no tarball URL")]
    MissingTarball(String),
    #[error("npm tarball URL `{0}` is invalid")]
    InvalidTarballUrl(String),
    #[error("npm entry name `{0}` is invalid")]
    InvalidEntryName(String),
    #[error("package name `{0}` is invalid")]
    InvalidPackageName(String),
    #[error(transparent)]
    Path(#[from] NpmPathError),
}

pub fn parse_metadata(
    source: &[u8],
    requested_package: &str,
) -> Result<NpmMetadata, NpmMetadataError> {
    parse_compatible_metadata(source, requested_package, is_npm_package_name)
}

pub fn parse_compatible_metadata(
    source: &[u8],
    requested_package: &str,
    package_validator: fn(&str) -> bool,
) -> Result<NpmMetadata, NpmMetadataError> {
    if !package_validator(requested_package) {
        return Err(NpmMetadataError::InvalidPackageName(
            requested_package.to_owned(),
        ));
    }
    let requested = requested_package.to_owned();
    let body: Value = serde_json::from_slice(source)?;
    let actual =
        body.get("name")
            .and_then(Value::as_str)
            .ok_or(NpmMetadataError::PackageMismatch {
                expected: requested.clone(),
                actual: String::new(),
            })?;
    if actual != requested {
        return Err(NpmMetadataError::PackageMismatch {
            expected: requested,
            actual: actual.to_owned(),
        });
    }
    let versions = body
        .get("versions")
        .and_then(Value::as_object)
        .ok_or(NpmMetadataError::MissingVersions)?;
    for (version, value) in versions {
        if semver::Version::parse(version).is_err() {
            return Err(NpmMetadataError::InvalidVersion(version.clone()));
        }
        let version_object = value
            .as_object()
            .ok_or_else(|| NpmMetadataError::InvalidVersionObject(version.clone()))?;
        let dist = version_object
            .get("dist")
            .and_then(Value::as_object)
            .ok_or_else(|| NpmMetadataError::MissingDist(version.clone()))?;
        let tarball = dist
            .get("tarball")
            .and_then(Value::as_str)
            .ok_or_else(|| NpmMetadataError::MissingTarball(version.clone()))?;
        validate_tarball_url(tarball)?;
    }
    Ok(NpmMetadata { body })
}

pub fn rewrite_metadata(
    metadata: &NpmMetadata,
    root_url: &Url,
    entry_name: &str,
    selected_proxy: &str,
) -> Result<RewrittenMetadata, NpmMetadataError> {
    rewrite_compatible_metadata(
        metadata,
        root_url,
        "npm",
        "npm",
        entry_name,
        selected_proxy,
        is_npm_package_name,
    )
}

pub fn rewrite_compatible_metadata(
    metadata: &NpmMetadata,
    root_url: &Url,
    ecosystem: &str,
    route_ecosystem: &str,
    entry_name: &str,
    selected_proxy: &str,
    package_validator: fn(&str) -> bool,
) -> Result<RewrittenMetadata, NpmMetadataError> {
    validate_entry_name(entry_name)?;
    if selected_proxy.is_empty() {
        return Err(NpmMetadataError::InvalidEntryName(
            selected_proxy.to_owned(),
        ));
    }

    let package = metadata
        .body
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| NpmMetadataError::PackageMismatch {
            expected: String::new(),
            actual: String::new(),
        })?
        .to_owned();
    if !package_validator(&package) {
        return Err(NpmMetadataError::InvalidPackageName(package));
    }

    let mut body = metadata.body.clone();
    let versions = body
        .get_mut("versions")
        .and_then(Value::as_object_mut)
        .ok_or(NpmMetadataError::MissingVersions)?;
    let mut descriptors = Vec::with_capacity(versions.len());

    for (version, value) in versions {
        let version_object = value
            .as_object_mut()
            .ok_or_else(|| NpmMetadataError::InvalidVersionObject(version.clone()))?;
        if semver::Version::parse(version).is_err() {
            return Err(NpmMetadataError::InvalidVersion(version.clone()));
        }
        let dist = version_object
            .get_mut("dist")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| NpmMetadataError::MissingDist(version.clone()))?;
        let upstream_tarball = dist
            .get("tarball")
            .and_then(Value::as_str)
            .ok_or_else(|| NpmMetadataError::MissingTarball(version.clone()))?
            .to_owned();
        validate_tarball_url(&upstream_tarball)?;
        let descriptor = NpmArtifactDescriptor {
            artifact_key: artifact_key(
                ecosystem,
                entry_name,
                &package,
                version,
                selected_proxy,
                &upstream_tarball,
            ),
            package: package.clone(),
            version: version.clone(),
            selected_proxy: selected_proxy.to_owned(),
            source_fingerprint: String::new(),
            upstream_tarball: upstream_tarball.clone(),
            integrity: dist
                .get("integrity")
                .and_then(Value::as_str)
                .map(str::to_owned),
            shasum: dist
                .get("shasum")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        let internal_url = internal_tarball_url(
            root_url,
            route_ecosystem,
            entry_name,
            &descriptor.artifact_key,
        );
        dist.insert("tarball".to_owned(), Value::String(internal_url));
        descriptors.push(descriptor);
    }

    Ok(RewrittenMetadata { body, descriptors })
}

fn artifact_key(
    ecosystem: &str,
    entry_name: &str,
    package: &str,
    version: &str,
    selected_proxy: &str,
    upstream_tarball: &str,
) -> String {
    let identity = format!(
        "{ecosystem}\0{entry_name}\0registry\0{package}\0{version}\0tarball\0{selected_proxy}\0{upstream_tarball}"
    );
    format!("{:x}", Sha256::digest(identity.as_bytes()))
}

fn internal_tarball_url(
    root_url: &Url,
    route_ecosystem: &str,
    entry_name: &str,
    artifact_key: &str,
) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/mirror/{route_ecosystem}/{entry_name}/tarballs/{artifact_key}"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn validate_entry_name(entry_name: &str) -> Result<(), NpmMetadataError> {
    if entry_name.is_empty()
        || !entry_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(NpmMetadataError::InvalidEntryName(entry_name.to_owned()));
    }
    Ok(())
}

fn validate_tarball_url(value: &str) -> Result<(), NpmMetadataError> {
    let url =
        Url::parse(value).map_err(|_| NpmMetadataError::InvalidTarballUrl(value.to_owned()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path().contains('\\')
        || url.path().contains('%')
        || url.path().chars().any(char::is_control)
        || url
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(NpmMetadataError::InvalidTarballUrl(value.to_owned()));
    }
    Ok(())
}

fn is_npm_package_name(value: &str) -> bool {
    validate_package_name(value).is_ok()
}
