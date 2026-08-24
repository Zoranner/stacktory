use std::{net::SocketAddr, path::PathBuf};

use axum::{Router, routing::get};
use sha2::{Digest, Sha256};
use stacktory::{AccessConfig, Config, RustupConfig, UpstreamConfig, service::RustupService};
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
