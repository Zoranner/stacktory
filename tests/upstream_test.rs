use axum::{Router, http::StatusCode, routing::get};
use sha2::{Digest, Sha256};
use stacktory::upstream::UpstreamClient;
use tokio::net::TcpListener;
use url::Url;

const MANIFEST: &str = "manifest-version = \"2\"\n";

#[tokio::test]
async fn fetches_manifest_from_the_next_upstream_after_a_failure() {
    let unavailable =
        start_server(Router::new().route("/{*path}", get(|| async { StatusCode::BAD_GATEWAY })))
            .await;
    let checksum = format!(
        "{:x}  channel-rust-stable.toml\n",
        Sha256::digest(MANIFEST.as_bytes())
    );
    let available = start_server(
        Router::new()
            .route("/dist/channel-rust-stable.toml", get(|| async { MANIFEST }))
            .route(
                "/dist/channel-rust-stable.toml.sha256",
                get(move || {
                    let checksum = checksum.clone();
                    async move { checksum }
                }),
            ),
    )
    .await;
    let client = UpstreamClient::new(vec![unavailable, available]).expect("HTTP client");

    let manifest = client
        .fetch_channel_manifest("stable")
        .await
        .expect("fallback upstream manifest");

    assert_eq!(manifest, MANIFEST);
}

#[tokio::test]
async fn rejects_an_upstream_manifest_with_a_mismatched_checksum() {
    let upstream = start_server(
        Router::new()
            .route(
                "/dist/channel-rust-stable.toml",
                get(|| async { MANIFEST }),
            )
            .route(
                "/dist/channel-rust-stable.toml.sha256",
                get(|| async { "0000000000000000000000000000000000000000000000000000000000000000  channel-rust-stable.toml\n" }),
            ),
    )
    .await;
    let client = UpstreamClient::new(vec![upstream]).expect("HTTP client");

    let error = client
        .fetch_channel_manifest("stable")
        .await
        .expect_err("mismatched upstream checksum must fail");

    assert!(error.to_string().contains("checksum"));
}

#[tokio::test]
async fn fetches_an_artifact_from_the_next_upstream_when_the_first_hash_does_not_match() {
    let path = stacktory::rustup::DistPath::parse("2026-08-20/rustc.tar.xz").expect("dist path");
    let expected = b"verified artifact";
    let expected_checksum = format!("{:x}", Sha256::digest(expected));
    let invalid = start_server(Router::new().route(
        "/dist/2026-08-20/rustc.tar.xz",
        get(|| async { "tampered artifact" }),
    ))
    .await;
    let valid = start_server(Router::new().route(
        "/dist/2026-08-20/rustc.tar.xz",
        get(move || async move { expected }),
    ))
    .await;
    let client = UpstreamClient::new(vec![invalid, valid]).expect("HTTP client");

    let artifact = client
        .fetch_dist_artifact(&path, &expected_checksum)
        .await
        .expect("fallback artifact");

    assert_eq!(artifact, expected);
}

async fn start_server(router: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    let address = listener.local_addr().expect("listener address");
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("test server should run");
    });

    Url::parse(&format!("http://{address}")).expect("test server URL")
}
