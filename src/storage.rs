use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::rustup::{ArtifactSource, DistPath, RewrittenManifest};

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
    pub source_name: String,
    pub published_at_unix_secs: u64,
    pub artifact_sources: BTreeMap<DistPath, ArtifactSource>,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct PrunedSnapshots {
    pub removed_revisions: u64,
    pub failed_revisions: u64,
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
    #[error("rustup source name `{0}` is invalid")]
    InvalidSourceName(String),
    #[error("no published rustup snapshot exists for channel `{0}`")]
    MissingSnapshot(String),
    #[error("snapshot checksum `{0}` is invalid")]
    InvalidChecksum(String),
    #[error("current snapshot for channel `{0}` is inconsistent")]
    InconsistentSnapshot(String),
    #[error("current snapshot for channel `{0}` uses an incompatible artifact index")]
    IncompatibleSnapshot(String),
    #[error("artifact `{0}` does not match its expected SHA-256")]
    ArtifactHashMismatch(String),
}

#[derive(Deserialize, Serialize)]
struct CurrentPointer {
    revision: String,
    source_name: String,
    published_at_unix_secs: u64,
}

#[derive(Deserialize, Serialize)]
struct ArtifactIndex {
    artifact_sources: BTreeMap<String, ArtifactSource>,
}

impl SnapshotStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub fn publish(
        &self,
        entry_name: &str,
        upstream_source: &str,
        channel: &str,
        snapshot: RewrittenManifest,
    ) -> Result<(), StorageError> {
        validate_source_name(entry_name)?;
        validate_source_name(upstream_source)?;
        validate_channel(channel)?;
        validate_checksum(&snapshot.checksum)?;

        let snapshot_dir = self
            .snapshot_root(entry_name, channel)
            .join(&snapshot.checksum);
        fs::create_dir_all(&snapshot_dir)?;
        fs::write(snapshot_dir.join("manifest.toml"), snapshot.manifest)?;
        fs::write(
            snapshot_dir.join("checksum"),
            format!("{}\n", snapshot.checksum),
        )?;

        let artifact_sources = snapshot
            .artifact_sources
            .into_iter()
            .map(|(path, artifact_source)| {
                validate_checksum(&artifact_source.checksum)?;
                validate_source_name(&artifact_source.source_name)?;
                Ok((path.to_string(), artifact_source))
            })
            .collect::<Result<BTreeMap<_, _>, StorageError>>()?;
        fs::write(
            snapshot_dir.join("artifacts.toml"),
            toml::to_string(&ArtifactIndex { artifact_sources })?,
        )?;

        let pointer = toml::to_string(&CurrentPointer {
            revision: snapshot.checksum,
            source_name: upstream_source.to_owned(),
            published_at_unix_secs: unix_timestamp()?,
        })?;
        atomic_write(
            &self.snapshot_root(entry_name, channel).join("current.toml"),
            pointer.as_bytes(),
        )
    }

    pub fn current(&self, entry_name: &str, channel: &str) -> Result<StoredSnapshot, StorageError> {
        validate_source_name(entry_name)?;
        validate_channel(channel)?;
        let root = self.snapshot_root(entry_name, channel);
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
        validate_source_name(&pointer.source_name)?;

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
            toml::from_str(&fs::read_to_string(snapshot_dir.join("artifacts.toml"))?)
                .map_err(|_| StorageError::IncompatibleSnapshot(channel.to_owned()))?;
        let artifact_sources = artifact_index
            .artifact_sources
            .into_iter()
            .map(|(path, artifact_source)| {
                validate_checksum(&artifact_source.checksum)?;
                validate_source_name(&artifact_source.source_name)?;
                Ok((
                    DistPath::parse(&path)
                        .map_err(|_| StorageError::InconsistentSnapshot(channel.to_owned()))?,
                    artifact_source,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, StorageError>>()?;

        Ok(StoredSnapshot {
            manifest,
            checksum,
            source_name: pointer.source_name,
            published_at_unix_secs: pointer.published_at_unix_secs,
            artifact_sources,
        })
    }

    fn snapshot_root(&self, entry_name: &str, channel: &str) -> PathBuf {
        self.data_dir
            .join("rustup")
            .join("snapshots")
            .join(entry_name)
            .join(channel)
    }

    /// 每个 `{entry}/{channel}` 只保留最近 `keep_revisions` 个修订。
    ///
    /// `current.toml` 指向的修订始终保留，即使它不是最近的修订。`current.toml`
    /// 存在但无法解析时跳过该 channel，避免删除唯一可服务的快照。
    pub fn prune(
        &self,
        entry_name: &str,
        channel: &str,
        keep_revisions: u64,
    ) -> Result<PrunedSnapshots, StorageError> {
        validate_source_name(entry_name)?;
        validate_channel(channel)?;
        let root = self.snapshot_root(entry_name, channel);
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PrunedSnapshots::default());
            }
            Err(error) => return Err(StorageError::Io(error)),
        };
        let current = match fs::read_to_string(root.join("current.toml")) {
            Ok(pointer) => match toml::from_str::<CurrentPointer>(&pointer) {
                Ok(pointer) => Some(pointer.revision),
                Err(_) => return Ok(PrunedSnapshots::default()),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(StorageError::Io(error)),
        };

        let mut revisions = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str().map(str::to_owned) else {
                continue;
            };
            if !is_revision_name(&name) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
                continue;
            };
            revisions.push((modified, name, entry.path()));
        }
        revisions.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));

        let mut pruned = PrunedSnapshots::default();
        for (index, (_, name, path)) in revisions.iter().enumerate() {
            if index < keep_revisions as usize || current.as_deref() == Some(name.as_str()) {
                continue;
            }
            match fs::remove_dir_all(path) {
                Ok(()) => pruned.removed_revisions += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    pruned.failed_revisions += 1;
                    eprintln!(
                        "Stacktory snapshot cleanup failed for `{}`: {error}",
                        path.display()
                    );
                }
            }
        }
        Ok(pruned)
    }
}

fn is_revision_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn unix_timestamp() -> Result<u64, StorageError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| StorageError::Io(io::Error::other(error)))
        .map(|duration| duration.as_secs())
}

impl ArtifactCache {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub fn publish_verified(
        &self,
        source_name: &str,
        path: &DistPath,
        contents: &[u8],
        expected_checksum: &str,
    ) -> Result<(), StorageError> {
        validate_source_name(source_name)?;
        validate_checksum(expected_checksum)?;
        if checksum(contents) != expected_checksum {
            return Err(StorageError::ArtifactHashMismatch(path.to_string()));
        }

        atomic_write(&self.artifact_path(source_name, path), contents)
    }

    pub fn get_verified(
        &self,
        source_name: &str,
        path: &DistPath,
        expected_checksum: &str,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        validate_source_name(source_name)?;
        validate_checksum(expected_checksum)?;
        let artifact_path = self.artifact_path(source_name, path);
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

    fn artifact_path(&self, source_name: &str, path: &DistPath) -> PathBuf {
        self.data_dir
            .join("rustup")
            .join("artifacts")
            .join(source_name)
            .join("objects")
            .join(path.as_str())
    }
}

fn validate_source_name(source_name: &str) -> Result<(), StorageError> {
    if !source_name.is_empty()
        && source_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(StorageError::InvalidSourceName(source_name.to_owned()))
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
