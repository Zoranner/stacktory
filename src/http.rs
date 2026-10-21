use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{OriginalUri, Path, State},
    http::{Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{any, get},
};

use crate::{
    cargo::CargoAdapter,
    rustup::DistPath,
    service::{RustupService, ServiceError},
    sources::{
        SourcePool,
        registry::{AdapterRegistry, AdapterRequest},
    },
    storage::StorageError,
};

pub fn router() -> Router {
    Router::new().route("/healthz", get(health))
}

pub fn router_with_rustup(service: Arc<RustupService>) -> Router {
    Router::new()
        .route("/healthz", get(health))
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
    match service.artifact_for_request(&name, channel, &path).await {
        Ok(contents) => contents.into_response(),
        Err(error) => service_error_response(&error),
    }
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
