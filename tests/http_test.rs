use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use sha2::{Digest, Sha256};
use stacktory::{
    Config, RustupConfig, UpstreamConfig,
    http::{router, router_with_rustup},
    service::RustupService,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn health_route_reports_service_availability() {
    let response = router()
        .oneshot(
            Request::get("/healthz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn public_rustup_artifact_route_serves_only_a_verified_published_artifact() {
    let artifact = b"verified artifact".to_vec();
    let artifact_hash = format!("{:x}", Sha256::digest(&artifact));
    let artifact_requests = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let manifest = format!(
        "manifest-version = \"2\"\n[pkg.rust.target.x86_64-pc-windows-msvc]\nurl = \"http://{address}/dist/2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz\"\nhash = \"{artifact_hash}\"\n"
    );
    let manifest_checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(manifest.as_bytes())
    );
    let upstream_artifact = artifact.clone();
    let upstream_artifact_requests = artifact_requests.clone();
    let upstream = Router::new()
        .route(
            "/dist/channel-rust-stable.toml",
            get(move || {
                let manifest = manifest.clone();
                async move { manifest }
            }),
        )
        .route(
            "/dist/channel-rust-stable.toml.sha256",
            get(move || {
                let manifest_checksum = manifest_checksum.clone();
                async move { manifest_checksum }
            }),
        )
        .route(
            "/dist/2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz",
            get(move || {
                let artifact = upstream_artifact.clone();
                let artifact_requests = upstream_artifact_requests.clone();
                async move {
                    artifact_requests.fetch_add(1, Ordering::SeqCst);
                    artifact
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, upstream)
            .await
            .expect("upstream server");
    });

    let data_dir = tempdir().expect("temporary directory");
    let service = RustupService::new(Config {
        listen: "127.0.0.1:18761".parse::<SocketAddr>().expect("address"),
        data_dir: PathBuf::from(data_dir.path()),
        public_base_url: Url::parse("http://127.0.0.1:18761").expect("public URL"),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            upstreams: vec![UpstreamConfig {
                name: "test".to_owned(),
                base_url: Url::parse(&format!("http://{address}")).expect("upstream URL"),
                require_https: false,
            }],
        },
    })
    .expect("service");
    service.refresh("stable").await.expect("refresh");

    let response = router_with_rustup(Arc::new(service))
        .oneshot(
            Request::get("/mirror/rustup/dist/2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body"),
        artifact
    );
    assert_eq!(artifact_requests.load(Ordering::SeqCst), 1);
}
