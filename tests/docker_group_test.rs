use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Router,
    body::Body,
    extract::OriginalUri,
    http::{HeaderValue, Request, StatusCode, header},
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
async fn group_falls_back_in_order_and_keeps_one_selected_manifest_source() {
    let manifest =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json"}"#.to_vec();
    let digest = sha256_digest(&manifest);
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let first = start_manifest_server(
        StatusCode::NOT_FOUND,
        manifest.clone(),
        "sha256:".to_owned() + &"0".repeat(64),
        first_hits.clone(),
    )
    .await;
    let second = start_manifest_server(
        StatusCode::OK,
        manifest.clone(),
        digest.clone(),
        second_hits.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(first, second, data_dir.path());
    let request_path = "/mirror/docker/fallbacks/v2/library/demo/manifests/latest";

    let first_response = router
        .clone()
        .oneshot(get_request(request_path))
        .await
        .expect("first group response");
    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(first_response.into_body(), usize::MAX)
            .await
            .expect("first body"),
        manifest
    );

    let second_response = router
        .clone()
        .oneshot(get_request(request_path))
        .await
        .expect("cached group response");
    assert_eq!(second_response.status(), StatusCode::OK);

    let digest_response = router
        .oneshot(get_request(&format!(
            "/mirror/docker/second/v2/library/demo/manifests/{digest}"
        )))
        .await
        .expect("direct digest response");
    assert_eq!(digest_response.status(), StatusCode::OK);
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);

    let audit =
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(audit.contains(r#""selected_proxy":"second""#));
    assert!(audit.contains(r#""event":"docker_manifest_read""#));
}

#[tokio::test]
async fn group_falls_back_after_manifest_digest_mismatch() {
    let manifest =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json"}"#.to_vec();
    let digest = sha256_digest(&manifest);
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let first = start_manifest_server(
        StatusCode::OK,
        manifest.clone(),
        "sha256:".to_owned() + &"f".repeat(64),
        first_hits.clone(),
    )
    .await;
    let second = start_manifest_server(
        StatusCode::OK,
        manifest.clone(),
        digest,
        second_hits.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let response = docker_router(first, second, data_dir.path())
        .oneshot(get_request(
            "/mirror/docker/fallbacks/v2/library/demo/manifests/latest",
        ))
        .await
        .expect("digest fallback response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
    let audit =
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(audit.contains(r#""failure_category":"digest_mismatch""#));
    assert!(audit.contains(r#""selected_proxy":"second""#));
}

#[tokio::test]
async fn group_and_direct_proxy_reuse_blob_cache_for_the_selected_proxy() {
    let config = br#"{"architecture":"amd64","os":"linux"}"#.to_vec();
    let config_digest = sha256_digest(&config);
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{config_digest}","size":{size}}},"layers":[]}}"#,
        size = config.len(),
    )
    .into_bytes();
    let manifest_digest = sha256_digest(&manifest);
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let first = start_blob_group_server(
        false,
        manifest.clone(),
        manifest_digest.clone(),
        config.clone(),
        config_digest.clone(),
        first_hits,
    )
    .await;
    let second = start_blob_group_server(
        true,
        manifest,
        manifest_digest,
        config,
        config_digest.clone(),
        second_hits.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(first, second, data_dir.path());

    let group_manifest = router
        .clone()
        .oneshot(get_request(
            "/mirror/docker/fallbacks/v2/library/demo/manifests/latest",
        ))
        .await
        .expect("group manifest");
    assert_eq!(group_manifest.status(), StatusCode::OK);
    let group_blob = router
        .clone()
        .oneshot(get_request(&format!(
            "/mirror/docker/fallbacks/v2/library/demo/blobs/{config_digest}"
        )))
        .await
        .expect("group blob");
    assert_eq!(group_blob.status(), StatusCode::OK);
    let direct_blob = router
        .oneshot(get_request(&format!(
            "/mirror/docker/second/v2/library/demo/blobs/{config_digest}"
        )))
        .await
        .expect("direct cached blob");
    assert_eq!(direct_blob.status(), StatusCode::OK);
    assert_eq!(second_hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn direct_proxy_does_not_fallback_to_a_sibling_proxy() {
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let first = start_manifest_server(
        StatusCode::BAD_GATEWAY,
        Vec::new(),
        String::new(),
        first_hits.clone(),
    )
    .await;
    let second = start_manifest_server(
        StatusCode::OK,
        br#"{"schemaVersion":2}"#.to_vec(),
        "sha256:".to_owned() + &"0".repeat(64),
        second_hits.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let response = docker_router(first, second, data_dir.path())
        .oneshot(get_request(
            "/mirror/docker/first/v2/library/demo/manifests/latest",
        ))
        .await
        .expect("direct proxy response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 0);
}

fn docker_router(first: Url, second: Url, data_dir: &std::path::Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "docker",
            Arc::new(
                SourcePool::new(
                    "docker",
                    SourceGraph::new(vec![
                        proxy("first", first),
                        proxy("second", second),
                        group("fallbacks", &["first", "second"]),
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

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET request")
}

fn proxy(name: &str, url: Url) -> SourceConfig {
    SourceConfig {
        name: name.to_owned(),
        source_type: SourceType::Proxy,
        base_url: Some(url),
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

async fn start_manifest_server(
    status: StatusCode,
    body: Vec<u8>,
    digest: String,
    hits: Arc<AtomicUsize>,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(move |OriginalUri(_uri): OriginalUri| {
            let body = body.clone();
            let digest = digest.clone();
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                if !status.is_success() {
                    return status.into_response();
                }
                let mut response = (StatusCode::OK, body).into_response();
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/vnd.oci.image.manifest.v1+json"),
                );
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

async fn start_blob_group_server(
    available: bool,
    manifest: Vec<u8>,
    manifest_digest: String,
    config: Vec<u8>,
    config_digest: String,
    hits: Arc<AtomicUsize>,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(move |OriginalUri(uri): OriginalUri| {
            let path = uri.path().to_owned();
            let manifest = manifest.clone();
            let manifest_digest = manifest_digest.clone();
            let config = config.clone();
            let config_digest = config_digest.clone();
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                if !available {
                    return StatusCode::NOT_FOUND.into_response();
                }
                let (body, content_type, digest) = if path.ends_with("/manifests/latest") {
                    (
                        manifest,
                        "application/vnd.oci.image.manifest.v1+json",
                        manifest_digest,
                    )
                } else if path.ends_with(&format!("/blobs/{config_digest}")) {
                    (
                        config,
                        "application/vnd.oci.image.config.v1+json",
                        config_digest,
                    )
                } else {
                    return StatusCode::NOT_FOUND.into_response();
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
