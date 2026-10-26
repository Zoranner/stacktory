use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use super::{
    ArtifactCache, DistPath, ManifestPolicy, RustupError, SnapshotStore, StorageError,
    StoredSnapshot, rewrite_manifest,
};
use crate::{
    Config,
    audit::{AuditDetails, AuditSink},
    config::SourceCatalog,
    storage::locks::ShardedLocks,
    upstream::{ArtifactBudget, FetchTimeouts, SourcePool, SourcePoolError},
};

mod artifact;
mod manifest;

use artifact::cleanup_temporary_artifacts;

#[derive(Debug)]
pub struct RustupService {
    pub(super) channels: Vec<String>,
    pub(super) refresh_interval_secs: u64,
    pub(super) root_url: url::Url,
    pub(super) docs_catalog: SourceCatalog,
    pub(super) sources: SourcePool,
    pub(super) snapshots: SnapshotStore,
    pub(super) cache: ArtifactCache,
    pub(super) audit: AuditSink,
    pub(super) data_dir: PathBuf,
    pub(super) log_dir: PathBuf,
    pub(super) refresh_locks: ShardedLocks,
    pub(super) artifact_locks: ShardedLocks,
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("rustup channel `{0}` is not configured")]
    UnconfiguredChannel(String),
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error(transparent)]
    Rewrite(#[from] RustupError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("artifact `{0}` is not referenced by a published rustup manifest")]
    UnknownArtifact(DistPath),
}

impl RustupService {
    pub fn public_url(&self) -> &url::Url {
        &self.root_url
    }

    pub(crate) fn guide_catalog(&self) -> &SourceCatalog {
        &self.docs_catalog
    }

    pub fn new(config: Config) -> Result<Self, ServiceError> {
        Self::new_with_artifact_budget(config, ArtifactBudget::default())
    }

    pub fn new_with_artifact_budget(
        config: Config,
        artifact_budget: ArtifactBudget,
    ) -> Result<Self, ServiceError> {
        let docs_catalog = SourceCatalog::from_config(&config);
        let proxy_url = config.proxy.url.clone();
        let proxy_no_proxy = config.proxy.no_proxy.clone();
        let data_dir = config.storage.data_dir.clone();
        let log_dir = config.storage.log_dir.clone();
        let fetch_timeouts = FetchTimeouts {
            metadata: config.upstream.metadata_timeout(),
            artifact: config.upstream.artifact_timeout(),
        };
        cleanup_temporary_artifacts(&data_dir);
        let sources = SourcePool::new_with_proxy(
            "rustup",
            config.rustup.sources,
            proxy_url.as_ref(),
            proxy_no_proxy.as_deref(),
        )?
        .with_artifact_budget(artifact_budget)
        .with_fetch_timeouts(fetch_timeouts);
        Ok(Self {
            channels: config.rustup.channels,
            refresh_interval_secs: config.rustup.refresh_interval_secs,
            root_url: config.server.public_url,
            docs_catalog,
            sources,
            snapshots: SnapshotStore::new(data_dir.clone()),
            cache: ArtifactCache::new(data_dir.clone()),
            audit: AuditSink::new(data_dir.clone()),
            data_dir,
            log_dir,
            refresh_locks: ShardedLocks::default(),
            artifact_locks: ShardedLocks::default(),
        })
    }

    pub async fn is_ready(&self) -> bool {
        let data_dir = self.data_dir.clone();
        let log_dir = self.log_dir.clone();
        tokio::task::spawn_blocking(move || {
            directory_is_writable(&data_dir) && directory_is_writable(&log_dir)
        })
        .await
        .unwrap_or(false)
    }

    pub async fn refresh(&self, source_name: &str, channel: &str) -> Result<(), ServiceError> {
        self.ensure_channel(channel)?;
        let (selected_source, rewritten) =
            match self.fetch_and_rewrite_manifest(source_name, channel).await {
                Ok(result) => result,
                Err(error) => {
                    self.record_audit(
                        "manifest_refresh_failed",
                        &format!("{source_name}/{channel}"),
                        "upstream_failed",
                    );
                    return Err(error);
                }
            };
        self.snapshots
            .publish(source_name, &selected_source, channel, rewritten)?;
        self.record_audit(
            "manifest_published",
            &format!("{source_name}/{channel}"),
            "success",
        );
        Ok(())
    }

