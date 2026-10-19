use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

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
async fn verified_manifest_and_blob_content_are_reused_from_cache() {
    let config = br#"{"architecture":"amd64","os":"linux"}"#.to_vec();
    let layer = b"cached layer".to_vec();
    let config_digest = digest(&config);
    let layer_digest = digest(&layer);
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{config_digest}","size":{config_size}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"{layer_digest}","size":{layer_size}}}]}}"#,
        config_size = config.len(),
        layer_size = layer.len(),
    )
    .into_bytes();
    let manifest_digest = digest(&manifest);
    let manifest_hits = Arc::new(AtomicUsize::new(0));
    let blob_hits = Arc::new(AtomicUsize::new(0));
    let upstream = start_blob_server(BlobServerData {
        manifest: manifest.clone(),
        manifest_digest: manifest_digest.clone(),
        config: config.clone(),
        config_digest: config_digest.clone(),
        layer: layer.clone(),
        layer_digest: layer_digest.clone(),
        manifest_hits: manifest_hits.clone(),
        blob_hits: blob_hits.clone(),
        delay: None,
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    let manifest_path = "/mirror/docker/official/v2/library/demo/manifests/latest";
    let first_manifest = router
        .clone()
        .oneshot(get_request(manifest_path))
        .await
        .expect("first manifest");
    assert_eq!(first_manifest.status(), StatusCode::OK);
    let second_manifest = router
        .clone()
        .oneshot(get_request(manifest_path))
        .await
        .expect("cached manifest");
    assert_eq!(second_manifest.status(), StatusCode::OK);

    let blob_path = format!("/mirror/docker/official/v2/library/demo/blobs/{config_digest}");
    let first_blob = router
        .clone()
        .oneshot(get_request(&blob_path))
        .await
        .expect("first blob");
    assert_eq!(first_blob.status(), StatusCode::OK);
    let second_blob = router
        .oneshot(get_request(&blob_path))
        .await
        .expect("cached blob");
    assert_eq!(second_blob.status(), StatusCode::OK);

    assert_eq!(manifest_hits.load(Ordering::SeqCst), 1);
    assert_eq!(blob_hits.load(Ordering::SeqCst), 1);
    assert!(
        data_dir
            .path()
            .join(format!(
                "stores/docker/artifacts/official/objects/manifest/{}",
                manifest_digest.replace(':', "-")
            ))
            .exists()
    );
    assert!(
        data_dir
            .path()
            .join(format!(
                "stores/docker/artifacts/official/objects/config/{}",
                config_digest.replace(':', "-")
            ))
            .exists()
    );
}

#[tokio::test]
async fn invalid_blob_range_returns_registry_range_metadata() {
    let config = br#"{"architecture":"amd64","os":"linux"}"#.to_vec();
    let config_digest = digest(&config);
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{config_digest}","size":{size}}},"layers":[]}}"#,
        size = config.len(),
    )
    .into_bytes();
    let upstream = start_blob_server(BlobServerData {
        manifest_digest: digest(&manifest),
        manifest,
        config,
        config_digest: config_digest.clone(),
        layer: Vec::new(),
        layer_digest: digest(&[]),
        manifest_hits: Arc::new(AtomicUsize::new(0)),
        blob_hits: Arc::new(AtomicUsize::new(0)),
        delay: None,
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());
    let manifest_response = router
        .clone()
        .oneshot(get_request(
            "/mirror/docker/official/v2/library/demo/manifests/latest",
        ))
        .await
        .expect("manifest response");
    assert_eq!(manifest_response.status(), StatusCode::OK);

    let response = router
        .oneshot(
            Request::get(format!(
                "/mirror/docker/official/v2/library/demo/blobs/{config_digest}"
            ))
            .header(header::RANGE, "bytes=999-")
            .body(Body::empty())
            .expect("range request"),
        )
        .await
        .expect("range response");

    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok()),
        Some(
            format!(
                "bytes */{}",
                br#"{"architecture":"amd64","os":"linux"}"#.len()
            )
            .as_str()
        )
    );
}

