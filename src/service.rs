use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use crate::{
    Config,
    audit::{AuditDetails, AuditSink},
    docs::source_config::SourceCatalog,
    locks::ShardedLocks,
    rustup::{DistPath, ManifestPolicy, RustupError, rewrite_manifest},
    sources::{
        ArtifactBudget, FetchOperation, GroupRun, SourcePool, SourcePoolError,
        source_error_is_group_fallback, source_error_is_not_found,
    },
    storage::{ArtifactCache, ArtifactFile, SnapshotStore, StorageError, StoredSnapshot},
};

#[derive(Debug)]
pub struct RustupService {
    channels: Vec<String>,
    refresh_interval_secs: u64,
    root_url: url::Url,
    docs_catalog: SourceCatalog,
    sources: SourcePool,
    snapshots: SnapshotStore,
    cache: ArtifactCache,
    audit: AuditSink,
    data_dir: PathBuf,
    log_dir: PathBuf,
    refresh_locks: ShardedLocks,
    artifact_locks: ShardedLocks,
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

    pub(crate) fn docs_catalog(&self) -> &SourceCatalog {
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
        cleanup_temporary_artifacts(&data_dir);
        let sources = SourcePool::new_with_proxy(
            "rustup",
            config.rustup.sources,
            proxy_url.as_ref(),
            proxy_no_proxy.as_deref(),
        )?
        .with_artifact_budget(artifact_budget);
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

    pub async fn artifact_for_request(
        &self,
        entry_name: &str,
        channel: &str,
        path: &DistPath,
    ) -> Result<Vec<u8>, ServiceError> {
        let file = self
            .artifact_file_for_request(entry_name, channel, path)
            .await?;
        fs::read(file.path)
            .map_err(StorageError::Io)
            .map_err(Into::into)
    }

    pub async fn artifact_file_for_request(
        &self,
        entry_name: &str,
        channel: &str,
        path: &DistPath,
    ) -> Result<ArtifactFile, ServiceError> {
        self.ensure_entry(entry_name)?;
        self.ensure_channel(channel)?;
        let snapshot = self.snapshots.current(entry_name, channel)?;
        let artifact_source = snapshot
            .artifact_sources
            .get(path)
            .cloned()
            .ok_or_else(|| ServiceError::UnknownArtifact(path.clone()))?;
        let _artifact_guard = self
            .artifact_locks
            .lock(&format!(
                "{}\0{}",
                artifact_source.source_name,
                path.as_str()
            ))
            .await;
        let snapshot = self.snapshots.current(entry_name, channel)?;
        let artifact_source = snapshot
            .artifact_sources
            .get(path)
            .cloned()
            .ok_or_else(|| ServiceError::UnknownArtifact(path.clone()))?;
        let expected_checksum = artifact_source.checksum;
        let selected_source = artifact_source.source_name;
        if let Some(file) =
            self.cache
                .get_verified_file(&selected_source, path, &expected_checksum)?
        {
            self.record_audit("artifact_cache_hit", path.as_str(), "success");
            return Ok(file);
        }

        let temporary_path = temporary_artifact_path(&self.data_dir).map_err(StorageError::Io)?;
        let fetched = match self
            .sources
            .fetch_proxy_path_to_file(
                &selected_source,
                &format!("dist/{}", path.as_str()),
                FetchOperation::Artifact,
                temporary_path,
            )
            .await
        {
            Ok(file) => file,
            Err(error) => {
                self.record_audit("artifact_fetch_failed", path.as_str(), "upstream_failed");
                return Err(error.into());
            }
        };
        let file = match self.cache.publish_verified_file(
            &selected_source,
            path,
            &fetched.path,
            &expected_checksum,
        ) {
            Ok(file) => file,
            Err(StorageError::ArtifactHashMismatch(_)) => {
                self.record_audit("artifact_fetch_failed", path.as_str(), "hash_mismatch");
                return Err(ServiceError::Storage(StorageError::ArtifactHashMismatch(
                    path.to_string(),
                )));
            }
            Err(error) => return Err(error.into()),
        };
        self.record_audit("artifact_cached", path.as_str(), "success");
        Ok(file)
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
        let mut run = GroupRun::new(&self.sources, source_name, None)?;
        let is_group = run.is_group();
        let manifest_key = format!("dist/channel-rust-{channel}.toml");
        let checksum_key = format!("{manifest_key}.sha256");
        let entry_root = self.entry_root_url(source_name);
        let file_name = format!("channel-rust-{channel}.toml");
        let policy = self.manifest_policy();
        while let Some(member) = run.next_member() {
            let manifest = match self
                .sources
                .fetch(&member.name, &manifest_key, FetchOperation::Metadata)
                .await
            {
                Ok(response) => response.body,
                Err(error) if should_try_next_source(&error, is_group) => {
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        source_failure_category(&error),
                    );
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
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
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        source_failure_category(&error),
                    );
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };

            let manifest = match String::from_utf8(manifest) {
                Ok(manifest) => manifest,
                Err(_) => {
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        "protocol_invalid",
                    );
                    run.record_failure(&member.name, "manifest is not UTF-8", false);
                    continue;
                }
            };
            let checksum_file = match String::from_utf8(checksum_file) {
                Ok(checksum_file) => checksum_file,
                Err(_) => {
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        "protocol_invalid",
                    );
                    run.record_failure(&member.name, "checksum is not UTF-8", false);
                    continue;
                }
            };
            let actual_checksum = sha256(manifest.as_bytes());
            if parse_checksum_file(&checksum_file, &file_name) != Some(actual_checksum.as_str()) {
                self.record_manifest_member_failure(
                    source_name,
                    channel,
                    &member.name,
                    "checksum_mismatch",
                );
                run.record_failure(&member.name, "checksum does not match manifest", false);
                continue;
            }

            match rewrite_manifest(&manifest, &policy, &entry_root, channel) {
                Ok(rewritten) => return Ok((member.name, rewritten)),
                Err(error) => {
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        "protocol_invalid",
                    );
                    run.record_failure(&member.name, &error.to_string(), false);
                }
            }
        }

        Err(run.all_sources_failed(manifest_key).into())
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

    fn record_audit(&self, event: &str, target: &str, outcome: &str) {
        if let Err(error) = self.audit.record(event, target, outcome) {
            crate::logging::error(format!("audit write failed: {error}"));
        }
    }

    fn record_manifest_member_failure(
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

fn source_failure_category(error: &SourcePoolError) -> &'static str {
    match error {
        SourcePoolError::TerminalSourceError { status: 404, .. }
        | SourcePoolError::AllSourcesFailed {
            status: Some(404), ..
        } => "not_found",
        _ => "source_failed",
    }
}

fn temporary_artifact_path(data_dir: &Path) -> Result<PathBuf, io::Error> {
    let directory = data_dir.join("tmp");
    fs::create_dir_all(&directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(directory.join(format!("rustup-{}-{nonce}.part", std::process::id())))
}

fn cleanup_temporary_artifacts(data_dir: &Path) {
    let directory = data_dir.join("tmp");
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("rustup-") && name.ends_with(".part") {
            let _ = fs::remove_file(entry.path());
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
