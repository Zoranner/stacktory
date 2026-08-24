use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

#[derive(Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub public_base_url: Url,
    pub access: AccessConfig,
    pub rustup: RustupConfig,
}

#[derive(Debug, Deserialize)]
pub struct AccessConfig {
    pub allow_anonymous: bool,
    pub read_token_env: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RustupConfig {
    pub upstreams: Vec<UpstreamConfig>,
}

#[derive(Debug, Deserialize)]
pub struct UpstreamConfig {
    pub name: String,
    pub base_url: Url,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration TOML is invalid: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("listen address is invalid: {0}")]
    ListenAddress(#[from] std::net::AddrParseError),
    #[error("public_base_url must use https outside localhost")]
    PublicBaseUrlMustUseHttps,
    #[error("access.allow_anonymous is allowed only with a localhost public_base_url")]
    AnonymousOutsideLocalhost,
    #[error("access.read_token_env is required when anonymous access is disabled")]
    MissingReadTokenEnvironment,
    #[error("rustup.upstreams must contain at least one upstream")]
    MissingUpstream,
    #[error("rustup upstream `{0}` must use https without query or fragment")]
    InvalidUpstream(String),
}

#[derive(Deserialize)]
struct RawConfig {
    listen: String,
    data_dir: PathBuf,
    public_base_url: Url,
    access: AccessConfig,
    rustup: RustupConfig,
}

impl Config {
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(input)?;
        let listen = raw.listen.parse()?;

        if !is_localhost(&raw.public_base_url) && raw.public_base_url.scheme() != "https" {
            return Err(ConfigError::PublicBaseUrlMustUseHttps);
        }
        if raw.access.allow_anonymous && !is_localhost(&raw.public_base_url) {
            return Err(ConfigError::AnonymousOutsideLocalhost);
        }
        if !raw.access.allow_anonymous && raw.access.read_token_env.is_none() {
            return Err(ConfigError::MissingReadTokenEnvironment);
        }
        if raw.rustup.upstreams.is_empty() {
            return Err(ConfigError::MissingUpstream);
        }
        for upstream in &raw.rustup.upstreams {
            if upstream.base_url.scheme() != "https"
                || upstream.base_url.query().is_some()
                || upstream.base_url.fragment().is_some()
            {
                return Err(ConfigError::InvalidUpstream(upstream.name.clone()));
            }
        }

        Ok(Self {
            listen,
            data_dir: raw.data_dir,
            public_base_url: raw.public_base_url,
            access: raw.access,
            rustup: raw.rustup,
        })
    }
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
