use std::time::Duration;

use reqwest::redirect::Policy;
use thiserror::Error;
use url::Url;

#[derive(Debug)]
pub(crate) struct HttpFetcher {
    client: reqwest::Client,
}

#[derive(Debug, Error)]
pub(crate) enum FetchError {
    #[error("HTTP client cannot be created: {0}")]
    ClientBuild(#[from] reqwest::Error),
    #[error("request to `{0}` failed: {1}")]
    Request(Url, reqwest::Error),
    #[error("upstream `{0}` returned HTTP {1}")]
    HttpStatus(Url, reqwest::StatusCode),
    #[error("response from `{0}` could not be read: {1}")]
    ResponseBody(Url, reqwest::Error),
}

impl HttpFetcher {
    pub(crate) fn new() -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { client })
    }

    pub(crate) async fn fetch(&self, url: Url) -> Result<Vec<u8>, FetchError> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| FetchError::Request(url.clone(), error))?;
        if !response.status().is_success() {
            return Err(FetchError::HttpStatus(url, response.status()));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| FetchError::ResponseBody(url, error))
    }
}

impl FetchError {
    pub(crate) fn is_retryable(&self) -> bool {
        match self {
            Self::ClientBuild(_) | Self::Request(_, _) | Self::ResponseBody(_, _) => true,
            Self::HttpStatus(_, status) => status.is_redirection() || status.is_server_error(),
        }
    }
}
