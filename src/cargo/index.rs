use serde::Deserialize;
use thiserror::Error;
use url::Url;

use super::path::{
    CargoPathError, normalize_crate_name, sparse_index_path, validate_crate_name,
    validate_entry_name, validate_version,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexRecord {
    pub name: String,
    pub version: String,
    pub checksum: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoRegistryConfig {
    pub dl: String,
}

#[derive(Debug, Error)]
pub enum CargoConfigError {
    #[error("cargo registry config.json is invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("cargo registry config.json has no dl template")]
    MissingDownloadTemplate,
    #[error("cargo registry dl template is invalid: {0}")]
    InvalidDownloadTemplate(String),
    #[error(transparent)]
    Path(#[from] CargoPathError),
}

#[derive(Debug, Error)]
pub enum CargoIndexError {
    #[error("cargo index line {line} is invalid JSON: {error}")]
    InvalidJson {
        line: usize,
        error: serde_json::Error,
    },
    #[error("cargo index line {line} has invalid record: {error}")]
    InvalidRecord { line: usize, error: String },
    #[error("cargo index record `{name}` `{version}` has no checksum")]
    MissingChecksum { name: String, version: String },
    #[error("cargo index record `{name}` `{version}` has an invalid checksum")]
    InvalidChecksum { name: String, version: String },
    #[error(transparent)]
    Path(#[from] CargoPathError),
}

#[derive(Deserialize)]
struct RawIndexRecord {
    name: String,
    #[serde(rename = "vers")]
    version: String,
    #[serde(rename = "cksum")]
    checksum: Option<String>,
}

#[derive(Deserialize)]
struct RawRegistryConfig {
    dl: Option<String>,
}

pub fn parse_registry_config(source: &[u8]) -> Result<CargoRegistryConfig, CargoConfigError> {
    let raw: RawRegistryConfig = serde_json::from_slice(source)?;
    let dl = raw
        .dl
        .filter(|dl| !dl.is_empty())
        .ok_or(CargoConfigError::MissingDownloadTemplate)?;
    validate_template_markers(&dl)?;
    Ok(CargoRegistryConfig { dl })
}

pub fn parse_index_lines(source: &str) -> Result<Vec<IndexRecord>, CargoIndexError> {
    source
        .lines()
        .enumerate()
        .map(|(line, contents)| {
            let line = line + 1;
            let raw: RawIndexRecord = serde_json::from_str(contents)
                .map_err(|error| CargoIndexError::InvalidJson { line, error })?;
            validate_crate_name(&raw.name).map_err(|error| CargoIndexError::InvalidRecord {
                line,
                error: error.to_string(),
            })?;
            validate_version(&raw.version).map_err(|error| CargoIndexError::InvalidRecord {
                line,
                error: error.to_string(),
            })?;
            let checksum = raw
                .checksum
                .ok_or_else(|| CargoIndexError::MissingChecksum {
                    name: raw.name.clone(),
                    version: raw.version.clone(),
                })?;
            if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(CargoIndexError::InvalidChecksum {
                    name: raw.name.clone(),
                    version: raw.version.clone(),
                });
            }

            Ok(IndexRecord {
                name: raw.name,
                version: raw.version,
                checksum,
            })
        })
        .collect()
}

pub fn render_download_url(
    template: &str,
    crate_name: &str,
    version: &str,
    checksum: &str,
) -> Result<Url, CargoConfigError> {
    validate_crate_name(crate_name)?;
    validate_version(version)?;
    if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CargoConfigError::InvalidDownloadTemplate(
            "sha256 checksum is invalid".to_owned(),
        ));
    }

    let lower_name = normalize_crate_name(crate_name)?;
    let prefix = sparse_prefix(crate_name);
    let lower_prefix = sparse_prefix(&lower_name);
    let replacements = [
        ("{crate}", crate_name),
        ("{version}", version),
        ("{prefix}", &prefix),
        ("{lowerprefix}", &lower_prefix),
        ("{sha256-checksum}", checksum),
    ];
    let mut expanded = replace_template_markers(template, &replacements)?;
    if replacements
        .iter()
        .all(|(marker, _)| !template.contains(marker))
    {
        expanded = format!("{}/{{}}", template.trim_end_matches('/'));
        expanded = expanded.replace("{}", &format!("{crate_name}/{version}/download"));
    }

    if expanded.contains('\\')
        || expanded.contains('%')
        || expanded.split(['?', '#']).next().is_some_and(|value| {
            value
                .split('/')
                .any(|segment| segment == "." || segment == "..")
        })
    {
        return Err(CargoConfigError::InvalidDownloadTemplate(expanded));
    }
    let url = Url::parse(&expanded)
        .map_err(|error| CargoConfigError::InvalidDownloadTemplate(error.to_string()))?;
    validate_download_url(&url)?;
    Ok(url)
}

pub fn rewrite_index_lines(
    source: &str,
    _root_url: &Url,
    _entry_name: &str,
) -> Result<Vec<u8>, CargoIndexError> {
    parse_index_lines(source)?;
    Ok(source.as_bytes().to_vec())
}

pub fn rewrite_crate_download_url(
    root_url: &Url,
    entry_name: &str,
    crate_name: &str,
    version: &str,
) -> Result<Url, CargoPathError> {
    validate_entry_name(entry_name)?;
    let crate_name = normalize_crate_name(crate_name)?;
    validate_version(version)?;
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/mirror/cargo/{}/crates/{crate_name}/{version}.crate",
        entry_name
    ));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

