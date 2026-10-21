use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    extract::OriginalUri,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use sha2::{Digest, Sha256};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, docker::DockerAdapter,
    http::router_with_registry, sources::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn tag_manifest_forwards_accept_and_returns_registry_headers() {
    let manifest =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json"}"#.to_vec();
    let digest = sha256_digest(&manifest);
    let observed_path = Arc::new(Mutex::new(None));
    let observed_accept = Arc::new(Mutex::new(None));
    let upstream = start_manifest_server(
        manifest.clone(),
        digest.clone(),
        observed_path.clone(),
        observed_accept.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");

    let response = docker_router(upstream, data_dir.path())
        .oneshot(
            Request::get("/mirror/docker/official/v2/library/alpine/manifests/latest")
                .header(
                    header::ACCEPT,
                    "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
                )
                .body(Body::empty())
                .expect("manifest request"),
        )
        .await
        .expect("manifest response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.oci.image.manifest.v1+json")
    );
    assert_eq!(
        response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok()),
        Some(digest.as_str())
    );
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("manifest body"),
        manifest
    );
    assert_eq!(
        observed_path.lock().expect("path observation").as_deref(),
        Some("/v2/library/alpine/manifests/latest")
    );
    assert_eq!(
        observed_accept
            .lock()
            .expect("accept observation")
            .as_deref(),
        Some(
            "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json"
        )
    );
}

#[tokio::test]
async fn digest_manifest_verifies_the_requested_digest() {
    let manifest = br#"{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json"}"#.to_vec();
    let digest = sha256_digest(&manifest);
    let observed_path = Arc::new(Mutex::new(None));
    let observed_accept = Arc::new(Mutex::new(None));
    let upstream = start_manifest_server(
        manifest.clone(),
        digest.clone(),
        observed_path.clone(),
        observed_accept,
    )
    .await;
    let data_dir = tempdir().expect("data directory");

    let response = docker_router(upstream, data_dir.path())
        .oneshot(
            Request::get(format!(
                "/mirror/docker/official/v2/library/alpine/manifests/{digest}"
            ))
            .body(Body::empty())
            .expect("digest manifest request"),
        )
        .await
        .expect("digest manifest response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok()),
        Some(digest.as_str())
    );
    assert_eq!(
        observed_path.lock().expect("path observation").as_deref(),
        Some(format!("/v2/library/alpine/manifests/{digest}").as_str())
    );
}

#[tokio::test]
async fn manifest_with_wrong_digest_is_not_returned_successfully() {
    let manifest =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json"}"#.to_vec();
    let wrong_digest = format!("sha256:{}", "0".repeat(64));
    let upstream = start_manifest_server(
        manifest,
        wrong_digest,
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(None)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");

    let response = docker_router(upstream, data_dir.path())
        .oneshot(
            Request::get("/mirror/docker/official/v2/library/alpine/manifests/latest")
                .body(Body::empty())
                .expect("manifest request"),
        )
        .await
        .expect("manifest response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("error body")
            .len(),
        0
    );
}

#[tokio::test]
async fn head_manifest_returns_registry_headers_without_a_body() {
    let manifest =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json"}"#.to_vec();
    let digest = sha256_digest(&manifest);
    let upstream = start_manifest_server(
        manifest,
        digest.clone(),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(None)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");

    let response = docker_router(upstream, data_dir.path())
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/mirror/docker/official/v2/library/alpine/manifests/latest")
                .body(Body::empty())
                .expect("HEAD manifest request"),
        )
        .await
        .expect("HEAD manifest response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok()),
        Some(digest.as_str())
    );
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()),
        Some("76")
    );
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("HEAD body")
            .len(),
        0
    );
}

