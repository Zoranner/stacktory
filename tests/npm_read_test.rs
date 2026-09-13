use std::{net::SocketAddr, path::Path, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::OriginalUri,
    http::{Request, StatusCode, header::CONTENT_TYPE},
    response::IntoResponse,
    routing::get,
};
use serde_json::Value;
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, http::router_with_registry, npm::NpmAdapter,
    sources::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn search_forwards_supported_query_parameters_to_the_upstream() {
    let (upstream, _) = start_read_server().await;
    let router = npm_router(upstream, tempdir().expect("data directory").path());

    let response = router
        .oneshot(
            Request::get("/mirror/npm/official/-/v1/search?text=vector&size=5&from=10&quality=0.2")
                .body(Body::empty())
                .expect("search request"),
        )
        .await
        .expect("search response");

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("search body"),
    )
    .expect("search JSON");
    assert_eq!(body["objects"][0]["package"]["name"], "com.company.package");
    assert_eq!(body["total"], 1);
}

#[tokio::test]
async fn version_and_dist_tags_reads_preserve_npm_response_shapes() {
    let (upstream, _) = start_read_server().await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(upstream, data_dir.path());

    let version = router
        .clone()
        .oneshot(get_request("/mirror/npm/official/@scope%2Fpackage/1.0.0"))
        .await
        .expect("version response");
    assert_eq!(version.status(), StatusCode::OK);
    let version_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(version.into_body(), usize::MAX)
            .await
            .expect("version body"),
    )
    .expect("version JSON");
    assert_eq!(version_body["name"], "@scope/package");
    assert_eq!(version_body["version"], "1.0.0");
    assert!(version_body["dist"]["tarball"].is_string());

    let tags = router
        .oneshot(get_request(
            "/mirror/npm/official/-/package/@scope%2Fpackage/dist-tags",
        ))
        .await
        .expect("dist-tags response");
    let tags_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(tags.into_body(), usize::MAX)
            .await
            .expect("dist-tags body"),
    )
    .expect("dist-tags JSON");
    assert_eq!(tags_body["latest"], "1.0.0");
}

#[tokio::test]
async fn ping_is_a_read_only_upstream_request() {
    let (upstream, _) = start_read_server().await;
    let router = npm_router(upstream, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request("/mirror/npm/official/-/ping"))
        .await
        .expect("ping response");

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("ping body"),
    )
    .expect("ping JSON");
    assert_eq!(body["ping"], "pong");
}

#[tokio::test]
async fn rejects_invalid_read_queries_and_versions() {
    let (upstream, _) = start_read_server().await;
    let router = npm_router(upstream, tempdir().expect("data directory").path());

    let bad_query = router
        .clone()
        .oneshot(get_request(
            "/mirror/npm/official/-/v1/search?text=x&size=abc",
        ))
        .await
        .expect("bad query response");
    let bad_version = router
        .oneshot(get_request(
            "/mirror/npm/official/@scope/package/not-semver",
        ))
        .await
        .expect("bad version response");

    assert_eq!(bad_query.status(), StatusCode::BAD_REQUEST);
    assert_eq!(bad_version.status(), StatusCode::NOT_FOUND);
}

fn npm_router(upstream: Url, data_dir: &Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "npm",
            Arc::new(
                SourcePool::new(
                    "npm",
                    SourceGraph::new(vec![proxy("official", upstream.as_str())])
                        .expect("source graph"),
                )
                .expect("source pool"),
            ),
            Arc::new(NpmAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("adapter");
    router_with_registry(registry)
}

fn proxy(name: &str, base_url: &str) -> SourceConfig {
    SourceConfig {
        name: name.to_owned(),
        source_type: SourceType::Proxy,
        base_url: Some(Url::parse(base_url).expect("base URL")),
        require_https: false,
        members: vec![],
    }
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET request")
}

async fn start_read_server() -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let tarball_url = format!("http://{address}/@scope/package/-/package-1.0.0.tgz");
    let router = Router::new().route(
        "/-/v1/search",
        get(|OriginalUri(uri): OriginalUri| async move {
            assert_eq!(
                uri.query(),
                Some("text=vector&size=5&from=10&quality=0.2")
            );
            (
                [(CONTENT_TYPE, "application/json")],
                r#"{"objects":[{"package":{"name":"com.company.package","version":"1.0.0"}}],"total":1}"#,
            )
        }),
    )
    .fallback({
        let tarball_url = tarball_url.clone();
        move |OriginalUri(uri): OriginalUri| {
            let tarball_url = tarball_url.clone();
            async move {
                if uri.path().contains("@scope") {
                    return (
                        [(CONTENT_TYPE, "application/json")],
                        format!(
                            r#"{{"name":"@scope/package","dist-tags":{{"latest":"1.0.0"}},"versions":{{"1.0.0":{{"name":"@scope/package","version":"1.0.0","dist":{{"tarball":"{tarball_url}","integrity":"sha512-YWJj"}}}}}}}}"#
                        ),
                    )
                        .into_response();
                }
                StatusCode::NOT_FOUND.into_response()
            }
        }
    })
    .route(
        "/-/ping",
        get(|| async {
            (
                [(CONTENT_TYPE, "application/json")],
                r#"{"ping":"pong"}"#,
            )
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("read server");
    });
    (
        Url::parse(&format!("http://{address}")).expect("upstream URL"),
        address,
    )
}
