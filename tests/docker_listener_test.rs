use std::sync::Arc;

use axum::{body::Body, http::Request};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType,
    ecosystems::docker::DockerAdapter,
    http::{router_with_docker_entry, router_with_registry},
    upstream::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn standard_registry_root_is_bound_to_one_docker_entry() {
    let data_dir = tempdir().expect("data directory");
    let registry = docker_registry(data_dir.path());

    let response = router_with_docker_entry(registry, "official")
        .oneshot(
            Request::get("/v2/")
                .body(Body::empty())
                .expect("root probe request"),
        )
        .await
        .expect("root probe response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("docker-distribution-api-version")
            .and_then(|value| value.to_str().ok()),
        Some("registry/2.0")
    );
}

#[tokio::test]
async fn mirror_route_remains_available_alongside_standard_registry_root() {
    let data_dir = tempdir().expect("data directory");
    let registry = docker_registry(data_dir.path());
    let router = router_with_registry(registry.clone());
    let root_router = router_with_docker_entry(registry, "official");

    let mirror = router
        .oneshot(
            Request::get("/mirror/docker/official/v2/")
                .body(Body::empty())
                .expect("mirror probe request"),
        )
        .await
        .expect("mirror probe response");
    let root = root_router
        .oneshot(
            Request::get("/v2/")
                .body(Body::empty())
                .expect("root probe request"),
        )
        .await
        .expect("root probe response");

    assert_eq!(mirror.status(), axum::http::StatusCode::OK);
    assert_eq!(root.status(), axum::http::StatusCode::OK);
}

#[tokio::test]
async fn standard_registry_root_serves_probe_and_nested_paths() {
    let data_dir = tempdir().expect("data directory");
    let registry = docker_registry(data_dir.path());
    let router = router_with_docker_entry(registry, "official");

    let probe = router
        .clone()
        .oneshot(
            Request::get("/v2/")
                .body(Body::empty())
                .expect("probe request"),
        )
        .await
        .expect("probe response");
    let nested = router
        .oneshot(
            Request::post("/v2/library/alpine/manifests/latest")
                .body(Body::empty())
                .expect("nested request"),
        )
        .await
        .expect("nested response");

    assert_eq!(probe.status(), axum::http::StatusCode::OK);
    // 405 说明嵌套路径仍由 Docker 适配器处理；未匹配路由只会返回 404。
    assert_eq!(nested.status(), axum::http::StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn docker_listener_does_not_serve_source_config_pages() {
    let data_dir = tempdir().expect("data directory");
    let registry = docker_registry(data_dir.path());

    let response = router_with_docker_entry(registry, "official")
        .oneshot(
            Request::get("/docs")
                .body(Body::empty())
                .expect("docs request"),
        )
        .await
        .expect("docs response");

    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
}

fn docker_registry(data_dir: &std::path::Path) -> AdapterRegistry {
    AdapterRegistry::new()
        .with_adapter(
            "docker",
            Arc::new(
                SourcePool::new(
                    "docker",
                    SourceGraph::new(vec![SourceConfig {
                        name: "official".to_owned(),
                        source_type: SourceType::Proxy,
                        base_url: Some(Url::parse("https://registry.example").expect("source URL")),
                        require_https: true,
                        members: vec![],
                    }])
                    .expect("source graph"),
                )
                .expect("source pool"),
            ),
            Arc::new(DockerAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("adapter registration")
}
