//! HTTP 路由树：站点、使用指南、通用镜像入口和 Rustup 分发入口。
//!
//! 路由表在这里集中注册，便于确认哪些生态已经暴露；各入口只负责把请求
//! 转换成对应领域的调用。

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};

use crate::{ecosystems::rustup::RustupService, upstream::registry::AdapterRegistry};

pub(crate) mod guide;
mod mirror;
mod rustup_dist;
mod site;

pub fn router() -> Router {
    Router::new().route("/healthz", get(health))
}

pub fn router_with_rustup(service: Arc<RustupService>) -> Router {
    let site = Arc::new(site::SiteContext::new(
        service.public_url().clone(),
        service.guide_catalog().clone(),
    ));
    let site_router = Router::new()
        .route("/", get(site::landing_page))
        .route("/theme.css", get(site::theme_css))
        .route("/index.css", get(site::landing_css))
        .route("/index.js", get(site::landing_js))
        .route("/vendor/{*path}", get(site::vendor_module))
        .route("/docs", get(guide::index_page))
        .route("/docs.md", get(guide::index_markdown))
        .route("/docs/index.css", get(guide::stylesheet))
        .route("/docs/index.js", get(guide::script))
        .route("/docs/{stack}", get(guide::section_page))
        .route("/docs/{stack}/index.md", get(guide::section_markdown))
        .with_state(site);
    let rustup_router = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/mirror/rustup/{name}/{*path}", get(rustup_dist::dist))
        .with_state(service);
    site_router.merge(rustup_router)
}

pub fn router_with_registry(registry: AdapterRegistry) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .merge(mirror::registry_routes(registry, None))
}

pub fn router_with_docker_entry(
    registry: AdapterRegistry,
    entry_name: impl Into<String>,
) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .merge(mirror::registry_routes(registry, Some(entry_name.into())))
}

pub fn router_with_rustup_and_registry(
    rustup: Arc<RustupService>,
    registry: AdapterRegistry,
) -> Router {
    router_with_rustup(rustup).merge(mirror::registry_routes(registry, None))
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

pub(super) fn html_response(body: String) -> axum::response::Response {
    text_response("text/html; charset=utf-8", body)
}

pub(super) fn text_response(content_type: &'static str, body: String) -> axum::response::Response {
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
