use std::{net::SocketAddr, time::Duration};

use reqwest::redirect::Policy;
use thiserror::Error;
use url::Url;

const USER_AGENT: &str = concat!("Stacktory/", env!("CARGO_PKG_VERSION"));
const MARKETPLACE_ACCEPT: &str = "application/json;api-version=3.0-preview.1";

#[derive(Debug)]
pub(crate) struct FetchResponse {
    pub(crate) body: Vec<u8>,
    pub(crate) content_type: Option<String>,
}

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
        Self::build_client(reqwest::Client::builder())
    }

    pub(crate) fn new_with_resolver(host: &str, address: SocketAddr) -> Result<Self, FetchError> {
        Self::new_with_resolvers(&[(host, address)])
    }

    pub(crate) fn new_with_resolvers(resolvers: &[(&str, SocketAddr)]) -> Result<Self, FetchError> {
        let mut builder = reqwest::Client::builder();
        for (host, address) in resolvers {
            builder = builder.resolve(host, *address);
        }
        Self::build_client(builder)
    }

    fn build_client(builder: reqwest::ClientBuilder) -> Result<Self, FetchError> {
        let client = builder
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build()?;
        Ok(Self { client })
    }

    pub(crate) async fn fetch(
        &self,
        url: Url,
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        let response = self.client.get(url.clone()).timeout(timeout).send().await;
        self.response_bytes(url, response).await
    }

    pub(crate) async fn post_marketplace_query(
        &self,
        url: Url,
        body: &[u8],
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        let response = self
            .client
            .post(url.clone())
            .timeout(timeout)
            .header(reqwest::header::ACCEPT, MARKETPLACE_ACCEPT)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("X-Market-Client-Id", "VSCode")
            .body(body.to_vec())
            .send()
            .await;
        self.response_bytes(url, response).await
    }

    async fn response_bytes(
        &self,
        url: Url,
        response: Result<reqwest::Response, reqwest::Error>,
    ) -> Result<FetchResponse, FetchError> {
        let response = response.map_err(|error| FetchError::Request(url.clone(), error))?;
        if !response.status().is_success() {
            return Err(FetchError::HttpStatus(url, response.status()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        response
            .bytes()
            .await
            .map(|bytes| FetchResponse {
                body: bytes.to_vec(),
                content_type,
            })
            .map_err(|error| FetchError::ResponseBody(url, error))
    }
}

impl FetchError {
    pub(crate) fn status_code(&self) -> Option<u16> {
        match self {
            Self::HttpStatus(_, status) => Some(status.as_u16()),
            _ => None,
        }
    }

    pub(crate) fn is_retryable(&self) -> bool {
        match self {
            Self::ClientBuild(_) | Self::Request(_, _) | Self::ResponseBody(_, _) => true,
            Self::HttpStatus(_, status) => status.is_redirection() || status.is_server_error(),
        }
    }
}
