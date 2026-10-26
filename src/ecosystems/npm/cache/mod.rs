//! npm 与 UPM 共用的元数据和制品缓存。

mod artifacts;
mod integrity;
mod metadata;

use std::{
    fs, io,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub use integrity::{IntegrityExpectation, VerifiedArtifact};

use super::{metadata::NpmArtifactDescriptor, path::validate_package_name};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NpmCacheKey {
    pub ecosystem: String,
    pub entry: String,
    pub protocol_variant: String,
    pub package: String,
    pub version: String,
    pub representation: String,
    pub selected_proxy: String,
}

#[derive(Clone, Debug)]
pub struct StoredMetadata {
    pub key: NpmCacheKey,
    pub body: Vec<u8>,
    pub descriptors: Vec<NpmArtifactDescriptor>,
    pub validated_at_unix_secs: u64,
    pub content_digest: String,
    pub source_fingerprint: String,
}

#[derive(Clone, Debug)]
pub struct ArtifactFile {
    pub path: PathBuf,
    pub length: u64,
}

#[derive(Debug)]
pub struct NpmStorage {
    pub(super) data_dir: PathBuf,
    pub(super) ecosystem: String,
}

#[derive(Deserialize, Serialize)]
struct MetadataPointer {
    key: NpmCacheKey,
    #[serde(default)]
    canonical: bool,
    #[serde(default)]
    validated_at_unix_secs: u64,
    #[serde(default)]
    content_digest: String,
    #[serde(default)]
    source_fingerprint: String,
}

#[derive(Deserialize, Serialize)]
struct MetadataEnvelope {
    descriptors: Vec<NpmArtifactDescriptor>,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("npm storage I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("npm cache identity `{0}` is invalid")]
    InvalidIdentity(String),
    #[error("npm artifact key `{0}` is invalid")]
    InvalidArtifactKey(String),
    #[error("npm integrity value `{0}` is invalid")]
    InvalidIntegrity(String),
    #[error("npm artifact does not match its expected digest: {0}")]
    IntegrityMismatch(String),
}

impl NpmStorage {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self::new_for_ecosystem(data_dir, "npm")
    }

    pub fn new_for_ecosystem(data_dir: impl Into<PathBuf>, ecosystem: &str) -> Self {
        Self {
            data_dir: data_dir.into(),
            ecosystem: ecosystem.to_owned(),
        }
    }
}

fn validate_key(key: &NpmCacheKey) -> Result<(), StorageError> {
    validate_package_name(&key.package)
        .map_err(|error| StorageError::InvalidIdentity(error.to_string()))?;
    for value in [
        key.ecosystem.as_str(),
        key.entry.as_str(),
        key.protocol_variant.as_str(),
        key.version.as_str(),
        key.representation.as_str(),
        key.selected_proxy.as_str(),
    ] {
        if value.is_empty()
            || value == "."
            || value == ".."
            || value.contains('\\')
            || value.contains('/')
            || value.chars().any(char::is_control)
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'_' | b'-')
            })
        {
            return Err(StorageError::InvalidIdentity(value.to_owned()));
        }
    }
    Ok(())
}

fn validate_metadata_key(key: &NpmCacheKey) -> Result<(), StorageError> {
    validate_key(key)?;
    if key.version != "catalog" || key.representation != "metadata" {
        return Err(StorageError::InvalidIdentity(key.version.clone()));
    }
    Ok(())
}

fn validate_artifact_key(value: &str) -> Result<(), StorageError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains(['/', '\\'])
        || value.chars().any(char::is_control)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(StorageError::InvalidArtifactKey(value.to_owned()));
    }
    Ok(())
}

fn safe_component(value: &str) -> String {
    value.replace('@', "_at_").replace(['/', '\\'], "_slash_")
}

fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn remove_if_present(path: &std::path::Path) -> Result<(), StorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StorageError::Io(error)),
    }
}
