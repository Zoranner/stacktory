use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    path::PathBuf,
    time::Duration,
};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

use super::{
    public_url::PublicUrls,
    validate::{is_valid_source_name, is_valid_source_url},
};

#[derive(Debug)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub proxy: ProxyConfig,
    pub upstream: UpstreamConfig,
    pub retention: RetentionConfig,
    pub rustup: RustupConfig,
    pub cargo: Option<CargoConfig>,
    pub vscode: Option<VscodeConfig>,
    pub docker: Option<DockerConfig>,
    pub npm: Option<NpmConfig>,
    pub nuget: Option<NugetConfig>,
    pub pypi: Option<PyPiConfig>,
    pub upm: Option<UpmConfig>,
    pub winget: Option<WingetConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the process binds to, not the address clients use.
    pub address: IpAddr,
    /// Port the process binds to, not necessarily the port in `public_url`.
    pub port: u16,
    /// Client-visible base URLs used when rewriting protocol responses.
    ///
    /// The first entry is the default; the HTTP layer matches the current
    /// request against the remaining entries.
    pub public_url: PublicUrls,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Root directory for metadata, artifacts, snapshots and audit data.
    pub data_dir: PathBuf,
    /// Directory for rotating runtime logs.
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    #[serde(default)]
    pub url: Option<Url>,
    #[serde(default)]
    pub no_proxy: Option<String>,
}

pub const DEFAULT_METADATA_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_ARTIFACT_TIMEOUT_SECS: u64 = 600;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct UpstreamConfig {
    pub metadata_timeout_secs: u64,
    pub artifact_timeout_secs: u64,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            metadata_timeout_secs: DEFAULT_METADATA_TIMEOUT_SECS,
            artifact_timeout_secs: DEFAULT_ARTIFACT_TIMEOUT_SECS,
        }
    }
}

impl UpstreamConfig {
    pub fn metadata_timeout(&self) -> Duration {
        Duration::from_secs(self.metadata_timeout_secs)
    }

    pub fn artifact_timeout(&self) -> Duration {
        Duration::from_secs(self.artifact_timeout_secs)
    }
}

pub const DEFAULT_METADATA_RETENTION_SECS: u64 = 604_800;
pub const DEFAULT_AUDIT_RETENTION_DAYS: u64 = 30;
pub const DEFAULT_CLEANUP_INTERVAL_SECS: u64 = 3_600;
pub const DEFAULT_RUSTUP_SNAPSHOT_REVISIONS: u64 = 3;
pub const DEFAULT_ARTIFACT_IDLE_SECS: u64 = 7_776_000;
pub const DEFAULT_BINDING_IDLE_SECS: u64 = 7_776_000;
pub const DEFAULT_LOG_RETENTION_DAYS: u64 = 30;
pub const DEFAULT_LOG_DIR: &str = "logs";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct RetentionConfig {
    pub metadata_retention_secs: u64,
    pub audit_retention_days: u64,
    pub cleanup_interval_secs: u64,
    pub rustup_snapshot_revisions: u64,
    pub artifact_idle_secs: u64,
    pub binding_idle_secs: u64,
    pub log_retention_days: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            metadata_retention_secs: DEFAULT_METADATA_RETENTION_SECS,
            audit_retention_days: DEFAULT_AUDIT_RETENTION_DAYS,
            cleanup_interval_secs: DEFAULT_CLEANUP_INTERVAL_SECS,
            rustup_snapshot_revisions: DEFAULT_RUSTUP_SNAPSHOT_REVISIONS,
            artifact_idle_secs: DEFAULT_ARTIFACT_IDLE_SECS,
            binding_idle_secs: DEFAULT_BINDING_IDLE_SECS,
            log_retention_days: DEFAULT_LOG_RETENTION_DAYS,
        }
    }
}

#[derive(Debug)]
pub struct RustupConfig {
    pub refresh_interval_secs: u64,
    pub channels: Vec<String>,
    pub sources: SourceGraph,
}

#[derive(Clone, Debug)]
pub struct EcosystemConfig {
    pub sources: SourceGraph,
}

pub type CargoConfig = EcosystemConfig;
pub type VscodeConfig = EcosystemConfig;

