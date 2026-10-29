use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use url::Url;

use super::{
    model::{
        Config, ConfigError, DockerConfig, DockerRegistryEndpoint, EcosystemConfig, RustupConfig,
        SourceConfig, SourceGraph, SourceType, WingetConfig,
    },
    validate::{is_valid_channel, is_valid_docker_registry_public_url, validate_public_urls},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    server: super::model::ServerConfig,
    storage: super::model::StorageConfig,
    #[serde(default)]
    proxy: super::model::ProxyConfig,
    #[serde(default)]
    upstream: super::model::UpstreamConfig,
    #[serde(default)]
    retention: super::model::RetentionConfig,
    rustup: RawRustupConfig,
    cargo: Option<RawEcosystemConfig>,
    vscode: Option<RawEcosystemConfig>,
    docker: Option<RawDockerConfig>,
    npm: Option<RawEcosystemConfig>,
    nuget: Option<RawEcosystemConfig>,
    pypi: Option<RawEcosystemConfig>,
    upm: Option<RawEcosystemConfig>,
    winget: Option<RawWingetConfig>,
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
struct RawWingetConfig {
    sources: Vec<RawWingetSourceConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWingetSourceConfig {
    name: String,
    #[serde(rename = "type")]
    source_type: SourceType,
    #[serde(default = "default_visible")]
    visible: bool,
    #[serde(default)]
    base_url: Option<Url>,
    #[serde(default)]
    allow_http: Option<bool>,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    installer_hosts: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDockerSourceConfig {
    name: String,
    #[serde(rename = "type")]
    source_type: SourceType,
    #[serde(default = "default_visible")]
    visible: bool,
    #[serde(default)]
    base_url: Option<Url>,
    #[serde(default)]
    allow_http: Option<bool>,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    listen_port: Option<u16>,
    #[serde(default)]
    public_url: Option<Url>,
}

impl RawDockerSourceConfig {
    fn into_parts(self) -> Result<(RawSourceConfig, Option<DockerRegistryEndpoint>), ConfigError> {
        let name = self.name;
        let endpoint = match (self.listen_port, self.public_url) {
            (None, None) => None,
            (Some(_), None) => {
                return Err(ConfigError::DockerRegistryPublicUrlMissing(name));
            }
            (None, Some(_)) => {
                return Err(ConfigError::DockerRegistryListenPortMissing(name));
            }
            (Some(listen_port), Some(public_url)) => Some(DockerRegistryEndpoint {
                listen_port,
                public_url,
            }),
        };
        Ok((
            RawSourceConfig {
                name,
                source_type: self.source_type,
                visible: self.visible,
                base_url: self.base_url,
                allow_http: self.allow_http,
                members: self.members,
            },
            endpoint,
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSourceConfig {
    name: String,
    #[serde(rename = "type")]
    source_type: SourceType,
    #[serde(default = "default_visible")]
    visible: bool,
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

fn default_visible() -> bool {
    true
}

impl SourceGraph {
    fn from_raw(sources: Vec<RawSourceConfig>) -> Result<Self, ConfigError> {
        let mut hidden_from_docs = BTreeSet::new();
        let mut configs = Vec::with_capacity(sources.len());
        for source in sources {
            if !source.visible {
                hidden_from_docs.insert(source.name.clone());
            }
            configs.push(source.into_source_config()?);
        }
        let mut graph = Self::new(configs)?;
        graph.hidden_from_docs = hidden_from_docs;
        Ok(graph)
    }

    fn from_raw_docker(
        sources: Vec<RawDockerSourceConfig>,
    ) -> Result<(Self, BTreeMap<String, DockerRegistryEndpoint>), ConfigError> {
        let mut registry_endpoints = BTreeMap::new();
        let mut normalized = Vec::with_capacity(sources.len());
        for source in sources {
            let (source, endpoint) = source.into_parts()?;
            if let Some(endpoint) = endpoint {
                registry_endpoints.insert(source.name.clone(), endpoint);
            }
            normalized.push(source);
        }
        let graph = Self::from_raw(normalized)?;
        Ok((graph, registry_endpoints))
    }

    fn from_raw_winget(
        sources: Vec<RawWingetSourceConfig>,
    ) -> Result<(Self, BTreeMap<String, BTreeSet<String>>), ConfigError> {
        let mut installer_hosts = BTreeMap::new();
        let mut hidden_from_docs = BTreeSet::new();
        let mut normalized = Vec::with_capacity(sources.len());
        for source in sources {
            let name = source.name.clone();
            if !source.visible {
                hidden_from_docs.insert(name.clone());
            }
            match source.source_type {
                SourceType::Group => {
                    if !source.installer_hosts.is_empty() {
                        return Err(ConfigError::WingetGroupWithInstallerHosts(name));
                    }
                }
                SourceType::Proxy => {
                    if source.installer_hosts.is_empty() {
                        return Err(ConfigError::WingetInstallerHostsMissing(name));
                    }
                    let mut hosts = BTreeSet::new();
                    for entry in &source.installer_hosts {
                        let host = normalize_winget_installer_host(entry).ok_or_else(|| {
                            ConfigError::InvalidWingetInstallerHost(name.clone(), entry.clone())
                        })?;
                        hosts.insert(host);
                    }
                    installer_hosts.insert(name, hosts);
                }
            }
            normalized.push(RawSourceConfig {
                name: source.name,
                source_type: source.source_type,
                visible: source.visible,
                base_url: source.base_url,
                allow_http: source.allow_http,
                members: source.members,
            });
        }
        let mut graph = Self::from_raw(normalized)?;
        graph.hidden_from_docs = hidden_from_docs;
        Ok((graph, installer_hosts))
    }
}

impl Config {
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(input)?;
        validate_public_urls(&raw.server.public_url)?;
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
        if raw.upstream.metadata_timeout_secs == 0 {
            return Err(ConfigError::InvalidMetadataTimeout);
        }
        if raw.upstream.artifact_timeout_secs == 0 {
            return Err(ConfigError::InvalidArtifactTimeout);
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
        let server_port = raw.server.port;
        Ok(Self {
            server: raw.server,
            storage: raw.storage,
            proxy: raw.proxy,
            upstream: raw.upstream,
            retention: raw.retention,
            rustup: RustupConfig {
                refresh_interval_secs: raw.rustup.refresh_interval_secs,
                channels: raw.rustup.channels,
                sources: SourceGraph::from_raw(raw.rustup.sources)?,
            },
            cargo: ecosystem_config(raw.cargo)?,
            vscode: ecosystem_config(raw.vscode)?,
            docker: docker_config(raw.docker, server_port)?,
            npm: ecosystem_config(raw.npm)?,
            nuget: nuget_config(raw.nuget)?,
            pypi: ecosystem_config(raw.pypi)?,
            upm: ecosystem_config(raw.upm)?,
            winget: winget_config(raw.winget)?,
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
    let (sources, registry_endpoints) = SourceGraph::from_raw_docker(raw.sources)?;
    let mut used_ports = BTreeMap::new();
    let mut used_urls = BTreeMap::new();
    for source in sources.entries.values() {
        let Some(endpoint) = registry_endpoints.get(&source.name) else {
            continue;
        };
        let port = endpoint.listen_port;
        if port == 0 {
            return Err(ConfigError::InvalidDockerRegistryPort(source.name.clone()));
        }
        if port == server_port {
            return Err(ConfigError::DockerRegistryPortConflictsWithServer {
                source_name: source.name.clone(),
                port,
            });
        }
        if used_ports.insert(port, source.name.clone()).is_some() {
            return Err(ConfigError::DuplicateDockerRegistryPort {
                source_name: source.name.clone(),
                port,
            });
        }
        if !is_valid_docker_registry_public_url(&endpoint.public_url) {
            return Err(ConfigError::InvalidDockerRegistryPublicUrl(
                source.name.clone(),
            ));
        }
        let public_url = endpoint.public_url.as_str().to_owned();
        if used_urls
            .insert(public_url.clone(), source.name.clone())
            .is_some()
        {
            return Err(ConfigError::DuplicateDockerRegistryPublicUrl {
                source_name: source.name.clone(),
                url: public_url,
            });
        }
    }
    Ok(Some(DockerConfig {
        sources,
        registry_endpoints,
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

fn winget_config(raw: Option<RawWingetConfig>) -> Result<Option<WingetConfig>, ConfigError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let (sources, installer_hosts) = SourceGraph::from_raw_winget(raw.sources)?;
    Ok(Some(WingetConfig {
        sources,
        installer_hosts,
    }))
}

fn normalize_winget_installer_host(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        || value
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '@' | '#' | '?' | '*'))
    {
        return None;
    }
    let url = Url::parse(&format!("http://{value}/")).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.port() == Some(0)
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

#[cfg(test)]
mod tests {
    use super::super::model::{Config, ConfigError};

    const BASE: &str = r#"
[server]
address = "127.0.0.1"
port = 18761
public_url = "http://127.0.0.1:18761"

[storage]
data_dir = "./var/stacktory"

[rustup]
refresh_interval_secs = 3600
channels = ["stable"]

[[rustup.sources]]
name = "official"
type = "proxy"
base_url = "https://static.rust-lang.org"
"#;

    #[test]
    fn upstream_timeouts_default_and_can_be_overridden() {
        let config = Config::from_toml(BASE).expect("default upstream timeouts");
        assert_eq!(config.upstream.metadata_timeout_secs, 30);
        assert_eq!(config.upstream.artifact_timeout_secs, 600);
        let input = format!(
            "{BASE}\n[upstream]\nmetadata_timeout_secs = 45\nartifact_timeout_secs = 900\n"
        );
        let config = Config::from_toml(&input).expect("configured upstream timeouts");
        assert_eq!(config.upstream.metadata_timeout_secs, 45);
        assert_eq!(config.upstream.artifact_timeout_secs, 900);
    }

    #[test]
    fn zero_upstream_timeouts_are_rejected() {
        let input = format!("{BASE}\n[upstream]\nmetadata_timeout_secs = 0\n");
        let error = Config::from_toml(&input).expect_err("zero metadata timeout must fail");
        assert!(matches!(error, ConfigError::InvalidMetadataTimeout));
        let input = format!("{BASE}\n[upstream]\nartifact_timeout_secs = 0\n");
        let error = Config::from_toml(&input).expect_err("zero artifact timeout must fail");
        assert!(matches!(error, ConfigError::InvalidArtifactTimeout));
    }

    #[test]
    fn deploy_example_parses() {
        let input = include_str!("../../deploy/app/config.example.toml");
        Config::from_toml(input).expect("deploy example config must parse");
    }
}
