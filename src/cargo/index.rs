use serde::Deserialize;
use thiserror::Error;
use url::Url;

use super::path::{
    CargoPathError, normalize_crate_name, sparse_index_path, validate_entry_name, validate_version,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexRecord {
    pub name: String,
    pub version: String,
    pub checksum: String,
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

pub fn parse_index_lines(source: &str) -> Result<Vec<IndexRecord>, CargoIndexError> {
    source
        .lines()
        .enumerate()
        .map(|(line, contents)| {
            let line = line + 1;
            let raw: RawIndexRecord = serde_json::from_str(contents)
                .map_err(|error| CargoIndexError::InvalidJson { line, error })?;
            let name = normalize_crate_name(&raw.name).map_err(|error| {
                CargoIndexError::InvalidRecord {
                    line,
                    error: error.to_string(),
                }
            })?;
            if raw.name != name {
                return Err(CargoIndexError::InvalidRecord {
                    line,
                    error: "crate name is not normalized".to_owned(),
                });
            }
            validate_version(&raw.version).map_err(|error| CargoIndexError::InvalidRecord {
                line,
                error: error.to_string(),
            })?;
            let checksum = raw
                .checksum
                .ok_or_else(|| CargoIndexError::MissingChecksum {
                    name: name.clone(),
                    version: raw.version.clone(),
                })?;
            if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(CargoIndexError::InvalidChecksum {
                    name,
                    version: raw.version,
                });
            }

            Ok(IndexRecord {
                name,
                version: raw.version,
                checksum,
            })
        })
        .collect()
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