pub(crate) fn download_template(root_url: &Url, entry_name: &str) -> String {
    let mut root_url = root_url.clone();
    root_url.set_query(None);
    root_url.set_fragment(None);
    let base = root_url.as_str().trim_end_matches('/');
    format!("{base}/mirror/cargo/{entry_name}/crates/{{crate}}/{{version}}.crate")
}

pub(crate) fn sparse_path_for_request(path: &str) -> Result<(String, String), CargoPathError> {
    let crate_name = path
        .rsplit('/')
        .next()
        .ok_or_else(|| CargoPathError::Invalid(path.to_owned()))?;
    let normalized_name = normalize_crate_name(crate_name)?;
    let expected = sparse_index_path(&normalized_name)?;
    if path != expected {
        return Err(CargoPathError::Invalid(path.to_owned()));
    }
    Ok((normalized_name, expected))
}

fn validate_template_markers(template: &str) -> Result<(), CargoConfigError> {
    let supported = [
        "crate",
        "version",
        "prefix",
        "lowerprefix",
        "sha256-checksum",
    ];
    let mut remaining = template;
    while let Some(start) = remaining.find('{') {
        let after_start = &remaining[start + 1..];
        let end = after_start
            .find('}')
            .ok_or_else(|| CargoConfigError::InvalidDownloadTemplate(template.to_owned()))?;
        let marker = &after_start[..end];
        if !supported.contains(&marker) {
            return Err(CargoConfigError::InvalidDownloadTemplate(
                template.to_owned(),
            ));
        }
        remaining = &after_start[end + 1..];
    }
    if remaining.contains('}') {
        return Err(CargoConfigError::InvalidDownloadTemplate(
            template.to_owned(),
        ));
    }
    Ok(())
}

fn replace_template_markers(
    template: &str,
    replacements: &[(&str, &str)],
) -> Result<String, CargoConfigError> {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(start) = remaining.find('{') {
        result.push_str(&remaining[..start]);
        let after_start = &remaining[start..];
        let end = after_start
            .find('}')
            .ok_or_else(|| CargoConfigError::InvalidDownloadTemplate(template.to_owned()))?;
        let marker = &after_start[..=end];
        let replacement = replacements
            .iter()
            .find_map(|(name, value)| (*name == marker).then_some(*value))
            .ok_or_else(|| CargoConfigError::InvalidDownloadTemplate(template.to_owned()))?;
        result.push_str(replacement);
        remaining = &after_start[end + 1..];
    }
    result.push_str(remaining);
    Ok(result)
}

fn sparse_prefix(crate_name: &str) -> String {
    let bytes = crate_name.as_bytes();
    match bytes.len() {
        1 => "1/".to_owned(),
        2 => "2/".to_owned(),
        3 => format!("3/{}/", &crate_name[..1]),
        _ => format!("{}/{}/", &crate_name[..2], &crate_name[2..4]),
    }
}

fn validate_download_url(url: &Url) -> Result<(), CargoConfigError> {
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
        return Err(CargoConfigError::InvalidDownloadTemplate(url.to_string()));
    }
    Ok(())
}
