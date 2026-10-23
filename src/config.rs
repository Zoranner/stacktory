use std::{collections::BTreeMap, net::IpAddr, path::PathBuf};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

#[derive(Debug)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub proxy: ProxyConfig,
    pub retention: RetentionConfig,
    pub rustup: RustupConfig,
    pub cargo: Option<CargoConfig>,
    pub vscode: Option<VscodeConfig>,
    pub docker: Option<DockerConfig>,
    pub npm: Option<NpmConfig>,
    pub nuget: Option<NugetConfig>,
    pub upm: Option<UpmConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the process binds to, not the address clients use.
    pub address: IpAddr,
    /// Port the process binds to, not necessarily the port in `public_url`.
    pub port: u16,
    /// Client-visible base URL used when rewriting protocol responses.
    pub public_url: Url,
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
    /// Outbound HTTP/HTTPS proxy used for upstream requests.
    #[serde(default)]
    pub url: Option<Url>,
    /// Comma-separated hosts that bypass the outbound proxy.
    #[serde(default)]
    pub no_proxy: Option<String>,
}

pub const DEFAULT_METADATA_RETENTION_SECS: u64 = 604_800;
pub const DEFAULT_AUDIT_RETENTION_DAYS: u64 = 30;
pub const DEFAULT_CLEANUP_INTERVAL_SECS: u64 = 3_600;
pub const DEFAULT_RUSTUP_SNAPSHOT_REVISIONS: u64 = 3;
pub const DEFAULT_ARTIFACT_IDLE_SECS: u64 = 7_776_000;
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
    pub cli_listen_ports: BTreeMap<String, u16>,
}
pub type NpmConfig = EcosystemConfig;
pub type NugetConfig = EcosystemConfig;
pub type UpmConfig = EcosystemConfig;

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
    ///
    /// Metadata that stores absolute upstream URLs must be invalidated when
    /// this value changes, even if the configured name stays the same.
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
    entries: BTreeMap<String, SourceConfig>,
}

impl SourceGraph {
    fn from_raw(sources: Vec<RawSourceConfig>) -> Result<Self, ConfigError> {
        sources
            .into_iter()
            .map(RawSourceConfig::into_source_config)
            .collect::<Result<Vec<_>, _>>()
            .and_then(Self::new)
    }

