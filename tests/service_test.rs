use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};
use sha2::{Digest, Sha256};
use stacktory::{
    Config, RetentionConfig, RustupConfig, SourceConfig, SourceGraph, SourceType, rustup::DistPath,
    service::RustupService,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use url::Url;

const SOURCE_MANIFEST: &str = r#"manifest-version = "2"
[pkg.rust.target.x86_64-pc-windows-msvc]
url = "http://127.0.0.1:PORT/dist/2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz"
hash = "6ea0350d546321e56d11806b9e0ffd2d6d48a4e96147620caecfb5e79b0e4f90"
"#;

#[tokio::test]
async fn refresh_publishes_a_rewritten_manifest_snapshot() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let manifest = SOURCE_MANIFEST.replace("PORT", &address.port().to_string());
    let checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(manifest.as_bytes())
    );
    let router = Router::new()
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
                let checksum = checksum.clone();
                async move { checksum }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });

    let directory = tempdir().expect("temporary directory");
    let service = RustupService::new(Config {
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: PathBuf::from(directory.path()),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        proxy_url: None,
        proxy_no_proxy: None,
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![SourceConfig {
                name: "test".to_owned(),
                source_type: SourceType::Proxy,
                base_url: Some(Url::parse(&format!("http://{address}")).expect("upstream")),
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
    let snapshot = service.manifest("test", "stable").expect("snapshot");

    assert!(
        snapshot
            .manifest
            .contains("/mirror/rustup/test/dist/stable/2026-08-20/rustc-")
    );
    assert!(
        std::fs::read_to_string(directory.path().join("audit/events.jsonl"))
            .expect("audit log")
            .contains("manifest_published")
    );
}

#[tokio::test]
async fn published_snapshot_renders_for_a_new_root_url_without_refreshing() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let (upstream, _) = start_refresh_upstream(refreshes.clone()).await;
    let directory = tempdir().expect("temporary directory");
    let sources = vec![source("test", upstream.as_str())];
    let first = service_with_sources_and_root(
        sources.clone(),
        directory.path().to_path_buf(),
        Url::parse("http://127.0.0.1:18761").expect("first root URL"),
    );
    first
        .refresh("test", "stable")
        .await
        .expect("first refresh");
    let first_snapshot = first.manifest("test", "stable").expect("first snapshot");
    assert!(
        first_snapshot
            .manifest
            .contains("http://127.0.0.1:18761/mirror/rustup/test/dist/stable/")
    );

    let second = service_with_sources_and_root(
        sources,
        directory.path().to_path_buf(),
        Url::parse("https://pkg2.example").expect("second root URL"),
    );
    let second_snapshot = second.manifest("test", "stable").expect("second snapshot");

    assert!(
        second_snapshot
            .manifest
            .contains("https://pkg2.example/mirror/rustup/test/dist/stable/")
    );
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn group_manifest_refresh_falls_back_after_member_404() {
    let first = start_not_found_upstream().await;
    let (second, _) = start_refresh_upstream(Arc::new(AtomicUsize::new(0))).await;
    let data_dir = tempdir().expect("temporary directory");
    let service = service_with_sources(
        vec![
            source("first", first.as_str()),
            source("second", second.as_str()),
            group("group", &["first", "second"]),
        ],
        data_dir.path().to_path_buf(),
    );

    service
        .refresh("group", "stable")
        .await
        .expect("group refresh");
    let snapshot = service.manifest("group", "stable").expect("snapshot");

    assert_eq!(snapshot.source_name, "second");
}

#[tokio::test]
async fn group_manifest_refresh_falls_back_when_member_checksum_is_missing() {
    let first = start_manifest_without_checksum().await;
    let (second, _) = start_refresh_upstream(Arc::new(AtomicUsize::new(0))).await;
    let data_dir = tempdir().expect("temporary directory");
    let service = service_with_sources(
        vec![
            source("first", first.as_str()),
            source("second", second.as_str()),
            group("group", &["first", "second"]),
        ],
        data_dir.path().to_path_buf(),
    );

    service
        .refresh("group", "stable")
        .await
        .expect("group refresh");
    let snapshot = service.manifest("group", "stable").expect("snapshot");

    assert_eq!(snapshot.source_name, "second");
}

#[tokio::test]
async fn artifact_request_fetches_a_published_artifact_and_reuses_the_verified_cache() {
    let artifact = b"verified artifact";
    let expected_hash = format!("{:x}", Sha256::digest(artifact));
    let requests = Arc::new(AtomicUsize::new(0));
    let (upstream, path) =
        start_artifact_upstream(artifact.to_vec(), expected_hash, requests.clone()).await;
    let data_dir = tempdir().expect("temporary directory");
    let service = service_for(upstream, data_dir.path().to_path_buf());

    service.refresh("test", "stable").await.expect("refresh");
    let first = service
        .artifact_for_request("test", "stable", &path)
        .await
        .expect("first artifact request");
    let second = service
        .artifact_for_request("test", "stable", &path)
        .await
        .expect("cached artifact request");

    assert_eq!(first, artifact);
    assert_eq!(second, artifact);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let audit =
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(audit.contains("artifact_cached"));
    assert!(audit.contains("artifact_cache_hit"));
}

#[tokio::test]
async fn concurrent_artifact_requests_share_one_upstream_fetch() {
    let artifact = b"shared verified artifact";
    let expected_hash = format!("{:x}", Sha256::digest(artifact));
    let requests = Arc::new(AtomicUsize::new(0));
    let (upstream, path) =
        start_artifact_upstream(artifact.to_vec(), expected_hash, requests.clone()).await;
    let data_dir = tempdir().expect("temporary directory");
    let service = service_for(upstream, data_dir.path().to_path_buf());

    service.refresh("test", "stable").await.expect("refresh");
    let (first, second) = tokio::join!(
        service.artifact_for_request("test", "stable", &path),
        service.artifact_for_request("test", "stable", &path),
    );

    assert_eq!(first.expect("first artifact request"), artifact);
    assert_eq!(second.expect("second artifact request"), artifact);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn artifact_cache_is_shared_by_group_and_its_proxy() {
    let artifact = b"shared verified artifact";
    let expected_hash = format!("{:x}", Sha256::digest(artifact));
    let requests = Arc::new(AtomicUsize::new(0));
    let (upstream, path) =
        start_artifact_upstream(artifact.to_vec(), expected_hash, requests.clone()).await;
    let data_dir = tempdir().expect("temporary directory");
    let service = RustupService::new(Config {
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: data_dir.path().to_path_buf(),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        proxy_url: None,
        proxy_no_proxy: None,
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![
                source("official", upstream.as_str()),
                group("group", &["official"]),
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

    service
        .refresh("group", "stable")
        .await
        .expect("group refresh");
    service
        .artifact_for_request("group", "stable", &path)
        .await
        .expect("group artifact");
    service
        .refresh("official", "stable")
        .await
        .expect("proxy refresh");
    service
        .artifact_for_request("official", "stable", &path)
        .await
        .expect("proxy artifact");

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(
        data_dir
            .path()
            .join("stores/rustup/artifacts/official/objects")
            .join(path.as_str())
            .exists()
    );
}

#[tokio::test]
async fn proxy_manifest_can_reference_another_configured_proxy() {
    let artifact = b"artifact from a configured proxy".to_vec();
    let checksum = format!("{:x}", Sha256::digest(&artifact));
    let artifact_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("artifact listener");
    let artifact_address = artifact_listener.local_addr().expect("artifact address");
    let artifact_router = Router::new().route(
        "/dist/2026-09-03/rustc.tar.xz",
        get({
            let artifact = artifact.clone();
            move || {
                let artifact = artifact.clone();
                async move { artifact }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(artifact_listener, artifact_router)
            .await
            .expect("artifact server");
    });

    let manifest_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("manifest listener");
    let manifest_address = manifest_listener.local_addr().expect("manifest address");
    let manifest = format!(
        "manifest-version = \"2\"\n[pkg.rust.target.x86_64-pc-windows-msvc]\nurl = \"http://{artifact_address}/dist/2026-09-03/rustc.tar.xz\"\nhash = \"{checksum}\"\n"
    );
    let manifest_checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(manifest.as_bytes())
    );
    let manifest_router = Router::new()
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
            get(move || {
                let manifest_checksum = manifest_checksum.clone();
                async move { manifest_checksum }
            }),
        );
    tokio::spawn(async move {
        axum::serve(manifest_listener, manifest_router)
            .await
            .expect("manifest server");
    });

    let data_dir = tempdir().expect("temporary directory");
    let service = RustupService::new(Config {
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: data_dir.path().to_path_buf(),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        proxy_url: None,
        proxy_no_proxy: None,
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![
                source("mirror", format!("http://{manifest_address}")),
                source("official", format!("http://{artifact_address}")),
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
    let path = DistPath::parse("2026-09-03/rustc.tar.xz").expect("artifact path");

    service
        .refresh("mirror", "stable")
        .await
        .expect("mirror refresh");
    assert_eq!(
        service
            .artifact_for_request("mirror", "stable", &path)
            .await
            .expect("artifact"),
        artifact
    );
}

#[tokio::test]
async fn artifact_request_uses_the_source_matched_by_manifest_url() {
    let artifact = b"artifact from the matched source".to_vec();
    let checksum = format!("{:x}", Sha256::digest(&artifact));
    let artifact_hits = Arc::new(AtomicUsize::new(0));
    let artifact_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("artifact listener");
    let artifact_address = artifact_listener.local_addr().expect("artifact address");
    let artifact_hits_for_route = artifact_hits.clone();
    let artifact_for_route = artifact.clone();
    let artifact_router = Router::new().route(
        "/dist/2026-09-03/rustc.tar.xz",
        get(move || {
            let artifact_hits = artifact_hits_for_route.clone();
            let artifact = artifact_for_route.clone();
            async move {
                artifact_hits.fetch_add(1, Ordering::SeqCst);
                artifact
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(artifact_listener, artifact_router)
            .await
            .expect("artifact server");
    });

    let manifest_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("manifest listener");
    let manifest_address = manifest_listener.local_addr().expect("manifest address");
    let manifest = format!(
        "manifest-version = \"2\"\n[pkg.rust.target.x86_64-pc-windows-msvc]\nurl = \"http://{artifact_address}/dist/2026-09-03/rustc.tar.xz\"\nhash = \"{checksum}\"\n"
    );
    let manifest_checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(manifest.as_bytes())
    );
    let manifest_router = Router::new()
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
            get(move || {
                let manifest_checksum = manifest_checksum.clone();
                async move { manifest_checksum }
            }),
        );
    tokio::spawn(async move {
        axum::serve(manifest_listener, manifest_router)
            .await
            .expect("manifest server");
    });

    let data_dir = tempdir().expect("temporary directory");
    let service = RustupService::new(Config {
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir: data_dir.path().to_path_buf(),
        log_dir: PathBuf::from("logs"),
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        proxy_url: None,
        proxy_no_proxy: None,
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![
                source("manifest", format!("http://{manifest_address}")),
                source("artifact", format!("http://{artifact_address}")),
                group("group", &["manifest", "artifact"]),
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
    let path = DistPath::parse("2026-09-03/rustc.tar.xz").expect("artifact path");

    service.refresh("group", "stable").await.expect("refresh");
    assert_eq!(
        service
            .artifact_for_request("group", "stable", &path)
            .await
            .expect("artifact"),
        artifact
    );
    assert_eq!(artifact_hits.load(Ordering::SeqCst), 1);
    assert!(
        data_dir
            .path()
            .join("stores/rustup/artifacts/artifact/objects/2026-09-03/rustc.tar.xz")
            .exists()
    );
}

#[tokio::test]
async fn artifact_request_rejects_content_that_does_not_match_the_manifest_hash() {
    let expected_hash = format!("{:x}", Sha256::digest(b"verified artifact"));
    let requests = Arc::new(AtomicUsize::new(0));
    let (upstream, path) = start_artifact_upstream(
        b"tampered artifact".to_vec(),
        expected_hash,
        requests.clone(),
    )
    .await;
    let data_dir = tempdir().expect("temporary directory");
    let service = service_for(upstream, data_dir.path().to_path_buf());

    service.refresh("test", "stable").await.expect("refresh");
    let error = service
        .artifact_for_request("test", "stable", &path)
        .await
        .expect_err("mismatched artifact must fail");

    assert!(error.to_string().contains("does not match"));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl"))
            .expect("audit log")
            .contains("artifact_fetch_failed")
    );
    assert!(
        !data_dir
            .path()
            .join("stores/rustup/artifacts")
            .join(path.as_str())
            .exists()
    );
}

#[tokio::test]
async fn artifact_request_rejects_paths_not_referenced_by_a_published_manifest() {
    let artifact = b"verified artifact";
    let expected_hash = format!("{:x}", Sha256::digest(artifact));
    let requests = Arc::new(AtomicUsize::new(0));
    let (upstream, _) =
        start_artifact_upstream(artifact.to_vec(), expected_hash, requests.clone()).await;
    let data_dir = tempdir().expect("temporary directory");
    let service = service_for(upstream, data_dir.path().to_path_buf());
    let unknown = DistPath::parse("2026-08-20/unknown.tar.xz").expect("dist path");

    service.refresh("test", "stable").await.expect("refresh");
    let error = service
        .artifact_for_request("test", "stable", &unknown)
        .await
        .expect_err("unknown artifact must fail");

    assert!(error.to_string().contains("not referenced"));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn artifacts_follow_each_channel_snapshot_source_and_keep_separate_caches() {
    let stable_artifact = b"stable artifact";
    let beta_artifact = b"beta artifact";
    let stable_hash = format!("{:x}", Sha256::digest(stable_artifact));
    let beta_hash = format!("{:x}", Sha256::digest(beta_artifact));
    let stable_hits = Arc::new(AtomicUsize::new(0));
    let beta_hits = Arc::new(AtomicUsize::new(0));

    let first_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("first listener");
    let first_address = first_listener.local_addr().expect("first address");
    let stable_manifest = format!(
        "manifest-version = \"2\"\n[pkg.rust.target.x86_64-pc-windows-msvc]\nurl = \"http://{first_address}/dist/2026-08-20/rustc.tar.xz\"\nhash = \"{stable_hash}\"\n"
    );
    let stable_checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(stable_manifest.as_bytes())
    );
    let first_stable_hits = stable_hits.clone();
    let first = Router::new()
        .route(
            "/dist/channel-rust-stable.toml",
            get({
                let stable_manifest = stable_manifest.clone();
                move || {
                    let stable_manifest = stable_manifest.clone();
                    async move { stable_manifest }
                }
            }),
        )
        .route(
            "/dist/channel-rust-stable.toml.sha256",
            get({
                let stable_checksum = stable_checksum.clone();
                move || {
                    let stable_checksum = stable_checksum.clone();
                    async move { stable_checksum }
                }
            }),
        )
        .route(
            "/dist/channel-rust-beta.toml",
            get(|| async { axum::http::StatusCode::BAD_GATEWAY }),
        )
        .route(
            "/dist/2026-08-20/rustc.tar.xz",
            get(move || {
                let first_stable_hits = first_stable_hits.clone();
                async move {
                    first_stable_hits.fetch_add(1, Ordering::SeqCst);
                    stable_artifact.to_vec()
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
    let beta_manifest = format!(
        "manifest-version = \"2\"\n[pkg.rust.target.x86_64-pc-windows-msvc]\nurl = \"http://{second_address}/dist/2026-08-20/rustc.tar.xz\"\nhash = \"{beta_hash}\"\n"
    );
    let beta_checksum = format!(
        "{:x}  channel-rust-beta.toml\n",
        Sha256::digest(beta_manifest.as_bytes())
    );
    let second_beta_hits = beta_hits.clone();
    let second = Router::new()
        .route(
            "/dist/channel-rust-beta.toml",
            get({
                let beta_manifest = beta_manifest.clone();
                move || {
                    let beta_manifest = beta_manifest.clone();
                    async move { beta_manifest }
                }
            }),
        )
        .route(
            "/dist/channel-rust-beta.toml.sha256",
            get({
                let beta_checksum = beta_checksum.clone();
                move || {
                    let beta_checksum = beta_checksum.clone();
                    async move { beta_checksum }
                }
            }),
        )
        .route(
            "/dist/2026-08-20/rustc.tar.xz",
            get(move || {
                let second_beta_hits = second_beta_hits.clone();
                async move {
                    second_beta_hits.fetch_add(1, Ordering::SeqCst);
                    beta_artifact.to_vec()
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
        proxy_url: None,
        proxy_no_proxy: None,
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned(), "beta".to_owned()],
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

    service
        .refresh("group", "stable")
        .await
        .expect("stable refresh");
    service
        .refresh("group", "beta")
        .await
        .expect("beta refresh");

    let path = DistPath::parse("2026-08-20/rustc.tar.xz").expect("artifact path");
    assert_eq!(
        service
            .artifact_for_request("group", "stable", &path)
            .await
            .expect("stable artifact"),
        stable_artifact
    );
    assert_eq!(
        service
            .artifact_for_request("group", "stable", &path)
            .await
            .expect("cached stable artifact"),
        stable_artifact
    );
    assert_eq!(
        service
            .artifact_for_request("group", "beta", &path)
            .await
            .expect("beta artifact"),
        beta_artifact
    );
    assert_eq!(
        service
            .artifact_for_request("group", "beta", &path)
            .await
            .expect("cached beta artifact"),
        beta_artifact
    );
    assert_eq!(stable_hits.load(Ordering::SeqCst), 1);
    assert_eq!(beta_hits.load(Ordering::SeqCst), 1);
    assert!(
        data_dir
            .path()
            .join("stores/rustup/artifacts/first/objects/2026-08-20/rustc.tar.xz")
            .exists()
    );
    assert!(
        data_dir
            .path()
            .join("stores/rustup/artifacts/second/objects/2026-08-20/rustc.tar.xz")
            .exists()
    );
}

#[tokio::test]
async fn manifest_request_refreshes_an_expired_snapshot() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let (upstream, _) = start_refresh_upstream(refreshes.clone()).await;
    let data_dir = tempdir().expect("temporary directory");
    let service = service_for(upstream, data_dir.path().to_path_buf());

    service
        .refresh("test", "stable")
        .await
        .expect("initial refresh");
    expire_current_snapshot(data_dir.path(), "stable", &service);
    let refreshed = service
        .manifest_for_request("test", "stable")
        .await
        .expect("expired snapshot refresh");

    assert_eq!(refreshes.load(Ordering::SeqCst), 2);
    assert!(refreshed.published_at_unix_secs > 0);
}

#[tokio::test]
async fn manifest_request_returns_an_expired_snapshot_when_refresh_fails() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let (upstream, available) = start_refresh_upstream(refreshes.clone()).await;
    let data_dir = tempdir().expect("temporary directory");
    let service = service_for(upstream, data_dir.path().to_path_buf());

    service
        .refresh("test", "stable")
        .await
        .expect("initial refresh");
    let before_failure = service.manifest("test", "stable").expect("snapshot");
    expire_current_snapshot(data_dir.path(), "stable", &service);
    available.store(false, Ordering::SeqCst);
    let stale = service
        .manifest_for_request("test", "stable")
        .await
        .expect("stale fallback");

    assert_eq!(refreshes.load(Ordering::SeqCst), 2);
    assert_eq!(stale.checksum, before_failure.checksum);
    assert!(
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl"))
            .expect("audit log")
            .contains("manifest_refresh_failed")
    );
}

#[tokio::test]
async fn concurrent_manifest_requests_share_one_initial_refresh() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let (upstream, _) = start_refresh_upstream(refreshes.clone()).await;
    let data_dir = tempdir().expect("temporary directory");
    let service = service_for(upstream, data_dir.path().to_path_buf());

    let (first, second) = tokio::join!(
        service.manifest_for_request("test", "stable"),
        service.manifest_for_request("test", "stable"),
    );

    first.expect("first manifest request");
    second.expect("second manifest request");
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
}

async fn start_artifact_upstream(
    artifact: Vec<u8>,
    expected_hash: String,
    requests: Arc<AtomicUsize>,
) -> (Url, DistPath) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let path =
        DistPath::parse("2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz").expect("dist path");
    let manifest = format!(
        "manifest-version = \"2\"\n[pkg.rust.target.x86_64-pc-windows-msvc]\nurl = \"http://{address}/dist/{}\"\nhash = \"{expected_hash}\"\n",
        path.as_str()
    );
    let checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(manifest.as_bytes())
    );
    let router = Router::new()
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
                let checksum = checksum.clone();
                async move { checksum }
            }),
        )
        .route(
            "/dist/2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz",
            get(move || {
                let artifact = artifact.clone();
                let requests = requests.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    requests.fetch_add(1, Ordering::SeqCst);
                    artifact
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });

    (
        Url::parse(&format!("http://{address}")).expect("upstream URL"),
        path,
    )
}

async fn start_refresh_upstream(refreshes: Arc<AtomicUsize>) -> (Url, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let manifest = SOURCE_MANIFEST.replace("PORT", &address.port().to_string());
    let checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(manifest.as_bytes())
    );
    let available = Arc::new(AtomicBool::new(true));
    let manifest_available = available.clone();
    let manifest_refreshes = refreshes.clone();
    let checksum_available = available.clone();
    let router = Router::new()
        .route(
            "/dist/channel-rust-stable.toml",
            get(move || {
                let manifest = manifest.clone();
                let available = manifest_available.clone();
                let refreshes = manifest_refreshes.clone();
                async move {
                    refreshes.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    if available.load(Ordering::SeqCst) {
                        (axum::http::StatusCode::OK, manifest).into_response()
                    } else {
                        axum::http::StatusCode::BAD_GATEWAY.into_response()
                    }
                }
            }),
        )
        .route(
            "/dist/channel-rust-stable.toml.sha256",
            get(move || {
                let checksum = checksum.clone();
                let available = checksum_available.clone();
                async move {
                    if available.load(Ordering::SeqCst) {
                        (axum::http::StatusCode::OK, checksum).into_response()
                    } else {
                        axum::http::StatusCode::BAD_GATEWAY.into_response()
                    }
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });

    (
        Url::parse(&format!("http://{address}")).expect("upstream URL"),
        available,
    )
}

async fn start_not_found_upstream() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new()
        .route(
            "/dist/channel-rust-stable.toml",
            get(|| async { StatusCode::NOT_FOUND }),
        )
        .route(
            "/dist/channel-rust-stable.toml.sha256",
            get(|| async { StatusCode::NOT_FOUND }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });

    Url::parse(&format!("http://{address}")).expect("upstream URL")
}

async fn start_manifest_without_checksum() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let manifest = SOURCE_MANIFEST.replace("PORT", &address.port().to_string());
    let router = Router::new()
        .route(
            "/dist/channel-rust-stable.toml",
            get(move || {
                let manifest = manifest.clone();
                async move { manifest }
            }),
        )
        .route(
            "/dist/channel-rust-stable.toml.sha256",
            get(|| async { StatusCode::NOT_FOUND }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });

    Url::parse(&format!("http://{address}")).expect("upstream URL")
}

fn expire_current_snapshot(data_dir: &std::path::Path, channel: &str, service: &RustupService) {
    let snapshot = service.manifest("test", channel).expect("snapshot");
    std::fs::write(
        data_dir
            .join("stores/rustup/snapshots")
            .join("test")
            .join(channel)
            .join("current.toml"),
        format!(
            "revision = \"{}\"\nsource_name = \"{}\"\npublished_at_unix_secs = 0\nformat = 2\n",
            snapshot.canonical_checksum, snapshot.source_name
        ),
    )
    .expect("expire current snapshot");
}

fn service_for(upstream: Url, data_dir: PathBuf) -> RustupService {
    service_with_sources(vec![source("test", upstream.to_string())], data_dir)
}

fn service_with_sources(sources: Vec<SourceConfig>, data_dir: PathBuf) -> RustupService {
    service_with_sources_and_root(
        sources,
        data_dir,
        Url::parse("http://127.0.0.1:18761").expect("root URL"),
    )
}

fn service_with_sources_and_root(
    sources: Vec<SourceConfig>,
    data_dir: PathBuf,
    root_url: Url,
) -> RustupService {
    RustupService::new(Config {
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir,
        log_dir: PathBuf::from("logs"),
        root_url,
        proxy_url: None,
        proxy_no_proxy: None,
        retention: RetentionConfig::default(),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(sources).expect("source graph"),
        },
        cargo: None,
        vscode: None,
        docker: None,
        npm: None,
        nuget: None,
        pypi: None,
        upm: None,
    })
    .expect("service")
}

fn source(name: &str, base_url: impl Into<String>) -> SourceConfig {
    SourceConfig {
        name: name.to_owned(),
        source_type: SourceType::Proxy,
        base_url: Some(Url::parse(&base_url.into()).expect("source URL")),
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
