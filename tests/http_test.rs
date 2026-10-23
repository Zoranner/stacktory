use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header::CONTENT_LENGTH},
    response::IntoResponse,
    routing::get,
};
use sha2::{Digest, Sha256};
use stacktory::{
    Config, DockerConfig, DockerRegistryEndpoint, EcosystemConfig, NugetConfig, ProxyConfig,
    RetentionConfig, RustupConfig, ServerConfig, SourceConfig, SourceGraph, SourceType,
    StorageConfig, UpmConfig, VscodeConfig,
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
async fn landing_page_links_to_docs_and_serves_its_assets() {
    let data_dir = tempdir().expect("data directory");
    let log_dir = tempdir().expect("log directory");
    let service = rustup_service(data_dir.path().to_path_buf(), log_dir.path().to_path_buf());
    let router = router_with_rustup(Arc::new(service));

    let page = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("landing request"),
        )
        .await
        .expect("landing response");
    assert_eq!(page.status(), StatusCode::OK);
    let body = axum::body::to_bytes(page.into_body(), usize::MAX)
        .await
        .expect("landing body");
    let html = String::from_utf8(body.to_vec()).expect("landing UTF-8");
    assert!(html.contains("<title>Stacktory</title>"));
    assert!(html.contains("href=\"/docs\""));
    assert!(html.contains("https://pkg.rd.kim"));
    assert!(html.contains("href=\"/theme.css\""));
    assert!(html.contains("href=\"/index.css\""));
    assert!(html.contains("\"three\": \"/vendor/three.module.js\""));
    assert!(html.contains("\"three/addons/\": \"/vendor/three-addons/\""));
    assert!(html.contains("type=\"module\" src=\"/index.js\""));

    for (path, content_type) in [
        ("/theme.css", "text/css; charset=utf-8"),
        ("/index.css", "text/css; charset=utf-8"),
        ("/index.js", "text/javascript; charset=utf-8"),
        ("/vendor/three.min.js", "text/javascript; charset=utf-8"),
        ("/vendor/three.module.js", "text/javascript; charset=utf-8"),
        (
            "/vendor/three-addons/postprocessing/EffectComposer.js",
            "text/javascript; charset=utf-8",
        ),
        (
            "/vendor/three-addons/postprocessing/RenderPass.js",
            "text/javascript; charset=utf-8",
        ),
        (
            "/vendor/three-addons/postprocessing/UnrealBloomPass.js",
            "text/javascript; charset=utf-8",
        ),
        (
            "/vendor/three-addons/postprocessing/OutputPass.js",
            "text/javascript; charset=utf-8",
        ),
        (
            "/vendor/three-addons/postprocessing/Pass.js",
            "text/javascript; charset=utf-8",
        ),
        (
            "/vendor/three-addons/postprocessing/ShaderPass.js",
            "text/javascript; charset=utf-8",
        ),
        (
            "/vendor/three-addons/postprocessing/MaskPass.js",
            "text/javascript; charset=utf-8",
        ),
        (
            "/vendor/three-addons/shaders/CopyShader.js",
            "text/javascript; charset=utf-8",
        ),
        (
            "/vendor/three-addons/shaders/LuminosityHighPassShader.js",
            "text/javascript; charset=utf-8",
        ),
        (
            "/vendor/three-addons/shaders/OutputShader.js",
            "text/javascript; charset=utf-8",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::get(path)
                    .body(Body::empty())
                    .expect("asset request"),
            )
            .await
            .expect("asset response");
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some(content_type),
            "{path}"
        );
    }
}