#[derive(Clone, Debug)]
pub struct DockerConfig {
    pub sources: SourceGraph,
    pub registry_endpoints: BTreeMap<String, DockerRegistryEndpoint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DockerRegistryEndpoint {
    pub listen_port: u16,
    pub public_url: Url,
}

pub type NpmConfig = EcosystemConfig;
pub type NugetConfig = EcosystemConfig;
pub type PyPiConfig = EcosystemConfig;
pub type UpmConfig = EcosystemConfig;

#[derive(Clone, Debug)]
pub struct WingetConfig {
    pub sources: SourceGraph,
    pub installer_hosts: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    Proxy,
    Group,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceConfig {
    pub name: String,
    pub source_type: SourceType,
    pub base_url: Option<Url>,
    pub require_https: bool,
    pub members: Vec<String>,
}

impl SourceConfig {
    /// Identity of the resolved upstream target, not just the source name.
    pub fn fingerprint(&self) -> String {
        format!(
            "{}|{}|{}",
            self.name,
            self.base_url.as_ref().map(Url::as_str).unwrap_or_default(),
            self.require_https
        )
    }
}

#[derive(Clone, Debug)]
pub struct SourceGraph {
    pub(super) entries: BTreeMap<String, SourceConfig>,
    pub(super) hidden_from_docs: BTreeSet<String>,
}

impl SourceGraph {
    pub fn new(sources: Vec<SourceConfig>) -> Result<Self, ConfigError> {
        let mut entries = BTreeMap::new();
        for source in sources {
            let name = source.name.clone();
            if !is_valid_source_name(&source.name) {
                return Err(ConfigError::InvalidSourceName(source.name));
            }
            if entries.insert(name.clone(), source).is_some() {
                return Err(ConfigError::DuplicateSourceName(name));
            }
        }
        if entries.is_empty() {
            return Err(ConfigError::MissingSource);
        }

        for source in entries.values() {
            match source.source_type {
                SourceType::Proxy => {
                    let base_url = source
                        .base_url
                        .as_ref()
                        .ok_or_else(|| ConfigError::ProxyMissingBaseUrl(source.name.clone()))?;
                    if !is_valid_source_url(base_url) {
                        return Err(ConfigError::InvalidSourceUrl(source.name.clone()));
                    }
                    if base_url.scheme() == "http" && source.require_https {
                        return Err(ConfigError::InsecureHttpSource(source.name.clone()));
                    }
                    if !source.members.is_empty() {
                        return Err(ConfigError::ProxyWithMembers(source.name.clone()));
                    }
                }
                SourceType::Group => {
                    if source.base_url.is_some() {
                        return Err(ConfigError::GroupWithBaseUrl(source.name.clone()));
                    }
                    if source.require_https {
                        return Err(ConfigError::GroupWithAllowHttp(source.name.clone()));
                    }
                    if source.members.is_empty() {
                        return Err(ConfigError::EmptyGroup(source.name.clone()));
                    }
                    let mut members = BTreeMap::new();
                    for member in &source.members {
                        if members.insert(member, ()).is_some() {
                            return Err(ConfigError::DuplicateGroupMember(
                                source.name.clone(),
                                member.clone(),
                            ));
                        }
                        let referenced = entries.get(member).ok_or_else(|| {
                            ConfigError::UnknownGroupMember(source.name.clone(), member.clone())
                        })?;
                        if referenced.source_type == SourceType::Group {
                            return Err(ConfigError::NestedGroup(
                                source.name.clone(),
                                member.clone(),
                            ));
                        }
                    }
                }
            }
        }

        Ok(Self {
            entries,
            hidden_from_docs: BTreeSet::new(),
        })
    }

    pub fn get(&self, name: &str) -> Option<&SourceConfig> {
        self.entries.get(name)
    }

    pub fn entries(&self) -> impl Iterator<Item = &SourceConfig> {
        self.entries.values()
    }

    pub fn docs_visible_entries(&self) -> impl Iterator<Item = &SourceConfig> {
        self.entries
            .values()
            .filter(|source| !self.hidden_from_docs.contains(&source.name))
    }

    pub fn docs_visible(&self, name: &str) -> bool {
        self.entries.contains_key(name) && !self.hidden_from_docs.contains(name)
    }

    pub fn proxy_members(&self, name: &str) -> Option<Vec<&SourceConfig>> {
        let source = self.entries.get(name)?;
        match source.source_type {
            SourceType::Proxy => Some(vec![source]),
            SourceType::Group => Some(
                source
                    .members
                    .iter()
                    .map(|member| {
                        self.entries
                            .get(member)
                            .expect("validated group member must exist")
                    })
                    .collect(),
            ),
        }
    }

