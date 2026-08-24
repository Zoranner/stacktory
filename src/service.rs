use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::{
    Config,
    rustup::{DistPath, ManifestPolicy, RustupError, rewrite_manifest},
    storage::{ArtifactCache, SnapshotStore, StorageError, StoredSnapshot},
    upstream::{UpstreamClient, UpstreamError},
};

#[derive(Debug)]
pub struct RustupService {
    channels: Vec<String>,
    refresh_interval_secs: u64,
    public_base: url::Url,
    policy: ManifestPolicy,
    upstream: UpstreamClient,
    snapshots: SnapshotStore,
    cache: ArtifactCache,
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("rustup channel `{0}` is not configured")]
    UnconfiguredChannel(String),
    #[error(transparent)]
    Upstream(#[from] UpstreamError),
    #[error(transparent)]
    Rewrite(#[from] RustupError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("no published rustup snapshots are available")]
    NoPublishedSnapshots,
    #[error("artifact `{0}` is not referenced by a published rustup manifest")]
    UnknownArtifact(DistPath),
    #[error("artifact `{0}` has conflicting SHA-256 values across published rustup manifests")]
    ConflictingArtifactHash(DistPath),
}

impl RustupService {
    pub fn new(config: Config) -> Result<Self, ServiceError> {
        let bases: Vec<url::Url> = config
            .rustup
            .upstreams
            .iter()
            .map(|upstream| upstream.base_url.clone())
            .collect();
        Ok(Self {
            channels: config.rustup.channels,
            refresh_interval_secs: config.rustup.refresh_interval_secs,
            public_base: config.public_base_url,
            policy: ManifestPolicy::new(bases.clone()),
            upstream: UpstreamClient::new(bases)?,
            snapshots: SnapshotStore::new(config.data_dir.clone()),
            cache: ArtifactCache::new(config.data_dir),
        })
    }

    pub async fn refresh(&self, channel: &str) -> Result<(), ServiceError> {
        self.ensure_channel(channel)?;
        let source = self.upstream.fetch_channel_manifest(channel).await?;
        let rewritten = rewrite_manifest(&source, &self.policy, &self.public_base)?;
        self.snapshots.publish(channel, rewritten)?;
        Ok(())
    }

    pub fn manifest(&self, channel: &str) -> Result<StoredSnapshot, ServiceError> {
        self.ensure_channel(channel)?;
        Ok(self.snapshots.current(channel)?)
    }

    pub async fn manifest_for_request(
        &self,
        channel: &str,
    ) -> Result<StoredSnapshot, ServiceError> {
        match self.manifest(channel) {
            Ok(snapshot) if self.is_snapshot_stale(&snapshot) => {
                match self.refresh(channel).await {
                    Ok(()) => self.manifest(channel),
                    Err(_) => Ok(snapshot),
                }
            }
            Ok(snapshot) => Ok(snapshot),
            Err(ServiceError::Storage(StorageError::MissingSnapshot(_))) => {
                self.refresh(channel).await?;
                self.manifest(channel)
            }
            Err(error) => Err(error),
        }
    }

    fn is_snapshot_stale(&self, snapshot: &StoredSnapshot) -> bool {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .is_ok_and(|now| {
                now.as_secs()
                    .saturating_sub(snapshot.published_at_unix_secs)
                    >= self.refresh_interval_secs
            })
    }

    pub async fn artifact_for_request(&self, path: &DistPath) -> Result<Vec<u8>, ServiceError> {
        let expected_checksum = self.artifact_checksum(path)?;
        if let Some(contents) = self.cache.get_verified(path, &expected_checksum)? {
            return Ok(contents);
        }

        let contents = self
            .upstream
            .fetch_dist_artifact(path, &expected_checksum)
            .await?;
        self.cache
            .publish_verified(path, &contents, &expected_checksum)?;
        Ok(contents)
    }

    fn artifact_checksum(&self, path: &DistPath) -> Result<String, ServiceError> {
        let mut expected_checksum = None;
        let mut has_snapshot = false;

        for channel in &self.channels {
            match self.snapshots.current(channel) {
                Ok(snapshot) => {
                    has_snapshot = true;
                    let Some(checksum) = snapshot.artifact_hashes.get(path) else {
                        continue;
                    };
                    if let Some(previous) = &expected_checksum {
                        if previous != checksum {
                            return Err(ServiceError::ConflictingArtifactHash(path.clone()));
                        }
                    } else {
                        expected_checksum = Some(checksum.clone());
                    }
                }
                Err(StorageError::MissingSnapshot(_)) => continue,
                Err(error) => return Err(ServiceError::Storage(error)),
            }
        }

        expected_checksum.ok_or_else(|| {
            if has_snapshot {
                ServiceError::UnknownArtifact(path.clone())
            } else {
                ServiceError::NoPublishedSnapshots
            }
        })
    }

    fn ensure_channel(&self, channel: &str) -> Result<(), ServiceError> {
        if self.channels.iter().any(|configured| configured == channel) {
            Ok(())
        } else {
            Err(ServiceError::UnconfiguredChannel(channel.to_owned()))
        }
    }
}
