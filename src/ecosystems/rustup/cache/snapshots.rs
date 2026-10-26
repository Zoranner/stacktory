use std::{
    collections::BTreeMap,
    fs, io,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{StorageError, validate_channel, validate_checksum, validate_source_name};
use crate::{
    ecosystems::rustup::{ArtifactSource, DistPath, RewrittenManifest},
    storage::atomic::atomic_write,
};

#[derive(Debug)]
pub struct SnapshotStore {
    data_dir: PathBuf,
}

#[derive(Debug)]
pub struct StoredSnapshot {
    pub canonical_manifest: String,
    pub canonical_checksum: String,
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

#[derive(Deserialize, Serialize)]
struct CurrentPointer {
    revision: String,
    source_name: String,
    published_at_unix_secs: u64,
    #[serde(default)]
    format: u8,
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
        validate_checksum(&snapshot.canonical_checksum)?;
        let snapshot_dir = self
            .snapshot_root(entry_name, channel)
            .join(&snapshot.canonical_checksum);
        fs::create_dir_all(&snapshot_dir)?;
        fs::write(
            snapshot_dir.join("manifest.toml"),
            snapshot.canonical_manifest,
        )?;
        fs::write(
            snapshot_dir.join("checksum"),
            format!("{}\n", snapshot.canonical_checksum),
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
            revision: snapshot.canonical_checksum,
            source_name: upstream_source.to_owned(),
            published_at_unix_secs: unix_timestamp()?,
            format: 2,
        })?;
        Ok(atomic_write(
            &self.snapshot_root(entry_name, channel).join("current.toml"),
            pointer.as_bytes(),
        )?)
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
        if pointer.format != 2 {
            return Err(StorageError::IncompatibleSnapshot(channel.to_owned()));
        }
        validate_checksum(&pointer.revision)?;
        validate_source_name(&pointer.source_name)?;
        let snapshot_dir = root.join(&pointer.revision);
        let canonical_manifest = fs::read_to_string(snapshot_dir.join("manifest.toml"))?;
        let canonical_checksum = fs::read_to_string(snapshot_dir.join("checksum"))?
            .trim()
            .to_owned();
        if canonical_checksum != pointer.revision {
            return Err(StorageError::InconsistentSnapshot(channel.to_owned()));
        }
        validate_checksum(&canonical_checksum)?;
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
            manifest: canonical_manifest.clone(),
            checksum: canonical_checksum.clone(),
            canonical_manifest,
            canonical_checksum,
            source_name: pointer.source_name,
            published_at_unix_secs: pointer.published_at_unix_secs,
            artifact_sources,
        })
    }

    fn snapshot_root(&self, entry_name: &str, channel: &str) -> PathBuf {
        crate::storage::layout::snapshots_root(&self.data_dir, "rustup")
            .join(entry_name)
            .join(channel)
    }

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
                    crate::logging::error(format!(
                        "snapshot cleanup failed for `{}`: {error}",
                        path.display()
                    ));
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