    pub fn manifest(
        &self,
        source_name: &str,
        channel: &str,
    ) -> Result<StoredSnapshot, ServiceError> {
        self.ensure_channel(channel)?;
        let snapshot = self.snapshots.current(source_name, channel)?;
        self.render_snapshot(snapshot, source_name, channel)
    }

    fn render_snapshot(
        &self,
        mut snapshot: StoredSnapshot,
        source_name: &str,
        channel: &str,
    ) -> Result<StoredSnapshot, ServiceError> {
        let rewritten = rewrite_manifest(
            &snapshot.canonical_manifest,
            &self.manifest_policy(),
            &self.entry_root_url(source_name),
            channel,
        )?;
        snapshot.manifest = rewritten.manifest;
        snapshot.checksum = rewritten.checksum;
        snapshot.artifact_sources = rewritten.artifact_sources;
        Ok(snapshot)
    }

    pub async fn manifest_for_request(
        &self,
        source_name: &str,
        channel: &str,
    ) -> Result<StoredSnapshot, ServiceError> {
        self.ensure_channel(channel)?;
        let _guard = self
            .refresh_locks
            .lock(&format!("{source_name}\0{channel}"))
            .await;
        match self.manifest(source_name, channel) {
            Ok(snapshot) if self.is_snapshot_stale(&snapshot) => {
                match self.refresh(source_name, channel).await {
                    Ok(()) => self.manifest(source_name, channel),
                    Err(_) => Ok(snapshot),
                }
            }
            Ok(snapshot) => Ok(snapshot),
            Err(
                ServiceError::Storage(StorageError::MissingSnapshot(_))
                | ServiceError::Storage(StorageError::IncompatibleSnapshot(_)),
            ) => {
                self.refresh(source_name, channel).await?;
                self.manifest(source_name, channel)
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

    pub(super) fn ensure_channel(&self, channel: &str) -> Result<(), ServiceError> {
        if self.channels.iter().any(|configured| configured == channel) {
            Ok(())
        } else {
            Err(ServiceError::UnconfiguredChannel(channel.to_owned()))
        }
    }

    pub(super) fn ensure_entry(&self, entry_name: &str) -> Result<(), ServiceError> {
        self.sources
            .proxy_members(entry_name)
            .is_some()
            .then_some(())
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()).into())
    }

    fn entry_root_url(&self, source_name: &str) -> url::Url {
        let mut url = self.root_url.clone();
        let base_path = url.path().trim_end_matches('/');
        url.set_path(&format!("{base_path}/mirror/rustup/{source_name}"));
        url.set_query(None);
        url.set_fragment(None);
        url
    }

    fn manifest_policy(&self) -> ManifestPolicy {
        ManifestPolicy::with_sources(
            self.sources
                .proxies()
                .iter()
                .map(|member| {
                    (
                        member.name.clone(),
                        member
                            .base_url
                            .as_ref()
                            .expect("validated proxy source must have a base URL")
                            .clone(),
                    )
                })
                .collect(),
        )
    }

    pub(super) fn record_audit(&self, event: &str, target: &str, outcome: &str) {
        if let Err(error) = self.audit.record(event, target, outcome) {
            crate::logging::error(format!("audit write failed: {error}"));
        }
    }

    pub(super) fn record_manifest_member_failure(
        &self,
        entry_name: &str,
        channel: &str,
        source_name: &str,
        failure_category: &str,
    ) {
        let object_key = format!("dist/channel-rust-{channel}.toml");
        if let Err(error) = self.audit.record_detailed(
            "manifest_member_failed",
            &object_key,
            "failure",
            AuditDetails {
                ecosystem: Some("rustup"),
                entry_name: Some(entry_name),
                object_key: Some(&object_key),
                selected_proxy: Some(source_name),
                object_type: Some("manifest"),
                cache_status: Some("failed"),
                failure_category: Some(failure_category),
                count: None,
            },
        ) {
            crate::logging::error(format!("audit write failed: {error}"));
        }
    }
}

fn directory_is_writable(path: &Path) -> bool {
    if fs::create_dir_all(path).is_err() {
        return false;
    }
    let probe = path.join(format!(
        ".ready-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}
