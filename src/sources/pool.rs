use std::sync::Arc;

use thiserror::Error;
use url::Url;

use crate::{SourceConfig, SourceGraph, sources::fetcher::HttpFetcher};

#[derive(Debug)]
pub struct SourcePool {
    ecosystem: String,
    graph: SourceGraph,
    fetcher: Arc<HttpFetcher>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchOperation {
    Get,
}

#[derive(Debug)]
pub struct SourceResponse {
    pub source_name: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum SourcePoolError {
    #[error("HTTP fetcher cannot be created: {0}")]
    FetcherBuild(String),
    #[error("source entry `{0}` was not found")]
    UnknownEntry(String),
    #[error("request key `{0}` is invalid")]
    InvalidRequestKey(String),
    #[error("configured URL `{0}` is invalid")]
    InvalidConfiguredUrl(String),
    #[error("source `{source_name}` returned HTTP {status}: {error}")]
    TerminalSourceError {
        source_name: String,
        status: u16,
        error: String,
    },
    #[error("all sources for entry `{entry_name}` failed for `{request_key}`: {failures}")]
    AllSourcesFailed {
        entry_name: String,
        request_key: String,
        failures: String,
    },
}

impl SourcePool {
    pub fn new(ecosystem: impl Into<String>, graph: SourceGraph) -> Result<Self, SourcePoolError> {
        Ok(Self {
            ecosystem: ecosystem.into(),
            graph,
            fetcher: Arc::new(
                HttpFetcher::new()
                    .map_err(|error| SourcePoolError::FetcherBuild(error.to_string()))?,
            ),
        })
    }

    pub fn ecosystem(&self) -> &str {
        &self.ecosystem
    }

    pub fn proxy_members(&self, entry_name: &str) -> Option<Vec<SourceConfig>> {
        self.graph
            .proxy_members(entry_name)
            .map(|members| members.into_iter().cloned().collect())
    }

    pub async fn fetch(
        &self,
        entry_name: &str,
        request_key: &str,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let members = self
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
        let request_key = normalize_request_key(request_key)?;
        let mut failures = Vec::new();

        for source in members {
            let url = source_url(&source, &request_key)?;
            let result = match operation {
                FetchOperation::Get => self.fetcher.fetch(url).await,
            };
            match result {
                Ok(body) => {
                    return Ok(SourceResponse {
                        source_name: source.name,
                        body,
                    });
                }
                Err(error) if error.is_retryable() => {
                    failures.push(format!("{}: {error}", source.name));
                }
                Err(error) => {
                    return Err(SourcePoolError::TerminalSourceError {
                        source_name: source.name,
                        status: error.status_code().unwrap_or_default(),
                        error: error.to_string(),
                    });
                }
            }
        }

        Err(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key,
            failures: failures.join("; "),
        })
    }

    pub async fn fetch_configured_url(
        &self,
        source_name: &str,
        url: Url,
    ) -> Result<SourceResponse, SourcePoolError> {
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy || !valid_configured_url(source, &url) {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }

        match self.fetcher.fetch(url.clone()).await {
            Ok(body) => Ok(SourceResponse {
                source_name: source_name.to_owned(),
                body,
            }),
            Err(error) if error.is_retryable() => Err(SourcePoolError::AllSourcesFailed {
                entry_name: source_name.to_owned(),
                request_key: url.path().to_owned(),
                failures: error.to_string(),
            }),
            Err(error) => Err(SourcePoolError::TerminalSourceError {
                source_name: source_name.to_owned(),
                status: error.status_code().unwrap_or_default(),
                error: error.to_string(),
            }),
        }
    }
}

fn normalize_request_key(request_key: &str) -> Result<String, SourcePoolError> {
    if request_key.is_empty()
        || request_key.starts_with('/')
        || request_key.contains('\\')
        || request_key.contains('%')
        || request_key.contains('?')
        || request_key.contains('#')
        || request_key.chars().any(char::is_control)
        || request_key
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(SourcePoolError::InvalidRequestKey(request_key.to_owned()));
    }
    Ok(request_key.to_owned())
}

fn source_url(source: &SourceConfig, request_key: &str) -> Result<Url, SourcePoolError> {
    let base_url = source
        .base_url
        .as_ref()
        .expect("validated proxy source must have a base URL");
    let base_path = base_url.path().trim_end_matches('/');
    let mut url = base_url.clone();
    url.set_path(&format!("{base_path}/{request_key}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn valid_configured_url(source: &SourceConfig, url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && (!source.require_https || url.scheme() == "https")
        && !url.path().contains('\\')
        && !url.path().contains('%')
        && !url.path().chars().any(char::is_control)
        && !url
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
}