#[tokio::test]
async fn source_config_page_serves_external_assets() {
    let data_dir = tempdir().expect("data directory");
    let log_dir = tempdir().expect("log directory");
    let service = rustup_service(data_dir.path().to_path_buf(), log_dir.path().to_path_buf());
    let router = router_with_rustup(Arc::new(service));

    let page = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/docs")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("docs page request"),
        )
        .await
        .expect("docs page response");
    assert_eq!(page.status(), StatusCode::OK);
    let body = axum::body::to_bytes(page.into_body(), usize::MAX)
        .await
        .expect("docs page body");
    let html = String::from_utf8(body.to_vec()).expect("docs page UTF-8");
    assert!(html.contains("href=\"/theme.css?v="));
    assert!(html.contains("href=\"/docs/index.css?v="));
    assert!(html.contains("src=\"/docs/index.js?v="));

    for (path, content_type) in [
        ("/theme.css", "text/css; charset=utf-8"),
        ("/docs/index.css", "text/css; charset=utf-8"),
        ("/docs/index.js", "text/javascript; charset=utf-8"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::get(path)
                    .body(Body::empty())
                    .expect("docs asset request"),
            )
            .await
            .expect("docs asset response");
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some(content_type),
            "{path}"
        );
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn removed_vscode_script_endpoint_returns_not_found() {
    let data_dir = tempdir().expect("data directory");
    let log_dir = tempdir().expect("log directory");
    let service = rustup_service(data_dir.path().to_path_buf(), log_dir.path().to_path_buf());

    let response = router_with_rustup(Arc::new(service))
        .oneshot(
            Request::get("/tools/vscode-gallery.ps1")
                .body(Body::empty())
                .expect("removed script request"),
        )
        .await
        .expect("removed script response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn source_config_pages_use_the_request_host_and_stack_title() {
    let data_dir = tempdir().expect("data directory");
    let log_dir = tempdir().expect("log directory");
    let service = rustup_service(data_dir.path().to_path_buf(), log_dir.path().to_path_buf());
    let router = router_with_rustup(Arc::new(service));

    let index = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/docs")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("docs request"),
        )
        .await
        .expect("docs response");
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(
        index
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let body = axum::body::to_bytes(index.into_body(), usize::MAX)
        .await
        .expect("docs body");
    let html = String::from_utf8(body.to_vec()).expect("docs UTF-8");
    assert!(html.contains("<title>Stacktory | 文档</title>"));
    assert!(html.contains("href=\"/docs/cargo\""));
    assert!(html.contains(">技术栈</h2>"));
    assert!(html.contains("class=\"doc-card\""));
    assert!(!html.contains("使用说明"));
    assert!(html.contains("https://pkg.rd.kim"));

    let detail = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/docs/cargo")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("cargo detail request"),
        )
        .await
        .expect("cargo detail response");
    assert_eq!(detail.status(), StatusCode::OK);
    let body = axum::body::to_bytes(detail.into_body(), usize::MAX)
        .await
        .expect("cargo detail body");
    let html = String::from_utf8(body.to_vec()).expect("cargo detail UTF-8");
    assert!(html.contains("<title>Stacktory | 文档 | Cargo</title>"));
    assert!(html.contains("https://pkg.rd.kim/mirror/cargo/group/"));
    assert!(html.contains("data-copy-target=\"prompt-cargo\""));
    assert!(!html.contains("{base_url}"));

    let markdown = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/docs/cargo/index.md")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("cargo markdown request"),
        )
        .await
        .expect("cargo markdown response");
    assert_eq!(markdown.status(), StatusCode::OK);
    assert_eq!(
        markdown
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/markdown; charset=utf-8")
    );
    let body = axum::body::to_bytes(markdown.into_body(), usize::MAX)
        .await
        .expect("cargo markdown body");
    let markdown = String::from_utf8(body.to_vec()).expect("cargo markdown UTF-8");
    assert!(markdown.contains("# Stacktory | 文档 | Cargo"));
    assert!(markdown.contains("https://pkg.rd.kim/mirror/cargo/group/"));

    let docs_markdown = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/docs.md")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("docs markdown request"),
        )
        .await
        .expect("docs markdown response");
    assert_eq!(docs_markdown.status(), StatusCode::OK);
    let body = axum::body::to_bytes(docs_markdown.into_body(), usize::MAX)
        .await
        .expect("docs markdown body");
    let markdown = String::from_utf8(body.to_vec()).expect("docs markdown UTF-8");
    assert!(markdown.starts_with("# Stacktory | 文档"));
    assert!(markdown.contains("## Rust"));
    assert!(!markdown.contains("使用说明"));

    let unknown = router
        .oneshot(
            Request::get("/docs/unknown")
                .body(Body::empty())
                .expect("unknown section request"),
        )
        .await
        .expect("unknown section response");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn source_config_pages_fall_back_to_the_configured_public_url() {
    let data_dir = tempdir().expect("data directory");
    let log_dir = tempdir().expect("log directory");
    let service = rustup_service(data_dir.path().to_path_buf(), log_dir.path().to_path_buf());

    let response = router_with_rustup(Arc::new(service))
        .oneshot(
            Request::get("/docs/cargo")
                .body(Body::empty())
                .expect("fallback docs request"),
        )
        .await
        .expect("fallback docs response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("fallback docs body");
    let html = String::from_utf8(body.to_vec()).expect("fallback docs UTF-8");
    assert!(html.contains("http://127.0.0.1:18761/mirror/cargo/group/"));
}

#[tokio::test]
async fn source_config_pages_preserve_the_configured_public_path() {
    let data_dir = tempdir().expect("data directory");
    let log_dir = tempdir().expect("log directory");
    let service = rustup_service_with_public_url(
        data_dir.path().to_path_buf(),
        log_dir.path().to_path_buf(),
        Url::parse("http://127.0.0.1:18761/artifacts/").expect("public URL"),
    );

    let router = router_with_rustup(Arc::new(service));
    let index = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/docs")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("prefixed docs index request"),
        )
        .await
        .expect("prefixed docs index response");
    assert_eq!(index.status(), StatusCode::OK);
    let body = axum::body::to_bytes(index.into_body(), usize::MAX)
        .await
        .expect("prefixed docs index body");
    let html = String::from_utf8(body.to_vec()).expect("prefixed docs index UTF-8");
    assert!(html.contains("href=\"/artifacts/docs/cargo\""));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/docs/cargo")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("prefixed docs request"),
        )
        .await
        .expect("prefixed docs response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("prefixed docs body");
    let html = String::from_utf8(body.to_vec()).expect("prefixed docs UTF-8");
    assert!(html.contains("https://pkg.rd.kim/artifacts/mirror/cargo/group/"));
    assert!(html.contains("href=\"/artifacts/docs\""));
    assert!(html.contains("href=\"/artifacts/docs/cargo\""));
}

