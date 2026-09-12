use std::{net::SocketAddr, time::Duration};

use reqwest::redirect::Policy;
use reqwest::{
    Method,
    header::{HeaderMap, WWW_AUTHENTICATE},
};
use serde_json::Value;
use thiserror::Error;
use url::Url;

const USER_AGENT: &str = concat!("Stacktory/", env!("CARGO_PKG_VERSION"));
const MARKETPLACE_ACCEPT: &str = "application/json;api-version=3.0-preview.1";

#[derive(Debug)]
pub(crate) struct FetchResponse {
    pub(crate) body: Vec<u8>,
    pub(crate) content_type: Option<String>,
    pub(crate) headers: HeaderMap,
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
    #[error("upstream Registry authentication challenge from `{0}` is invalid")]
    InvalidRegistryChallenge(Url),
    #[error("upstream Registry token response from `{0}` is invalid")]
    InvalidRegistryToken(Url),
    #[error("upstream Registry redirect from `{0}` is unsafe")]
    UnsafeRegistryRedirect(Url),
    #[error("upstream Registry redirect chain from `{0}` is too long")]
    RegistryRedirectLimit(Url),
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
        self.fetch_with_headers(url, HeaderMap::new(), timeout)
            .await
    }

    pub(crate) async fn fetch_with_headers(
        &self,
        url: Url,
        headers: HeaderMap,
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        let response = self
            .client
            .get(url.clone())
            .headers(headers)
            .timeout(timeout)
            .send()
            .await;
        self.response_bytes(url, response).await
    }

    pub(crate) async fn fetch_registry(
        &self,
        method: Method,
        url: Url,
        headers: HeaderMap,
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        let original_url = url.clone();
        let mut current_url = url;
        let mut authorization = None;

        for _ in 0..=3 {
            let mut request = self
                .client
                .request(method.clone(), current_url.clone())
                .headers(headers.clone())
                .timeout(timeout);
            if let Some(authorization) = &authorization {
                request = request.header(reqwest::header::AUTHORIZATION, authorization);
            }
            let response = request
                .send()
                .await
                .map_err(|error| FetchError::Request(current_url.clone(), error))?;

            if response.status() == reqwest::StatusCode::UNAUTHORIZED && authorization.is_none() {
                let challenge = response
                    .headers()
                    .get(WWW_AUTHENTICATE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(parse_bearer_challenge)
                    .ok_or_else(|| FetchError::InvalidRegistryChallenge(current_url.clone()))?;
                let token = self
                    .fetch_anonymous_registry_token(&original_url, challenge, timeout)
                    .await?;
                authorization = Some(format!("Bearer {token}"));
                continue;
            }

            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| FetchError::UnsafeRegistryRedirect(current_url.clone()))?;
                let next_url = current_url
                    .join(location)
                    .map_err(|_| FetchError::UnsafeRegistryRedirect(current_url.clone()))?;
                if !is_safe_registry_redirect(&original_url, &next_url) {
                    return Err(FetchError::UnsafeRegistryRedirect(next_url));
                }
                if !same_origin(&current_url, &next_url) {
                    authorization = None;
                }
                current_url = next_url;
                continue;
            }

            return self.response_bytes(current_url, Ok(response)).await;
        }

        Err(FetchError::RegistryRedirectLimit(current_url))
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
        let headers = response.headers().clone();
        response
            .bytes()
            .await
            .map(|bytes| FetchResponse {
                body: bytes.to_vec(),
                content_type,
                headers,
            })
            .map_err(|error| FetchError::ResponseBody(url, error))
    }

    async fn fetch_anonymous_registry_token(
        &self,
        source_url: &Url,
        challenge: BearerChallenge,
        timeout: Duration,
    ) -> Result<String, FetchError> {
        if !is_safe_registry_token_url(source_url, &challenge.realm) {
            return Err(FetchError::InvalidRegistryChallenge(source_url.clone()));
        }
        let mut token_url = challenge.realm;
        {
            let mut query = token_url.query_pairs_mut();
            if let Some(service) = challenge.service {
                query.append_pair("service", &service);
            }
            if let Some(scope) = challenge.scope {
                query.append_pair("scope", &scope);
            }
        }
        let response = self
            .client
            .get(token_url.clone())
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| FetchError::Request(token_url.clone(), error))?;
        if !response.status().is_success() {
            return Err(FetchError::HttpStatus(token_url, response.status()));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| FetchError::ResponseBody(token_url.clone(), error))?;
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| FetchError::InvalidRegistryToken(token_url.clone()))?;
        value
            .get("token")
            .or_else(|| value.get("access_token"))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .ok_or(FetchError::InvalidRegistryToken(token_url))
    }
}

#[derive(Debug)]
struct BearerChallenge {
    realm: Url,
    service: Option<String>,
    scope: Option<String>,
}

fn parse_bearer_challenge(value: &str) -> Option<BearerChallenge> {
    let mut fields = value.trim().splitn(2, ' ');
    if !fields.next()?.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let parameters = fields.next()?;
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for parameter in parameters.split(',') {
        let (key, value) = parameter.trim().split_once('=')?;
        let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        match key {
            "realm" => realm = Url::parse(value).ok(),
            "service" => service = Some(value.to_owned()),
            "scope" => scope = Some(value.to_owned()),
            _ => {}
        }
    }
    Some(BearerChallenge {
        realm: realm?,
        service,
        scope,
    })
}

fn is_safe_registry_token_url(source_url: &Url, token_url: &Url) -> bool {
    matches!(token_url.scheme(), "http" | "https")
        && token_url.host_str().is_some()
        && token_url.username().is_empty()
        && token_url.password().is_none()
        && token_url.fragment().is_none()
        && (same_origin(source_url, token_url) || is_public_host(token_url.host_str().unwrap()))
}

fn is_safe_registry_redirect(source_url: &Url, redirect_url: &Url) -> bool {
    matches!(redirect_url.scheme(), "http" | "https")
        && redirect_url.host_str().is_some()
        && redirect_url.username().is_empty()
        && redirect_url.password().is_none()
        && redirect_url.fragment().is_none()
        && !redirect_url
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
        && (same_origin(source_url, redirect_url)
            || is_public_host(redirect_url.host_str().unwrap()))
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_public_host(host: &str) -> bool {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".lan")
        || host.ends_with(".home.arpa")
    {
        return false;
    }
    host.parse::<std::net::IpAddr>()
        .map(|address| match address {
            std::net::IpAddr::V4(address) => {
                let [first, second, ..] = address.octets();
                first == 0
                    || first == 10
                    || first == 127
                    || (first == 169 && second == 254)
                    || (first == 172 && (16..=31).contains(&second))
                    || (first == 192 && second == 168)
            }
            std::net::IpAddr::V6(address) => {
                address.is_loopback()
                    || address.is_unspecified()
                    || (address.segments()[0] & 0xfe00) == 0xfc00
                    || (address.segments()[0] & 0xffc0) == 0xfe80
            }
        })
        .map_or(true, |private| !private)
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
            Self::ClientBuild(_)
            | Self::Request(_, _)
            | Self::ResponseBody(..)
            | Self::InvalidRegistryChallenge(_)
            | Self::InvalidRegistryToken(_)
            | Self::UnsafeRegistryRedirect(_)
            | Self::RegistryRedirectLimit(_) => true,
            Self::HttpStatus(_, status) => status.is_redirection() || status.is_server_error(),
        }
    }
}
