use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{OriginalUri, Path, State},
    http::{HeaderMap, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{any, get},
};
use tokio_util::io::ReaderStream;
use url::Url;

use crate::{
    cargo::CargoAdapter,
    rustup::DistPath,
    service::{RustupService, ServiceError},
    sources::{
        SourcePool,
        registry::{AdapterRegistry, AdapterRequest},
    },
    storage::{ArtifactFile, StorageError},
};

const LANDING_HTML: &str = include_str!("../web/index.html");
const LANDING_CSS: &str = include_str!("../web/index.css");
const LANDING_JS: &str = include_str!("../web/index.js");
const THEME_CSS: &str = include_str!("../web/theme.css");
const THREE_JS: &str = include_str!("../web/vendor/three.min.js");
const DOCS_CSS: &str = include_str!("../web/docs/index.css");
const DOCS_JS: &str = include_str!("../web/docs/index.js");

pub fn router() -> Router {
    Router::new().route("/healthz", get(health))
}

pub fn router_with_rustup(service: Arc<RustupService>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/", get(landing_page))
        .route("/theme.css", get(theme_css))
        .route("/index.css", get(landing_css))
        .route("/index.js", get(landing_js))
        .route("/vendor/three.min.js", get(three_js))
        .route("/docs", get(source_config_index_page))
        .route("/docs.md", get(source_config_index_markdown))
        .route("/docs/index.css", get(docs_css))
        .route("/docs/index.js", get(docs_js))
        .route("/docs/{stack}", get(source_config_section_page))
        .route(
            "/docs/{stack}/index.md",
            get(source_config_section_markdown),
        )
        .route("/mirror/rustup/{name}/{*path}", get(rustup_dist))
        .with_state(service)
}

pub fn router_with_registry(registry: AdapterRegistry) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .merge(registry_routes(registry, None))
}

pub fn router_with_docker_entry(
    registry: AdapterRegistry,
    entry_name: impl Into<String>,
) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .merge(registry_routes(registry, Some(entry_name.into())))
}

pub fn router_with_rustup_and_cargo(
    rustup: Arc<RustupService>,
    cargo: Option<(Arc<CargoAdapter>, Arc<SourcePool>)>,
) -> Router {
    let mut registry = AdapterRegistry::new();
    if let Some((adapter, pool)) = cargo {
        registry
            .register("cargo", pool, adapter)
            .expect("cargo adapter and source pool must share their ecosystem");
    }
    router_with_rustup_and_registry(rustup, registry)
}

pub fn router_with_rustup_and_registry(
    rustup: Arc<RustupService>,
    registry: AdapterRegistry,
) -> Router {
    router_with_rustup(rustup).merge(registry_routes(registry, None))
}

#[derive(Clone)]
struct RegistryRouteState {
    registry: AdapterRegistry,
    docker_entry: Option<String>,
}

