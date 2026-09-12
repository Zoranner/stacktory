use std::sync::{Arc, Mutex};

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
async fn pulls_oci_index_config_and_layer_through_registry_v2_paths() {
    let config = br#"{"architecture":"amd64","os":"linux"}"#.to_vec();
    let layer = b"layer bytes".to_vec();
    let config_digest = sha256_digest(&config);
    let layer_digest = sha256_digest(&layer);
    let image_manifest = format!(
        r#"{{
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {{
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": "{config_digest}",
                "size": {config_size}
            }},
            "layers": [{{
                "mediaType": "application/vnd.oci.image.layer.v1.tar",
                "digest": "{layer_digest}",
                "size": {layer_size}
            }}]
        }}"#,
        config_size = config.len(),
        layer_size = layer.len(),
    )
    .into_bytes();
    let image_digest = sha256_digest(&image_manifest);
    let index = format!(
        r#"{{
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "{image_digest}",
                "size": {image_size},
                "platform": {{"architecture":"amd64","os":"linux"}}
            }}]
        }}"#,
        image_size = image_manifest.len(),
    )
    .into_bytes();
    let index_digest = sha256_digest(&index);
    let paths = Arc::new(Mutex::new(Vec::new()));
    let upstream = start_registry(
        index.clone(),
        index_digest,
        image_manifest.clone(),
        image_digest.clone(),
        config.clone(),
        config_digest.clone(),
        layer.clone(),
        layer_digest.clone(),
        paths.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    let index_response = router
        .clone()
        .oneshot(
            Request::get("/mirror/docker/official/v2/library/demo/manifests/latest")
                .header(
                    header::ACCEPT,
                    "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json",
                )
                .body(Body::empty())
                .expect("index request"),
        )
        .await
        .expect("index response");
    assert_eq!(index_response.status(), StatusCode::OK);
    assert_eq!(
        index_response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.oci.image.index.v1+json")
    );
    assert_eq!(
        axum::body::to_bytes(index_response.into_body(), usize::MAX)
            .await
            .expect("index body"),
        index
    );

    let image_response = router
        .clone()
        .oneshot(
            Request::get(format!(
                "/mirror/docker/official/v2/library/demo/manifests/{image_digest}"
            ))
            .body(Body::empty())
            .expect("image manifest request"),
        )
        .await
        .expect("image manifest response");
    assert_eq!(image_response.status(), StatusCode::OK);

    let config_response = router
        .clone()
        .oneshot(
            Request::get(format!(
                "/mirror/docker/official/v2/library/demo/blobs/{config_digest}"
            ))
            .body(Body::empty())
            .expect("config request"),
        )
        .await
        .expect("config response");
    assert_eq!(config_response.status(), StatusCode::OK);
    assert_eq!(
        config_response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.oci.image.config.v1+json")
    );
    assert_eq!(
        config_response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok()),
        Some(config_digest.as_str())
    );
    assert_eq!(
        axum::body::to_bytes(config_response.into_body(), usize::MAX)
            .await
            .expect("config body"),
        config
    );

    let layer_response = router
        .oneshot(
            Request::get(format!(
                "/mirror/docker/official/v2/library/demo/blobs/{layer_digest}"
            ))
            .body(Body::empty())
            .expect("layer request"),
        )
        .await
        .expect("layer response");
    assert_eq!(layer_response.status(), StatusCode::OK);
    assert_eq!(
        layer_response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.oci.image.layer.v1.tar")
    );
    assert_eq!(
        axum::body::to_bytes(layer_response.into_body(), usize::MAX)
            .await
            .expect("layer body"),
        layer
    );

    assert_eq!(
        *paths.lock().expect("path observations"),
        vec![
            "/v2/library/demo/manifests/latest".to_owned(),
            format!("/v2/library/demo/manifests/{image_digest}"),
            format!("/v2/library/demo/blobs/{config_digest}"),
            format!("/v2/library/demo/blobs/{layer_digest}"),
        ]
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

#[allow(clippy::too_many_arguments)]
async fn start_registry(
    index: Vec<u8>,
    index_digest: String,
    image_manifest: Vec<u8>,
    image_digest: String,
    config: Vec<u8>,
    config_digest: String,
    layer: Vec<u8>,
    layer_digest: String,
    paths: Arc<Mutex<Vec<String>>>,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(move |OriginalUri(uri): OriginalUri| {
            let path = uri.path().to_owned();
            let index = index.clone();
            let index_digest = index_digest.clone();
            let image_manifest = image_manifest.clone();
            let image_digest = image_digest.clone();
            let config = config.clone();
            let config_digest = config_digest.clone();
            let layer = layer.clone();
            let layer_digest = layer_digest.clone();
            let paths = paths.clone();
            async move {
                paths.lock().expect("path observations").push(path.clone());
                let (body, content_type, digest) = match path.as_str() {
                    "/v2/library/demo/manifests/latest" => (
                        index,
                        "application/vnd.oci.image.index.v1+json",
                        index_digest,
                    ),
                    value if value == format!("/v2/library/demo/manifests/{image_digest}") => (
                        image_manifest,
                        "application/vnd.oci.image.manifest.v1+json",
                        image_digest,
                    ),
                    value if value == format!("/v2/library/demo/blobs/{config_digest}") => (
                        config,
                        "application/vnd.oci.image.config.v1+json",
                        config_digest,
                    ),
                    value if value == format!("/v2/library/demo/blobs/{layer_digest}") => (
                        layer,
                        "application/vnd.oci.image.layer.v1.tar",
                        layer_digest,
                    ),
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

fn sha256_digest(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}
