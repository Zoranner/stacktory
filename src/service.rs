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
    sources::{FetchOperation, SourcePool, SourcePoolError, source_error_is_group_fallback},
    storage::{ArtifactCache, SnapshotStore, StorageError, StoredSnapshot},
};

#[derive(Debug)]
pub struct RustupService {
    channels: Vec<String>,
    refresh_interval_secs: u64,
    root_url: url::Url,
    sources: SourcePool,
    snapshots: SnapshotStore,
    cache: ArtifactCache,
    audit: AuditSink,
    refresh_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    artifact_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
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
    pub fn new(config: Config) -> Result<Self, ServiceError> {
        let proxy_url = config.proxy_url.clone();
        let proxy_no_proxy = config.proxy_no_proxy.clone();
        let sources = SourcePool::new_with_proxy(
            "rustup",
            config.rustup.sources,
            proxy_url.as_ref(),
            proxy_no_proxy.as_deref(),
        )?;
        Ok(Self {
            channels: config.rustup.channels,
            refresh_interval_secs: config.rustup.refresh_interval_secs,
            root_url: config.root_url,
            sources,
            snapshots: SnapshotStore::new(config.data_dir.clone()),
            cache: ArtifactCache::new(config.data_dir.clone()),
            audit: AuditSink::new(config.data_dir),
            refresh_locks: Mutex::new(BTreeMap::new()),
            artifact_locks: Mutex::new(BTreeMap::new()),
        })
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
        Ok(self.snapshots.current(source_name, channel)?)
    }