    fn from_raw_docker(
        sources: Vec<RawDockerSourceConfig>,
    ) -> Result<(Self, BTreeMap<String, u16>), ConfigError> {
        let mut cli_listen_ports = BTreeMap::new();
        let sources = sources
            .into_iter()
            .map(|source| {
                let (source, port) = source.into_parts();
                if let Some(port) = port {
                    cli_listen_ports.insert(source.name.clone(), port);
                }
                source
            })
            .collect();
        let graph = Self::from_raw(sources)?;
        Ok((graph, cli_listen_ports))
    }

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
                    if !source.members.is_empty() {
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
                    } else {
                        return Err(ConfigError::EmptyGroup(source.name.clone()));
                    }
                }
            }
        }

        Ok(Self { entries })
    }

    pub fn get(&self, name: &str) -> Option<&SourceConfig> {
        self.entries.get(name)
    }

    pub fn entries(&self) -> impl Iterator<Item = &SourceConfig> {
        self.entries.values()
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
    #[error("server.public_url must use https outside localhost")]
    PublicUrlMustUseHttps,
    #[error("outbound proxy URL must use http or https")]
    InvalidProxyScheme,
    #[error(
        "outbound proxy URL must include a host and must not include a path, query or fragment"
    )]
    InvalidProxyUrl,
    #[error("proxy.no_proxy requires proxy.url")]
    ProxyNoProxyWithoutProxy,
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
    #[error("NuGet source `{0}` base_url must be an actual `/v3/index.json` service index")]
    InvalidNugetServiceIndexUrl(String),
    #[error("Docker source `{0}` has an invalid cli_listen_port")]
    InvalidDockerCliPort(String),
    #[error("Docker source `{source_name}` uses duplicate cli_listen_port {port}")]
    DuplicateDockerCliPort { source_name: String, port: u16 },
    #[error("Docker source `{source_name}` cli_listen_port {port} conflicts with server.port")]
    DockerCliPortConflictsWithServer { source_name: String, port: u16 },
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    server: ServerConfig,
    storage: StorageConfig,
    #[serde(default)]
    proxy: ProxyConfig,
    #[serde(default)]
    retention: RetentionConfig,
    rustup: RawRustupConfig,
    cargo: Option<RawEcosystemConfig>,
    vscode: Option<RawEcosystemConfig>,
    docker: Option<RawDockerConfig>,
    npm: Option<RawEcosystemConfig>,
    nuget: Option<RawEcosystemConfig>,
    upm: Option<RawEcosystemConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRustupConfig {
    refresh_interval_secs: u64,
    channels: Vec<String>,
    sources: Vec<RawSourceConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEcosystemConfig {
    sources: Vec<RawSourceConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDockerConfig {
    sources: Vec<RawDockerSourceConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDockerSourceConfig {
    // Keep these fields explicit. serde(flatten) would bypass deny_unknown_fields.
    name: String,
    #[serde(rename = "type")]
    source_type: SourceType,
    #[serde(default)]
    base_url: Option<Url>,
    #[serde(default)]
    allow_http: Option<bool>,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    cli_listen_port: Option<u16>,
}

impl RawDockerSourceConfig {
    fn into_parts(self) -> (RawSourceConfig, Option<u16>) {
        (
            RawSourceConfig {
                name: self.name,
                source_type: self.source_type,
                base_url: self.base_url,
                allow_http: self.allow_http,
                members: self.members,
            },
            self.cli_listen_port,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSourceConfig {
    name: String,
    #[serde(rename = "type")]
    source_type: SourceType,
    #[serde(default)]
    base_url: Option<Url>,
    #[serde(default)]
    allow_http: Option<bool>,
    #[serde(default)]
    members: Vec<String>,
}

impl RawSourceConfig {
    fn into_source_config(self) -> Result<SourceConfig, ConfigError> {
        if self.source_type == SourceType::Group && self.allow_http.is_some() {
            return Err(ConfigError::GroupWithAllowHttp(self.name));
        }
        let require_https = match self.source_type {
            SourceType::Proxy => !self.allow_http.unwrap_or(false),
            SourceType::Group => false,
        };

        Ok(SourceConfig {
            name: self.name,
            source_type: self.source_type,
            base_url: self.base_url,
            require_https,
            members: self.members,
        })
    }
}

impl Config {
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(input)?;

        validate_public_url(&raw.server.public_url)?;
        if let Some(proxy_url) = &raw.proxy.url {
            if !matches!(proxy_url.scheme(), "http" | "https") {
                return Err(ConfigError::InvalidProxyScheme);
            }
            if proxy_url.host_str().is_none()
                || proxy_url.path() != "/"
                || proxy_url.query().is_some()
                || proxy_url.fragment().is_some()
            {
                return Err(ConfigError::InvalidProxyUrl);
            }
        } else if raw.proxy.no_proxy.is_some() {
            return Err(ConfigError::ProxyNoProxyWithoutProxy);
        }
        if raw.rustup.refresh_interval_secs == 0 {
            return Err(ConfigError::InvalidRustupRefreshInterval);
        }
        if raw.retention.metadata_retention_secs == 0 {
            return Err(ConfigError::InvalidMetadataRetention);
        }
        if raw.retention.audit_retention_days == 0 {
            return Err(ConfigError::InvalidAuditRetentionDays);
        }
        if raw.retention.cleanup_interval_secs == 0 {
            return Err(ConfigError::InvalidCleanupInterval);
        }
        if raw.retention.rustup_snapshot_revisions == 0 {
            return Err(ConfigError::InvalidRustupSnapshotRevisions);
        }
        if raw.retention.log_retention_days == 0 {
            return Err(ConfigError::InvalidLogRetentionDays);
        }
        if raw.rustup.channels.is_empty() {
            return Err(ConfigError::MissingRustupChannel);
        }
        for channel in &raw.rustup.channels {
            if !is_valid_channel(channel) {
                return Err(ConfigError::InvalidRustupChannel(channel.clone()));
            }
        }
        let rustup_sources = SourceGraph::from_raw(raw.rustup.sources)?;
        let cargo = ecosystem_config(raw.cargo)?;
        let vscode = ecosystem_config(raw.vscode)?;
        let docker = docker_config(raw.docker, raw.server.port)?;
        let npm = ecosystem_config(raw.npm)?;
        let nuget = nuget_config(raw.nuget)?;
        let upm = ecosystem_config(raw.upm)?;

        Ok(Self {
            server: raw.server,
            storage: raw.storage,
            proxy: raw.proxy,
            retention: raw.retention,
            rustup: RustupConfig {
                refresh_interval_secs: raw.rustup.refresh_interval_secs,
                channels: raw.rustup.channels,
                sources: rustup_sources,
            },
            cargo,
            vscode,
            docker,
            npm,
            nuget,
            upm,
        })
    }
}

fn ecosystem_config(
    raw: Option<RawEcosystemConfig>,
) -> Result<Option<EcosystemConfig>, ConfigError> {
    raw.map(|ecosystem| SourceGraph::from_raw(ecosystem.sources))
        .transpose()
        .map(|sources| sources.map(|sources| EcosystemConfig { sources }))
}

fn docker_config(
    raw: Option<RawDockerConfig>,
    server_port: u16,
) -> Result<Option<DockerConfig>, ConfigError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let (sources, cli_listen_ports) = SourceGraph::from_raw_docker(raw.sources)?;
    let mut used_ports = BTreeMap::new();
    for source in sources.entries.values() {
        let Some(port) = cli_listen_ports.get(&source.name).copied() else {
            continue;
        };
        if port == 0 {
            return Err(ConfigError::InvalidDockerCliPort(source.name.clone()));
        }
        if port == server_port {
            return Err(ConfigError::DockerCliPortConflictsWithServer {
                source_name: source.name.clone(),
                port,
            });
        }
        if used_ports.insert(port, source.name.clone()).is_some() {
            return Err(ConfigError::DuplicateDockerCliPort {
                source_name: source.name.clone(),
                port,
            });
        }
    }
    Ok(Some(DockerConfig {
        sources,
        cli_listen_ports,
    }))
}

fn nuget_config(raw: Option<RawEcosystemConfig>) -> Result<Option<EcosystemConfig>, ConfigError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let sources = SourceGraph::from_raw(raw.sources)?;
    for source in sources.proxies() {
        if !source
            .base_url
            .as_ref()
            .is_some_and(|url| url.path().ends_with("/v3/index.json"))
        {
            return Err(ConfigError::InvalidNugetServiceIndexUrl(
                source.name.clone(),
            ));
        }
    }
    Ok(Some(EcosystemConfig { sources }))
}

fn default_log_dir() -> PathBuf {
    PathBuf::from(DEFAULT_LOG_DIR)
}

fn validate_public_url(url: &Url) -> Result<(), ConfigError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ConfigError::PublicUrlUnsupportedScheme);
    }
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::InvalidPublicUrl);
    }
    if url.scheme() != "https" && !is_localhost(url) {
        return Err(ConfigError::PublicUrlMustUseHttps);
    }
    Ok(())
}

fn is_valid_channel(channel: &str) -> bool {
    !channel.is_empty()
        && channel != "."
        && channel != ".."
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

fn is_valid_source_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_valid_source_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn is_localhost(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}
