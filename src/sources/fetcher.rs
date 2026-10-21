use std::{io, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;
use reqwest::{
    Method,
    header::{HeaderMap, WWW_AUTHENTICATE},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{fs::File, io::AsyncWriteExt};
use url::Url;

use super::policy::{is_private_or_local_address, is_public_host, same_origin};

const USER_AGENT: &str = concat!("Stacktory/", env!("CARGO_PKG_VERSION"));
const MARKETPLACE_ACCEPT: &str = "application/json;api-version=3.0-preview.1";

#[derive(Debug)]
pub(crate) struct FetchResponse {
    pub(crate) body: Vec<u8>,
    pub(crate) content_type: Option<String>,
    pub(crate) headers: HeaderMap,
}

#[derive(Debug)]
pub(crate) struct FetchedFile {
    pub(crate) path: PathBuf,
    pub(crate) length: u64,
    pub(crate) sha256: String,
}

#[derive(Debug)]
pub(crate) struct HttpFetcher {
    client: reqwest::Client,
    public_client: reqwest::Client,
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

struct PublicResolver {
    allowed_proxy_host: Option<String>,
}

impl PublicResolver {
    fn new(allowed_proxy_host: Option<String>) -> Self {
        Self { allowed_proxy_host }
    }
}

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let allowed_proxy_host = self.allowed_proxy_host.clone();
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| -> BoxError { Box::new(error) })?;
            let allow_private = allowed_proxy_host.as_deref() == Some(host.as_str());
            let allowed = addresses
                .filter(|address| allow_private || !is_private_or_local_address(address.ip()))
                .collect::<Vec<_>>();
            if allowed.is_empty() {
                return Err(Box::new(std::io::Error::other(
                    "host resolved only to private or local addresses",
                )) as BoxError);
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
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
    #[error("temporary file write failed: {0}")]
    TempFile(io::Error),
    #[error("upstream Registry authentication challenge from `{0}` is invalid")]
    InvalidRegistryChallenge(Url),
    #[error("upstream Registry token response from `{0}` is invalid")]
    InvalidRegistryToken(Url),
    #[error("upstream Registry redirect from `{0}` is unsafe")]
    UnsafeRegistryRedirect(Url),
    #[error("upstream Registry redirect chain from `{0}` is too long")]
    RegistryRedirectLimit(Url),
    #[error("outbound proxy cannot be configured: {0}")]
    InvalidProxy(reqwest::Error),
}

impl HttpFetcher {
    pub(crate) fn new() -> Result<Self, FetchError> {
        Self::build_clients(&[], None, None)
    }

    pub(crate) fn new_with_proxy(
        proxy_url: Option<&Url>,
        proxy_no_proxy: Option<&str>,
    ) -> Result<Self, FetchError> {
        Self::build_clients(&[], proxy_url, proxy_no_proxy)
    }

    pub(crate) fn new_with_resolver(host: &str, address: SocketAddr) -> Result<Self, FetchError> {
        Self::new_with_resolvers(&[(host, address)])
    }

    pub(crate) fn new_with_resolvers(resolvers: &[(&str, SocketAddr)]) -> Result<Self, FetchError> {
        Self::build_clients(resolvers, None, None)
    }

    fn build_clients(
        resolvers: &[(&str, SocketAddr)],
        proxy_url: Option<&Url>,
        proxy_no_proxy: Option<&str>,
    ) -> Result<Self, FetchError> {
        Ok(Self {
            client: Self::build_client(resolvers, false, proxy_url, proxy_no_proxy)?,
            public_client: Self::build_client(resolvers, true, proxy_url, proxy_no_proxy)?,
        })
    }

    fn build_client(
        resolvers: &[(&str, SocketAddr)],
        public: bool,
        proxy_url: Option<&Url>,
        proxy_no_proxy: Option<&str>,
    ) -> Result<reqwest::Client, FetchError> {
        let mut builder = reqwest::Client::builder();
        for (host, address) in resolvers {
            builder = builder.resolve(host, *address);
        }
        if let Some(proxy_url) = proxy_url {
            let mut proxy =
                reqwest::Proxy::all(proxy_url.as_str()).map_err(FetchError::InvalidProxy)?;
            let no_proxy = proxy_no_proxy
                .and_then(reqwest::NoProxy::from_string)
                .or_else(reqwest::NoProxy::from_env);
            if let Some(no_proxy) = no_proxy {
                proxy = proxy.no_proxy(Some(no_proxy));
            }
            builder = builder.proxy(proxy);
        }
        if public {
            builder = builder.dns_resolver(Arc::new(PublicResolver::new(
                proxy_url
                    .and_then(|url| url.host_str())
                    .map(|host| host.to_ascii_lowercase()),
            )));
        }
        Ok(builder
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build()?)
    }

    fn client_for(&self, allow_private: bool) -> &reqwest::Client {
        if allow_private {
            &self.client
        } else {
            &self.public_client
        }
    }

    pub(crate) async fn fetch(
        &self,
        url: Url,
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        self.fetch_with_policy(url, HeaderMap::new(), true, timeout)
            .await
    }

    pub(crate) async fn fetch_public(
        &self,
        url: Url,
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        self.fetch_with_policy(url, HeaderMap::new(), false, timeout)
            .await
    }

    pub(crate) async fn fetch_with_headers(
        &self,
        url: Url,
        headers: HeaderMap,
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        self.fetch_with_policy(url, headers, true, timeout).await
    }

    async fn fetch_with_policy(
        &self,
        url: Url,
        headers: HeaderMap,
        allow_private: bool,
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        let response = self
            .client_for(allow_private)
            .get(url.clone())
            .headers(headers)
            .timeout(timeout)
            .send()
            .await;
        self.response_bytes(url, response).await
    }

    pub(crate) async fn fetch_safe_redirects(
        &self,
        url: Url,
        require_https: bool,
        allow_private_initial: bool,
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        let (url, response) = self
            .safe_redirect_response(url, require_https, allow_private_initial, timeout)
            .await?;
        self.response_bytes(url, Ok(response)).await
    }

    pub(crate) async fn fetch_safe_redirects_to_file(
        &self,
        url: Url,
        require_https: bool,
        allow_private_initial: bool,
        timeout: Duration,
        path: PathBuf,
    ) -> Result<FetchedFile, FetchError> {
        let (url, response) = self
            .safe_redirect_response(url, require_https, allow_private_initial, timeout)
            .await?;
        let cleanup_path = path.clone();
        let result = self.response_file(url, response, path).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&cleanup_path).await;
        }
        result
    }

    async fn safe_redirect_response(
        &self,
        url: Url,
        require_https: bool,
        allow_private_initial: bool,
        timeout: Duration,
    ) -> Result<(Url, reqwest::Response), FetchError> {
        let original_url = url.clone();
        let mut current_url = url;
        for _ in 0..=3 {
            let allow_private = allow_private_initial && same_origin(&original_url, &current_url);
            let response = self
                .client_for(allow_private)
                .get(current_url.clone())
                .timeout(timeout)
                .send()
                .await
                .map_err(|error| FetchError::Request(current_url.clone(), error))?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| FetchError::UnsafeRegistryRedirect(current_url.clone()))?;
                let next_url = current_url
                    .join(location)
                    .map_err(|_| FetchError::UnsafeRegistryRedirect(current_url.clone()))?;
                if !is_safe_http_redirect(&original_url, &next_url, require_https) {
                    return Err(FetchError::UnsafeRegistryRedirect(next_url));
                }
                current_url = next_url;
                continue;
            }
            return Ok((current_url, response));
        }
        Err(FetchError::RegistryRedirectLimit(current_url))
    }

    pub(crate) async fn fetch_registry(
        &self,
        method: Method,
        url: Url,
        headers: HeaderMap,
        require_https: bool,
        timeout: Duration,
    ) -> Result<FetchResponse, FetchError> {
        let (url, response) = self
            .registry_response(method, url, headers, require_https, timeout)
            .await?;
        self.response_bytes(url, Ok(response)).await
    }

    pub(crate) async fn fetch_registry_to_file(
        &self,
        method: Method,
        url: Url,
        headers: HeaderMap,
        require_https: bool,
        timeout: Duration,
        path: PathBuf,
    ) -> Result<FetchedFile, FetchError> {
        let (url, response) = self
            .registry_response(method, url, headers, require_https, timeout)
            .await?;
        let cleanup_path = path.clone();
        let result = self.response_file(url, response, path).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&cleanup_path).await;
        }
        result
    }

    async fn registry_response(
        &self,
        method: Method,
        url: Url,
        headers: HeaderMap,
        require_https: bool,
        timeout: Duration,
    ) -> Result<(Url, reqwest::Response), FetchError> {
        let original_url = url.clone();
        let mut current_url = url;
        let mut authorization = None;

        for _ in 0..=3 {
            let allow_private = same_origin(&original_url, &current_url);
            let mut request = self
                .client_for(allow_private)
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
                    .fetch_anonymous_registry_token(
                        &original_url,
                        challenge,
                        require_https,
                        timeout,
                    )
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
                if !is_safe_registry_redirect(&original_url, &next_url, require_https) {
                    return Err(FetchError::UnsafeRegistryRedirect(next_url));
                }
                if !same_origin(&current_url, &next_url) {
                    authorization = None;
                }
                current_url = next_url;
                continue;
            }

            return Ok((current_url, response));
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

    async fn response_file(
        &self,
        url: Url,
        response: reqwest::Response,
        path: PathBuf,
    ) -> Result<FetchedFile, FetchError> {
        if !response.status().is_success() {
            return Err(FetchError::HttpStatus(url, response.status()));
        }
        let mut response = response;
        let mut file = File::create(&path).await.map_err(FetchError::TempFile)?;
        let mut hasher = Sha256::new();
        let mut length = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| FetchError::ResponseBody(url.clone(), error))?
        {
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(FetchError::TempFile)?;
            length += chunk.len() as u64;
        }
        file.flush().await.map_err(FetchError::TempFile)?;
        Ok(FetchedFile {
            path,
            length,
            sha256: format!("sha256:{:x}", hasher.finalize()),
        })
    }

    async fn fetch_anonymous_registry_token(
        &self,
        source_url: &Url,
        challenge: BearerChallenge,
        require_https: bool,
        timeout: Duration,
    ) -> Result<String, FetchError> {
        if !is_safe_registry_token_url(source_url, &challenge.realm, require_https) {
            return Err(FetchError::InvalidRegistryChallenge(source_url.clone()));
        }
        let allow_private = same_origin(source_url, &challenge.realm);
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
            .client_for(allow_private)
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

fn is_safe_registry_token_url(source_url: &Url, token_url: &Url, require_https: bool) -> bool {
    matches!(token_url.scheme(), "http" | "https")
        && (!require_https || token_url.scheme() == "https")
        && token_url.host_str().is_some()
        && token_url.username().is_empty()
        && token_url.password().is_none()
        && token_url.fragment().is_none()
        && (same_origin(source_url, token_url)
            || is_public_host(token_url.host_str().unwrap_or_default()))
}

fn is_safe_registry_redirect(source_url: &Url, redirect_url: &Url, require_https: bool) -> bool {
    matches!(redirect_url.scheme(), "http" | "https")
        && (!require_https || redirect_url.scheme() == "https")
        && redirect_url.host_str().is_some()
        && redirect_url.username().is_empty()
        && redirect_url.password().is_none()
        && redirect_url.fragment().is_none()
        && !redirect_url
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
        && (same_origin(source_url, redirect_url)
            || is_public_host(redirect_url.host_str().unwrap_or_default()))
}

fn is_safe_http_redirect(source_url: &Url, redirect_url: &Url, require_https: bool) -> bool {
    matches!(redirect_url.scheme(), "http" | "https")
        && (!require_https || redirect_url.scheme() == "https")
        && redirect_url.host_str().is_some()
        && redirect_url.username().is_empty()
        && redirect_url.password().is_none()
        && redirect_url.query().is_none()
        && redirect_url.fragment().is_none()
        && !redirect_url.path().contains('\\')
        && !redirect_url.path().contains('%')
        && !redirect_url.path().chars().any(char::is_control)
        && !redirect_url
            .path()
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
        && (same_origin(source_url, redirect_url)
            || is_public_host(redirect_url.host_str().unwrap_or_default()))
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
            Self::TempFile(_) | Self::InvalidProxy(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{PublicResolver, is_safe_registry_redirect, is_safe_registry_token_url};
    use reqwest::dns::Resolve;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use url::Url;

    #[test]
    fn registry_redirect_rejects_https_downgrade() {
        let source = Url::parse("https://registry.example/").expect("source");
        let target = Url::parse("http://registry.example/v2/library/image/blobs/sha256:abc")
            .expect("target");

        assert!(!is_safe_registry_redirect(&source, &target, true));
        assert!(is_safe_registry_redirect(&source, &target, false));
    }

    #[test]
    fn registry_token_url_rejects_https_downgrade() {
        let source = Url::parse("https://registry.example/").expect("source");
        let target = Url::parse("http://auth.example/token").expect("target");

        assert!(!is_safe_registry_token_url(&source, &target, true));
        assert!(is_safe_registry_token_url(&source, &target, false));
    }

    #[test]
    fn registry_redirect_allows_public_https_target() {
        let source = Url::parse("https://registry.example/").expect("source");
        let target = Url::parse("https://cdn.example/blob").expect("target");

        assert!(is_safe_registry_redirect(&source, &target, true));
    }

    #[test]
    fn registry_redirect_rejects_private_literal_target() {
        let source = Url::parse("https://registry.example/").expect("source");
        let target = Url::parse("https://127.0.0.1/blob").expect("target");

        assert!(!is_safe_registry_redirect(&source, &target, true));
    }

    #[tokio::test]
    async fn public_resolver_rejects_loopback_hostname() {
        let name = "localhost".parse().expect("DNS name");
        let result = PublicResolver::new(None).resolve(name).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn public_resolver_allows_the_configured_proxy_hostname() {
        let name = "localhost".parse().expect("DNS name");
        let result = PublicResolver::new(Some("localhost".to_owned()))
            .resolve(name)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn public_client_uses_the_configured_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy listener");
        let address = listener.local_addr().expect("proxy address");
        let proxy_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("proxy connection");
            let mut buffer = vec![0; 4096];
            let read = stream.read(&mut buffer).await.expect("proxy request");
            let request = String::from_utf8_lossy(&buffer[..read]);
            assert!(
                request.starts_with("GET http://upstream.invalid/probe HTTP/1.1"),
                "unexpected proxy request: {request}"
            );
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nproxy",
                )
                .await
                .expect("proxy response");
        });

        let proxy_url = Url::parse(&format!("http://{address}")).expect("proxy URL");
        let fetcher = super::HttpFetcher::new_with_proxy(Some(&proxy_url), Some(""))
            .expect("fetcher with proxy");
        let response = fetcher
            .fetch_public(
                Url::parse("http://upstream.invalid/probe").expect("target URL"),
                Duration::from_secs(5),
            )
            .await
            .expect("proxied response");

        assert_eq!(response.body, b"proxy");
        proxy_task.await.expect("proxy task");
    }
}
