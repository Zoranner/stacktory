use std::{
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
    Config, RetentionConfig, RustupConfig, SourceConfig, SourceGraph, SourceType,
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
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: PathBuf::from(data_dir.path()),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![SourceConfig {
                name: "test".to_owned(),
                source_type: SourceType::Proxy,
                base_url: Some(Url::parse(&format!("http://{address}")).expect("upstream URL")),
                require_https: false,
                members: vec![],
            }])
            .expect("source graph"),
        },
        cargo: None,
        vscode: None,
        docker: None,
        npm: None,
        nuget: None,
        pypi: None,
        upm: None,
    })
    .expect("service");
    service.refresh("test", "stable").await.expect("refresh");

    let response = router_with_rustup(Arc::new(service))
        .oneshot(
            Request::get(
                "/mirror/rustup/test/dist/stable/2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz",
            )
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

#[tokio::test]
async fn rustup_group_route_serves_manifest() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let manifest = "manifest-version = \"2\"\n[pkg.rust]\nversion = \"1.99.0\"\n".to_owned();
    let checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(manifest.as_bytes())
    );
    let upstream = Router::new()
        .route(
            "/dist/channel-rust-stable.toml",
            get({
                let manifest = manifest.clone();
                move || {
                    let manifest = manifest.clone();
                    async move { manifest }
                }
            }),
        )
        .route(
            "/dist/channel-rust-stable.toml.sha256",
            get({
                let checksum = checksum.clone();
                move || {
                    let checksum = checksum.clone();
                    async move { checksum }
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
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: data_dir.path().to_path_buf(),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![
                source("first", "http://127.0.0.1:1"),
                source("second", format!("http://{address}")),
                group("group", &["first", "second"]),
            ])
            .expect("source graph"),
        },
        cargo: None,
        vscode: None,
        docker: None,
        npm: None,
        nuget: None,
        pypi: None,
        upm: None,
    })
    .expect("service");

    let response = router_with_rustup(Arc::new(service))
        .oneshot(
            Request::get("/mirror/rustup/group/dist/channel-rust-stable.toml")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn rustup_group_artifact_stays_with_the_manifest_source() {
    let artifact = b"manifest source artifact";
    let artifact_hash = format!("{:x}", Sha256::digest(artifact));
    let first_artifact_hits = Arc::new(AtomicUsize::new(0));
    let second_artifact_hits = Arc::new(AtomicUsize::new(0));

    let first_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("first listener");
    let first_address = first_listener.local_addr().expect("first address");
    let manifest = format!(
        "manifest-version = \"2\"\n[pkg.rust.target.x86_64-pc-windows-msvc]\nurl = \"http://{first_address}/dist/2026-08-20/rustc.tar.xz\"\nhash = \"{artifact_hash}\"\n"
    );
    let checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(manifest.as_bytes())
    );
    let first_hits = first_artifact_hits.clone();
    let first = Router::new()
        .route(
            "/dist/channel-rust-stable.toml",
            get({
                let manifest = manifest.clone();
                move || {
                    let manifest = manifest.clone();
                    async move { manifest }
                }
            }),
        )
        .route(
            "/dist/channel-rust-stable.toml.sha256",
            get({
                let checksum = checksum.clone();
                move || {
                    let checksum = checksum.clone();
                    async move { checksum }
                }
            }),
        )
        .route(
            "/dist/2026-08-20/rustc.tar.xz",
            get(move || {
                let first_hits = first_hits.clone();
                async move {
                    first_hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::BAD_GATEWAY
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(first_listener, first)
            .await
            .expect("first server");
    });

    let second_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("second listener");
    let second_address = second_listener.local_addr().expect("second address");
    let second_hits = second_artifact_hits.clone();
    let second = Router::new().route(
        "/dist/2026-08-20/rustc.tar.xz",
        get(move || {
            let second_hits = second_hits.clone();
            async move {
                second_hits.fetch_add(1, Ordering::SeqCst);
                artifact.to_vec()
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(second_listener, second)
            .await
            .expect("second server");
    });

    let data_dir = tempdir().expect("temporary directory");
    let service = RustupService::new(Config {
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: data_dir.path().to_path_buf(),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![
                source("first", format!("http://{first_address}")),
                source("second", format!("http://{second_address}")),
                group("group", &["first", "second"]),
            ])
            .expect("source graph"),
        },
        cargo: None,
        vscode: None,
        docker: None,
        npm: None,
        nuget: None,
        pypi: None,
        upm: None,
    })
    .expect("service");
    let router = router_with_rustup(Arc::new(service));

    let manifest_response = router
        .clone()
        .oneshot(
            Request::get("/mirror/rustup/group/dist/channel-rust-stable.toml")
                .body(Body::empty())
                .expect("manifest request"),
        )
        .await
        .expect("manifest response");
    assert_eq!(manifest_response.status(), StatusCode::OK);

    let artifact_response = router
        .oneshot(
            Request::get("/mirror/rustup/group/dist/stable/2026-08-20/rustc.tar.xz")
                .body(Body::empty())
                .expect("artifact request"),
        )
        .await
        .expect("artifact response");

    assert_eq!(artifact_response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(first_artifact_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_artifact_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rustup_proxy_route_serves_only_its_proxy() {
    let sibling_hits = Arc::new(AtomicUsize::new(0));
    let sibling_hits_for_route = sibling_hits.clone();
    let sibling = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let sibling_address = sibling.local_addr().expect("sibling address");
    let sibling_router = Router::new().fallback(move || {
        let sibling_hits = sibling_hits_for_route.clone();
        async move {
            sibling_hits.fetch_add(1, Ordering::SeqCst);
            "sibling"
        }
    });
    tokio::spawn(async move {
        axum::serve(sibling, sibling_router)
            .await
            .expect("sibling server");
    });

    let data_dir = tempdir().expect("temporary directory");
    let service = RustupService::new(Config {
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: data_dir.path().to_path_buf(),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![
                source("primary", "http://127.0.0.1:1"),
                source("sibling", format!("http://{sibling_address}")),
            ])
            .expect("source graph"),
        },
        cargo: None,
        vscode: None,
        docker: None,
        npm: None,
        nuget: None,
        pypi: None,
        upm: None,
    })
    .expect("service");

    let response = router_with_rustup(Arc::new(service))
        .oneshot(
            Request::get("/mirror/rustup/primary/dist/channel-rust-stable.toml")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(sibling_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unknown_rustup_artifact_source_returns_not_found_before_snapshot_lookup() {
    let data_dir = tempdir().expect("temporary directory");
    let service = RustupService::new(Config {
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: data_dir.path().to_path_buf(),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![source("known", "http://127.0.0.1:1")])
                .expect("source graph"),
        },
        cargo: None,
        vscode: None,
        docker: None,
        npm: None,
        nuget: None,
        pypi: None,
        upm: None,
    })
    .expect("service");
    let router = router_with_rustup(Arc::new(service));

    let unknown = router
        .clone()
        .oneshot(
            Request::get("/mirror/rustup/missing/dist/stable/2026-08-20/rustc.tar.xz")
                .body(Body::empty())
                .expect("unknown source request"),
        )
        .await
        .expect("unknown source response");
    let known_without_snapshot = router
        .oneshot(
            Request::get("/mirror/rustup/known/dist/stable/2026-08-20/rustc.tar.xz")
                .body(Body::empty())
                .expect("known source request"),
        )
        .await
        .expect("known source response");

    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        known_without_snapshot.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn legacy_rustup_route_returns_not_found() {
    let artifact = b"legacy route artifact".to_vec();
    let artifact_hash = format!("{:x}", Sha256::digest(&artifact));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let manifest = format!(
        "manifest-version = \"2\"\n[pkg.rust.target.x86_64-pc-windows-msvc]\nurl = \"http://{address}/dist/2026-08-20/rustc.tar.xz\"\nhash = \"{artifact_hash}\"\n"
    );
    let checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(manifest.as_bytes())
    );
    let upstream = Router::new()
        .route(
            "/dist/channel-rust-stable.toml",
            get({
                let manifest = manifest.clone();
                move || {
                    let manifest = manifest.clone();
                    async move { manifest }
                }
            }),
        )
        .route(
            "/dist/channel-rust-stable.toml.sha256",
            get({
                let checksum = checksum.clone();
                move || {
                    let checksum = checksum.clone();
                    async move { checksum }
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
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: data_dir.path().to_path_buf(),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![source("group", format!("http://{address}"))])
                .expect("source graph"),
        },
        cargo: None,
        vscode: None,
        docker: None,
        npm: None,
        nuget: None,
        pypi: None,
        upm: None,
    })
    .expect("service");

    let service = Arc::new(service);
    service.refresh("group", "stable").await.expect("refresh");
    let response = router_with_rustup(service)
        .oneshot(
            Request::get("/mirror/rustup/dist/channel-rust-stable.toml")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

fn source(name: &str, base_url: impl Into<String>) -> SourceConfig {
    SourceConfig {
        name: name.to_owned(),
        source_type: SourceType::Proxy,
        base_url: Some(Url::parse(&base_url.into()).expect("upstream URL")),
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
