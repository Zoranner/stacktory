use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::http::{HeaderMap, Method};
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
    Metadata,
    Artifact,
}

impl FetchOperation {
    const fn timeout(self) -> Duration {
        match self {
            Self::Metadata => Duration::from_secs(30),
            Self::Artifact => Duration::from_secs(300),
        }
    }
}

#[derive(Debug)]
pub struct SourceResponse {
    pub source_name: String,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub headers: HeaderMap,
}

#[derive(Clone, Copy)]
enum SourceRequest<'a> {
    Get,
    MarketplaceQuery(&'a [u8]),
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
        status: Option<u16>,
    },
}

impl SourcePool {
    pub fn new(ecosystem: impl Into<String>, graph: SourceGraph) -> Result<Self, SourcePoolError> {
        Self::with_fetcher(ecosystem, graph, HttpFetcher::new())
    }

    pub fn new_with_resolver(
        ecosystem: impl Into<String>,
        graph: SourceGraph,
        host: &str,
        address: SocketAddr,
    ) -> Result<Self, SourcePoolError> {
        Self::with_fetcher(
            ecosystem,
            graph,
            HttpFetcher::new_with_resolver(host, address),
        )
    }

    pub fn new_with_resolvers(
        ecosystem: impl Into<String>,
        graph: SourceGraph,
        resolvers: &[(&str, SocketAddr)],
    ) -> Result<Self, SourcePoolError> {
        Self::with_fetcher(ecosystem, graph, HttpFetcher::new_with_resolvers(resolvers))
    }

