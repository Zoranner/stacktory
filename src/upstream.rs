use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::rustup::DistPath;

#[derive(Debug)]
pub struct UpstreamClient {
    client: reqwest::Client,
    bases: Vec<Url>,
}

#[derive(Debug, Error)]
pub enum UpstreamError {
    #[error("at least one rustup upstream is required")]
    MissingUpstream,
    #[error("HTTP client cannot be created: {0}")]
    ClientBuild(#[from] reqwest::Error),
    #[error("rustup channel `{0}` is invalid")]
    InvalidChannel(String),
    #[error("all configured rustup upstreams failed: {0}")]
    AllUpstreamsFailed(String),
}

impl UpstreamClient {
    pub fn new(bases: Vec<Url>) -> Result<Self, UpstreamError> {
        if bases.is_empty() {
            return Err(UpstreamError::MissingUpstream);
        }

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { client, bases })
    }

    pub async fn fetch_channel_manifest(&self, channel: &str) -> Result<String, UpstreamError> {
        if !is_valid_channel(channel) {
            return Err(UpstreamError::InvalidChannel(channel.to_owned()));
        }

        let manifest_name = format!("channel-rust-{channel}.toml");
        let checksum_name = format!("{manifest_name}.sha256");
        let mut failures = Vec::new();

        for base in &self.bases {
            let manifest_url = dist_url(base, &manifest_name);
            let manifest = match self.fetch_success_bytes(&manifest_url).await {
                Ok(manifest) => manifest,
                Err(error) => {
                    failures.push(format!("{manifest_url}: {error}"));
                    continue;
                }
            };
            let checksum_url = dist_url(base, &checksum_name);
            let checksum_file = match self.fetch_success_bytes(&checksum_url).await {
                Ok(checksum_file) => checksum_file,
                Err(error) => {
                    failures.push(format!("{checksum_url}: {error}"));
                    continue;
                }
            };

            let checksum_file = match std::str::from_utf8(&checksum_file) {
                Ok(checksum_file) => checksum_file,
                Err(_) => {
                    failures.push(format!("{checksum_url}: checksum is not UTF-8"));
                    continue;
                }
            };
            let expected_checksum = match parse_checksum_file(checksum_file, &manifest_name) {
                Some(checksum) => checksum,
                None => {
                    failures.push(format!("{checksum_url}: checksum format is invalid"));
                    continue;
                }
            };
            let actual_checksum = checksum(&manifest);
            if expected_checksum != actual_checksum {
                failures.push(format!("{manifest_url}: checksum does not match manifest"));
                continue;
            }

            match String::from_utf8(manifest) {
                Ok(manifest) => return Ok(manifest),
                Err(_) => {
                    failures.push(format!("{manifest_url}: manifest is not UTF-8"));
                }
            }
        }

        Err(UpstreamError::AllUpstreamsFailed(failures.join("; ")))
    }

    pub async fn fetch_dist_artifact(&self, path: &DistPath) -> Result<Vec<u8>, UpstreamError> {
        let mut failures = Vec::new();
        for base in &self.bases {
            let url = dist_url(base, path.as_str());
            match self.fetch_success_bytes(&url).await {
                Ok(contents) => return Ok(contents),
                Err(error) => failures.push(format!("{url}: {error}")),
            }
        }

        Err(UpstreamError::AllUpstreamsFailed(failures.join("; ")))
    }

    async fn fetch_success_bytes(&self, url: &Url) -> Result<Vec<u8>, String> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("received HTTP {}", response.status()));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| error.to_string())
    }
}

fn dist_url(base: &Url, relative_path: &str) -> Url {
    let mut url = base.clone();
    let base_path = base.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/dist/{relative_path}"));
    url.set_query(None);
    url.set_fragment(None);
    url
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

fn checksum(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn is_valid_channel(channel: &str) -> bool {
    !channel.is_empty()
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}
