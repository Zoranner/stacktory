use std::{future::Future, sync::Arc};

use thiserror::Error;
use url::Url;

use crate::{
    SourceConfig, SourceGraph,
    sources::fetcher::{FetchError, HttpFetcher},
};

#[derive(Debug)]
pub struct SourcePool {
    graph: SourceGraph,
    fetcher: Arc<HttpFetcher>,
}

#[derive(Debug)]
pub struct SourceResponse {
    pub source_name: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum SourcePoolError {
    #[error("source entry `{0}` was not found")]
    UnknownEntry(String),
    #[error("request key `{0}` is invalid")]
    InvalidRequestKey(String),
    #[error("all sources for entry `{entry_name}` failed for `{request_key}`: {failures}")]
    AllSourcesFailed {
        entry_name: String,
        request_key: String,
        failures: String,
    },
}

impl SourcePool {
    pub fn new(graph: SourceGraph) -> Result<Self, FetchError> {
        Ok(Self {
            graph,
            fetcher: Arc::new(HttpFetcher::new()?),
        })
    }

    pub fn fetcher(&self) -> Arc<HttpFetcher> {
        self.fetcher.clone()
    }

    pub fn proxy_members(&self, entry_name: &str) -> Option<Vec<SourceConfig>> {
        self.graph
            .proxy_members(entry_name)
            .map(|members| members.into_iter().cloned().collect())
    }

    pub async fn fetch<F, Fut>(
        &self,
        entry_name: &str,
        request_key: &str,
        operation: F,
    ) -> Result<SourceResponse, SourcePoolError>
    where
        F: Fn(Url) -> Fut,
        Fut: Future<Output = Result<Vec<u8>, FetchError>>,
    {
        let members = self
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
        let request_key = normalize_request_key(request_key)?;
        let mut failures = Vec::new();

        for source in members {
            let url = source_url(&source, &request_key)?;
            match operation(url).await {
                Ok(body) => {
                    return Ok(SourceResponse {
                        source_name: source.name,
                        body,
                    });
                }
                Err(error) => failures.push(format!("{}: {error}", source.name)),
            }
        }

        Err(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key,
            failures: failures.join("; "),
        })
    }
}

fn normalize_request_key(request_key: &str) -> Result<String, SourcePoolError> {
    let request_key = request_key.trim_start_matches('/');
    if request_key.is_empty()
        || request_key.contains('?')
        || request_key.contains('#')
        || request_key.split('/').any(|segment| segment == "..")
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
