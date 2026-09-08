use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};

use crate::{
    cargo::CargoAdapter,
    rustup::DistPath,
    service::{RustupService, ServiceError},
    sources::{SourcePool, registry::AdapterRegistry},
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
        .merge(registry_routes(registry))
}

pub fn router_with_rustup_and_cargo(
    rustup: Arc<RustupService>,
    cargo: Option<(Arc<CargoAdapter>, Arc<SourcePool>)>,
) -> Router {
    let rustup_router = router_with_rustup(rustup);
    let Some((adapter, pool)) = cargo else {
        return rustup_router;
    };
    let registry = AdapterRegistry::new()
        .with_adapter("cargo", pool, adapter)
        .expect("cargo adapter and source pool must share their ecosystem");
    rustup_router.merge(registry_routes(registry))
}

fn registry_routes(registry: AdapterRegistry) -> Router {
    Router::new()
        .route("/mirror/{ecosystem}/{name}/{*path}", get(named_source))
        .with_state(registry)
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn named_source(
    State(registry): State<AdapterRegistry>,
    Path((ecosystem, name, path)): Path<(String, String, String)>,
) -> axum::response::Response {
    let Some(registered) = registry.get(&ecosystem) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if registered.pool.proxy_members(&name).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    registered
        .adapter
        .handle(name, path, registered.pool.clone())
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
