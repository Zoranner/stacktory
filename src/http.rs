use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};

use crate::service::RustupService;

pub fn router() -> Router {
    Router::new().route("/healthz", get(health))
}

pub fn router_with_rustup(service: Arc<RustupService>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/mirror/rustup/dist/{*path}", get(rustup_dist))
        .with_state(service)
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn rustup_dist(
    State(service): State<Arc<RustupService>>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    let Some((channel, checksum)) = channel_request(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    match service.manifest_for_request(channel).await {
        Ok(snapshot) if checksum => {
            format!("{}  channel-rust-{channel}.toml\n", snapshot.checksum).into_response()
        }
        Ok(snapshot) => snapshot.manifest.into_response(),
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}

fn channel_request(path: &str) -> Option<(&str, bool)> {
    let value = path.strip_prefix("channel-rust-")?;
    if let Some(channel) = value.strip_suffix(".toml.sha256") {
        return Some((channel, true));
    }
    value.strip_suffix(".toml").map(|channel| (channel, false))
}
