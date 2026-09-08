use std::{path::Path, sync::Arc};

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
    let second = start_configured_server(index_body.clone(), crate_body.clone(), &crate_hash).await;

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
        .oneshot(get_request("/mirror/cargo/group/se/rd/serde"))
        .await
        .expect("index response");
    assert_eq!(index_response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(index_response.into_body(), usize::MAX)
            .await
            .expect("index body"),
        index_body.as_bytes()
    );
    let legacy_index_response = router
        .clone()
        .oneshot(get_request("/mirror/cargo/group/index/se/rd/serde"))
        .await
        .expect("legacy index response");
    assert_eq!(legacy_index_response.status(), StatusCode::NOT_FOUND);

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
async fn cargo_group_uses_first_proxy_config_and_dl_template() {
    let crate_body = b"serde crate from first proxy".to_vec();
    let crate_hash = format!("{:x}", Sha256::digest(&crate_body));
    let index_body = valid_index_body(std::str::from_utf8(&crate_body).expect("crate UTF-8"));
    let first = start_configured_server(index_body, crate_body, &crate_hash).await;
    let second_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let second = start_server({
        let second_hits = second_hits.clone();
        Router::new().fallback(move || {
            let second_hits = second_hits.clone();
            async move {
                second_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                StatusCode::BAD_GATEWAY
            }
        })
    })
    .await;
    let data_dir = tempdir().expect("data directory");
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
            Arc::new(CargoAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir.path(),
            )),
        )
        .expect("registry");

    let response = router_with_registry(registry)
        .oneshot(get_request("/mirror/cargo/group/crates/serde/1.0.0.crate"))
        .await
        .expect("crate response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("crate body"),
        &b"serde crate from first proxy"[..]
    );
    assert_eq!(second_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cargo_crate_hash_mismatch_does_not_publish_cache() {
    let expected_body = b"expected crate";
    let expected_hash = format!("{:x}", Sha256::digest(expected_body));
    let index_body = format!(
        r#"{{"name":"serde","vers":"1.0.0","deps":[],"cksum":"{expected_hash}","features":{{}},"yanked":false}}"#
    );
    let data_dir = tempdir().expect("data directory");
    let upstream =
        start_configured_server(index_body, b"tampered crate".to_vec(), &expected_hash).await;
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
                    SourceGraph::new(vec![proxy("primary", "https://index.crates.io")])
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

#[tokio::test]
async fn cargo_upstream_not_found_maps_to_not_found() {
    let upstream =
        start_server(Router::new().route("/se/rd/serde", get(|| async { StatusCode::NOT_FOUND })))
            .await;
    let router = cargo_router(upstream, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request("/mirror/cargo/primary/se/rd/serde"))
        .await
        .expect("404 response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cargo_upstream_unauthorized_preserves_unauthorized_status() {
    let upstream = start_server(
        Router::new().route("/se/rd/serde", get(|| async { StatusCode::UNAUTHORIZED })),
    )
    .await;
    let router = cargo_router(upstream, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request("/mirror/cargo/primary/se/rd/serde"))
        .await
        .expect("401 response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cargo_missing_proxy_config_does_not_publish_cache() {
    let index_body = valid_index_body("expected crate");
    let data_dir = tempdir().expect("data directory");
    let upstream = start_server(Router::new().route(
        "/se/rd/serde",
        get({
            let index_body = index_body.clone();
            move || {
                let index_body = index_body.clone();
                async move { index_body }
            }
        }),
    ))
    .await;
    let router = cargo_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request(
            "/mirror/cargo/primary/crates/serde/1.0.0.crate",
        ))
        .await
        .expect("missing config response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("cargo/artifacts/primary/crates/serde/1.0.0.crate")
            .exists()
    );
}

#[tokio::test]
async fn cargo_invalid_proxy_dl_does_not_publish_cache() {
    let index_body = valid_index_body("expected crate");
    let data_dir = tempdir().expect("data directory");
    let upstream = start_server(
        Router::new()
            .route(
                "/config.json",
                get(|| async { r#"{"dl":"https://user:password@example/{crate}/{version}"}"# }),
            )
            .route(
                "/se/rd/serde",
                get({
                    let index_body = index_body.clone();
                    move || {
                        let index_body = index_body.clone();
                        async move { index_body }
                    }
                }),
            ),
    )
    .await;
    let router = cargo_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request(
            "/mirror/cargo/primary/crates/serde/1.0.0.crate",
        ))
        .await
        .expect("invalid dl response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("cargo/artifacts/primary/crates/serde/1.0.0.crate")
            .exists()
    );
}

#[tokio::test]
async fn cargo_upstream_forbidden_preserves_forbidden_status() {
    let index_body = valid_index_body("expected crate");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let expected_hash = format!("{:x}", Sha256::digest(b"expected crate"));
    let dl = format!(
        "http://{address}/download/{{lowerprefix}}{{crate}}/{{version}}/{{sha256-checksum}}"
    );
    let download_path = format!("/download/se/rd/serde/1.0.0/{expected_hash}");
    let upstream_router = Router::new()
        .route(
            "/config.json",
            get(move || {
                let dl = dl.clone();
                async move { format!(r#"{{"dl":"{dl}"}}"#) }
            }),
        )
        .route(
            "/se/rd/serde",
            get({
                let index_body = index_body.clone();
                move || {
                    let index_body = index_body.clone();
                    async move { index_body }
                }
            }),
        )
        .route(&download_path, get(|| async { StatusCode::FORBIDDEN }));
    tokio::spawn(async move {
        axum::serve(listener, upstream_router)
            .await
            .expect("server");
    });
    let upstream = Url::parse(&format!("http://{address}")).expect("server URL");
    let router = cargo_router(upstream, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request(
            "/mirror/cargo/primary/crates/serde/1.0.0.crate",
        ))
        .await
        .expect("403 response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn invalid_index_cache_is_deleted_and_refetched() {
    let index_body = valid_index_body("expected crate");
    let data_dir = tempdir().expect("data directory");
    let cache_path = data_dir
        .path()
        .join("cargo/metadata/primary/index/se/rd/serde");
    std::fs::create_dir_all(cache_path.parent().expect("cache parent")).expect("cache directory");
    std::fs::write(&cache_path, b"invalid index").expect("invalid cache");
    let upstream = start_server(Router::new().route(
        "/se/rd/serde",
        get({
            let index_body = index_body.clone();
            move || {
                let index_body = index_body.clone();
                async move { index_body }
            }
        }),
    ))
    .await;
    let router = cargo_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request("/mirror/cargo/primary/se/rd/serde"))
        .await
        .expect("refetched index response");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("refetched index body");

    assert_eq!(body, index_body.as_bytes());
    assert_eq!(std::fs::read(cache_path).expect("updated cache"), body);
}

#[tokio::test]
async fn cargo_storage_failure_returns_internal_server_error() {
    let index_body = valid_index_body("expected crate");
    let data_dir = tempdir().expect("data directory");
    std::fs::write(data_dir.path().join("cargo"), b"not a directory").expect("blocking cache file");
    let upstream = start_server(Router::new().route(
        "/se/rd/serde",
        get({
            let index_body = index_body.clone();
            move || {
                let index_body = index_body.clone();
                async move { index_body }
            }
        }),
    ))
    .await;
    let router = cargo_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request("/mirror/cargo/primary/se/rd/serde"))
        .await
        .expect("storage failure response");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
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

fn cargo_router(upstream: Url, data_dir: &Path) -> axum::Router {
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
            Arc::new(CargoAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("registry");
    router_with_registry(registry)
}

fn valid_index_body(crate_body: &str) -> String {
    let checksum = format!("{:x}", Sha256::digest(crate_body.as_bytes()));
    format!(
        r#"{{"name":"serde","vers":"1.0.0","deps":[],"cksum":"{checksum}","features":{{}},"yanked":false}}"#
    )
}

async fn start_configured_server(
    index_body: String,
    crate_body: Vec<u8>,
    download_checksum: &str,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let download_path = format!("/download/se/rd/serde/1.0.0/{download_checksum}");
    let dl = format!(
        "http://{address}/download/{{lowerprefix}}{{crate}}/{{version}}/{{sha256-checksum}}"
    );
    let router = Router::new()
        .route(
            "/config.json",
            get(move || {
                let dl = dl.clone();
                async move { format!(r#"{{"dl":"{dl}"}}"#) }
            }),
        )
        .route(
            "/se/rd/serde",
            get(move || {
                let index_body = index_body.clone();
                async move { index_body }
            }),
        )
        .route(
            &download_path,
            get(move || {
                let crate_body = crate_body.clone();
                async move { crate_body }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    Url::parse(&format!("http://{address}")).expect("server URL")
}

async fn start_server(router: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    Url::parse(&format!("http://{address}")).expect("server URL")
}