    fn with_fetcher(
        ecosystem: impl Into<String>,
        graph: SourceGraph,
        fetcher: Result<HttpFetcher, crate::sources::fetcher::FetchError>,
    ) -> Result<Self, SourcePoolError> {
        Ok(Self {
            ecosystem: ecosystem.into(),
            graph,
            fetcher: Arc::new(
                fetcher.map_err(|error| SourcePoolError::FetcherBuild(error.to_string()))?,
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

    pub fn proxies(&self) -> Vec<SourceConfig> {
        self.graph.proxies().into_iter().cloned().collect()
    }

    pub fn is_group(&self, entry_name: &str) -> bool {
        self.graph
            .get(entry_name)
            .is_some_and(|source| source.source_type == crate::SourceType::Group)
    }

    pub async fn fetch(
        &self,
        entry_name: &str,
        request_key: &str,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        self.fetch_request(entry_name, request_key, operation, SourceRequest::Get)
            .await
    }

    pub async fn fetch_proxy_path(
        &self,
        source_name: &str,
        request_key: &str,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        self.fetch_proxy_path_with_headers(source_name, request_key, HeaderMap::new(), operation)
            .await
    }

    pub async fn fetch_proxy_path_with_headers(
        &self,
        source_name: &str,
        request_key: &str,
        headers: HeaderMap,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy {
            return Err(SourcePoolError::UnknownEntry(source_name.to_owned()));
        }
        let request_key = normalize_proxy_path(request_key)?;
        let url = source_url_raw(source, &request_key)?;
        match self
            .fetcher
            .fetch_with_headers(url, headers, operation.timeout())
            .await
        {
            Ok(response) => Ok(SourceResponse {
                source_name: source_name.to_owned(),
                body: response.body,
                content_type: response.content_type,
                headers: response.headers,
            }),
            Err(error) if error.is_retryable() => Err(SourcePoolError::AllSourcesFailed {
                entry_name: source_name.to_owned(),
                request_key,
                failures: error.to_string(),
                status: None,
            }),
            Err(error) => Err(SourcePoolError::TerminalSourceError {
                source_name: source_name.to_owned(),
                status: error.status_code().unwrap_or_default(),
                error: error.to_string(),
            }),
        }
    }

    pub async fn fetch_proxy_registry_path(
        &self,
        source_name: &str,
        request_key: &str,
        method: Method,
        headers: HeaderMap,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy {
            return Err(SourcePoolError::UnknownEntry(source_name.to_owned()));
        }
        let request_key = normalize_proxy_path(request_key)?;
        let url = source_url_raw(source, &request_key)?;
        match self
            .fetcher
            .fetch_registry(method, url, headers, operation.timeout())
            .await
        {
            Ok(response) => Ok(SourceResponse {
                source_name: source_name.to_owned(),
                body: response.body,
                content_type: response.content_type,
                headers: response.headers,
            }),
            Err(error) if error.is_retryable() => Err(SourcePoolError::AllSourcesFailed {
                entry_name: source_name.to_owned(),
                request_key,
                failures: error.to_string(),
                status: error.status_code(),
            }),
            Err(error) => Err(SourcePoolError::TerminalSourceError {
                source_name: source_name.to_owned(),
                status: error.status_code().unwrap_or_default(),
                error: error.to_string(),
            }),
        }
    }

    pub async fn post_json(
        &self,
        entry_name: &str,
        request_key: &str,
        body: &[u8],
    ) -> Result<SourceResponse, SourcePoolError> {
        self.fetch_request(
            entry_name,
            request_key,
            FetchOperation::Metadata,
            SourceRequest::MarketplaceQuery(body),
        )
        .await
    }

    async fn fetch_request(
        &self,
        entry_name: &str,
        request_key: &str,
        operation: FetchOperation,
        request: SourceRequest<'_>,
    ) -> Result<SourceResponse, SourcePoolError> {
        let members = self
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?;
        let is_group = self
            .graph
            .get(entry_name)
            .is_some_and(|source| source.source_type == crate::SourceType::Group);
        let request_key = normalize_request_key(request_key)?;
        let mut failures = Vec::new();
        let mut saw_failure = false;
        let mut all_not_found = true;

        for source in members {
            let url = source_url(&source, &request_key)?;
            let result = match request {
                SourceRequest::Get => self.fetcher.fetch(url, operation.timeout()).await,
                SourceRequest::MarketplaceQuery(body) => {
                    self.fetcher
                        .post_marketplace_query(url, body, operation.timeout())
                        .await
                }
            };
            match result {
                Ok(response) => {
                    return Ok(SourceResponse {
                        source_name: source.name,
                        body: response.body,
                        content_type: response.content_type,
                        headers: response.headers,
                    });
                }
                Err(error) if error.is_retryable() => {
                    saw_failure = true;
                    all_not_found = false;
                    failures.push(format!("{}: {error}", source.name));
                }
                Err(error) => {
                    let status = error.status_code().unwrap_or_default();
                    if is_group && status == 404 {
                        saw_failure = true;
                        failures.push(format!("{}: {error}", source.name));
                        continue;
                    }
                    return Err(SourcePoolError::TerminalSourceError {
                        source_name: source.name,
                        status,
                        error: error.to_string(),
                    });
                }
            }
        }

        Err(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key,
            failures: failures.join("; "),
            status: (is_group && saw_failure && all_not_found).then_some(404),
        })
    }

    pub async fn fetch_configured_url(
        &self,
        source_name: &str,
        url: Url,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy || !valid_configured_url(source, &url) {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }

        match self.fetcher.fetch(url.clone(), operation.timeout()).await {
            Ok(response) => Ok(SourceResponse {
                source_name: source_name.to_owned(),
                body: response.body,
                content_type: response.content_type,
                headers: response.headers,
            }),
            Err(error) if error.is_retryable() => Err(SourcePoolError::AllSourcesFailed {
                entry_name: source_name.to_owned(),
                request_key: url.path().to_owned(),
                failures: error.to_string(),
                status: None,
            }),
            Err(error) => Err(SourcePoolError::TerminalSourceError {
                source_name: source_name.to_owned(),
                status: error.status_code().unwrap_or_default(),
                error: error.to_string(),
            }),
        }
    }

    pub async fn fetch_configured_url_following_safe_redirects(
        &self,
        source_name: &str,
        url: Url,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy || !valid_configured_url(source, &url) {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }
        self.fetch_safe_redirects(source_name, url, source.require_https, operation)
            .await
    }

    pub async fn fetch_discovered_url(
        &self,
        source_name: &str,
        url: Url,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy || !valid_discovered_url(source, &url) {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }

        match self.fetcher.fetch(url.clone(), operation.timeout()).await {
            Ok(response) => Ok(SourceResponse {
                source_name: source_name.to_owned(),
                body: response.body,
                content_type: response.content_type,
                headers: response.headers,
            }),
            Err(error) if error.is_retryable() => Err(SourcePoolError::AllSourcesFailed {
                entry_name: source_name.to_owned(),
                request_key: url.path().to_owned(),
                failures: error.to_string(),
                status: None,
            }),
            Err(error) => Err(SourcePoolError::TerminalSourceError {
                source_name: source_name.to_owned(),
                status: error.status_code().unwrap_or_default(),
                error: error.to_string(),
            }),
        }
    }

    pub async fn fetch_discovered_url_following_safe_redirects(
        &self,
        source_name: &str,
        url: Url,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy || !valid_discovered_url(source, &url) {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }
        self.fetch_safe_redirects(source_name, url, source.require_https, operation)
            .await
    }

    async fn fetch_safe_redirects(
        &self,
        source_name: &str,
        url: Url,
        require_https: bool,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        match self
            .fetcher
            .fetch_safe_redirects(url.clone(), require_https, operation.timeout())
            .await
        {
            Ok(response) => Ok(SourceResponse {
                source_name: source_name.to_owned(),
                body: response.body,
                content_type: response.content_type,
                headers: response.headers,
            }),
            Err(error) if error.is_retryable() => Err(SourcePoolError::AllSourcesFailed {
                entry_name: source_name.to_owned(),
                request_key: url.path().to_owned(),
                failures: error.to_string(),
                status: None,
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

fn source_url_raw(source: &SourceConfig, request_key: &str) -> Result<Url, SourcePoolError> {
    let base_url = source
        .base_url
        .as_ref()
        .expect("validated proxy source must have a base URL");
    let base_path = base_url.path().trim_end_matches('/');
    let mut url = base_url.clone();
    url.set_raw_path(&format!("{base_path}/{request_key}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn normalize_proxy_path(request_key: &str) -> Result<String, SourcePoolError> {
    if request_key.is_empty()
        || request_key.starts_with('/')
        || request_key.contains('\\')
        || request_key.contains('?')
        || request_key.contains('#')
        || request_key.chars().any(char::is_control)
        || request_key
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(SourcePoolError::InvalidRequestKey(request_key.to_owned()));
    }
    let bytes = request_key.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(SourcePoolError::InvalidRequestKey(request_key.to_owned()));
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(request_key.to_owned())
}

fn valid_configured_url(source: &SourceConfig, url: &Url) -> bool {
    source.base_url.as_ref().is_some_and(|base_url| {
        is_safe_configured_url_for_source(base_url, source.require_https, url)
    })
}

fn valid_discovered_url(source: &SourceConfig, url: &Url) -> bool {
    let Some(base_url) = source.base_url.as_ref() else {
        return false;
    };
    (base_url.scheme() == url.scheme()
        && base_url.host_str() == url.host_str()
        && base_url.port_or_known_default() == url.port_or_known_default()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && !url.path().contains('\\')
        && !url.path().contains('%')
        && !url.path().chars().any(char::is_control)
        && !url
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
        && (!source.require_https || url.scheme() == "https"))
        || is_safe_configured_url(url)
}

pub(crate) fn is_safe_configured_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(is_public_host)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && !url.path().contains('\\')
        && !url.path().contains('%')
        && !url.path().chars().any(char::is_control)
        && !url
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
}

pub(crate) fn is_safe_configured_url_for_source(
    base_url: &Url,
    require_https: bool,
    url: &Url,
) -> bool {
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (require_https && url.scheme() != "https")
        || url.path().contains('\\')
        || url.path().contains('%')
        || url.path().chars().any(char::is_control)
        || url
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return false;
    }

    if same_origin(base_url, url) {
        return path_is_within(base_url.path(), url.path());
    }
    is_public_host(url.host_str().unwrap_or_default())
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn path_is_within(base_path: &str, candidate_path: &str) -> bool {
    let base_path = base_path.trim_end_matches('/');
    base_path.is_empty()
        || candidate_path == base_path
        || candidate_path.starts_with(&format!("{base_path}/"))
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
    host.parse::<IpAddr>()
        .map(|address| !is_private_or_local_address(address))
        .unwrap_or(true)
}

fn is_private_or_local_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [first, second, ..] = address.octets();
            first == 0
                || first == 10
                || first == 127
                || (first == 169 && second == 254)
                || (first == 172 && (16..=31).contains(&second))
                || (first == 192 && second == 168)
                || (first == 100 && (64..=127).contains(&second))
                || (first == 198 && (18..=19).contains(&second))
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || (address.segments()[0] & 0xfe00) == 0xfc00
                || (address.segments()[0] & 0xffc0) == 0xfe80
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| is_private_or_local_address(IpAddr::V4(mapped)))
        }
    }
}
