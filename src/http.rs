use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};

use crate::{
    rustup::DistPath,
    service::{RustupService, ServiceError},
    sources::registry::AdapterRegistry,
    storage::StorageError,
};

pub fn router() -> Router {
    Router::new().route("/healthz", get(health))
}

pub fn router_with_rustup(service: Arc<RustupService>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/mirror/rustup/dist/{*path}", get(rustup_dist))
        .with_state(service)
}

pub fn router_with_registry(registry: AdapterRegistry) -> Router {
    Router::new()
        .route("/healthz", get(health))
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
    Path(path): Path<String>,
) -> impl IntoResponse {
    if let Some((channel, checksum)) = channel_request(&path) {
        return match service.manifest_for_request(channel).await {
            Ok(snapshot) if checksum => {
                format!("{}  channel-rust-{channel}.toml\n", snapshot.checksum).into_response()
            }
            Ok(snapshot) => snapshot.manifest.into_response(),
            Err(error) => service_error_response(&error),
        };
    }

    let path = match DistPath::parse(&path) {
        Ok(path) => path,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    match service.artifact_for_request(&path).await {
        Ok(contents) => contents.into_response(),
        Err(error) => service_error_response(&error),
    }
}

fn service_error_response(error: &ServiceError) -> axum::response::Response {
    let status = match error {
        ServiceError::UnconfiguredChannel(_) | ServiceError::UnknownArtifact(_) => {
            StatusCode::NOT_FOUND
        }
        ServiceError::NoPublishedSnapshots
        | ServiceError::Storage(StorageError::MissingSnapshot(_)) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        ServiceError::Upstream(_)
        | ServiceError::Rewrite(_)
        | ServiceError::Storage(StorageError::ArtifactHashMismatch(_)) => StatusCode::BAD_GATEWAY,
        ServiceError::Storage(_) | ServiceError::ConflictingArtifactHash(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
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