    pub async fn manifest_for_request(
        &self,
        source_name: &str,
        channel: &str,
    ) -> Result<StoredSnapshot, ServiceError> {
        self.ensure_channel(channel)?;
        let refresh_lock = self.refresh_lock(source_name, channel).await;
        let _guard = refresh_lock.lock().await;

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

    async fn refresh_lock(&self, source_name: &str, channel: &str) -> Arc<Mutex<()>> {
        let mut locks = self.refresh_locks.lock().await;
        locks
            .entry(format!("{source_name}\0{channel}"))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn artifact_lock(&self, source_name: &str, path: &DistPath) -> Arc<Mutex<()>> {
        let mut locks = self.artifact_locks.lock().await;
        locks
            .entry(format!("{source_name}\0{}", path.as_str()))
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

    pub async fn artifact_for_request(
        &self,
        entry_name: &str,
        channel: &str,
        path: &DistPath,
    ) -> Result<Vec<u8>, ServiceError> {
        self.ensure_entry(entry_name)?;
        self.ensure_channel(channel)?;
        let snapshot = self.snapshots.current(entry_name, channel)?;
        let artifact_source = snapshot
            .artifact_sources
            .get(path)
            .cloned()
            .ok_or_else(|| ServiceError::UnknownArtifact(path.clone()))?;
        let artifact_lock = self.artifact_lock(&artifact_source.source_name, path).await;
        let _artifact_guard = artifact_lock.lock().await;
        let snapshot = self.snapshots.current(entry_name, channel)?;
        let artifact_source = snapshot
            .artifact_sources
            .get(path)
            .cloned()
            .ok_or_else(|| ServiceError::UnknownArtifact(path.clone()))?;
        let expected_checksum = artifact_source.checksum;
        let selected_source = artifact_source.source_name;
        if let Some(contents) =
            self.cache
                .get_verified(&selected_source, path, &expected_checksum)?
        {
            self.record_audit("artifact_cache_hit", path.as_str(), "success");
            return Ok(contents);
        }

        let contents = match self
            .sources
            .fetch(
                &selected_source,
                &format!("dist/{}", path.as_str()),
                FetchOperation::Artifact,
            )
            .await
        {
            Ok(response) if sha256(&response.body) == expected_checksum => response.body,
            Ok(_) => {
                self.record_audit("artifact_fetch_failed", path.as_str(), "hash_mismatch");
                return Err(ServiceError::Storage(StorageError::ArtifactHashMismatch(
                    path.to_string(),
                )));
            }
            Err(error) => {
                self.record_audit("artifact_fetch_failed", path.as_str(), "upstream_failed");
                return Err(error.into());
            }
        };
        self.cache
            .publish_verified(&selected_source, path, &contents, &expected_checksum)?;
        self.record_audit("artifact_cached", path.as_str(), "success");
        Ok(contents)
    }

    fn ensure_channel(&self, channel: &str) -> Result<(), ServiceError> {
        if self.channels.iter().any(|configured| configured == channel) {
            Ok(())
        } else {
            Err(ServiceError::UnconfiguredChannel(channel.to_owned()))
        }
    }

    fn ensure_entry(&self, entry_name: &str) -> Result<(), ServiceError> {
        self.sources
            .proxy_members(entry_name)
            .is_some()
            .then_some(())
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()).into())
    }

    async fn fetch_and_rewrite_manifest(
        &self,
        source_name: &str,
        channel: &str,
    ) -> Result<(String, crate::rustup::RewrittenManifest), ServiceError> {
        let members = self
            .sources
            .proxy_members(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        let is_group = self.sources.is_group(source_name);
        let manifest_key = format!("dist/channel-rust-{channel}.toml");
        let checksum_key = format!("{manifest_key}.sha256");
        let entry_root = self.entry_root_url(source_name);
        let file_name = format!("channel-rust-{channel}.toml");
        let policy = ManifestPolicy::with_sources(
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
        );
        let mut failures = Vec::new();

        for member in members {
            let manifest = match self
                .sources
                .fetch(&member.name, &manifest_key, FetchOperation::Metadata)
                .await
            {
                Ok(response) => response.body,
                Err(error) if should_try_next_source(&error, is_group) => {
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let checksum_file = match self
                .sources
                .fetch(&member.name, &checksum_key, FetchOperation::Metadata)
                .await
            {
                Ok(response) => response.body,
                Err(error) if should_try_next_source(&error, is_group) => {
                    failures.push(format!("{}: {error}", member.name));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };

            let manifest = match String::from_utf8(manifest) {
                Ok(manifest) => manifest,
                Err(_) => {
                    failures.push(format!("{}: manifest is not UTF-8", member.name));
                    continue;
                }
            };
            let checksum_file = match String::from_utf8(checksum_file) {
                Ok(checksum_file) => checksum_file,
                Err(_) => {
                    failures.push(format!("{}: checksum is not UTF-8", member.name));
                    continue;
                }
            };
            let actual_checksum = sha256(manifest.as_bytes());
            if parse_checksum_file(&checksum_file, &file_name) != Some(actual_checksum.as_str()) {
                failures.push(format!("{}: checksum does not match manifest", member.name));
                continue;
            }

            match rewrite_manifest(&manifest, &policy, &entry_root, channel) {
                Ok(rewritten) => return Ok((member.name, rewritten)),
                Err(error) => failures.push(format!("{}: {error}", member.name)),
            }
        }

        Err(SourcePoolError::AllSourcesFailed {
            entry_name: source_name.to_owned(),
            request_key: manifest_key,
            failures: failures.join("; "),
            status: None,
        }
        .into())
    }

    fn entry_root_url(&self, source_name: &str) -> url::Url {
        let mut url = self.root_url.clone();
        let base_path = url.path().trim_end_matches('/');
        url.set_path(&format!("{base_path}/mirror/rustup/{source_name}"));
        url.set_query(None);
        url.set_fragment(None);
        url
    }

    fn record_audit(&self, event: &str, target: &str, outcome: &str) {
        if let Err(error) = self.audit.record(event, target, outcome) {
            crate::logging::error(format!("audit write failed: {error}"));
        }
    }
}

fn should_try_next_source(error: &SourcePoolError, is_group: bool) -> bool {
    matches!(error, SourcePoolError::AllSourcesFailed { .. })
        || (is_group && source_error_is_group_fallback(error))
}

fn parse_checksum_file<'a>(contents: &'a str, expected_file_name: &str) -> Option<&'a str> {
    let mut fields = contents.split_whitespace();
    let checksum = fields.next()?;
    let file_name = fields.next()?.trim_start_matches('*');
    if fields.next().is_some()
        || file_name != expected_file_name
        || checksum.len() != 64
        || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(checksum)
}

fn sha256(contents: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    format!("{:x}", Sha256::digest(contents))
}
