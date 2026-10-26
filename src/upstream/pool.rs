use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::http::{HeaderMap, Method};
use thiserror::Error;
use tokio::sync::OwnedSemaphorePermit;
use url::Url;

use crate::{
    SourceConfig, SourceGraph,
    upstream::{
        GroupRun,
        budget::{ArtifactBudget, FetchTimeouts},
        fetch::policy::{is_public_host, same_origin},
        fetch::{FetchedFile, HttpFetcher},
        fetch_error_is_group_fallback,
    },
};

#[derive(Debug)]
pub struct SourcePool {
    ecosystem: String,
    graph: SourceGraph,
    fetcher: Arc<HttpFetcher>,
    artifact_budget: ArtifactBudget,
    metadata_timeout: Duration,
    artifact_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchOperation {
    Metadata,
    Artifact,
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

    pub fn new_with_proxy(
        ecosystem: impl Into<String>,
        graph: SourceGraph,
        proxy_url: Option<&Url>,
        proxy_no_proxy: Option<&str>,
    ) -> Result<Self, SourcePoolError> {
        Self::with_fetcher(
            ecosystem,
            graph,
            HttpFetcher::new_with_proxy(proxy_url, proxy_no_proxy),
        )
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
        fetcher: Result<HttpFetcher, crate::upstream::fetch::FetchError>,
    ) -> Result<Self, SourcePoolError> {
        Ok(Self {
            ecosystem: ecosystem.into(),
            graph,
            fetcher: Arc::new(
                fetcher.map_err(|error| SourcePoolError::FetcherBuild(error.to_string()))?,
            ),
            artifact_budget: ArtifactBudget::default(),
            metadata_timeout: Duration::from_secs(crate::config::DEFAULT_METADATA_TIMEOUT_SECS),
            artifact_timeout: Duration::from_secs(crate::config::DEFAULT_ARTIFACT_TIMEOUT_SECS),
        })
    }

    pub fn with_artifact_budget(mut self, budget: ArtifactBudget) -> Self {
        self.artifact_budget = budget;
        self
    }

    pub fn with_fetch_timeouts(mut self, timeouts: FetchTimeouts) -> Self {
        self.metadata_timeout = timeouts.metadata;
        self.artifact_timeout = timeouts.artifact;
        self
    }

    fn timeout(&self, operation: FetchOperation) -> Duration {
        match operation {
            FetchOperation::Metadata => self.metadata_timeout,
            FetchOperation::Artifact => self.artifact_timeout,
        }
    }

    async fn artifact_permit(&self, operation: FetchOperation) -> Option<OwnedSemaphorePermit> {
        if operation != FetchOperation::Artifact {
            return None;
        }
        Some(self.artifact_budget.acquire().await)
    }

    pub fn ecosystem(&self) -> &str {
        &self.ecosystem
    }

    pub fn proxy_members(&self, entry_name: &str) -> Option<Vec<SourceConfig>> {
        self.graph
            .proxy_members(entry_name)
            .map(|members| members.into_iter().cloned().collect())
    }

    pub fn source_fingerprint(&self, entry_name: &str, source_name: &str) -> Option<String> {
        self.graph
            .proxy_members(entry_name)?
            .into_iter()
            .find(|source| source.name == source_name)
            .map(SourceConfig::fingerprint)
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
        let _artifact_permit = self.artifact_permit(operation).await;
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
            .fetch_with_headers(url, headers, self.timeout(operation))
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

    pub(crate) async fn fetch_proxy_path_to_file(
        &self,
        source_name: &str,
        request_key: &str,
        operation: FetchOperation,
        path: PathBuf,
    ) -> Result<FetchedFile, SourcePoolError> {
        let _artifact_permit = self.artifact_permit(operation).await;
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
            .fetch_safe_redirects_to_file(
                url.clone(),
                source.require_https,
                true,
                self.timeout(operation),
                path,
            )
            .await
        {
            Ok(file) => Ok(file),
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

    pub async fn fetch_proxy_registry_path(
        &self,
        source_name: &str,
        request_key: &str,
        method: Method,
        headers: HeaderMap,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        self.fetch_proxy_registry_path_with_query(
            source_name,
            request_key,
            None,
            method,
            headers,
            operation,
        )
        .await
    }

    pub async fn fetch_proxy_registry_path_with_query(
        &self,
        source_name: &str,
        request_key: &str,
        query: Option<&str>,
        method: Method,
        headers: HeaderMap,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let _artifact_permit = self.artifact_permit(operation).await;
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy {
            return Err(SourcePoolError::UnknownEntry(source_name.to_owned()));
        }
        let request_key = normalize_proxy_path(request_key)?;
        let mut url = source_url_raw(source, &request_key)?;
        if let Some(query) = query {
            if query.contains('#') || query.chars().any(char::is_control) {
                return Err(SourcePoolError::InvalidRequestKey(query.to_owned()));
            }
            url.set_query(Some(query));
        }
        match self
            .fetcher
            .fetch_registry(
                method,
                url,
                headers,
                source.require_https,
                self.timeout(operation),
            )
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

    pub(crate) async fn post_json_to_source(
        &self,
        source_name: &str,
        request_key: &str,
        body: &[u8],
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
            .post_marketplace_query(url, body, self.timeout(FetchOperation::Metadata))
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

    pub(crate) async fn fetch_proxy_registry_path_to_file(
        &self,
        source_name: &str,
        request_key: &str,
        method: Method,
        headers: HeaderMap,
        operation: FetchOperation,
        path: PathBuf,
    ) -> Result<FetchedFile, SourcePoolError> {
        let _artifact_permit = self.artifact_permit(operation).await;
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
            .fetch_registry_to_file(
                method,
                url,
                headers,
                source.require_https,
                self.timeout(operation),
                path,
            )
            .await
        {
            Ok(file) => Ok(file),
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

    async fn fetch_request(
        &self,
        entry_name: &str,
        request_key: &str,
        operation: FetchOperation,
        request: SourceRequest<'_>,
    ) -> Result<SourceResponse, SourcePoolError> {
        let _artifact_permit = self.artifact_permit(operation).await;
        let mut run = GroupRun::new(self, entry_name, None)?;
        let is_group = run.is_group();
        let request_key = normalize_request_key(request_key)?;

        while let Some(source) = run.next_member() {
            let url = source_url(&source, &request_key)?;
            let result = match request {
                SourceRequest::Get => self.fetcher.fetch(url, self.timeout(operation)).await,
                SourceRequest::MarketplaceQuery(body) => {
                    self.fetcher
                        .post_marketplace_query(url, body, self.timeout(operation))
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
                Err(error) if is_group && fetch_error_is_group_fallback(&error) => {
                    run.record_failure(
                        &source.name,
                        &error.to_string(),
                        error.status_code() == Some(404),
                    );
                }
                Err(error) if error.is_retryable() => {
                    run.record_failure(&source.name, &error.to_string(), false);
                }
                Err(error) => {
                    let status = error.status_code().unwrap_or_default();
                    return Err(SourcePoolError::TerminalSourceError {
                        source_name: source.name,
                        status,
                        error: error.to_string(),
                    });
                }
            }
        }

        Err(run.all_sources_failed(request_key))
    }

    pub async fn fetch_configured_url(
        &self,
        source_name: &str,
        url: Url,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let _artifact_permit = self.artifact_permit(operation).await;
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy || !valid_configured_url(source, &url) {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }

        let allow_private = source
            .base_url
            .as_ref()
            .is_some_and(|base_url| same_origin(base_url, &url));
        let result = if allow_private {
            self.fetcher
                .fetch(url.clone(), self.timeout(operation))
                .await
        } else {
            self.fetcher
                .fetch_public(url.clone(), self.timeout(operation))
                .await
        };
        match result {
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
        let allow_private = source
            .base_url
            .as_ref()
            .is_some_and(|base_url| same_origin(base_url, &url));
        self.fetch_safe_redirects(
            source_name,
            url,
            source.require_https,
            allow_private,
            operation,
        )
        .await
    }

    pub(crate) async fn fetch_configured_url_following_safe_redirects_to_file(
        &self,
        source_name: &str,
        url: Url,
        operation: FetchOperation,
        path: PathBuf,
    ) -> Result<FetchedFile, SourcePoolError> {
        let _artifact_permit = self.artifact_permit(operation).await;
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy || !valid_configured_url(source, &url) {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }
        let allow_private = source
            .base_url
            .as_ref()
            .is_some_and(|base_url| same_origin(base_url, &url));
        match self
            .fetcher
            .fetch_safe_redirects_to_file(
                url.clone(),
                source.require_https,
                allow_private,
                self.timeout(operation),
                path,
            )
            .await
        {
            Ok(file) => Ok(file),
            Err(error) if error.is_retryable() => Err(SourcePoolError::AllSourcesFailed {
                entry_name: source_name.to_owned(),
                request_key: url.path().to_owned(),
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

    pub async fn fetch_discovered_url(
        &self,
        source_name: &str,
        url: Url,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let _artifact_permit = self.artifact_permit(operation).await;
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy || !valid_discovered_url(source, &url) {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }

        let allow_private = source
            .base_url
            .as_ref()
            .is_some_and(|base_url| same_origin(base_url, &url));
        let result = if allow_private {
            self.fetcher
                .fetch(url.clone(), self.timeout(operation))
                .await
        } else {
            self.fetcher
                .fetch_public(url.clone(), self.timeout(operation))
                .await
        };
        match result {
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
        let allow_private = source
            .base_url
            .as_ref()
            .is_some_and(|base_url| same_origin(base_url, &url));
        self.fetch_safe_redirects(
            source_name,
            url,
            source.require_https,
            allow_private,
            operation,
        )
        .await
    }

    pub(crate) async fn fetch_discovered_url_following_safe_redirects_to_file(
        &self,
        source_name: &str,
        url: Url,
        operation: FetchOperation,
        path: PathBuf,
    ) -> Result<FetchedFile, SourcePoolError> {
        let _artifact_permit = self.artifact_permit(operation).await;
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy || !valid_discovered_url(source, &url) {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }
        let allow_private = source
            .base_url
            .as_ref()
            .is_some_and(|base_url| same_origin(base_url, &url));
        match self
            .fetcher
            .fetch_safe_redirects_to_file(
                url.clone(),
                source.require_https,
                allow_private,
                self.timeout(operation),
                path,
            )
            .await
        {
            Ok(file) => Ok(file),
            Err(error) if error.is_retryable() => Err(SourcePoolError::AllSourcesFailed {
                entry_name: source_name.to_owned(),
                request_key: url.path().to_owned(),
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

    pub async fn fetch_discovered_url_with_query_following_safe_redirects(
        &self,
        source_name: &str,
        url: Url,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let source = self
            .graph
            .get(source_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(source_name.to_owned()))?;
        if source.source_type != crate::SourceType::Proxy
            || !valid_discovered_url_with_query(source, &url)
        {
            return Err(SourcePoolError::InvalidConfiguredUrl(url.to_string()));
        }
        let allow_private = source
            .base_url
            .as_ref()
            .is_some_and(|base_url| same_origin(base_url, &url));
        self.fetch_safe_redirects(
            source_name,
            url,
            source.require_https,
            allow_private,
            operation,
        )
        .await
    }

    async fn fetch_safe_redirects(
        &self,
        source_name: &str,
        url: Url,
        require_https: bool,
        allow_private_initial: bool,
        operation: FetchOperation,
    ) -> Result<SourceResponse, SourcePoolError> {
        let _artifact_permit = self.artifact_permit(operation).await;
        match self
            .fetcher
            .fetch_safe_redirects(
                url.clone(),
                require_https,
                allow_private_initial,
                self.timeout(operation),
            )
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
    let base = base_url.as_str().trim_end_matches('/');
    Url::parse(&format!("{base}/{request_key}"))
        .map_err(|_| SourcePoolError::InvalidRequestKey(request_key.to_owned()))
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
    base_url.scheme() == url.scheme()
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
        && (!source.require_https || url.scheme() == "https")
}

fn valid_discovered_url_with_query(source: &SourceConfig, url: &Url) -> bool {
    let Some(base_url) = source.base_url.as_ref() else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (source.require_https && url.scheme() != "https")
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
    (base_url.scheme() == url.scheme()
        && base_url.host_str() == url.host_str()
        && base_url.port_or_known_default() == url.port_or_known_default())
        || {
            let mut url_without_query = url.clone();
            url_without_query.set_query(None);
            is_safe_configured_url(&url_without_query)
        }
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

fn path_is_within(base_path: &str, candidate_path: &str) -> bool {
    let base_path = base_path.trim_end_matches('/');
    base_path.is_empty()
        || candidate_path == base_path
        || candidate_path.starts_with(&format!("{base_path}/"))
}

#[cfg(test)]
mod tests {
    use super::{ArtifactBudget, FetchOperation, SourcePool};
    use crate::{SourceConfig, SourceGraph, SourceType};
    use url::Url;

    fn pool(name: &str, budget: ArtifactBudget) -> SourcePool {
        SourcePool::new(
            name,
            SourceGraph::new(vec![SourceConfig {
                name: "official".to_owned(),
                source_type: SourceType::Proxy,
                base_url: Some(Url::parse("http://127.0.0.1:9").expect("base URL")),
                require_https: false,
                members: vec![],
            }])
            .expect("source graph"),
        )
        .expect("source pool")
        .with_artifact_budget(budget)
    }

    #[tokio::test]
    async fn artifact_budget_is_shared_across_pools() {
        let budget = ArtifactBudget::new(1);
        let first = pool("first", budget.clone());
        let second = pool("second", budget);

        let permit = first
            .artifact_permit(FetchOperation::Artifact)
            .await
            .expect("artifact permit");
        assert_eq!(second.artifact_budget.available_permits(), 0);
        drop(permit);
        assert_eq!(second.artifact_budget.available_permits(), 1);
    }

    #[tokio::test]
    async fn metadata_does_not_consume_the_artifact_budget() {
        let budget = ArtifactBudget::new(1);
        let pool = pool("metadata", budget);

        assert!(
            pool.artifact_permit(FetchOperation::Metadata)
                .await
                .is_none()
        );
        assert_eq!(pool.artifact_budget.available_permits(), 1);
    }
}
