use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    Config,
    audit::AuditSink,
    rustup::{DistPath, ManifestPolicy, RustupError, rewrite_manifest},
    storage::{ArtifactCache, SnapshotStore, StorageError, StoredSnapshot},
    upstream::{UpstreamClient, UpstreamError},
};

#[derive(Debug)]
pub struct RustupService {
    channels: Vec<String>,
    refresh_interval_secs: u64,
    root_url: url::Url,
    policy: ManifestPolicy,
    upstream: UpstreamClient,
    snapshots: SnapshotStore,
    cache: ArtifactCache,
    audit: AuditSink,
    refresh_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
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
        let bases = config.rustup.legacy_upstream_bases();
        Ok(Self {
            channels: config.rustup.channels,
            refresh_interval_secs: config.rustup.refresh_interval_secs,
            root_url: config.root_url,
            policy: ManifestPolicy::new(bases.clone()),
            upstream: UpstreamClient::new(bases)?,
            snapshots: SnapshotStore::new(config.data_dir.clone()),
            cache: ArtifactCache::new(config.data_dir.clone()),
            audit: AuditSink::new(config.data_dir),
            refresh_locks: Mutex::new(BTreeMap::new()),
        })
    }

    pub async fn refresh(&self, channel: &str) -> Result<(), ServiceError> {
        self.ensure_channel(channel)?;
        let source = match self.upstream.fetch_channel_manifest(channel).await {
            Ok(source) => source,
            Err(error) => {
                self.record_audit("manifest_refresh_failed", channel, "upstream_failed");
                return Err(error.into());
            }
        };
        let rewritten = rewrite_manifest(&source, &self.policy, &self.root_url)?;
        self.snapshots.publish(channel, rewritten)?;
        self.record_audit("manifest_published", channel, "success");
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
        self.ensure_channel(channel)?;
        let refresh_lock = self.refresh_lock(channel).await;
        let _guard = refresh_lock.lock().await;

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

    async fn refresh_lock(&self, channel: &str) -> Arc<Mutex<()>> {
        let mut locks = self.refresh_locks.lock().await;
        locks
            .entry(channel.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
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
            self.record_audit("artifact_cache_hit", path.as_str(), "success");
            return Ok(contents);
        }

        let contents = match self
            .upstream
            .fetch_dist_artifact(path, &expected_checksum)
            .await
        {
            Ok(contents) => contents,
            Err(error) => {
                self.record_audit("artifact_fetch_failed", path.as_str(), "upstream_failed");
                return Err(error.into());
            }
        };
        self.cache
            .publish_verified(path, &contents, &expected_checksum)?;
        self.record_audit("artifact_cached", path.as_str(), "success");
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

    fn record_audit(&self, event: &str, target: &str, outcome: &str) {
        if let Err(error) = self.audit.record(event, target, outcome) {
            eprintln!("Stacktory audit write failed: {error}");
        }
    }
}
