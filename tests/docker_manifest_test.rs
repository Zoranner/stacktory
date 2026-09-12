use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    extract::OriginalUri,
    http::{HeaderMap, Request, StatusCode, header},
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

fn sha256_digest(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}
