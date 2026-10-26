use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Router,
    body::Body,
    extract::Query,
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, ecosystems::docker::DockerAdapter,
    http::router_with_registry, upstream::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[derive(Deserialize)]
struct TokenQuery {
    service: String,
    scope: String,
}

#[tokio::test]
async fn docker_adapter_resolves_anonymous_bearer_challenge_server_side() {
    let manifest =
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json"}"#;
    let digest = format!("sha256:{:x}", Sha256::digest(manifest));
    let token_hits = Arc::new(AtomicUsize::new(0));
    let manifest_hits = Arc::new(AtomicUsize::new(0));
    let upstream = start_bearer_registry(
        manifest.to_vec(),
        digest.clone(),
        token_hits.clone(),
        manifest_hits.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    let response = router
        .oneshot(
            Request::get("/mirror/docker/official/v2/library/alpine/manifests/latest")
                .header(header::ACCEPT, "application/vnd.oci.image.manifest.v1+json")
                .body(Body::empty())
                .expect("manifest request"),
        )
        .await
        .expect("manifest response");

    assert_eq!(response.status(), StatusCode::OK);
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
        manifest.to_vec()
    );
    assert_eq!(token_hits.load(Ordering::SeqCst), 1);
    assert_eq!(manifest_hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn docker_adapter_follows_a_safe_blob_redirect_server_side() {
    let layer = b"redirected layer".to_vec();
    let layer_digest = format!("sha256:{:x}", Sha256::digest(&layer));
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{layer_digest}","size":{size}}},"layers":[]}}"#,
        size = layer.len(),
    )
    .into_bytes();
    let manifest_digest = format!("sha256:{:x}", Sha256::digest(&manifest));
    let blob_hits = Arc::new(AtomicUsize::new(0));
    let redirect_hits = Arc::new(AtomicUsize::new(0));
    let upstream = start_redirect_registry(
        manifest,
        manifest_digest,
        layer,
        layer_digest.clone(),
        blob_hits.clone(),
        redirect_hits.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = docker_router(upstream, data_dir.path());

    let manifest_response = router
        .clone()
        .oneshot(
            Request::get("/mirror/docker/official/v2/library/alpine/manifests/latest")
                .body(Body::empty())
                .expect("manifest request"),
        )
        .await
        .expect("manifest response");
    assert_eq!(manifest_response.status(), StatusCode::OK);

    let blob_response = router
        .oneshot(
            Request::get(format!(
                "/mirror/docker/official/v2/library/alpine/blobs/{layer_digest}"
            ))
            .body(Body::empty())
            .expect("blob request"),
        )
        .await
        .expect("blob response");

    assert_eq!(blob_response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(blob_response.into_body(), usize::MAX)
            .await
            .expect("blob body"),
        b"redirected layer".to_vec()
    );
    assert_eq!(blob_hits.load(Ordering::SeqCst), 1);
    assert_eq!(redirect_hits.load(Ordering::SeqCst), 1);
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

async fn start_bearer_registry(
    manifest: Vec<u8>,
    digest: String,
    token_hits: Arc<AtomicUsize>,
    manifest_hits: Arc<AtomicUsize>,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let token_path = format!("http://{address}/token");
    let router = Router::new()
        .route(
            "/token",
            get(move |Query(query): Query<TokenQuery>| {
                let token_hits = token_hits.clone();
                async move {
                    token_hits.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(query.service, "registry.test");
                    assert_eq!(query.scope, "repository:library/alpine:pull");
                    r#"{"token":"anonymous-pull-token"}"#
                }
            }),
        )
        .route(
            "/{*path}",
            get(
                move |headers: HeaderMap, uri: axum::http::Uri| {
                    let manifest = manifest.clone();
                    let digest = digest.clone();
                    let token_path = token_path.clone();
                    let manifest_hits = manifest_hits.clone();
                    async move {
                        if uri.path() != "/v2/library/alpine/manifests/latest" {
                            return StatusCode::NOT_FOUND.into_response();
                        }
                        manifest_hits.fetch_add(1, Ordering::SeqCst);
                        if headers.get(header::AUTHORIZATION)
                            != Some(&HeaderValue::from_static("Bearer anonymous-pull-token"))
                        {
                            let mut response = StatusCode::UNAUTHORIZED.into_response();
                            response.headers_mut().insert(
                                header::WWW_AUTHENTICATE,
                                HeaderValue::from_str(&format!(
                                    "Bearer realm=\"{token_path}\",service=\"registry.test\",scope=\"repository:library/alpine:pull\""
                                ))
                                .expect("challenge header"),
                            );
                            return response;
                        }
                        let mut response = (StatusCode::OK, manifest).into_response();
                        response.headers_mut().insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static(
                                "application/vnd.oci.image.manifest.v1+json",
                            ),
                        );
                        response.headers_mut().insert(
                            header::HeaderName::from_static("docker-content-digest"),
                            HeaderValue::from_str(&digest).expect("digest header"),
                        );
                        response
                    }
                },
            ),
        );
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("upstream server");
    });
    Url::parse(&format!("http://{address}")).expect("upstream URL")
}

async fn start_redirect_registry(
    manifest: Vec<u8>,
    manifest_digest: String,
    layer: Vec<u8>,
    layer_digest: String,
    blob_hits: Arc<AtomicUsize>,
    redirect_hits: Arc<AtomicUsize>,
) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route(
        "/{*path}",
        get(move |uri: axum::http::Uri| {
            let manifest = manifest.clone();
            let manifest_digest = manifest_digest.clone();
            let layer = layer.clone();
            let layer_digest = layer_digest.clone();
            let blob_hits = blob_hits.clone();
            let redirect_hits = redirect_hits.clone();
            async move {
                if uri.path() == "/v2/library/alpine/manifests/latest" {
                    let mut response = (StatusCode::OK, manifest).into_response();
                    response.headers_mut().insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/vnd.oci.image.manifest.v1+json"),
                    );
                    response.headers_mut().insert(
                        header::HeaderName::from_static("docker-content-digest"),
                        HeaderValue::from_str(&manifest_digest).expect("digest header"),
                    );
                    return response;
                }
                if uri.path() == format!("/v2/library/alpine/blobs/{layer_digest}") {
                    blob_hits.fetch_add(1, Ordering::SeqCst);
                    return (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(header::LOCATION, "/objects/layer")],
                    )
                        .into_response();
                }
                if uri.path() == "/objects/layer" {
                    redirect_hits.fetch_add(1, Ordering::SeqCst);
                    let mut response = (StatusCode::OK, layer).into_response();
                    response.headers_mut().insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/octet-stream"),
                    );
                    response.headers_mut().insert(
                        header::HeaderName::from_static("docker-content-digest"),
                        HeaderValue::from_str(&layer_digest).expect("digest header"),
                    );
                    return response;
                }
                StatusCode::NOT_FOUND.into_response()
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
