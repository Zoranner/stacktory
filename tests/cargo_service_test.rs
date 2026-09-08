use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use sha2::{Digest, Sha256};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, cargo::CargoAdapter,
    http::router_with_registry, sources::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn cargo_group_serves_index_and_crate_with_fallback() {
    let crate_body = b"serde crate from fallback".to_vec();
    let crate_hash = format!("{:x}", Sha256::digest(&crate_body));
    let index_body = format!(
        r#"{{"name":"serde","vers":"1.0.0","deps":[],"cksum":"{crate_hash}","features":{{}},"yanked":false}}"#
    );

    let first = start_server(Router::new().fallback(|| async { StatusCode::BAD_GATEWAY })).await;
    let second = start_server(
        Router::new()
            .route(
                "/index/se/rd/serde",
                get({
                    let index_body = index_body.clone();
                    move || {
                        let index_body = index_body.clone();
                        async move { index_body }
                    }
                }),
            )
            .route(
                "/crates/serde/1.0.0.crate",
                get({
                    let crate_body = crate_body.clone();
                    move || {
                        let crate_body = crate_body.clone();
                        async move { crate_body }
                    }
                }),
            ),
    )
    .await;

    let data_dir = tempdir().expect("data directory");
    let root = Url::parse("http://127.0.0.1:18761").expect("root URL");
    let registry = AdapterRegistry::new()
        .with_adapter(
            "cargo",
            Arc::new(
                SourcePool::new(
                    "cargo",
                    SourceGraph::new(vec![
                        proxy("first", first.as_str()),
                        proxy("second", second.as_str()),
                        group("group", &["first", "second"]),
                    ])
                    .expect("source graph"),
                )
                .expect("source pool"),
            ),
            Arc::new(CargoAdapter::new(root, data_dir.path())),
        )
        .expect("registry");
    let router = router_with_registry(registry);

    let index_response = router
        .clone()
        .oneshot(get_request("/mirror/cargo/group/index/se/rd/serde"))
        .await
        .expect("index response");
    assert_eq!(index_response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(index_response.into_body(), usize::MAX)
            .await
            .expect("index body"),
        index_body.as_bytes()
    );

    let crate_response = router
        .oneshot(get_request("/mirror/cargo/group/crates/serde/1.0.0.crate"))
        .await
        .expect("crate response");
    assert_eq!(crate_response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(crate_response.into_body(), usize::MAX)
            .await
            .expect("crate body"),
        crate_body
    );
}

#[tokio::test]
async fn cargo_crate_hash_mismatch_does_not_publish_cache() {
    let expected_body = b"expected crate";
    let expected_hash = format!("{:x}", Sha256::digest(expected_body));
    let index_body = format!(
        r#"{{"name":"serde","vers":"1.0.0","deps":[],"cksum":"{expected_hash}","features":{{}},"yanked":false}}"#
    );
    let data_dir = tempdir().expect("data directory");
    let upstream = start_server(
        Router::new()
            .route(
                "/index/se/rd/serde",
                get({
                    let index_body = index_body.clone();
                    move || {
                        let index_body = index_body.clone();
                        async move { index_body }
                    }
                }),
            )
            .route(
                "/crates/serde/1.0.0.crate",
                get(|| async { "tampered crate" }),
            ),
    )
    .await;
    let root = Url::parse("http://127.0.0.1:18761").expect("root URL");
    let registry = AdapterRegistry::new()
        .with_adapter(
            "cargo",
            Arc::new(
                SourcePool::new(
                    "cargo",
                    SourceGraph::new(vec![proxy("primary", upstream.as_str())])
                        .expect("source graph"),
                )
                .expect("source pool"),
            ),
            Arc::new(CargoAdapter::new(root, data_dir.path())),
        )
        .expect("registry");

    let response = router_with_registry(registry)
        .oneshot(get_request(
            "/mirror/cargo/primary/crates/serde/1.0.0.crate",
        ))
        .await
        .expect("crate response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("cargo/artifacts/primary/crates/serde/1.0.0.crate")
            .exists()
    );
}

#[tokio::test]
async fn cargo_config_points_to_named_entry_without_upstream_url() {
    let data_dir = tempdir().expect("data directory");
    let root = Url::parse("http://127.0.0.1:18761").expect("root URL");
    let registry = AdapterRegistry::new()
        .with_adapter(
            "cargo",
            Arc::new(
                SourcePool::new(
                    "cargo",
                    SourceGraph::new(vec![proxy("primary", "https://crates.io")])
                        .expect("source graph"),
                )
                .expect("source pool"),
            ),
            Arc::new(CargoAdapter::new(root, data_dir.path())),
        )
        .expect("registry");

    let response = router_with_registry(registry)
        .oneshot(get_request("/mirror/cargo/primary/config.json"))
        .await
        .expect("config response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("config body");
    let body = String::from_utf8(body.to_vec()).expect("UTF-8 config");
    assert!(
        body.contains("http://127.0.0.1:18761/mirror/cargo/primary/crates/{crate}/{version}.crate")
    );
    assert!(!body.contains("crates.io"));
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("request")
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

fn group(name: &str, members: &[&str]) -> SourceConfig {
    SourceConfig {
        name: name.to_owned(),
        source_type: SourceType::Group,
        base_url: None,
        require_https: false,
        members: members.iter().map(|member| (*member).to_owned()).collect(),
    }
}

async fn start_server(router: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    Url::parse(&format!("http://{address}")).expect("server URL")
}
