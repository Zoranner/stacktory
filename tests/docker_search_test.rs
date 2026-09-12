use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::get,
};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, docker::DockerAdapter,
    http::router_with_registry, sources::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn searches_one_registry_catalog_without_merging_sources() {
    let upstream = start_catalog_server().await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    let response = router
        .oneshot(
            Request::get("/mirror/docker/official/search?q=alpine&n=1")
                .body(Body::empty())
                .expect("search request"),
        )
        .await
        .expect("search response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("search body"),
        br#"{"query":"alpine","results":[{"name":"library/alpine"}]}"#.to_vec()
    );
}

#[tokio::test]
async fn rejects_invalid_search_query_and_v1_search_path() {
    let upstream = start_catalog_server().await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    let invalid = router
        .clone()
        .oneshot(
            Request::get("/mirror/docker/official/search?n=1")
                .body(Body::empty())
                .expect("invalid search request"),
        )
        .await
        .expect("invalid search response");
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let v1 = router
        .oneshot(
            Request::get("/mirror/docker/official/v1/search?q=alpine")
                .body(Body::empty())
                .expect("V1 search request"),
        )
        .await
        .expect("V1 search response");
    assert_eq!(v1.status(), StatusCode::NOT_FOUND);
}

fn docker_router(upstream: Url, data_dir: &std::path::Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "docker",
            Arc::new(
                SourcePool::new(
                    "docker",
                    SourceGraph::new(vec![SourceConfig {
                        name: "official".to_owned(),
                        source_type: SourceType::Proxy,
                        base_url: Some(upstream),
                        require_https: false,
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
        .expect("adapter registration");
    router_with_registry(registry)
}

async fn start_catalog_server() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/v2/_catalog",
        get(|| async {
            (
                StatusCode::OK,
                br#"{"repositories":["library/alpine","library/busybox","company/alpine-tools"]}"#,
            )
                .into_response()
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("catalog server");
    });
    Url::parse(&format!("http://{address}")).expect("upstream URL")
}
