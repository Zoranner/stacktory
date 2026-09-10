use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::get,
};
use sha2::{Digest, Sha256};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType,
    http::router_with_registry,
    sources::registry::AdapterRegistry,
    vscode::{VscodeAdapter, VscodePath},
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn metadata_is_rewritten_to_the_named_stacktory_route_and_cached() {
    let requests = Arc::new(AtomicUsize::new(0));
    let vsix_hash = format!("{:x}", Sha256::digest(b"vsix payload"));
    let upstream = start_server({
        let requests = requests.clone();
        let vsix_hash = vsix_hash.clone();
        Router::new().route(
            "/api/redhat/vscode-yaml",
            get(move || {
                let requests = requests.clone();
                let vsix_hash = vsix_hash.clone();
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    if requests.load(Ordering::SeqCst) > 1 {
                        return StatusCode::BAD_GATEWAY.into_response();
                    }
                    axum::Json(serde_json::json!({
                        "namespace": "redhat",
                        "name": "vscode-yaml",
                        "version": "1.17.0",
                        "files": {
                            "download": "https://open-vsx.example/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix",
                            "sha256": vsix_hash
                        }
                    }))
                    .into_response()
                }
            }),
        )
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = vscode_router(upstream, data_dir.path());

    let first = router
        .clone()
        .oneshot(get_request(
            "/mirror/vscode/official/api/redhat/vscode-yaml",
        ))
        .await
        .expect("first metadata response");
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
        .await
        .expect("first metadata body");
    let first_json: serde_json::Value = serde_json::from_slice(&first_body).expect("metadata JSON");
    assert_eq!(
        first_json["files"]["download"],
        "http://127.0.0.1:18761/mirror/vscode/official/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix"
    );

    let second = router
        .oneshot(get_request(
            "/mirror/vscode/official/api/redhat/vscode-yaml",
        ))
        .await
        .expect("cached metadata response");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("cached metadata body"),
        first_body
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn version_metadata_requires_the_requested_version_and_verified_sha256() {
    let upstream = start_server(Router::new().route(
        "/api/redhat/vscode-yaml/1.17.0",
        get(|| async {
            axum::Json(serde_json::json!({
                "namespace": "redhat",
                "name": "vscode-yaml",
                "version": "1.16.0",
                "files": {
                    "download": "https://open-vsx.example/api/redhat/vscode-yaml/1.16.0/file/redhat.vscode-yaml-1.16.0.vsix",
                    "sha256": "not-a-sha256"
                }
            }))
        }),
    ))
    .await;
    let router = vscode_router(upstream, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request(
            "/mirror/vscode/official/api/redhat/vscode-yaml/1.17.0",
        ))
        .await
        .expect("invalid version metadata response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn vsix_download_falls_back_after_hash_mismatch_and_then_uses_cache() {
    let vsix = b"verified vsix".to_vec();
    let expected_hash = format!("{:x}", Sha256::digest(&vsix));
    let first_file_requests = Arc::new(AtomicUsize::new(0));
    let second_file_requests = Arc::new(AtomicUsize::new(0));
    let metadata = metadata_body(&expected_hash);
    let first = start_server({
        let latest_metadata = metadata.clone();
        let version_metadata = metadata.clone();
        let first_file_requests = first_file_requests.clone();
        Router::new()
            .route(
                "/api/redhat/vscode-yaml",
                get(move || {
                    let metadata = latest_metadata.clone();
                    async move { metadata }
                }),
            )
            .route(
                "/api/redhat/vscode-yaml/1.17.0",
                get(move || {
                    let metadata = version_metadata.clone();
                    async move { metadata }
                }),
            )
            .route(
                "/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix",
                get(move || {
                    let first_file_requests = first_file_requests.clone();
                    async move {
                        first_file_requests.fetch_add(1, Ordering::SeqCst);
                        "tampered vsix"
                    }
                }),
            )
    })
    .await;
    let second = start_server({
        let latest_metadata = metadata.clone();
        let version_metadata = metadata.clone();
        let second_file_requests = second_file_requests.clone();
        let expected_vsix = vsix.clone();
        Router::new()
            .route(
                "/api/redhat/vscode-yaml",
                get(move || {
                    let metadata = latest_metadata.clone();
                    async move { metadata }
                }),
            )
            .route(
                "/api/redhat/vscode-yaml/1.17.0",
                get(move || {
                    let metadata = version_metadata.clone();
                    async move { metadata }
                }),
            )
            .route(
                "/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix",
                get(move || {
                    let second_file_requests = second_file_requests.clone();
                    let vsix = expected_vsix.clone();
                    async move {
                        second_file_requests.fetch_add(1, Ordering::SeqCst);
                        vsix
                    }
                }),
            )
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = vscode_group_router(first, second, data_dir.path());

    let metadata_response = router
        .clone()
        .oneshot(get_request("/mirror/vscode/group/api/redhat/vscode-yaml"))
        .await
        .expect("metadata response");
    assert_eq!(metadata_response.status(), StatusCode::OK);

    let first_download = router
        .clone()
        .oneshot(get_request(
            "/mirror/vscode/group/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix",
        ))
        .await
        .expect("first VSIX response");
    assert_eq!(first_download.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(first_download.into_body(), usize::MAX)
            .await
            .expect("first VSIX body"),
        vsix
    );
    assert_eq!(first_file_requests.load(Ordering::SeqCst), 1);
    assert_eq!(second_file_requests.load(Ordering::SeqCst), 1);

    let second_download = router
        .oneshot(get_request(
            "/mirror/vscode/group/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix",
        ))
        .await
        .expect("cached VSIX response");
    assert_eq!(second_download.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(second_download.into_body(), usize::MAX)
            .await
            .expect("cached VSIX body"),
        vsix
    );
    assert_eq!(first_file_requests.load(Ordering::SeqCst), 1);
    assert_eq!(second_file_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mismatched_vsix_is_not_published_to_cache() {
    let expected_hash = format!("{:x}", Sha256::digest(b"expected vsix"));
    let metadata = metadata_body(&expected_hash);
    let upstream = start_server(
        Router::new()
            .route(
                "/api/redhat/vscode-yaml/1.17.0",
                get(move || {
                    let metadata = metadata.clone();
                    async move { metadata }
                }),
            )
            .route(
                "/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix",
                get(|| async { "tampered vsix" }),
            ),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = vscode_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request(
            "/mirror/vscode/official/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix",
        ))
        .await
        .expect("mismatched VSIX response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join(
                "vscode/artifacts/official/redhat/vscode-yaml/1.17.0/redhat.vscode-yaml-1.17.0.vsix"
            )
            .exists()
    );
}

#[test]
fn vscode_path_rejects_unbounded_or_non_vsix_file_paths() {
    for path in [
        "",
        "api/redhat/vscode-yaml/",
        "api/redhat/vscode-yaml/1.17.0/file/other.vsix",
        "api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.zip",
        "api/redhat/../vscode-yaml",
    ] {
        assert!(
            VscodePath::parse(path).is_err(),
            "path should be rejected: {path}"
        );
    }
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("request")
}

fn vscode_router(upstream: Url, data_dir: &std::path::Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "vscode",
            Arc::new(
                SourcePool::new(
                    "vscode",
                    SourceGraph::new(vec![proxy("official", upstream.as_str())])
                        .expect("source graph"),
                )
                .expect("source pool"),
            ),
            Arc::new(VscodeAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("registry");
    router_with_registry(registry)
}

fn vscode_group_router(first: Url, second: Url, data_dir: &std::path::Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "vscode",
            Arc::new(
                SourcePool::new(
                    "vscode",
                    SourceGraph::new(vec![
                        proxy("first", first.as_str()),
                        proxy("second", second.as_str()),
                        group("group", &["first", "second"]),
                    ])
                    .expect("source graph"),
                )
                .expect("source pool"),
            ),
            Arc::new(VscodeAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("registry");
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

fn group(name: &str, members: &[&str]) -> SourceConfig {
    SourceConfig {
        name: name.to_owned(),
        source_type: SourceType::Group,
        base_url: None,
        require_https: false,
        members: members.iter().map(|member| (*member).to_owned()).collect(),
    }
}

fn metadata_body(checksum: &str) -> String {
    serde_json::json!({
        "namespace": "redhat",
        "name": "vscode-yaml",
        "version": "1.17.0",
        "files": {
            "download": "https://open-vsx.example/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix",
            "sha256": checksum
        }
    })
    .to_string()
}

async fn start_server(router: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address: SocketAddr = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    Url::parse(&format!("http://{address}")).expect("server URL")
}
