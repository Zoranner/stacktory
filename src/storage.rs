use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::rustup::{DistPath, RewrittenManifest};

#[derive(Debug)]
pub struct SnapshotStore {
    data_dir: PathBuf,
}

#[derive(Debug)]
pub struct ArtifactCache {
    data_dir: PathBuf,
}

#[derive(Debug)]
pub struct StoredSnapshot {
    pub manifest: String,
    pub checksum: String,
    pub artifact_hashes: BTreeMap<DistPath, String>,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("storage metadata cannot be parsed: {0}")]
    MetadataParse(#[from] toml::de::Error),
    #[error("storage metadata cannot be serialized: {0}")]
    MetadataSerialize(#[from] toml::ser::Error),
    #[error("rustup channel `{0}` is invalid")]
    InvalidChannel(String),
    #[error("no published rustup snapshot exists for channel `{0}`")]
    MissingSnapshot(String),
    #[error("snapshot checksum `{0}` is invalid")]
    InvalidChecksum(String),
    #[error("current snapshot for channel `{0}` is inconsistent")]
    InconsistentSnapshot(String),
    #[error("artifact `{0}` does not match its expected SHA-256")]
    ArtifactHashMismatch(String),
}

#[derive(Deserialize, Serialize)]
struct CurrentPointer {
    revision: String,
}

#[derive(Deserialize, Serialize)]
struct ArtifactIndex {
    artifact_hashes: BTreeMap<String, String>,
}

impl SnapshotStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub fn publish(&self, channel: &str, snapshot: RewrittenManifest) -> Result<(), StorageError> {
        validate_channel(channel)?;
        validate_checksum(&snapshot.checksum)?;

        let snapshot_dir = self.snapshot_root(channel).join(&snapshot.checksum);
        fs::create_dir_all(&snapshot_dir)?;
        fs::write(snapshot_dir.join("manifest.toml"), snapshot.manifest)?;
        fs::write(
            snapshot_dir.join("checksum"),
            format!("{}\n", snapshot.checksum),
        )?;

        let artifact_hashes = snapshot
            .artifact_hashes
            .into_iter()
            .map(|(path, hash)| {
                validate_checksum(&hash)?;
                Ok((path.to_string(), hash))
            })
            .collect::<Result<BTreeMap<_, _>, StorageError>>()?;
        fs::write(
            snapshot_dir.join("artifacts.toml"),
            toml::to_string(&ArtifactIndex { artifact_hashes })?,
        )?;

        let pointer = toml::to_string(&CurrentPointer {
            revision: snapshot.checksum,
        })?;
        atomic_write(
            &self.snapshot_root(channel).join("current.toml"),
            pointer.as_bytes(),
        )
    }

    pub fn current(&self, channel: &str) -> Result<StoredSnapshot, StorageError> {
        validate_channel(channel)?;
        let root = self.snapshot_root(channel);
        let pointer_path = root.join("current.toml");
        let pointer = fs::read_to_string(&pointer_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                StorageError::MissingSnapshot(channel.to_owned())
            } else {
                StorageError::Io(error)
            }
        })?;
        let pointer: CurrentPointer = toml::from_str(&pointer)?;
        validate_checksum(&pointer.revision)?;

        let snapshot_dir = root.join(&pointer.revision);
        let manifest = fs::read_to_string(snapshot_dir.join("manifest.toml"))?;
        let checksum = fs::read_to_string(snapshot_dir.join("checksum"))?
            .trim()
            .to_owned();
        if checksum != pointer.revision {
            return Err(StorageError::InconsistentSnapshot(channel.to_owned()));
        }
        validate_checksum(&checksum)?;

        let artifact_index: ArtifactIndex =
            toml::from_str(&fs::read_to_string(snapshot_dir.join("artifacts.toml"))?)?;
        let artifact_hashes = artifact_index
            .artifact_hashes
            .into_iter()
            .map(|(path, hash)| {
                validate_checksum(&hash)?;
                Ok((
                    DistPath::parse(&path)
                        .map_err(|_| StorageError::InconsistentSnapshot(channel.to_owned()))?,
                    hash,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, StorageError>>()?;

        Ok(StoredSnapshot {
            manifest,
            checksum,
            artifact_hashes,
        })
    }

    fn snapshot_root(&self, channel: &str) -> PathBuf {
        self.data_dir.join("rustup").join("snapshots").join(channel)
    }
}

impl ArtifactCache {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub fn publish_verified(
        &self,
        path: &DistPath,
        contents: &[u8],
        expected_checksum: &str,
    ) -> Result<(), StorageError> {
        validate_checksum(expected_checksum)?;
        if checksum(contents) != expected_checksum {
            return Err(StorageError::ArtifactHashMismatch(path.to_string()));
        }

        atomic_write(&self.artifact_path(path), contents)
    }

    pub fn get_verified(
        &self,
        path: &DistPath,
        expected_checksum: &str,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        validate_checksum(expected_checksum)?;
        let artifact_path = self.artifact_path(path);
        let contents = match fs::read(&artifact_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };

        if checksum(&contents) == expected_checksum {
            Ok(Some(contents))
        } else {
            fs::remove_file(artifact_path)?;
            Ok(None)
        }
    }

    fn artifact_path(&self, path: &DistPath) -> PathBuf {
        self.data_dir
            .join("rustup")
            .join("artifacts")
            .join(path.as_str())
    }
}

fn validate_channel(channel: &str) -> Result<(), StorageError> {
    if channel.is_empty()
        || !channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err(StorageError::InvalidChannel(channel.to_owned()));
    }
    Ok(())
}

fn validate_checksum(checksum: &str) -> Result<(), StorageError> {
    if checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(StorageError::InvalidChecksum(checksum.to_owned()))
    }
}

fn checksum(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), StorageError> {
    let parent = path.parent().ok_or_else(|| {
        StorageError::Io(io::Error::other("storage target has no parent directory"))
    })?;
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| StorageError::Io(io::Error::other("storage target has no file name")))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| StorageError::Io(io::Error::other(error)))?
        .as_nanos();
    let temporary_path = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));

    fs::write(&temporary_path, contents)?;
    match fs::rename(&temporary_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(temporary_path);
            Err(StorageError::Io(error))
        }
    }
}
