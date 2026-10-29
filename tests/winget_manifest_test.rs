use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    extract::OriginalUri,
    http::{Request, StatusCode, header::LOCATION},
    response::IntoResponse,
    routing::get,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, ecosystems::winget::WingetAdapter,
    http::router_with_registry, upstream::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn manifest_preserves_fields_and_rewrites_only_installer_url() {
    let installer = b"installer bytes".to_vec();
    let (port, address) = start_manifest_server(
        installer.clone(),
        hash(&installer),
        None,
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = manifest_router(port, address, data_dir.path());

    let response = router
        .oneshot(get_request(
            "/mirror/winget/official/packageManifests/Test.Package?Version=1.0.0",
        ))
        .await
        .expect("manifest response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("manifest body"),
    )
    .expect("manifest JSON");

    assert_eq!(body["Data"]["PackageIdentifier"], "Test.Package");
    assert_eq!(body["Data"]["UpstreamQuery"], "Version=1.0.0");
    assert_eq!(body["Data"]["UpstreamDataField"]["kept"], true);
    assert_eq!(body["ContinuationToken"], "upstream-token");
    assert_eq!(body["Data"]["Versions"][0]["UpstreamVersionField"], "kept");
    assert_eq!(body["Data"]["Versions"][0]["Channel"], "stable");
    let installer = &body["Data"]["Versions"][0]["Installers"][0];
    assert_eq!(installer["InstallerType"], "exe");
    assert_eq!(installer["Architecture"], "x64");
    assert_eq!(installer["Scope"], "user");
    assert_eq!(installer["InstallerLocale"], "en-US");
    assert_eq!(installer["UpstreamInstallerField"]["kept"], true);
    assert_eq!(installer["InstallerSha256"], hash(b"installer bytes"));
    let local_url = installer["InstallerUrl"]
        .as_str()
        .expect("rewritten installer URL");
    let token = local_url
        .strip_prefix("http://127.0.0.1:18761/mirror/winget/official/installers/")
        .expect("local installer URL");
    let (token, extension) = token.split_once('.').expect("installer extension");
    assert_eq!(extension, "bin");
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let sidecar: Value = serde_json::from_slice(
        &std::fs::read(data_dir.path().join(format!(
            "stores/winget/metadata/official/installers/{token}.json"
        )))
        .expect("installer sidecar"),
    )
    .expect("sidecar JSON");
    assert_eq!(sidecar["selected_proxy"], "official");
    assert_eq!(sidecar["installer_token"], token);
    assert_eq!(
        sidecar["upstream_url"],
        format!("http://127.0.0.1:{port}/download/Test.Package.bin")
    );
}

#[tokio::test]
async fn manifest_can_be_read_by_package_id_without_a_version_query() {
    let installer = b"installer bytes".to_vec();
    let (port, address) = start_manifest_server(
        installer.clone(),
        hash(&installer),
        None,
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = manifest_router(port, address, data_dir.path());

    let response = router
        .oneshot(get_request(
            "/mirror/winget/official/packageManifests/Test.Package",
        ))
        .await
        .expect("manifest response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("manifest body"),
    )
    .expect("manifest JSON");
    assert_eq!(body["Data"]["PackageIdentifier"], "Test.Package");
    assert_eq!(body["Data"]["UpstreamQuery"], "");
    assert_eq!(body["Data"]["Versions"][0]["PackageVersion"], "1.0.0");
}

#[tokio::test]
async fn installer_download_verifies_sha_and_reuses_the_cache() {
    let installer = b"verified installer".to_vec();
    let requests = Arc::new(AtomicUsize::new(0));
    let (port, address) =
        start_manifest_server(installer.clone(), hash(&installer), None, requests.clone()).await;
    let data_dir = tempdir().expect("data directory");
    let router = manifest_router(port, address, data_dir.path());

    let manifest = router
        .clone()
        .oneshot(get_request(
            "/mirror/winget/official/packageManifests/Test.Package?Version=1.0.0",
        ))
        .await
        .expect("manifest response");
    let manifest: Value = serde_json::from_slice(
        &axum::body::to_bytes(manifest.into_body(), usize::MAX)
            .await
            .expect("manifest body"),
    )
    .expect("manifest JSON");
    let local_url = manifest["Data"]["Versions"][0]["Installers"][0]["InstallerUrl"]
        .as_str()
        .expect("local URL");
    let path = Url::parse(local_url)
        .expect("local URL parse")
        .path()
        .to_owned();

    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(get_request(&path))
            .await
            .expect("installer response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("installer body"),
            installer
        );
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(!has_part_files(data_dir.path()));
    let token = path.rsplit('/').next().expect("token");
    let token = token.split('.').next().expect("token");
    assert!(
        data_dir
            .path()
            .join(format!(
                "stores/winget/artifacts/official/objects/official/Test.Package/1.0.0/{token}"
            ))
            .exists()
    );
}

#[tokio::test]
async fn installer_hash_mismatch_does_not_publish_or_return_bad_bytes() {
    let expected = b"expected".to_vec();
    let actual = b"tampered".to_vec();
    let (port, address) =
        start_manifest_server(actual, hash(&expected), None, Arc::new(AtomicUsize::new(0))).await;
    let data_dir = tempdir().expect("data directory");
    let router = manifest_router(port, address, data_dir.path());

    let manifest = router
        .clone()
        .oneshot(get_request(
            "/mirror/winget/official/packageManifests/Test.Package?Version=1.0.0",
        ))
        .await
        .expect("manifest response");
    let manifest: Value = serde_json::from_slice(
        &axum::body::to_bytes(manifest.into_body(), usize::MAX)
            .await
            .expect("manifest body"),
    )
    .expect("manifest JSON");
    let local_url = manifest["Data"]["Versions"][0]["Installers"][0]["InstallerUrl"]
        .as_str()
        .expect("local URL");
    let path = Url::parse(local_url)
        .expect("local URL parse")
        .path()
        .to_owned();

    let response = router
        .oneshot(get_request(&path))
        .await
        .expect("installer response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let token = path.rsplit('/').next().expect("token");
    let token = token.split('.').next().expect("token");
    assert!(
        !data_dir
            .path()
            .join(format!(
                "stores/winget/artifacts/official/objects/official/Test.Package/1.0.0/{token}"
            ))
            .exists()
    );
    assert!(!has_part_files(data_dir.path()));
}

#[tokio::test]
async fn installer_host_outside_the_allowlist_rejects_the_manifest() {
    let installer = b"installer".to_vec();
    let (port, address) = start_manifest_server(
        installer.clone(),
        hash(&installer),
        Some("evil.example"),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = manifest_router(port, address, data_dir.path());

    let response = router
        .oneshot(get_request(
            "/mirror/winget/official/packageManifests/Test.Package?Version=1.0.0",
        ))
        .await
        .expect("manifest response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("stores/winget/metadata/official/installers")
            .exists()
    );
}

#[tokio::test]
async fn redirect_outside_the_allowlist_does_not_publish_the_installer() {
    let installer = b"redirect expected".to_vec();
    let (port, address) = start_redirect_server(hash(&installer)).await;
    let data_dir = tempdir().expect("data directory");
    let router = manifest_router(port, address, data_dir.path());

    let manifest = router
        .clone()
        .oneshot(get_request(
            "/mirror/winget/official/packageManifests/Test.Package?Version=1.0.0",
        ))
        .await
        .expect("manifest response");
    assert_eq!(manifest.status(), StatusCode::OK);
    let manifest: Value = serde_json::from_slice(
        &axum::body::to_bytes(manifest.into_body(), usize::MAX)
            .await
            .expect("manifest body"),
    )
    .expect("manifest JSON");
    let path = Url::parse(
        manifest["Data"]["Versions"][0]["Installers"][0]["InstallerUrl"]
            .as_str()
            .expect("installer URL"),
    )
    .expect("installer URL parse")
    .path()
    .to_owned();

    let response = router
        .oneshot(get_request(&path))
        .await
        .expect("installer response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let token = path.rsplit('/').next().expect("token");
    assert!(
        !data_dir
            .path()
            .join(format!(
                "stores/winget/artifacts/official/objects/official/Test.Package/1.0.0/{token}"
            ))
            .exists()
    );
    assert!(!has_part_files(data_dir.path()));
}

#[tokio::test]
async fn unindexed_installer_token_is_not_a_url_proxy() {
    let (port, address) = start_empty_server().await;
    let data_dir = tempdir().expect("data directory");
    let router = manifest_router(port, address, data_dir.path());

    let response = router
        .oneshot(get_request(&format!(
            "/mirror/winget/official/installers/{}",
            "a".repeat(64)
        )))
        .await
        .expect("installer response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn local_binding_write_failure_returns_internal_server_error() {
    let installer = b"installer".to_vec();
    let (port, address) = start_manifest_server(
        installer.clone(),
        hash(&installer),
        None,
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let root = tempdir().expect("data directory");
    let data_dir = root.path().join("not-a-directory");
    std::fs::write(&data_dir, b"file").expect("data path file");
    let router = manifest_router(port, address, &data_dir);

    let response = router
        .oneshot(get_request(
            "/mirror/winget/official/packageManifests/Test.Package?Version=1.0.0",
        ))
        .await
        .expect("manifest response");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

fn manifest_router(port: u16, address: SocketAddr, data_dir: &Path) -> Router {
    let graph = SourceGraph::new(vec![SourceConfig {
        name: "official".to_owned(),
        source_type: SourceType::Proxy,
        base_url: Some(
            Url::parse(&format!("http://winget.example:{port}/api")).expect("source URL"),
        ),
        require_https: false,
        members: Vec::new(),
    }])
    .expect("source graph");
    let pool = SourcePool::new_with_resolver("winget", graph, "winget.example", address)
        .expect("source pool");
    let hosts = BTreeMap::from([(
        "official".to_owned(),
        BTreeSet::from([format!("127.0.0.1:{port}")]),
    )]);
    let registry = AdapterRegistry::new()
        .with_adapter(
            "winget",
            Arc::new(pool),
            Arc::new(WingetAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("public URL"),
                data_dir,
                hosts,
            )),
        )
        .expect("registry");
    router_with_registry(registry)
}

async fn start_manifest_server(
    installer: Vec<u8>,
    manifest_hash: String,
    installer_host: Option<&str>,
    requests: Arc<AtomicUsize>,
) -> (u16, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("manifest listener");
    let address = listener.local_addr().expect("manifest address");
    let port = address.port();
    let host = installer_host.unwrap_or("127.0.0.1");
    let manifest = format!(
        r#"{{
            "Data": {{
                "PackageIdentifier": "Test.Package",
                "UpstreamQuery": "__QUERY__",
                "UpstreamDataField": {{"kept": true}},
                "Versions": [{{
                    "PackageVersion": "1.0.0",
                    "Channel": "stable",
                    "UpstreamVersionField": "kept",
                    "DefaultLocale": {{
                        "PackageLocale": "en-US",
                        "Publisher": "Test",
                        "PackageName": "Test",
                        "ShortDescription": "test"
                    }},
                    "Installers": [{{
                        "InstallerUrl": "http://{host}:{port}/download/Test.Package.bin",
                        "InstallerSha256": "{manifest_hash}",
                        "InstallerType": "exe",
                        "Architecture": "x64",
                        "Scope": "user",
                        "InstallerLocale": "en-US",
                        "UpstreamInstallerField": {{"kept": true}}
                    }}]
                }}]
            }},
            "ContinuationToken": "upstream-token"
        }}"#
    );
    let router = Router::new()
        .route(
            "/api/packageManifests/Test.Package",
            get(move |OriginalUri(uri): OriginalUri| {
                let manifest = manifest.clone();
                let query = uri.query().unwrap_or_default().to_owned();
                async move {
                    let query = serde_json::to_string(&query).expect("query JSON");
                    (StatusCode::OK, manifest.replace("\"__QUERY__\"", &query))
                }
            }),
        )
        .route(
            "/download/Test.Package.bin",
            get(move || {
                let installer = installer.clone();
                let requests = requests.clone();
                async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                        installer,
                    )
                        .into_response()
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("manifest server");
    });
    (port, address)
}

async fn start_empty_server() -> (u16, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("empty listener");
    let address = listener.local_addr().expect("empty address");
    let port = address.port();
    tokio::spawn(async move {
        axum::serve(listener, Router::new())
            .await
            .expect("empty server");
    });
    (port, address)
}

async fn start_redirect_server(manifest_hash: String) -> (u16, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("redirect listener");
    let address = listener.local_addr().expect("redirect address");
    let port = address.port();
    let manifest = format!(
        r#"{{
            "Data": {{
                "PackageIdentifier": "Test.Package",
                "Versions": [{{
                    "PackageVersion": "1.0.0",
                    "DefaultLocale": {{
                        "PackageLocale": "en-US",
                        "Publisher": "Test",
                        "PackageName": "Test",
                        "ShortDescription": "test"
                    }},
                    "Installers": [{{
                        "InstallerUrl": "http://127.0.0.1:{port}/redirect",
                        "InstallerSha256": "{manifest_hash}",
                        "InstallerType": "exe",
                        "Architecture": "x64",
                        "Scope": "user",
                        "InstallerLocale": "en-US"
                    }}]
                }}]
            }}
        }}"#
    );
    let router = Router::new()
        .route(
            "/api/packageManifests/Test.Package",
            get(move || {
                let manifest = manifest.clone();
                async move { (StatusCode::OK, manifest) }
            }),
        )
        .route(
            "/redirect",
            get(|| async {
                (
                    StatusCode::FOUND,
                    [(LOCATION, "http://evil.example/payload")],
                )
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("redirect server");
    });
    (port, address)
}

fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn has_part_files(data_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(data_dir.join("tmp")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with("winget-") && name.ends_with(".part")
    })
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET request")
}
