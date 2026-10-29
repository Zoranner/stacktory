//! PyPI 项目的规范索引缓存与文件对象缓存。
//!
//! 项目索引属于 metadata：`current.toml` 指针、规范 `index.json`、按入口/项目/
//! selected proxy 隔离的 HTML/JSON 表示缓存，以及 `files/` 下的文件绑定。
//! wheel、sdist 和其他项目文件属于 artifact，按 selected proxy 隔离并先校验再原子发布。

mod artifacts;
mod metadata;

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

pub(crate) use metadata::{PyPiFileBinding, PyPiMetadataKey, StoredProjectIndex};

use super::{
    accept::PyPiRepresentation,
    path::{is_safe_file_name, is_valid_entry_name},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PyPiRepresentationKey {
    pub(crate) entry: String,
    pub(crate) project: String,
    pub(crate) selected_proxy: String,
    pub(crate) canonical_digest: String,
    pub(crate) base_url_fingerprint: String,
    pub(crate) representation: PyPiRepresentation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PyPiArtifactKey {
    pub(crate) entry: String,
    pub(crate) project: String,
    pub(crate) selected_proxy: String,
    pub(crate) filename: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ArtifactFile {
    pub(crate) path: PathBuf,
    pub(crate) length: u64,
}

#[derive(Debug, Error)]
pub(crate) enum StorageError {
    #[error("PyPI storage I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("PyPI cache identity `{0}` is invalid")]
    InvalidIdentity(String),
    #[error("PyPI cache digest `{0}` is invalid")]
    InvalidDigest(String),
    #[error("PyPI artifact does not match its expected digest: {0}")]
    IntegrityMismatch(String),
}

#[derive(Clone, Debug)]
pub(crate) struct PyPiStorage {
    data_dir: PathBuf,
}

impl PyPiStorage {
    pub(crate) fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub(crate) fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub(crate) fn cleanup_temporary_artifacts(&self) {
        let directory = self.data_dir.join("tmp");
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("pypi-") && name.ends_with(".part") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    pub(crate) fn temporary_artifact_path(&self) -> Result<PathBuf, StorageError> {
        let directory = self.data_dir.join("tmp");
        fs::create_dir_all(&directory)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        Ok(directory.join(format!("pypi-{}-{nonce}.part", std::process::id())))
    }

    pub(crate) fn read_representation(
        &self,
        key: &PyPiRepresentationKey,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        validate_representation_key(key)?;
        let path = self.representation_path(key);
        let body = match fs::read(&path) {
            Ok(body) => body,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        let digest_path = representation_digest_path(&path);
        let expected = match fs::read_to_string(&digest_path) {
            Ok(expected) => expected.trim().to_owned(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let _ = fs::remove_file(&path);
                return Ok(None);
            }
            Err(error) => return Err(StorageError::Io(error)),
        };
        if expected != sha256_hex(&body) {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(&digest_path);
            return Ok(None);
        }
        let _ = crate::storage::access::note_access(&path);
        Ok(Some(body))
    }

    pub(crate) fn publish_representation(
        &self,
        key: &PyPiRepresentationKey,
        body: &[u8],
    ) -> Result<(), StorageError> {
        validate_representation_key(key)?;
        let path = self.representation_path(key);
        crate::storage::atomic::atomic_write(&path, body)?;
        crate::storage::atomic::atomic_write(
            &representation_digest_path(&path),
            sha256_hex(body).as_bytes(),
        )?;
        Ok(())
    }

    fn representation_path(&self, key: &PyPiRepresentationKey) -> PathBuf {
        crate::storage::layout::metadata_root(&self.data_dir, "pypi")
            .join(&key.entry)
            .join(&key.project)
            .join("representations")
            .join(&key.selected_proxy)
            .join(&key.canonical_digest)
            .join(&key.base_url_fingerprint)
            .join(format!("{}.body", key.representation.as_str()))
    }
}

pub(crate) fn base_url_fingerprint(base_url: &Url) -> String {
    sha256_hex(base_url.as_str().as_bytes())[..16].to_owned()
}

pub(crate) fn sha256_hex(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

pub(crate) fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn representation_digest_path(path: &Path) -> PathBuf {
    path.with_extension("body.sha256")
}

fn validate_representation_key(key: &PyPiRepresentationKey) -> Result<(), StorageError> {
    validate_component(&key.entry)?;
    validate_component(&key.project)?;
    validate_component(&key.selected_proxy)?;
    validate_digest(&key.canonical_digest, 64)?;
    validate_digest(&key.base_url_fingerprint, 16)?;
    Ok(())
}

fn validate_digest(value: &str, length: usize) -> Result<(), StorageError> {
    if value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(StorageError::InvalidDigest(value.to_owned()))
    }
}

fn validate_component(value: &str) -> Result<(), StorageError> {
    if is_valid_entry_name(value) {
        Ok(())
    } else {
        Err(StorageError::InvalidIdentity(value.to_owned()))
    }
}

fn validate_artifact_key(key: &PyPiArtifactKey) -> Result<(), StorageError> {
    validate_component(&key.entry)?;
    validate_component(&key.project)?;
    validate_component(&key.selected_proxy)?;
    if is_safe_file_name(&key.filename) {
        Ok(())
    } else {
        Err(StorageError::InvalidIdentity(key.filename.clone()))
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::{base_url_fingerprint, sha256_hex};

    #[test]
    fn base_url_fingerprint_is_stable_and_short() {
        let first = base_url_fingerprint(&Url::parse("https://pypi.example").expect("URL"));
        let second = base_url_fingerprint(&Url::parse("https://pypi.example").expect("URL"));
        let other = base_url_fingerprint(&Url::parse("https://pypi.example/simple").expect("URL"));
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(first.len(), 16);
        assert_eq!(sha256_hex(b"abc").len(), 64);
    }
}