#[tokio::test]
async fn wrong_blob_digest_is_not_returned_or_cached() {
    let expected_blob = b"expected config".to_vec();
    let wrong_blob = b"wrong config".to_vec();
    let expected_digest = digest(&expected_blob);
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{expected_digest}","size":{size}}},"layers":[]}}"#,
        size = expected_blob.len(),
    )
    .into_bytes();
    let manifest_digest = digest(&manifest);
    let upstream = start_blob_server(BlobServerData {
        manifest,
        manifest_digest,
        config: wrong_blob,
        config_digest: expected_digest.clone(),
        layer: Vec::new(),
        layer_digest: digest(&[]),
        manifest_hits: Arc::new(AtomicUsize::new(0)),
        blob_hits: Arc::new(AtomicUsize::new(0)),
        delay: None,
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    let manifest_response = router
        .clone()
        .oneshot(get_request(
            "/mirror/docker/official/v2/library/demo/manifests/latest",
        ))
        .await
        .expect("manifest response");
    assert_eq!(manifest_response.status(), StatusCode::OK);

    let response = router
        .oneshot(get_request(&format!(
            "/mirror/docker/official/v2/library/demo/blobs/{expected_digest}"
        )))
        .await
        .expect("wrong blob response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join(format!(
                "stores/docker/artifacts/official/objects/config/{}",
                expected_digest.replace(':', "-")
            ))
            .exists()
    );
}

#[tokio::test]
async fn concurrent_blob_requests_share_one_upstream_fetch() {
    let config = b"shared config".to_vec();
    let config_digest = digest(&config);
    let layer = b"shared large layer".to_vec();
    let layer_digest = digest(&layer);
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{config_digest}","size":{config_size}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"{layer_digest}","size":{layer_size}}}]}}"#,
        config_size = config.len(),
        layer_size = layer.len(),
    )
    .into_bytes();
    let blob_hits = Arc::new(AtomicUsize::new(0));
    let upstream = start_blob_server(BlobServerData {
        manifest_digest: digest(&manifest),
        manifest,
        config,
        config_digest,
        layer,
        layer_digest: layer_digest.clone(),
        manifest_hits: Arc::new(AtomicUsize::new(0)),
        blob_hits: blob_hits.clone(),
        delay: Some(Duration::from_millis(100)),
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());
    let manifest_response = router
        .clone()
        .oneshot(get_request(
            "/mirror/docker/official/v2/library/demo/manifests/latest",
        ))
        .await
        .expect("manifest response");
    assert_eq!(manifest_response.status(), StatusCode::OK);

    let blob_path = format!("/mirror/docker/official/v2/library/demo/blobs/{layer_digest}");
    let (first, second) = tokio::join!(
        router.clone().oneshot(get_request(&blob_path)),
        router.clone().oneshot(get_request(&blob_path)),
    );
    assert_eq!(first.expect("first blob").status(), StatusCode::OK);
    assert_eq!(second.expect("second blob").status(), StatusCode::OK);
    assert_eq!(blob_hits.load(Ordering::SeqCst), 1);
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
    Request::get(path).body(Body::empty()).expect("GET request")
}

struct BlobServerData {
    manifest: Vec<u8>,
    manifest_digest: String,
    config: Vec<u8>,
    config_digest: String,
    layer: Vec<u8>,
    layer_digest: String,
    manifest_hits: Arc<AtomicUsize>,
    blob_hits: Arc<AtomicUsize>,
    delay: Option<Duration>,
}

async fn start_blob_server(data: BlobServerData) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(move |OriginalUri(uri): OriginalUri| {
            let path = uri.path().to_owned();
            let data = BlobServerData {
                manifest: data.manifest.clone(),
                manifest_digest: data.manifest_digest.clone(),
                config: data.config.clone(),
                config_digest: data.config_digest.clone(),
                layer: data.layer.clone(),
                layer_digest: data.layer_digest.clone(),
                manifest_hits: data.manifest_hits.clone(),
                blob_hits: data.blob_hits.clone(),
                delay: data.delay,
            };
            async move {
                let BlobServerData {
                    manifest,
                    manifest_digest,
                    config,
                    config_digest,
                    layer,
                    layer_digest,
                    manifest_hits,
                    blob_hits,
                    delay,
                } = data;
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                let (body, content_type, digest) = match path.as_str() {
                    "/v2/library/demo/manifests/latest" => {
                        manifest_hits.fetch_add(1, Ordering::SeqCst);
                        (
                            manifest,
                            "application/vnd.oci.image.manifest.v1+json",
                            manifest_digest,
                        )
                    }
                    value if value == format!("/v2/library/demo/blobs/{config_digest}") => {
                        blob_hits.fetch_add(1, Ordering::SeqCst);
                        (
                            config,
                            "application/vnd.oci.image.config.v1+json",
                            config_digest,
                        )
                    }
                    value if value == format!("/v2/library/demo/blobs/{layer_digest}") => {
                        blob_hits.fetch_add(1, Ordering::SeqCst);
                        (
                            layer,
                            "application/vnd.oci.image.layer.v1.tar",
                            layer_digest,
                        )
                    }
                    _ => return StatusCode::NOT_FOUND.into_response(),
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

fn digest(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}