    pub fn proxies(&self) -> Vec<&SourceConfig> {
        self.entries
            .values()
            .filter(|source| source.source_type == SourceType::Proxy)
            .collect()
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration TOML is invalid: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("server.public_url must use http or https")]
    PublicUrlUnsupportedScheme,
    #[error(
        "server.public_url must include a host and must not include credentials, query or fragment"
    )]
    InvalidPublicUrl,
    #[error("outbound proxy URL must use http or https")]
    InvalidProxyScheme,
    #[error(
        "outbound proxy URL must include a host and must not include a path, query or fragment"
    )]
    InvalidProxyUrl,
    #[error("proxy.no_proxy requires proxy.url")]
    ProxyNoProxyWithoutProxy,
    #[error("upstream.metadata_timeout_secs must be greater than zero")]
    InvalidMetadataTimeout,
    #[error("upstream.artifact_timeout_secs must be greater than zero")]
    InvalidArtifactTimeout,
    #[error("sources must contain at least one source")]
    MissingSource,
    #[error("source name `{0}` is invalid")]
    InvalidSourceName(String),
    #[error("source name `{0}` is duplicated")]
    DuplicateSourceName(String),
    #[error("proxy source `{0}` requires base_url")]
    ProxyMissingBaseUrl(String),
    #[error("group source `{0}` must not define base_url")]
    GroupWithBaseUrl(String),
    #[error("group source `{0}` must not define allow_http")]
    GroupWithAllowHttp(String),
    #[error("proxy source `{0}` must not define members")]
    ProxyWithMembers(String),
    #[error("group source `{0}` must contain at least one member")]
    EmptyGroup(String),
    #[error("group source `{0}` references unknown member `{1}`")]
    UnknownGroupMember(String, String),
    #[error("group source `{0}` contains duplicate member `{1}`")]
    DuplicateGroupMember(String, String),
    #[error("group source `{0}` cannot reference group `{1}`")]
    NestedGroup(String, String),
    #[error("source `{0}` has an invalid base_url")]
    InvalidSourceUrl(String),
    #[error("source `{0}` uses http; set allow_http = true to opt in")]
    InsecureHttpSource(String),
    #[error("Winget proxy source `{0}` requires at least one installer_hosts entry")]
    WingetInstallerHostsMissing(String),
    #[error("Winget group source `{0}` must not define installer_hosts")]
    WingetGroupWithInstallerHosts(String),
    #[error("Winget source `{0}` has invalid installer_hosts entry `{1}`")]
    InvalidWingetInstallerHost(String, String),
    #[error("NuGet source `{0}` base_url must be an actual `/v3/index.json` service index")]
    InvalidNugetServiceIndexUrl(String),
    #[error("Docker source `{0}` has an invalid listen_port")]
    InvalidDockerRegistryPort(String),
    #[error("Docker source `{0}` sets listen_port without public_url")]
    DockerRegistryPublicUrlMissing(String),
    #[error("Docker source `{0}` sets public_url without listen_port")]
    DockerRegistryListenPortMissing(String),
    #[error("Docker source `{0}` has an invalid public_url")]
    InvalidDockerRegistryPublicUrl(String),
    #[error("Docker source `{source_name}` uses duplicate listen_port {port}")]
    DuplicateDockerRegistryPort { source_name: String, port: u16 },
    #[error("Docker source `{source_name}` uses duplicate public_url `{url}`")]
    DuplicateDockerRegistryPublicUrl { source_name: String, url: String },
    #[error("Docker source `{source_name}` listen_port {port} conflicts with server.port")]
    DockerRegistryPortConflictsWithServer { source_name: String, port: u16 },
    #[error("rustup.channels must contain at least one channel")]
    MissingRustupChannel,
    #[error("rustup channel `{0}` is invalid")]
    InvalidRustupChannel(String),
    #[error("rustup.refresh_interval_secs must be greater than zero")]
    InvalidRustupRefreshInterval,
    #[error("retention.metadata_retention_secs must be greater than zero")]
    InvalidMetadataRetention,
    #[error("retention.audit_retention_days must be greater than zero")]
    InvalidAuditRetentionDays,
    #[error("retention.cleanup_interval_secs must be greater than zero")]
    InvalidCleanupInterval,
    #[error("retention.rustup_snapshot_revisions must be greater than zero")]
    InvalidRustupSnapshotRevisions,
    #[error("retention.log_retention_days must be greater than zero")]
    InvalidLogRetentionDays,
}

fn default_log_dir() -> PathBuf {
    PathBuf::from(DEFAULT_LOG_DIR)
}
