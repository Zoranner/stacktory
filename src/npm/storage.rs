use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmCacheKey {
    pub entry: String,
    pub package: String,
    pub version: String,
    pub representation: String,
    pub selected_proxy: String,
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
        Self {
            data_dir: data_dir.into(),
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
        validate_artifact_key(artifact_key)?;
        let path = self.artifact_path(key, artifact_key);
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        match verify_contents(artifact_key, expectation, &contents) {
            Ok(_) => Ok(Some(contents)),
            Err(StorageError::IntegrityMismatch(_)) => {
                fs::remove_file(path)?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn artifact_path(&self, key: &NpmCacheKey, artifact_key: &str) -> PathBuf {
        self.data_dir
            .join("npm")
            .join("artifacts")
            .join(safe_component(&key.selected_proxy))
            .join("objects")
            .join(safe_component(&key.entry))
            .join(safe_component(&key.package))
            .join(safe_component(&key.version))
            .join(safe_component(&key.representation))
            .join(artifact_key)
    }
}

fn validate_key(key: &NpmCacheKey) -> Result<(), StorageError> {
    for value in [
        key.entry.as_str(),
        key.package.as_str(),
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
