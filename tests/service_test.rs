use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{Router, routing::get};
use sha2::{Digest, Sha256};
use stacktory::{
    AccessConfig, Config, RustupConfig, UpstreamConfig, rustup::DistPath, service::RustupService,
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
        listen: "127.0.0.1:18081".parse::<SocketAddr>().expect("address"),
        data_dir: PathBuf::from(directory.path()),
        public_base_url: Url::parse("http://127.0.0.1:18081").expect("public URL"),
        access: AccessConfig {
            allow_anonymous: true,
            read_token_env: None,
        },
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            upstreams: vec![UpstreamConfig {
                name: "test".to_owned(),
                base_url: Url::parse(&format!("http://{address}")).expect("upstream"),
                require_https: false,
            }],
        },
    })
    .expect("service");

    service.refresh("stable").await.expect("refresh");
    let snapshot = service.manifest("stable").expect("snapshot");

    assert!(
        snapshot
            .manifest
            .contains("/mirror/rustup/dist/2026-08-20/rustc-")
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

    service.refresh("stable").await.expect("refresh");
    let first = service
        .artifact_for_request(&path)
        .await
        .expect("first artifact request");
    let second = service
        .artifact_for_request(&path)
        .await
        .expect("cached artifact request");

    assert_eq!(first, artifact);
    assert_eq!(second, artifact);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
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

    service.refresh("stable").await.expect("refresh");
    let error = service
        .artifact_for_request(&path)
        .await
        .expect_err("mismatched artifact must fail");

    assert!(error.to_string().contains("does not match"));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
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

    service.refresh("stable").await.expect("refresh");
    let error = service
        .artifact_for_request(&unknown)
        .await
        .expect_err("unknown artifact must fail");

    assert!(error.to_string().contains("not referenced"));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
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

fn service_for(upstream: Url, data_dir: PathBuf) -> RustupService {
    RustupService::new(Config {
        listen: "127.0.0.1:18081".parse::<SocketAddr>().expect("address"),
        data_dir,
        public_base_url: Url::parse("http://127.0.0.1:18081").expect("public URL"),
        access: AccessConfig {
            allow_anonymous: true,
            read_token_env: None,
        },
        rustup: RustupConfig {
            refresh_interval_secs: 3600,
            channels: vec!["stable".to_owned()],
            upstreams: vec![UpstreamConfig {
                name: "test".to_owned(),
                base_url: upstream,
                require_https: false,
            }],
        },
    })
    .expect("service")
}
