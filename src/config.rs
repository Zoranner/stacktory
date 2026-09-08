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
}

#[derive(Debug)]
pub struct RustupConfig {
    pub refresh_interval_secs: u64,
    pub channels: Vec<String>,
    pub sources: SourceGraph,
}

#[derive(Debug)]
pub struct CargoConfig {
    pub sources: SourceGraph,
}

impl RustupConfig {
    pub(crate) fn legacy_upstream_bases(&self) -> Vec<Url> {
        self.sources
            .proxy_sources()
            .filter_map(|source| source.base_url.clone())
            .collect()
    }
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

#[derive(Debug)]
pub struct SourceGraph {
    entries: BTreeMap<String, SourceConfig>,
    order: Vec<String>,
}

impl SourceGraph {
    fn from_raw(sources: Vec<RawSourceConfig>) -> Result<Self, ConfigError> {
        sources
            .into_iter()
            .map(RawSourceConfig::into_source_config)
            .collect::<Result<Vec<_>, _>>()
            .and_then(Self::new)
    }

    pub fn new(sources: Vec<SourceConfig>) -> Result<Self, ConfigError> {
        let mut entries = BTreeMap::new();
        let mut order = Vec::with_capacity(sources.len());

        for source in sources {
            let name = source.name.clone();
            if !is_valid_source_name(&source.name) {
                return Err(ConfigError::InvalidSourceName(source.name));
            }
            if entries.insert(name.clone(), source).is_some() {
                return Err(ConfigError::DuplicateSourceName(name));
            }
            order.push(name);
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

        Ok(Self { entries, order })
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

    fn proxy_sources(&self) -> impl Iterator<Item = &SourceConfig> {
        self.order.iter().filter_map(|name| {
            let source = self.entries.get(name)?;
            (source.source_type == SourceType::Proxy).then_some(source)
        })
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
        let cargo = raw
            .cargo
            .map(|cargo| SourceGraph::from_raw(cargo.sources))
            .transpose()?
            .map(|sources| CargoConfig { sources });

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
        })
    }
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
