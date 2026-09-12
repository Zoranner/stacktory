use std::{collections::BTreeMap, net::IpAddr, path::PathBuf};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

#[derive(Debug)]
pub struct Config {
    pub listen_address: IpAddr,
    pub listen_port: u16,
    pub data_dir: PathBuf,
    pub root_url: Url,
    pub rustup: RustupConfig,
    pub cargo: Option<CargoConfig>,
    pub vscode: Option<VscodeConfig>,
    pub docker: Option<DockerConfig>,
    pub npm: Option<NpmConfig>,
    pub nuget: Option<NugetConfig>,
    pub pypi: Option<PypiConfig>,
    pub upm: Option<UpmConfig>,
}

#[derive(Debug)]
pub struct RustupConfig {
    pub refresh_interval_secs: u64,
    pub channels: Vec<String>,
    pub sources: SourceGraph,
}

#[derive(Debug)]
pub struct EcosystemConfig {
    pub sources: SourceGraph,
}

pub type CargoConfig = EcosystemConfig;
pub type VscodeConfig = EcosystemConfig;
#[derive(Debug)]
pub struct DockerConfig {
    pub sources: SourceGraph,
    pub listen_ports: BTreeMap<String, u16>,
}
pub type NpmConfig = EcosystemConfig;
pub type NugetConfig = EcosystemConfig;
pub type PypiConfig = EcosystemConfig;
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

#[derive(Debug)]
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
        let listen_ports = sources
            .iter()
            .filter_map(|source| {
                source
                    .listen_port
                    .map(|port| (source.source.name.clone(), port))
            })
            .collect::<BTreeMap<_, _>>();
        let graph = Self::from_raw(sources.into_iter().map(|source| source.source).collect())?;
        Ok((graph, listen_ports))
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
                        return Err(ConfigError::GroupWithRequireHttps(source.name.clone()));
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
    #[error("root_url must use https outside localhost")]
    RootUrlMustUseHttps,
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
    #[error("group source `{0}` must not define require_https")]
    GroupWithRequireHttps(String),
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
    #[error("source `{0}` uses http; set require_https = false to opt in")]
    InsecureHttpSource(String),
    #[error("NuGet source `{0}` base_url must be an actual `/v3/index.json` service index")]
    InvalidNugetServiceIndexUrl(String),
    #[error("Docker source `{0}` has an invalid listen_port")]
    InvalidDockerListenPort(String),
    #[error("Docker source `{source_name}` uses duplicate listen_port {port}")]
    DuplicateDockerListenPort { source_name: String, port: u16 },
    #[error("Docker source `{source_name}` listen_port {port} conflicts with the main listen_port")]
    DockerListenPortConflictsWithMain { source_name: String, port: u16 },
    #[error("rustup.channels must contain at least one channel")]
    MissingRustupChannel,
    #[error("rustup channel `{0}` is invalid")]
    InvalidRustupChannel(String),
    #[error("rustup.refresh_interval_secs must be greater than zero")]
    InvalidRustupRefreshInterval,
}

#[derive(Deserialize)]
struct RawConfig {
    listen_address: IpAddr,
    listen_port: u16,
    data_dir: PathBuf,
    root_url: Url,
    rustup: RawRustupConfig,
    cargo: Option<RawEcosystemConfig>,
    vscode: Option<RawEcosystemConfig>,
    docker: Option<RawDockerConfig>,
    npm: Option<RawEcosystemConfig>,
    nuget: Option<RawEcosystemConfig>,
    pypi: Option<RawEcosystemConfig>,
    upm: Option<RawEcosystemConfig>,
}

#[derive(Deserialize)]
struct RawRustupConfig {
    refresh_interval_secs: u64,
    channels: Vec<String>,
    sources: Vec<RawSourceConfig>,
}

#[derive(Deserialize)]
struct RawEcosystemConfig {
    sources: Vec<RawSourceConfig>,
}

#[derive(Deserialize)]
struct RawDockerConfig {
    sources: Vec<RawDockerSourceConfig>,
}

#[derive(Deserialize)]
struct RawDockerSourceConfig {
    #[serde(flatten)]
    source: RawSourceConfig,
    #[serde(default)]
    listen_port: Option<u16>,
}

#[derive(Deserialize)]
struct RawSourceConfig {
    name: String,
    #[serde(rename = "type")]
    source_type: SourceType,
    #[serde(default)]
    base_url: Option<Url>,
    #[serde(default)]
    require_https: Option<bool>,
    #[serde(default)]
    members: Vec<String>,
}

impl RawSourceConfig {
    fn into_source_config(self) -> Result<SourceConfig, ConfigError> {
        if self.source_type == SourceType::Group && self.require_https.is_some() {
            return Err(ConfigError::GroupWithRequireHttps(self.name));
        }
        let require_https = match self.source_type {
            SourceType::Proxy => self.require_https.unwrap_or_else(requires_https),
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

        if !is_localhost(&raw.root_url) && raw.root_url.scheme() != "https" {
            return Err(ConfigError::RootUrlMustUseHttps);
        }
        if raw.rustup.refresh_interval_secs == 0 {
            return Err(ConfigError::InvalidRustupRefreshInterval);
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
        let docker = docker_config(raw.docker, raw.listen_port)?;
        let npm = ecosystem_config(raw.npm)?;
        let nuget = nuget_config(raw.nuget)?;
        let pypi = ecosystem_config(raw.pypi)?;
        let upm = ecosystem_config(raw.upm)?;

        Ok(Self {
            listen_address: raw.listen_address,
            listen_port: raw.listen_port,
            data_dir: raw.data_dir,
            root_url: raw.root_url,
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
            pypi,
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
    main_listen_port: u16,
) -> Result<Option<DockerConfig>, ConfigError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let (sources, listen_ports) = SourceGraph::from_raw_docker(raw.sources)?;
    let mut used_ports = BTreeMap::new();
    for source in sources.entries.values() {
        let Some(port) = listen_ports.get(&source.name).copied() else {
            continue;
        };
        if port == 0 {
            return Err(ConfigError::InvalidDockerListenPort(source.name.clone()));
        }
        if port == main_listen_port {
            return Err(ConfigError::DockerListenPortConflictsWithMain {
                source_name: source.name.clone(),
                port,
            });
        }
        if used_ports.insert(port, source.name.clone()).is_some() {
            return Err(ConfigError::DuplicateDockerListenPort {
                source_name: source.name.clone(),
                port,
            });
        }
    }
    Ok(Some(DockerConfig {
        sources,
        listen_ports,
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

const fn requires_https() -> bool {
    true
}

fn is_valid_channel(channel: &str) -> bool {
    !channel.is_empty()
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

fn is_valid_source_name(name: &str) -> bool {
    !name.is_empty()
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