#[tokio::test]
async fn tag_manifest_cache_isolated_by_accept_representation() {
    let index =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json"}"#.to_vec();
    let manifest =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json"}"#.to_vec();
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let upstream = start_accept_server(AcceptServerData {
        index: index.clone(),
        index_digest: sha256_digest(&index),
        manifest: manifest.clone(),
        manifest_digest: sha256_digest(&manifest),
        hits: hits.clone(),
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    let index_response = router
        .clone()
        .oneshot(
            Request::get("/mirror/docker/official/v2/library/alpine/manifests/latest")
                .header(header::ACCEPT, "application/vnd.oci.image.index.v1+json")
                .body(Body::empty())
                .expect("index request"),
        )
        .await
        .expect("index response");
    assert_eq!(
        axum::body::to_bytes(index_response.into_body(), usize::MAX)
            .await
            .expect("index body"),
        index
    );

    let manifest_response = router
        .oneshot(
            Request::get("/mirror/docker/official/v2/library/alpine/manifests/latest")
                .header(
                    header::ACCEPT,
                    "application/vnd.docker.distribution.manifest.v2+json",
                )
                .body(Body::empty())
                .expect("manifest request"),
        )
        .await
        .expect("manifest response");
    assert_eq!(
        axum::body::to_bytes(manifest_response.into_body(), usize::MAX)
            .await
            .expect("manifest body"),
        manifest
    );
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn tag_manifest_cache_is_invalidated_when_the_source_target_changes() {
    let first_manifest =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","first":true}"#
            .to_vec();
    let second_manifest =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","second":true}"#
            .to_vec();
    let first_upstream = start_manifest_server(
        first_manifest.clone(),
        sha256_digest(&first_manifest),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(None)),
    )
    .await;
    let second_upstream = start_manifest_server(
        second_manifest.clone(),
        sha256_digest(&second_manifest),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(None)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let path = "/mirror/docker/official/v2/library/alpine/manifests/latest";

    let first = docker_router(first_upstream, data_dir.path())
        .oneshot(get_request(path))
        .await
        .expect("first manifest response");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("first manifest body"),
        first_manifest
    );

    let second = docker_router(second_upstream, data_dir.path())
        .oneshot(get_request(path))
        .await
        .expect("second manifest response");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("second manifest body"),
        second_manifest
    );
}

#[tokio::test]
async fn invalid_manifest_is_not_written_to_cache() {
    let body = b"not valid json".to_vec();
    let digest = sha256_digest(&body);
    let observed_path = Arc::new(Mutex::new(None));
    let upstream =
        start_manifest_server(body, digest, observed_path, Arc::new(Mutex::new(None))).await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());
    let request = || {
        Request::get("/mirror/docker/official/v2/library/alpine/manifests/latest")
            .body(Body::empty())
            .expect("manifest request")
    };

    let first = router
        .clone()
        .oneshot(request())
        .await
        .expect("first invalid response");
    let second = router
        .oneshot(request())
        .await
        .expect("second invalid response");

    assert_eq!(first.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(second.status(), StatusCode::BAD_GATEWAY);
    assert!(!data_dir.path().join("docker").join("artifacts").exists());
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

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("request")
}

async fn start_manifest_server(
    manifest: Vec<u8>,
    digest: String,
    observed_path: Arc<Mutex<Option<String>>>,
    observed_accept: Arc<Mutex<Option<String>>>,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(move |OriginalUri(uri): OriginalUri, headers: HeaderMap| {
            let manifest = manifest.clone();
            let digest = digest.clone();
            let observed_path = observed_path.clone();
            let observed_accept = observed_accept.clone();
            async move {
                *observed_path.lock().expect("path observation") = Some(uri.path().to_owned());
                *observed_accept.lock().expect("accept observation") = headers
                    .get(header::ACCEPT)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let mut response = (StatusCode::OK, manifest).into_response();
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/vnd.oci.image.manifest.v1+json"),
                );
                response.headers_mut().insert(
                    header::HeaderName::from_static("docker-content-digest"),
                    header::HeaderValue::from_str(&digest).expect("digest header"),
                );
                response
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("upstream server");
    });
    Url::parse(&format!("http://{address}")).expect("upstream URL")
}

struct AcceptServerData {
    index: Vec<u8>,
    index_digest: String,
    manifest: Vec<u8>,
    manifest_digest: String,
    hits: Arc<std::sync::atomic::AtomicUsize>,
}

async fn start_accept_server(data: AcceptServerData) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(move |headers: HeaderMap| {
            let data = AcceptServerData {
                index: data.index.clone(),
                index_digest: data.index_digest.clone(),
                manifest: data.manifest.clone(),
                manifest_digest: data.manifest_digest.clone(),
                hits: data.hits.clone(),
            };
            async move {
                data.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let accepts_index = headers
                    .get(header::ACCEPT)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.contains("image.index"));
                let (body, digest, content_type) = if accepts_index {
                    (
                        data.index,
                        data.index_digest,
                        "application/vnd.oci.image.index.v1+json",
                    )
                } else {
                    (
                        data.manifest,
                        data.manifest_digest,
                        "application/vnd.docker.distribution.manifest.v2+json",
                    )
                };
                let mut response = (StatusCode::OK, body).into_response();
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
                response.headers_mut().insert(
                    header::HeaderName::from_static("docker-content-digest"),
                    HeaderValue::from_str(&digest).expect("digest header"),
                );
                response
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("upstream server");
    });
    Url::parse(&format!("http://{address}")).expect("upstream URL")
}

fn sha256_digest(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}