fn registry_routes(registry: AdapterRegistry, docker_entry: Option<String>) -> Router {
    let mut router = Router::new()
        .route("/mirror/{ecosystem}/{name}/{*path}", any(named_source))
        .layer(middleware::from_fn(registry_cors));
    if docker_entry.is_some() {
        router = router
            .route("/v2/", any(docker_root_source))
            .route("/v2/{*path}", any(docker_root_source));
    }
    router.with_state(RegistryRouteState {
        registry,
        docker_entry,
    })
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(service): State<Arc<RustupService>>) -> StatusCode {
    if service.is_ready().await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn landing_page(
    State(service): State<Arc<RustupService>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let base_url = public_base_url_for_request(&headers, service.public_url());
    let home = site_href(&base_url, "/");
    let docs = site_href(&base_url, "/docs");
    let css = site_href(&base_url, "/index.css");
    let theme = site_href(&base_url, "/theme.css");
    let js = site_href(&base_url, "/index.js");
    let three = site_href(&base_url, "/vendor/three.min.js");
    let html = LANDING_HTML
        .replace("{{HOME}}", &home)
        .replace("{{DOCS}}", &docs)
        .replace("{{CSS}}", &css)
        .replace("{{THEME}}", &theme)
        .replace("{{JS}}", &js)
        .replace("{{THREE}}", &three)
        .replace("{{SERVICE_URL}}", base_url.as_str());
    html_response(html)
}

async fn landing_css() -> axum::response::Response {
    asset_response("text/css; charset=utf-8", LANDING_CSS)
}

async fn theme_css() -> axum::response::Response {
    text_response("text/css; charset=utf-8", THEME_CSS.to_owned())
}

async fn landing_js() -> axum::response::Response {
    asset_response("text/javascript; charset=utf-8", LANDING_JS)
}

async fn three_js() -> axum::response::Response {
    asset_response("text/javascript; charset=utf-8", THREE_JS)
}

async fn docs_css() -> axum::response::Response {
    text_response("text/css; charset=utf-8", DOCS_CSS.to_owned())
}

async fn docs_js() -> axum::response::Response {
    text_response("text/javascript; charset=utf-8", DOCS_JS.to_owned())
}

async fn source_config_index_page(
    State(service): State<Arc<RustupService>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let base_url = public_base_url_for_request(&headers, service.public_url());
    html_response(crate::docs::source_config::render_index_html(
        base_url.as_str(),
        service.docs_catalog(),
    ))
}

async fn source_config_section_page(
    State(service): State<Arc<RustupService>>,
    Path(stack): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(section) = crate::docs::source_config::section_by_id(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let base_url = public_base_url_for_request(&headers, service.public_url());
    html_response(crate::docs::source_config::render_section_html(
        section,
        base_url.as_str(),
        service.docs_catalog(),
    ))
}

async fn source_config_index_markdown(
    State(service): State<Arc<RustupService>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let base_url = public_base_url_for_request(&headers, service.public_url());
    text_response(
        "text/markdown; charset=utf-8",
        crate::docs::source_config::render_index_markdown(
            base_url.as_str(),
            service.docs_catalog(),
        ),
    )
}

async fn source_config_section_markdown(
    State(service): State<Arc<RustupService>>,
    Path(stack): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(section) = crate::docs::source_config::section_by_id(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let base_url = public_base_url_for_request(&headers, service.public_url());
    text_response(
        "text/markdown; charset=utf-8",
        crate::docs::source_config::render_section_markdown(
            section,
            base_url.as_str(),
            service.docs_catalog(),
        ),
    )
}

fn html_response(body: String) -> axum::response::Response {
    text_response("text/html; charset=utf-8", body)
}

fn text_response(content_type: &'static str, body: String) -> axum::response::Response {
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(content_type),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

fn asset_response(content_type: &'static str, body: &'static str) -> axum::response::Response {
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(content_type),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

fn public_base_url_for_request(headers: &HeaderMap, fallback: &Url) -> Url {
    let host = forwarded_header(headers, "x-forwarded-host")
        .or_else(|| forwarded_header(headers, "host"))
        .filter(|host| is_valid_forwarded_host(host));
    let scheme = forwarded_header(headers, "x-forwarded-proto")
        .filter(|scheme| matches!(*scheme, "http" | "https"));
    let fallback_path = match fallback.path().trim_end_matches('/') {
        "" => "/",
        path => path,
    };
    let mut url = host
        .and_then(|host| {
            let scheme =
                scheme.unwrap_or_else(|| if is_local_host(host) { "http" } else { "https" });
            Url::parse(&format!("{scheme}://{host}")).ok()
        })
        .unwrap_or_else(|| fallback.clone());
    url.set_path(fallback_path);
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn forwarded_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn is_valid_forwarded_host(host: &str) -> bool {
    host.len() <= 255
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '[' | ']'))
}

fn is_local_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.starts_with("localhost:")
        || host == "127.0.0.1"
        || host.starts_with("127.0.0.1:")
        || host == "[::1]"
        || host.starts_with("[::1]:")
}

fn site_href(base_url: &Url, suffix: &str) -> String {
    format!(
        "{}{suffix}",
        base_url.path().trim_end_matches('/').to_owned()
    )
}

async fn registry_cors(request: Request<Body>, next: Next) -> axum::response::Response {
    let origin = request.headers().get(header::ORIGIN).cloned();
    let requested_headers = request
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned();
    let mut response = if request.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        origin.unwrap_or_else(|| header::HeaderValue::from_static("*")),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        requested_headers.unwrap_or_else(|| {
            header::HeaderValue::from_static("Accept, Content-Type, X-Market-Client-Id")
        }),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        header::HeaderValue::from_static("true"),
    );
    headers.insert(
        header::HeaderName::from_static("access-control-allow-private-network"),
        header::HeaderValue::from_static("true"),
    );
    headers.insert(
        header::VARY,
        header::HeaderValue::from_static("Origin, Access-Control-Request-Headers"),
    );
    response
}

async fn named_source(
    State(state): State<RegistryRouteState>,
    Path((ecosystem, name, path)): Path<(String, String, String)>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: header::HeaderMap,
    body: Body,
) -> axum::response::Response {
    let Some(registered) = state.registry.get(&ecosystem) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if registered.pool.proxy_members(&name).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let body = match to_bytes(body, 1024 * 1024).await {
        Ok(body) => body.to_vec(),
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let prefix = format!("/mirror/{ecosystem}/{name}/");
    let raw_path = uri.path().strip_prefix(&prefix).unwrap_or(&path).to_owned();
    registered
        .adapter
        .handle(
            name,
            AdapterRequest {
                method,
                path,
                raw_path,
                query: uri.query().map(str::to_owned),
                headers,
                body,
            },
            registered.pool.clone(),
        )
        .await
}

async fn docker_root_source(
    State(state): State<RegistryRouteState>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: header::HeaderMap,
    body: Body,
) -> axum::response::Response {
    let Some(entry_name) = state.docker_entry.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(registered) = state.registry.get("docker") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if registered.pool.proxy_members(entry_name).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let body = match to_bytes(body, 1024 * 1024).await {
        Ok(body) => body.to_vec(),
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let raw_path = uri
        .path()
        .strip_prefix('/')
        .unwrap_or(uri.path())
        .to_owned();
    registered
        .adapter
        .handle(
            entry_name.to_owned(),
            AdapterRequest {
                method,
                path: raw_path.clone(),
                raw_path,
                query: uri.query().map(str::to_owned),
                headers,
                body,
            },
            registered.pool.clone(),
        )
        .await
}

async fn rustup_dist(
    State(service): State<Arc<RustupService>>,
    Path((name, path)): Path<(String, String)>,
) -> impl IntoResponse {
    let Some(path) = path.strip_prefix("dist/") else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Some((channel, checksum)) = channel_request(path) {
        return match service.manifest_for_request(&name, channel).await {
            Ok(snapshot) if checksum => {
                format!("{}  channel-rust-{channel}.toml\n", snapshot.checksum).into_response()
            }
            Ok(snapshot) => snapshot.manifest.into_response(),
            Err(error) => service_error_response(&error),
        };
    }

    let Some((channel, artifact_path)) = path.split_once('/') else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = match DistPath::parse(artifact_path) {
        Ok(path) => path,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    match service
        .artifact_file_for_request(&name, channel, &path)
        .await
    {
        Ok(file) => rustup_artifact_response(file).await,
        Err(error) => service_error_response(&error),
    }
}

async fn rustup_artifact_response(file: ArtifactFile) -> axum::response::Response {
    let handle = match tokio::fs::File::open(&file.path).await {
        Ok(handle) => handle,
        Err(error) => {
            return service_error_response(&ServiceError::Storage(StorageError::Io(error)));
        }
    };
    let mut response =
        (StatusCode::OK, Body::from_stream(ReaderStream::new(handle))).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(length) = axum::http::HeaderValue::from_str(&file.length.to_string()) {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, length);
    }
    response
}

fn service_error_response(error: &ServiceError) -> axum::response::Response {
    let status = match error {
        ServiceError::UnconfiguredChannel(_) | ServiceError::UnknownArtifact(_) => {
            StatusCode::NOT_FOUND
        }
        ServiceError::Source(crate::sources::SourcePoolError::UnknownEntry(_)) => {
            StatusCode::NOT_FOUND
        }
        ServiceError::Source(crate::sources::SourcePoolError::TerminalSourceError {
            status,
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        ServiceError::Source(crate::sources::SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        ServiceError::Storage(StorageError::MissingSnapshot(_)) => StatusCode::SERVICE_UNAVAILABLE,
        ServiceError::Source(_)
        | ServiceError::Rewrite(_)
        | ServiceError::Storage(StorageError::ArtifactHashMismatch(_)) => StatusCode::BAD_GATEWAY,
        ServiceError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn channel_request(path: &str) -> Option<(&str, bool)> {
    let value = path.strip_prefix("channel-rust-")?;
    if let Some(channel) = value.strip_suffix(".toml.sha256") {
        return Some((channel, true));
    }
    value.strip_suffix(".toml").map(|channel| (channel, false))
}
