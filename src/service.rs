use thiserror::Error;

use crate::{
    Config,
    rustup::{ManifestPolicy, RustupError, rewrite_manifest},
    storage::{SnapshotStore, StorageError, StoredSnapshot},
    upstream::{UpstreamClient, UpstreamError},
};

#[derive(Debug)]
pub struct RustupService {
    channels: Vec<String>,
    public_base: url::Url,
    policy: ManifestPolicy,
    upstream: UpstreamClient,
    snapshots: SnapshotStore,
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
            public_base: config.public_base_url,
            policy: ManifestPolicy::new(bases.clone()),
            upstream: UpstreamClient::new(bases)?,
            snapshots: SnapshotStore::new(config.data_dir),
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
            Ok(snapshot) => Ok(snapshot),
            Err(ServiceError::Storage(StorageError::MissingSnapshot(_))) => {
                self.refresh(channel).await?;
                self.manifest(channel)
            }
            Err(error) => Err(error),
        }
    }

    fn ensure_channel(&self, channel: &str) -> Result<(), ServiceError> {
        if self.channels.iter().any(|configured| configured == channel) {
            Ok(())
        } else {
            Err(ServiceError::UnconfiguredChannel(channel.to_owned()))
        }
    }
}
