use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Router,
    body::Body,
    extract::OriginalUri,
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
async fn forwards_catalog_and_tag_list_queries_to_registry_v2() {
    let upstream = start_query_server().await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    let catalog = router
        .clone()
        .oneshot(
            Request::get("/mirror/docker/official/v2/_catalog?n=2&last=library%2Falpine")
                .body(Body::empty())
                .expect("catalog request"),
        )
        .await
        .expect("catalog response");
    assert_eq!(catalog.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(catalog.into_body(), usize::MAX)
            .await
            .expect("catalog body"),
        br#"{"repositories":["library/alpine","library/busybox"]}"#.to_vec()
    );

    let tags = router
        .oneshot(
            Request::get("/mirror/docker/official/v2/library/alpine/tags/list?n=2")
                .body(Body::empty())
                .expect("tags request"),
        )
        .await
        .expect("tags response");
    assert_eq!(tags.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(tags.into_body(), usize::MAX)
            .await
            .expect("tags body"),
        br#"{"name":"library/alpine","tags":["latest","3.20"]}"#.to_vec()
    );
}

#[tokio::test]
async fn group_query_returns_one_successful_source_without_merging() {
    let first = start_json_server(
        br#"{"repositories":["from-first"]}"#.to_vec(),
        StatusCode::OK,
    )
    .await;
    let second = start_json_server(
        br#"{"repositories":["from-second"]}"#.to_vec(),
        StatusCode::OK,
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router_with_sources(first, second, data_dir.path());

    let response = router
        .oneshot(
            Request::get("/mirror/docker/fallbacks/v2/_catalog")
                .body(Body::empty())
                .expect("group catalog request"),
        )
        .await
        .expect("group catalog response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("group catalog body"),
        br#"{"repositories":["from-first"]}"#.to_vec()
    );
}

#[tokio::test]
async fn catalog_and_tags_queries_use_entry_bound_verified_cache() {
    let hits = Arc::new(AtomicUsize::new(0));
    let upstream = start_counted_query_server(hits.clone()).await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    for path in [
        "/mirror/docker/official/v2/_catalog?n=2",
        "/mirror/docker/official/v2/library/alpine/tags/list?n=2",
    ] {
        let first = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("first query"))
            .await
            .expect("first response");
        assert_eq!(first.status(), StatusCode::OK);
        let second = router
            .clone()
            .oneshot(
                Request::get(path)
                    .body(Body::empty())
                    .expect("cached query"),
            )
            .await
            .expect("cached response");
        assert_eq!(second.status(), StatusCode::OK);
    }

    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

fn docker_router(upstream: Url, data_dir: &std::path::Path) -> axum::Router {
    docker_router_with_sources(upstream.clone(), upstream, data_dir)
}

fn docker_router_with_sources(first: Url, second: Url, data_dir: &std::path::Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "docker",
            Arc::new(
                SourcePool::new(
                    "docker",
                    SourceGraph::new(vec![
                        proxy("official", first.clone()),
                        proxy("second", second),
                        group("fallbacks", &["official", "second"]),
                    ])
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

fn proxy(name: &str, base_url: Url) -> SourceConfig {
    SourceConfig {
        name: name.to_owned(),
        source_type: SourceType::Proxy,
        base_url: Some(base_url),
        require_https: false,
        members: vec![],
    }
}

fn group(name: &str, members: &[&str]) -> SourceConfig {
    SourceConfig {
        name: name.to_owned(),
        source_type: SourceType::Group,
        base_url: None,
        require_https: false,
        members: members.iter().map(|member| (*member).to_owned()).collect(),
    }
}

async fn start_query_server() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(|OriginalUri(uri): OriginalUri| async move {
            match (uri.path(), uri.query()) {
                ("/v2/_catalog", Some("n=2&last=library%2Falpine")) => (
                    StatusCode::OK,
                    br#"{"repositories":["library/alpine","library/busybox"]}"#,
                )
                    .into_response(),
                ("/v2/library/alpine/tags/list", Some("n=2")) => (
                    StatusCode::OK,
                    br#"{"name":"library/alpine","tags":["latest","3.20"]}"#,
                )
                    .into_response(),
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("query server");
    });
    Url::parse(&format!("http://{address}")).expect("upstream URL")
}

async fn start_counted_query_server(hits: Arc<AtomicUsize>) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(move |OriginalUri(uri): OriginalUri| {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                match uri.path() {
                    "/v2/_catalog" => {
                        (StatusCode::OK, br#"{"repositories":["library/alpine"]}"#).into_response()
                    }
                    "/v2/library/alpine/tags/list" => (
                        StatusCode::OK,
                        br#"{"name":"library/alpine","tags":["latest"]}"#,
                    )
                        .into_response(),
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("counted server");
    });
    Url::parse(&format!("http://{address}")).expect("upstream URL")
}

async fn start_json_server(body: Vec<u8>, status: StatusCode) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(move || {
            let body = body.clone();
            async move { (status, body).into_response() }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("JSON server");
    });
    Url::parse(&format!("http://{address}")).expect("upstream URL")
}
