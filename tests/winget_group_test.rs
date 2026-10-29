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
    http::{Request, StatusCode},
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
async fn installer_falls_back_and_records_the_selected_proxy() {
    let installer = b"group installer".to_vec();
    let first_downloads = Arc::new(AtomicUsize::new(0));
    let (port, address) = start_group_server(
        StatusCode::OK,
        StatusCode::OK,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::OK,
        installer.clone(),
        first_downloads.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = group_router(port, address, data_dir.path());

    let manifest = router
        .clone()
        .oneshot(get_request(
            "/mirror/winget/group/packageManifests/Test.Package?Version=1.0.0",
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
    let local_url = manifest["Data"]["Versions"][0]["Installers"][0]["InstallerUrl"]
        .as_str()
        .expect("local installer URL");
    let path = Url::parse(local_url).expect("local URL").path().to_owned();
    let token = path.rsplit('/').next().expect("token");
    let token = token.split('.').next().expect("token");
    let sidecar_path = data_dir.path().join(format!(
        "stores/winget/metadata/group/installers/{token}.json"
    ));
    let initial: Value =
        serde_json::from_slice(&std::fs::read(&sidecar_path).expect("initial sidecar"))
            .expect("initial sidecar JSON");
    assert_eq!(initial["selected_proxy"], "first");

    let installer_response = router
        .oneshot(get_request(&path))
        .await
        .expect("installer response");
    assert_eq!(installer_response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(installer_response.into_body(), usize::MAX)
            .await
            .expect("installer body"),
        installer
    );
    assert_eq!(first_downloads.load(Ordering::SeqCst), 1);

    let updated: Value =
        serde_json::from_slice(&std::fs::read(&sidecar_path).expect("updated sidecar"))
            .expect("updated sidecar JSON");
    assert_eq!(updated["selected_proxy"], "second");
    assert!(
        data_dir
            .path()
            .join(format!(
                "stores/winget/artifacts/second/objects/group/Test.Package/1.0.0/{token}"
            ))
            .exists()
    );
    let audit =
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(audit.contains(r#""event":"winget_installer_fallback""#));
    assert!(audit.contains(r#""selected_proxy":"second""#));
}

#[tokio::test]
async fn manifest_falls_back_without_merging_and_records_the_selected_proxy() {
    let installer = b"manifest fallback".to_vec();
    let (port, address) = start_group_server(
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::OK,
        StatusCode::OK,
        StatusCode::OK,
        installer,
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = group_router(port, address, data_dir.path());

    let response = router
        .oneshot(get_request(
            "/mirror/winget/group/packageManifests/Test.Package?Version=1.0.0",
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
    let token = body["Data"]["Versions"][0]["Installers"][0]["InstallerUrl"]
        .as_str()
        .expect("installer URL")
        .rsplit('/')
        .next()
        .expect("token");
    let token = token.split('.').next().expect("token");
    let sidecar: Value = serde_json::from_slice(
        &std::fs::read(data_dir.path().join(format!(
            "stores/winget/metadata/group/installers/{token}.json"
        )))
        .expect("sidecar"),
    )
    .expect("sidecar JSON");
    assert_eq!(sidecar["selected_proxy"], "second");
}

#[tokio::test]
async fn group_all_source_failures_and_all_missing_keep_distinct_statuses() {
    let installer = b"missing".to_vec();
    let (port, address) = start_group_server(
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::INTERNAL_SERVER_ERROR,
        installer.clone(),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let router = group_router(port, address, tempdir().expect("data directory").path());
    let failed = router
        .oneshot(get_request(
            "/mirror/winget/group/packageManifests/Test.Package?Version=1.0.0",
        ))
        .await
        .expect("failed manifest response");
    assert_eq!(failed.status(), StatusCode::BAD_GATEWAY);

    let (port, address) = start_group_server(
        StatusCode::NOT_FOUND,
        StatusCode::NOT_FOUND,
        StatusCode::NOT_FOUND,
        StatusCode::NOT_FOUND,
        installer,
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let router = group_router(port, address, tempdir().expect("data directory").path());
    let missing = router
        .oneshot(get_request(
            "/mirror/winget/group/packageManifests/Test.Package?Version=1.0.0",
        ))
        .await
        .expect("missing manifest response");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

fn group_router(port: u16, address: SocketAddr, data_dir: &Path) -> Router {
    let first = format!("http://first.example:{port}/first/api");
    let second = format!("http://second.example:{port}/second/api");
    let graph = SourceGraph::new(vec![
        SourceConfig {
            name: "first".to_owned(),
            source_type: SourceType::Proxy,
            base_url: Some(Url::parse(&first).expect("first URL")),
            require_https: false,
            members: Vec::new(),
        },
        SourceConfig {
            name: "second".to_owned(),
            source_type: SourceType::Proxy,
            base_url: Some(Url::parse(&second).expect("second URL")),
            require_https: false,
            members: Vec::new(),
        },
        SourceConfig {
            name: "group".to_owned(),
            source_type: SourceType::Group,
            base_url: None,
            require_https: false,
            members: vec!["first".to_owned(), "second".to_owned()],
        },
    ])
    .expect("source graph");
    let pool = SourcePool::new_with_resolvers(
        "winget",
        graph,
        &[("first.example", address), ("second.example", address)],
    )
    .expect("source pool");
    let hosts = BTreeMap::from([
        (
            "first".to_owned(),
            BTreeSet::from([format!("127.0.0.1:{port}")]),
        ),
        (
            "second".to_owned(),
            BTreeSet::from([format!("127.0.0.1:{port}")]),
        ),
    ]);
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

async fn start_group_server(
    first_manifest: StatusCode,
    second_manifest: StatusCode,
    first_download: StatusCode,
    second_download: StatusCode,
    installer: Vec<u8>,
    first_downloads: Arc<AtomicUsize>,
) -> (u16, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("group listener");
    let address = listener.local_addr().expect("group address");
    let port = address.port();
    let installer_hash = format!("{:x}", Sha256::digest(&installer));
    let first_manifest_body = manifest_body(port, "first", &installer_hash);
    let second_manifest_body = manifest_body(port, "second", &installer_hash);
    let router = Router::new()
        .route(
            "/first/api/information",
            get(|| async {
                r#"{"Data":{"SourceIdentifier":"first","ServerSupportedVersions":["1.0.0"]}}"#
            }),
        )
        .route(
            "/second/api/information",
            get(|| async {
                r#"{"Data":{"SourceIdentifier":"second","ServerSupportedVersions":["1.0.0"]}}"#
            }),
        )
        .route(
            "/first/api/packageManifests/Test.Package",
            get(move || {
                let body = first_manifest_body.clone();
                async move { (first_manifest, body).into_response() }
            }),
        )
        .route(
            "/second/api/packageManifests/Test.Package",
            get(move || {
                let body = second_manifest_body.clone();
                async move { (second_manifest, body).into_response() }
            }),
        )
        .route(
            "/first/download/Test.Package.bin",
            get({
                let installer = installer.clone();
                let first_downloads = first_downloads.clone();
                move || {
                    let installer = installer.clone();
                    let first_downloads = first_downloads.clone();
                    async move {
                        first_downloads.fetch_add(1, Ordering::SeqCst);
                        if first_download == StatusCode::OK {
                            (first_download, installer).into_response()
                        } else {
                            first_download.into_response()
                        }
                    }
                }
            }),
        )
        .route(
            "/second/download/Test.Package.bin",
            get({
                let installer = installer.clone();
                move || {
                    let installer = installer.clone();
                    async move {
                        if second_download == StatusCode::OK {
                            (second_download, installer).into_response()
                        } else {
                            second_download.into_response()
                        }
                    }
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("group server");
    });
    (port, address)
}

fn manifest_body(port: u16, source: &str, installer_hash: &str) -> String {
    format!(
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
                        "InstallerUrl": "http://127.0.0.1:{port}/{source}/download/Test.Package.bin",
                        "InstallerSha256": "{installer_hash}",
                        "InstallerType": "exe",
                        "Architecture": "x64",
                        "Scope": "user",
                        "InstallerLocale": "en-US"
                    }}]
                }}]
            }}
        }}"#
    )
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET request")
}
