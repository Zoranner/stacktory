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
    pub rustup: RustupConfig,
}

#[derive(Debug, Deserialize)]
pub struct RustupConfig {
    pub refresh_interval_secs: u64,
    pub channels: Vec<String>,
    pub upstreams: Vec<UpstreamConfig>,
}

#[derive(Debug, Deserialize)]
pub struct UpstreamConfig {
    pub name: String,
    pub base_url: Url,
    #[serde(default = "requires_https")]
    pub require_https: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration TOML is invalid: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("listen address is invalid: {0}")]
    ListenAddress(#[from] std::net::AddrParseError),
    #[error("public_base_url must use https outside localhost")]
    PublicBaseUrlMustUseHttps,
    #[error("rustup.upstreams must contain at least one upstream")]
    MissingUpstream,
    #[error("rustup.channels must contain at least one channel")]
    MissingRustupChannel,
    #[error("rustup channel `{0}` is invalid")]
    InvalidRustupChannel(String),
    #[error("rustup.refresh_interval_secs must be greater than zero")]
    InvalidRustupRefreshInterval,
    #[error(
        "rustup upstream `{0}` must use http or https with a host and without credentials, query, or fragment"
    )]
    InvalidUpstream(String),
    #[error("rustup upstream `{0}` uses http; set require_https = false to opt in")]
    InsecureHttpUpstream(String),
}

#[derive(Deserialize)]
struct RawConfig {
    listen: String,
    data_dir: PathBuf,
    public_base_url: Url,
    rustup: RustupConfig,
}

impl Config {
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(input)?;
        let listen = raw.listen.parse()?;

        if !is_localhost(&raw.public_base_url) && raw.public_base_url.scheme() != "https" {
            return Err(ConfigError::PublicBaseUrlMustUseHttps);
        }
        if raw.rustup.upstreams.is_empty() {
            return Err(ConfigError::MissingUpstream);
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
        for upstream in &raw.rustup.upstreams {
            if !matches!(upstream.base_url.scheme(), "http" | "https")
                || upstream.base_url.host_str().is_none()
                || !upstream.base_url.username().is_empty()
                || upstream.base_url.password().is_some()
                || upstream.base_url.query().is_some()
                || upstream.base_url.fragment().is_some()
            {
                return Err(ConfigError::InvalidUpstream(upstream.name.clone()));
            }
            if upstream.base_url.scheme() == "http" && upstream.require_https {
                return Err(ConfigError::InsecureHttpUpstream(upstream.name.clone()));
            }
        }

        Ok(Self {
            listen,
            data_dir: raw.data_dir,
            public_base_url: raw.public_base_url,
            rustup: raw.rustup,
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

fn is_localhost(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}
