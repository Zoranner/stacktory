use axum::{Router, http::StatusCode, routing::get};

pub fn router() -> Router {
    Router::new().route("/healthz", get(health))
}

async fn health() -> StatusCode {
    StatusCode::OK
}