#[tokio::test]
async fn readiness_checks_data_and_log_directories() {
    let data_dir = tempdir().expect("data directory");
    let log_dir = tempdir().expect("log directory");

    let writable = rustup_service(data_dir.path().to_path_buf(), log_dir.path().to_path_buf());
    let response = router_with_rustup(Arc::new(writable))
        .oneshot(
            Request::get("/readyz")
                .body(Body::empty())
                .expect("readiness request"),
        )
        .await
        .expect("readiness response");
    assert_eq!(response.status(), StatusCode::OK);

    let blocked_data = data_dir.path().join("blocked-data");
    fs::write(&blocked_data, b"not a directory").expect("data blocker");
    let blocked_data_service = rustup_service(blocked_data, log_dir.path().to_path_buf());
    let response = router_with_rustup(Arc::new(blocked_data_service))
        .oneshot(
            Request::get("/readyz")
                .body(Body::empty())
                .expect("blocked data request"),
        )
        .await
        .expect("blocked data response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let blocked_log = log_dir.path().join("blocked-log");
    fs::write(&blocked_log, b"not a directory").expect("log blocker");
    let blocked_log_service = rustup_service(data_dir.path().to_path_buf(), blocked_log);
    let response = router_with_rustup(Arc::new(blocked_log_service))
        .oneshot(
            Request::get("/readyz")
                .body(Body::empty())
                .expect("blocked log request"),
        )
        .await
        .expect("blocked log response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn rustup_direct_source_not_found_preserves_the_upstream_status() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let upstream = Router::new().route(
        "/{*path}",
        get(|| async { StatusCode::NOT_FOUND.into_response() }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream)
            .await
            .expect("upstream server");
    });
    let data_dir = tempdir().expect("temporary directory");
    let service = RustupService::new(Config {
        server: ServerConfig {
            address: "127.0.0.1".parse().expect("address"),
            port: 18761,
            public_url: Url::parse("http://127.0.0.1:18761").expect("public URL"),
        },
        storage: StorageConfig {
            data_dir: data_dir.path().to_path_buf(),
            log_dir: PathBuf::from("logs"),
        },
        proxy: ProxyConfig::default(),
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![source("test", format!("http://{address}"))])
                .expect("source graph"),
        },
        cargo: None,
        vscode: None,
        docker: None,
        npm: None,
        nuget: None,
        upm: None,
    })
    .expect("service");

    let response = router_with_rustup(Arc::new(service))
        .oneshot(
            Request::get("/mirror/rustup/test/dist/channel-rust-stable.toml")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
        server: ServerConfig {
            address: "127.0.0.1".parse().expect("address"),
            port: 18761,
            public_url: Url::parse("http://127.0.0.1:18761").expect("public URL"),
        },
        storage: StorageConfig {
            data_dir: PathBuf::from(data_dir.path()),
            log_dir: PathBuf::from("logs"),
        },
        proxy: ProxyConfig::default(),
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
        response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        Some(artifact.len().to_string())
    );
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body"),
        artifact
    );
    assert_eq!(artifact_requests.load(Ordering::SeqCst), 1);
    let temporary_files = fs::read_dir(data_dir.path().join("tmp"))
        .expect("temporary directory")
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("rustup-") && name.ends_with(".part")
        })
        .count();
    assert_eq!(temporary_files, 0);
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
        server: ServerConfig {
            address: "127.0.0.1".parse().expect("address"),
            port: 18761,
            public_url: Url::parse("http://127.0.0.1:18761").expect("public URL"),
        },
        storage: StorageConfig {
            data_dir: data_dir.path().to_path_buf(),
            log_dir: PathBuf::from("logs"),
        },
        proxy: ProxyConfig::default(),
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
        server: ServerConfig {
            address: "127.0.0.1".parse().expect("address"),
            port: 18761,
            public_url: Url::parse("http://127.0.0.1:18761").expect("public URL"),
        },
        storage: StorageConfig {
            data_dir: data_dir.path().to_path_buf(),
            log_dir: PathBuf::from("logs"),
        },
        proxy: ProxyConfig::default(),
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
        server: ServerConfig {
            address: "127.0.0.1".parse().expect("address"),
            port: 18761,
            public_url: Url::parse("http://127.0.0.1:18761").expect("public URL"),
        },
        storage: StorageConfig {
            data_dir: data_dir.path().to_path_buf(),
            log_dir: PathBuf::from("logs"),
        },
        proxy: ProxyConfig::default(),
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
        server: ServerConfig {
            address: "127.0.0.1".parse().expect("address"),
            port: 18761,
            public_url: Url::parse("http://127.0.0.1:18761").expect("public URL"),
        },
        storage: StorageConfig {
            data_dir: data_dir.path().to_path_buf(),
            log_dir: PathBuf::from("logs"),
        },
        proxy: ProxyConfig::default(),
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
        server: ServerConfig {
            address: "127.0.0.1".parse().expect("address"),
            port: 18761,
            public_url: Url::parse("http://127.0.0.1:18761").expect("public URL"),
        },
        storage: StorageConfig {
            data_dir: data_dir.path().to_path_buf(),
            log_dir: PathBuf::from("logs"),
        },
        proxy: ProxyConfig::default(),
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

