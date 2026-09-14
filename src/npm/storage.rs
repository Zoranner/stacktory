use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use thiserror::Error;

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
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IntegrityExpectation {
    pub integrity: Option<String>,
    pub shasum: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedArtifact {
    pub local_sha256: String,
    pub upstream_verified: bool,
}

#[derive(Debug)]
pub struct NpmStorage {
    data_dir: PathBuf,
    ecosystem: String,
}

#[derive(Deserialize, Serialize)]
struct MetadataPointer {
    key: NpmCacheKey,
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

    pub fn publish_verified_artifact(
        &self,
        key: &NpmCacheKey,
        artifact_key: &str,
        expectation: &IntegrityExpectation,
        contents: &[u8],
    ) -> Result<VerifiedArtifact, StorageError> {
        validate_key(key)?;
        if key.ecosystem != self.ecosystem {
            return Err(StorageError::InvalidIdentity(key.ecosystem.clone()));
        }
        validate_artifact_key(artifact_key)?;
        let upstream_verified = verify_contents(artifact_key, expectation, contents)?;
        let result = VerifiedArtifact {
            local_sha256: sha256(contents),
            upstream_verified,
        };
        atomic_write(&self.artifact_path(key, artifact_key), contents)?;
        Ok(result)
    }

    pub fn read_verified_artifact(
        &self,
        key: &NpmCacheKey,
        artifact_key: &str,
        expectation: &IntegrityExpectation,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        validate_key(key)?;
        if key.ecosystem != self.ecosystem {
            return Err(StorageError::InvalidIdentity(key.ecosystem.clone()));
        }
        validate_artifact_key(artifact_key)?;
        let path = self.artifact_path(key, artifact_key);
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        match verify_contents(artifact_key, expectation, &contents) {
            Ok(_) => {
                let _ = crate::retention::note_access(&path);
                Ok(Some(contents))
            }
            Err(StorageError::IntegrityMismatch(_)) => {
                fs::remove_file(path)?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn publish_metadata(
        &self,
        key: &NpmCacheKey,
        body: &[u8],
        descriptors: &[NpmArtifactDescriptor],
    ) -> Result<(), StorageError> {
        validate_metadata_key(key)?;
        if key.ecosystem != self.ecosystem {
            return Err(StorageError::InvalidIdentity(key.ecosystem.clone()));
        }
        for descriptor in descriptors {
            validate_artifact_key(&descriptor.artifact_key)?;
            atomic_write(
                &self.descriptor_path(&key.entry, &descriptor.artifact_key),
                &serde_json::to_vec(descriptor)
                    .map_err(|error| StorageError::Io(io::Error::other(error)))?,
            )?;
        }
        atomic_write(&self.metadata_body_path(key), body)?;
        atomic_write(
            &self.metadata_envelope_path(key),
            &serde_json::to_vec(&MetadataEnvelope {
                descriptors: descriptors.to_vec(),
            })
            .map_err(|error| StorageError::Io(io::Error::other(error)))?,
        )?;
        atomic_write(
            &self.metadata_pointer_path(&key.entry, &key.package),
            &toml::to_string(&MetadataPointer { key: key.clone() })
                .map_err(|error| StorageError::Io(io::Error::other(error)))?
                .into_bytes(),
        )
    }

    pub fn read_metadata(
        &self,
        entry: &str,
        package: &str,
    ) -> Result<Option<StoredMetadata>, StorageError> {
        let pointer_path = self.metadata_pointer_path(entry, package);
        let pointer_contents = match fs::read_to_string(&pointer_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        let pointer: MetadataPointer = toml::from_str(&pointer_contents)
            .map_err(|error| StorageError::Io(io::Error::other(error)))?;
        validate_metadata_key(&pointer.key)?;
        if pointer.key.ecosystem != self.ecosystem
            || pointer.key.entry != entry
            || pointer.key.package != package
        {
            return Err(StorageError::InvalidIdentity(pointer.key.package));
        }
        let body = fs::read(self.metadata_body_path(&pointer.key))?;
        let envelope: MetadataEnvelope =
            serde_json::from_slice(&fs::read(self.metadata_envelope_path(&pointer.key))?)
                .map_err(|error| StorageError::Io(io::Error::other(error)))?;
        Ok(Some(StoredMetadata {
            key: pointer.key,
            body,
            descriptors: envelope.descriptors,
        }))
    }

    pub fn read_descriptor(
        &self,
        entry: &str,
        artifact_key: &str,
    ) -> Result<Option<NpmArtifactDescriptor>, StorageError> {
        validate_artifact_key(artifact_key)?;
        match fs::read(self.descriptor_path(entry, artifact_key)) {
            Ok(contents) => serde_json::from_slice(&contents)
                .map(Some)
                .map_err(|error| StorageError::Io(io::Error::other(error))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StorageError::Io(error)),
        }
    }

    pub fn artifact_path(&self, key: &NpmCacheKey, artifact_key: &str) -> PathBuf {
        crate::layout::artifact_objects_root(
            &self.data_dir,
            &self.ecosystem,
            &safe_component(&key.selected_proxy),
        )
        .join(safe_component(&key.entry))
        .join(safe_component(&key.package))
        .join(safe_component(&key.version))
        .join(safe_component(&key.representation))
        .join(artifact_key)
    }

    fn metadata_body_path(&self, key: &NpmCacheKey) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, &self.ecosystem)
            .join(safe_component(&key.entry))
            .join(safe_component(&key.package))
            .join(safe_component(&key.selected_proxy))
            .join(format!("{}-{}.json", key.version, key.representation))
    }

    fn metadata_envelope_path(&self, key: &NpmCacheKey) -> PathBuf {
        self.metadata_body_path(key).with_extension("envelope.json")
    }

    fn metadata_pointer_path(&self, entry: &str, package: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, &self.ecosystem)
            .join(safe_component(entry))
            .join(safe_component(package))
            .join("current.toml")
    }

    fn descriptor_path(&self, entry: &str, artifact_key: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, &self.ecosystem)
            .join(safe_component(entry))
            .join("descriptors")
            .join(format!("{artifact_key}.json"))
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

fn verify_contents(
    artifact_key: &str,
    expectation: &IntegrityExpectation,
    contents: &[u8],
) -> Result<bool, StorageError> {
    if let Some(integrity) = expectation.integrity.as_deref() {
        verify_integrity(artifact_key, integrity, contents)?;
        return Ok(true);
    }
    if let Some(shasum) = expectation.shasum.as_deref() {
        if shasum.len() != 40 || !shasum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StorageError::InvalidIntegrity(shasum.to_owned()));
        }
        let actual = format!("{:x}", Sha1::digest(contents));
        if !actual.eq_ignore_ascii_case(shasum) {
            return Err(StorageError::IntegrityMismatch(artifact_key.to_owned()));
        }
        return Ok(true);
    }
    Ok(false)
}

fn verify_integrity(
    artifact_key: &str,
    integrity: &str,
    contents: &[u8],
) -> Result<(), StorageError> {
    let mut supported = false;
    for token in integrity.split_whitespace() {
        let Some((algorithm, encoded)) = token.split_once('-') else {
            continue;
        };
        let expected = STANDARD
            .decode(encoded)
            .map_err(|_| StorageError::InvalidIntegrity(integrity.to_owned()))?;
        let actual = match algorithm {
            "sha512" => {
                supported = true;
                sha2::Sha512::digest(contents).to_vec()
            }
            "sha384" => {
                supported = true;
                sha2::Sha384::digest(contents).to_vec()
            }
            "sha256" => {
                supported = true;
                Sha256::digest(contents).to_vec()
            }
            _ => continue,
        };
        if actual == expected {
            return Ok(());
        }
    }
    if !supported {
        return Err(StorageError::InvalidIntegrity(integrity.to_owned()));
    }
    Err(StorageError::IntegrityMismatch(artifact_key.to_owned()))
}

fn safe_component(value: &str) -> String {
    value.replace('@', "_at_").replace(['/', '\\'], "_slash_")
}

fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::Io(io::Error::other("npm cache has no parent")))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| StorageError::Io(io::Error::other("npm cache has no file name")))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));
    fs::write(&temporary, contents)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(StorageError::Io(error))
        }
    }
}
