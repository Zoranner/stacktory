use std::{
    net::SocketAddr,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    extract::OriginalUri,
    http::{Request, StatusCode, header::CONTENT_TYPE},
    response::IntoResponse,
};
use sha2::{Digest, Sha512};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, http::router_with_registry, npm::NpmAdapter,
    sources::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn serves_unscoped_and_scoped_metadata_with_internal_tarball_urls() {
    let package_body = b"verified npm tarball".to_vec();
    let (upstream, _) = start_npm_server(
        "left-pad",
        package_body.clone(),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(vec![proxy("official", upstream.as_str())], data_dir.path());

    let metadata = router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("metadata response");
    assert_eq!(metadata.status(), StatusCode::OK);
    let metadata: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(metadata.into_body(), usize::MAX)
            .await
            .expect("metadata body"),
    )
    .expect("metadata JSON");
    let tarball = metadata["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .expect("internal tarball URL");
    assert!(tarball.starts_with("http://127.0.0.1:18761/mirror/npm/official/tarballs/"));
    assert!(!tarball.contains("registry.example"));

    let tarball_path = Url::parse(tarball).expect("tarball URL").path().to_owned();
    let artifact = router
        .clone()
        .oneshot(get_request(&tarball_path))
        .await
        .expect("tarball response");
    assert_eq!(artifact.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(artifact.into_body(), usize::MAX)
            .await
            .expect("tarball body"),
        package_body
    );

    let scoped = router
        .oneshot(get_request("/mirror/npm/official/@scope%2Fpackage"))
        .await
        .expect("scoped metadata response");
    assert_eq!(scoped.status(), StatusCode::OK);
}

#[tokio::test]
async fn falls_back_after_tarball_integrity_failure_and_keeps_metadata_source_binding() {
    let expected_body = b"verified npm tarball".to_vec();
    let first_metadata_requests = Arc::new(AtomicUsize::new(0));
    let first_artifact_requests = Arc::new(AtomicUsize::new(0));
    let second_metadata_requests = Arc::new(AtomicUsize::new(0));
    let second_artifact_requests = Arc::new(AtomicUsize::new(0));
    let (first, _) = start_npm_server(
        "left-pad",
        b"tampered".to_vec(),
        first_metadata_requests.clone(),
        first_artifact_requests.clone(),
    )
    .await;
    let (second, _) = start_npm_server(
        "left-pad",
        expected_body.clone(),
        second_metadata_requests.clone(),
        second_artifact_requests.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(
        vec![
            proxy("first", first.as_str()),
            proxy("second", second.as_str()),
            group("group", &["first", "second"]),
        ],
        data_dir.path(),
    );

    let metadata = router
        .clone()
        .oneshot(get_request("/mirror/npm/group/left-pad"))
        .await
        .expect("metadata response");
    assert_eq!(metadata.status(), StatusCode::OK);
    let metadata: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(metadata.into_body(), usize::MAX)
            .await
            .expect("metadata body"),
    )
    .expect("metadata JSON");
    let tarball_path = Url::parse(
        metadata["versions"]["1.0.0"]["dist"]["tarball"]
            .as_str()
            .expect("tarball URL"),
    )
    .expect("tarball URL")
    .path()
    .to_owned();

    let artifact = router
        .oneshot(get_request(&tarball_path))
        .await
        .expect("fallback tarball response");
    assert_eq!(artifact.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(artifact.into_body(), usize::MAX)
            .await
            .expect("fallback body"),
        expected_body
    );
    assert_eq!(first_metadata_requests.load(Ordering::SeqCst), 1);
    assert_eq!(first_artifact_requests.load(Ordering::SeqCst), 1);
    assert_eq!(second_metadata_requests.load(Ordering::SeqCst), 1);
    assert_eq!(second_artifact_requests.load(Ordering::SeqCst), 1);
    assert!(
        data_dir
            .path()
            .join("stores/npm/artifacts/second/objects")
            .exists()
    );
}

#[tokio::test]
async fn rejects_unbound_artifacts_and_invalid_scoped_paths() {
    let (upstream, _) = start_npm_server(
        "left-pad",
        b"verified npm tarball".to_vec(),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let router = npm_router(
        vec![proxy("official", upstream.as_str())],
        tempdir().expect("data directory").path(),
    );

    let unbound = router
        .clone()
        .oneshot(get_request(&format!(
            "/mirror/npm/official/tarballs/{}",
            "a".repeat(64)
        )))
        .await
        .expect("unbound artifact response");
    let decoded_scope = router
        .oneshot(get_request("/mirror/npm/official/@scope/package"))
        .await
        .expect("decoded scope response");

    assert_eq!(unbound.status(), StatusCode::NOT_FOUND);
    assert_eq!(decoded_scope.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn falls_back_for_forbidden_upstream() {
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let first = start_status_server(StatusCode::FORBIDDEN, first_hits.clone()).await;
    let second = start_npm_server(
        "left-pad",
        b"verified npm tarball".to_vec(),
        second_hits.clone(),
        Arc::new(AtomicUsize::new(0)),
    )
    .await
    .0;
    let router = npm_router(
        vec![
            proxy("first", first.as_str()),
            proxy("second", second.as_str()),
            group("group", &["first", "second"]),
        ],
        tempdir().expect("data directory").path(),
    );

    let response = router
        .oneshot(get_request("/mirror/npm/group/left-pad"))
        .await
        .expect("terminal response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
}

fn npm_router(sources: Vec<SourceConfig>, data_dir: &Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "npm",
            Arc::new(
                SourcePool::new("npm", SourceGraph::new(sources).expect("source graph"))
                    .expect("source pool"),
            ),
            Arc::new(NpmAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("registry");
    router_with_registry(registry)
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET request")
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

async fn start_npm_server(
    package: &str,
    artifact_body: Vec<u8>,
    metadata_requests: Arc<AtomicUsize>,
    artifact_requests: Arc<AtomicUsize>,
) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let metadata_url = format!("http://{address}/left-pad/-/left-pad-1.0.0.tgz");
    let scoped_metadata_url = format!("http://{address}/@scope/package/-/package-1.0.0.tgz");
    let package = package.to_owned();
    let expected_integrity = format!(
        "sha512-{}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            Sha512::digest(b"verified npm tarball")
        )
    );
    let router = Router::new().fallback(move |OriginalUri(uri): OriginalUri| {
        let metadata_url = metadata_url.clone();
        let scoped_metadata_url = scoped_metadata_url.clone();
        let package = package.clone();
        let artifact_body = artifact_body.clone();
        let expected_integrity = expected_integrity.clone();
        let metadata_requests = metadata_requests.clone();
        let artifact_requests = artifact_requests.clone();
        async move {
            if uri.path().ends_with(".tgz") {
                artifact_requests.fetch_add(1, Ordering::SeqCst);
                return ([(CONTENT_TYPE, "application/octet-stream")], artifact_body)
                    .into_response();
            }
            metadata_requests.fetch_add(1, Ordering::SeqCst);
            let (name, tarball) = if uri.path().contains("@scope") {
                ("@scope/package", scoped_metadata_url)
            } else {
                (package.as_str(), metadata_url)
            };
            (
                [(CONTENT_TYPE, "application/json")],
                metadata_body(name, &tarball, &expected_integrity),
            )
                .into_response()
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("npm server");
    });
    (
        Url::parse(&format!("http://{address}")).expect("upstream URL"),
        address,
    )
}

async fn start_status_server(status: StatusCode, hits: Arc<AtomicUsize>) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().fallback(move || {
        let hits = hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            status
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("status server");
    });
    Url::parse(&format!("http://{address}")).expect("status URL")
}

fn metadata_body(package: &str, tarball: &str, integrity: &str) -> String {
    format!(
        r#"{{
            "name": "{package}",
            "dist-tags": {{"latest":"1.0.0"}},
            "versions": {{
                "1.0.0": {{
                    "name": "{package}",
                    "version": "1.0.0",
                    "dependencies": {{}},
                    "dist": {{
                        "tarball": "{tarball}",
                        "integrity": "{integrity}"
                    }}
                }}
            }}
        }}"#
    )
}