fn rustup_service(data_dir: PathBuf, log_dir: PathBuf) -> RustupService {
    rustup_service_with_public_url(
        data_dir,
        log_dir,
        Url::parse("http://127.0.0.1:18761").expect("public URL"),
    )
}

fn rustup_service_with_public_url(
    data_dir: PathBuf,
    log_dir: PathBuf,
    public_url: Url,
) -> RustupService {
    RustupService::new(Config {
        server: ServerConfig {
            address: "127.0.0.1".parse().expect("address"),
            port: 18761,
            public_url,
        },
        storage: StorageConfig { data_dir, log_dir },
        proxy: ProxyConfig::default(),
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![source("test", "http://127.0.0.1:1")])
                .expect("source graph"),
        },
        cargo: Some(EcosystemConfig {
            sources: SourceGraph::new(vec![
                source("official", "https://index.crates.io"),
                group("group", &["official"]),
            ])
            .expect("cargo sources"),
        }),
        vscode: Some(VscodeConfig {
            sources: SourceGraph::new(vec![
                source(
                    "official",
                    "https://marketplace.visualstudio.com/_apis/public/gallery",
                ),
                group("group", &["official"]),
            ])
            .expect("vscode sources"),
        }),
        docker: Some(DockerConfig {
            sources: SourceGraph::new(vec![
                source("official", "https://registry-1.docker.io"),
                group("group", &["official"]),
            ])
            .expect("docker sources"),
            registry_endpoints: BTreeMap::from([(
                "group".to_owned(),
                DockerRegistryEndpoint {
                    listen_port: 19000,
                    public_url: Url::parse("http://127.0.0.1:19000").expect("Docker registry URL"),
                },
            )]),
        }),
        npm: Some(EcosystemConfig {
            sources: SourceGraph::new(vec![
                source("npmjs", "https://registry.npmjs.org"),
                group("group", &["npmjs"]),
            ])
            .expect("npm sources"),
        }),
        nuget: Some(NugetConfig {
            sources: SourceGraph::new(vec![
                source("official", "https://api.nuget.org/v3/index.json"),
                group("group", &["official"]),
            ])
            .expect("nuget sources"),
        }),
        upm: Some(UpmConfig {
            sources: SourceGraph::new(vec![
                source("unity", "https://packages.unity.com"),
                source("openupm", "https://package.openupm.com"),
                group("group", &["unity", "openupm"]),
            ])
            .expect("upm sources"),
        }),
    })
    .expect("service")
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
