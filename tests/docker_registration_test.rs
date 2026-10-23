use std::path::PathBuf;

use axum::{body::Body, http::Request};
use stacktory::{
    DockerConfig, SourceConfig, SourceGraph, SourceType, docker::register_adapter,
    http::router_with_registry, sources::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn registers_docker_adapter_for_configured_sources() {
    let data_dir = tempdir().expect("data directory");
    let mut registry = AdapterRegistry::new();
    register_adapter(
        &mut registry,
        DockerConfig {
            sources: SourceGraph::new(vec![SourceConfig {
                name: "official".to_owned(),
                source_type: SourceType::Proxy,
                base_url: Some(Url::parse("https://registry.example").expect("source URL")),
                require_https: true,
                members: vec![],
            }])
            .expect("source graph"),
            registry_endpoints: std::collections::BTreeMap::new(),
        },
        Url::parse("http://127.0.0.1:18761").expect("root URL"),
        PathBuf::from(data_dir.path()),
    )
    .expect("Docker adapter registration");

    let response = router_with_registry(registry)
        .oneshot(
            Request::get("/mirror/docker/official/v2/")
                .body(Body::empty())
                .expect("probe request"),
        )
        .await
        .expect("probe response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
}
