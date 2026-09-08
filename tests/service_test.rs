use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Router, response::IntoResponse, routing::get};
use sha2::{Digest, Sha256};
use stacktory::{
    Config, RustupConfig, SourceConfig, SourceGraph, SourceType, rustup::DistPath,
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
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
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
    })
    .expect("service");

    service.refresh("test", "stable").await.expect("refresh");
    let snapshot = service.manifest("test", "stable").expect("snapshot");

    assert!(
        snapshot
            .manifest
            .contains("/mirror/rustup/test/dist/2026-08-20/rustc-")
    );
    assert!(
        std::fs::read_to_string(directory.path().join("audit/events.jsonl"))
            .expect("audit log")
            .contains("manifest_published")
    );
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
        .artifact_for_request("test", &path)
        .await
        .expect("first artifact request");
    let second = service
        .artifact_for_request("test", &path)
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
        .artifact_for_request("test", &path)
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
            .join("rustup/artifacts")
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
        .artifact_for_request("test", &unknown)
        .await
        .expect_err("unknown artifact must fail");

    assert!(error.to_string().contains("not referenced"));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
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

fn expire_current_snapshot(data_dir: &std::path::Path, channel: &str, service: &RustupService) {
    let snapshot = service.manifest("test", channel).expect("snapshot");
    std::fs::write(
        data_dir
            .join("rustup/snapshots")
            .join("test")
            .join(channel)
            .join("current.toml"),
        format!(
            "revision = \"{}\"\nsource_name = \"{}\"\npublished_at_unix_secs = 0\n",
            snapshot.checksum, snapshot.source_name
        ),
    )
    .expect("expire current snapshot");
}

fn service_for(upstream: Url, data_dir: PathBuf) -> RustupService {
    RustupService::new(Config {
        listen_address: "127.0.0.1".parse().expect("address"),
        listen_port: 18761,
        data_dir,
        root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            sources: SourceGraph::new(vec![SourceConfig {
                name: "test".to_owned(),
                source_type: SourceType::Proxy,
                base_url: Some(upstream.clone()),
                require_https: false,
                members: vec![],
            }])
            .expect("source graph"),
        },
        cargo: None,
    })
    .expect("service")
}
