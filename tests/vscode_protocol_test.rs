use std::{
    fs,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, http::router_with_registry,
    sources::registry::AdapterRegistry, vscode::VscodeAdapter,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn marketplace_query_rewrites_assets_and_publishes_descriptors() {
    let vsix = b"verified marketplace VSIX".to_vec();
    let (upstream, asset_address) = start_gallery_server(
        vsix.clone(),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = vscode_group_router(upstream, asset_address, data_dir.path());

    let response = router
        .clone()
        .oneshot(query_request())
        .await
        .expect("query response");

    assert_eq!(response.status(), StatusCode::OK);
    let query: serde_json::Value =
        serde_json::from_slice(&body_bytes(response).await).expect("query JSON");
    let source = query["results"][0]["extensions"][0]["versions"][0]["files"][0]["source"]
        .as_str()
        .expect("rewritten source");
    assert!(source.starts_with("http://127.0.0.1:18761/mirror/vscode/group/asset/"));
    let asset_base = query["results"][0]["extensions"][0]["versions"][0]["assetUri"]
        .as_str()
        .expect("rewritten asset base");
    assert!(asset_base.starts_with("http://127.0.0.1:18761/mirror/vscode/group/asset-base/"));
    let asset_id = source.rsplit('/').next().expect("asset ID");
    assert_eq!(asset_id.len(), 64);
    assert!(
        data_dir
            .path()
            .join(format!(
                "stores/vscode/metadata/group/marketplace/assets/{asset_id}.toml"
            ))
            .exists()
    );

    let asset = router
        .clone()
        .oneshot(get_request(&format!(
            "/mirror/vscode/group/asset/{asset_id}"
        )))
        .await
        .expect("asset response");
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(
        asset
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.ms-vsix")
    );
    assert_eq!(body_bytes(asset).await, vsix);

    let asset_base_id = asset_base.rsplit('/').next().expect("asset base ID");
    let gallery_asset = router
        .clone()
        .oneshot(get_request(&format!(
            "/mirror/vscode/group/asset-base/{asset_base_id}/Microsoft.VisualStudio.Services.VSIXPackage"
        )))
        .await
        .expect("asset base response");
    assert_eq!(gallery_asset.status(), StatusCode::OK);
    assert_eq!(
        body_bytes(gallery_asset).await,
        b"verified marketplace VSIX"
    );

    let resource = router
        .clone()
        .oneshot(get_request(
            "/mirror/vscode/group/resource/redhat/vscode-yaml/1.25.0/extension",
        ))
        .await
        .expect("resource response");
    assert_eq!(resource.status(), StatusCode::OK);
    assert_eq!(
        resource
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(body_bytes(resource).await, br#"{"name":"vscode-yaml"}"#);

    let extension = router
        .oneshot(get_request(
            "/mirror/vscode/group/extension/redhat/vscode-yaml/latest",
        ))
        .await
        .expect("extension package response");
    assert_eq!(extension.status(), StatusCode::OK);
    assert_eq!(
        extension
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.ms-vsix")
    );
    assert_eq!(body_bytes(extension).await, b"verified marketplace VSIX");
}

#[tokio::test]
async fn marketplace_query_uses_fixed_gallery_headers_and_preserves_request_body() {
    let observed = Arc::new(AtomicUsize::new(0));
    let (upstream, asset_address) = start_gallery_server(
        b"VSIX".to_vec(),
        observed.clone(),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = vscode_group_router(upstream, asset_address, data_dir.path());

    let response = router
        .oneshot(query_request())
        .await
        .expect("query response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(observed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn marketplace_client_and_upstream_json_errors_use_distinct_statuses() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let upstream = Router::new().route(
        "/extensionquery",
        post(|| async { (StatusCode::OK, "not-json").into_response() }),
    );
    tokio::spawn(async move {
        axum::serve(listener, upstream)
            .await
            .expect("gallery server");
    });
    let data_dir = tempdir().expect("data directory");
    let registry = AdapterRegistry::new()
        .with_adapter(
            "vscode",
            Arc::new(
                SourcePool::new(
                    "vscode",
                    SourceGraph::new(vec![proxy("official", &format!("http://{address}"))])
                        .expect("source graph"),
                )
                .expect("source pool"),
            ),
            Arc::new(VscodeAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir.path(),
            )),
        )
        .expect("registry");
    let router = router_with_registry(registry);

    let request_error = router
        .clone()
        .oneshot(
            Request::post("/mirror/vscode/official/extensionquery")
                .body(Body::from("not-json"))
                .expect("invalid query request"),
        )
        .await
        .expect("invalid query response");
    assert_eq!(request_error.status(), StatusCode::BAD_REQUEST);

    let upstream_error = router
        .oneshot(
            Request::post("/mirror/vscode/official/extensionquery")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"filters":[],"assetTypes":[],"flags":914}"#))
                .expect("query request"),
        )
        .await
        .expect("invalid upstream response");
    assert_eq!(upstream_error.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn marketplace_rejects_unbound_assets_and_unsupported_methods() {
    let (upstream, asset_address) = start_gallery_server(
        b"VSIX".to_vec(),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let router = vscode_group_router(
        upstream,
        asset_address,
        tempdir().expect("data directory").path(),
    );

    let missing_asset = router
        .clone()
        .oneshot(get_request(&format!(
            "/mirror/vscode/group/asset/{}",
            "a".repeat(64)
        )))
        .await
        .expect("missing asset response");
    let get_query = router
        .oneshot(get_request("/mirror/vscode/group/extensionquery"))
        .await
        .expect("GET query response");

    assert_eq!(missing_asset.status(), StatusCode::NOT_FOUND);
    assert_eq!(get_query.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn marketplace_asset_cache_is_shared_by_group_and_its_proxy() {
    let vsix = b"shared marketplace VSIX".to_vec();
    let asset_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, asset_address) = start_gallery_server(
        vsix.clone(),
        Arc::new(AtomicUsize::new(0)),
        asset_requests.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = vscode_group_router(upstream, asset_address, data_dir.path());

    let group_query = router
        .clone()
        .oneshot(query_request_for("group"))
        .await
        .expect("group query response");
    let group_asset_url = asset_url(group_query).await;
    let group_asset = router
        .clone()
        .oneshot(get_request(&group_asset_url))
        .await
        .expect("group asset response");
    assert_eq!(group_asset.status(), StatusCode::OK);

    let artifacts = data_dir
        .path()
        .join("stores/vscode/artifacts/official/objects/marketplace");
    let object = fs::read_dir(&artifacts)
        .expect("artifacts directory")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_none())
        .expect("cached asset object");
    let sidecar = object.with_extension("toml");
    let backdated = SystemTime::now() - Duration::from_secs(3_600);
    for path in [&object, &sidecar] {
        fs::File::options()
            .write(true)
            .open(path)
            .expect("open cached asset file")
            .set_times(fs::FileTimes::new().set_modified(backdated))
            .expect("backdate cached asset file");
    }

    let direct_query = router
        .clone()
        .oneshot(query_request_for("official"))
        .await
        .expect("direct query response");
    let direct_asset_url = asset_url(direct_query).await;
    let direct_asset = router
        .oneshot(get_request(&direct_asset_url))
        .await
        .expect("direct asset response");
    assert_eq!(direct_asset.status(), StatusCode::OK);
    assert_eq!(asset_requests.load(Ordering::SeqCst), 1);
    assert!(artifacts.exists());
    let refreshed_after = SystemTime::now() - Duration::from_secs(60);
    for path in [&object, &sidecar] {
        let modified = fs::metadata(path)
            .expect("cached asset metadata")
            .modified()
            .expect("cached asset modification time");
        assert!(
            modified > refreshed_after,
            "cache hit must refresh access time for `{}`",
            path.display()
        );
    }
}

fn query_request() -> Request<Body> {
    query_request_for("group")
}

fn query_request_for(entry_name: &str) -> Request<Body> {
    Request::post(format!("/mirror/vscode/{entry_name}/extensionquery"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"filters":[{"criteria":[{"filterType":10,"value":"redhat.vscode-yaml"}]}],"assetTypes":[],"flags":914}"#,
        ))
        .expect("query request")
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET request")
}

async fn asset_url(response: axum::response::Response) -> String {
    assert_eq!(response.status(), StatusCode::OK);
    let query: serde_json::Value =
        serde_json::from_slice(&body_bytes(response).await).expect("query JSON");
    query["results"][0]["extensions"][0]["versions"][0]["files"][0]["source"]
        .as_str()
        .expect("asset URL")
        .replace("http://127.0.0.1:18761", "")
}

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body")
        .to_vec()
}

fn vscode_group_router(
    upstream: Url,
    asset_address: SocketAddr,
    data_dir: &std::path::Path,
) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "vscode",
            Arc::new(
                SourcePool::new_with_resolvers(
                    "vscode",
                    SourceGraph::new(vec![
                        proxy("official", upstream.as_str()),
                        group("group", &["official"]),
                    ])
                    .expect("source graph"),
                    &[
                        ("redhat.vscode-unpkg.net", asset_address),
                        ("www.vscode-unpkg.net", asset_address),
                    ],
                )
                .expect("source pool"),
            ),
            Arc::new(VscodeAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("registry");
    router_with_registry(registry)
}

fn proxy(name: &str, base_url: &str) -> SourceConfig {
    SourceConfig {
        name: name.to_owned(),
        source_type: SourceType::Proxy,
        base_url: Some(Url::parse(base_url).expect("base URL")),
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

async fn start_gallery_server(
    vsix: Vec<u8>,
    query_requests: Arc<AtomicUsize>,
    asset_requests: Arc<AtomicUsize>,
) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address: SocketAddr = listener.local_addr().expect("address");
    let asset_url = format!(
        "http://redhat.vscode-unpkg.net:{}/assets/redhat.vscode-yaml.vsix",
        address.port()
    );
    let asset_base_url = format!(
        "http://redhat.vscode-unpkg.net:{}/asset-base/redhat/vscode-yaml/1.25.0/1000",
        address.port()
    );
    let router = Router::new()
        .route(
            "/extensionquery",
            post({
                let asset_url = asset_url.clone();
                let asset_base_url = asset_base_url.clone();
                move |headers: HeaderMap, body: String| {
                    let asset_url = asset_url.clone();
                    let asset_base_url = asset_base_url.clone();
                    let query_requests = query_requests.clone();
                    async move {
                        assert_eq!(
                            headers
                                .get("accept")
                                .and_then(|value| value.to_str().ok()),
                            Some("application/json;api-version=3.0-preview.1")
                        );
                        assert_eq!(
                            headers
                                .get("content-type")
                                .and_then(|value| value.to_str().ok()),
                            Some("application/json")
                        );
                        assert_eq!(
                            headers
                                .get("x-market-client-id")
                                .and_then(|value| value.to_str().ok()),
                            Some("VSCode")
                        );
                        assert!(body.contains("redhat.vscode-yaml"));
                        query_requests.fetch_add(1, Ordering::SeqCst);
                        axum::Json(serde_json::json!({
                            "results": [{
                                "extensions": [{
                                    "publisher": { "publisherName": "redhat" },
                                    "extensionName": "vscode-yaml",
                                    "versions": [{
                                        "version": "1.25.0",
                                        "assetUri": asset_base_url,
                                        "files": [{
                                            "assetType": "Microsoft.VisualStudio.Services.VSIXPackage",
                                            "source": asset_url
                                        }]
                                    }]
                                }]
                            }]
                        }))
                    }
                }
            }),
        )
        .route(
            "/assets/redhat.vscode-yaml.vsix",
            get(move || {
                let vsix = vsix.clone();
                let asset_requests = asset_requests.clone();
                async move {
                    asset_requests.fetch_add(1, Ordering::SeqCst);
                    (
                        [("content-type", "application/vnd.ms-vsix")],
                        vsix,
                    )
                }
            }),
        )
        .route(
            "/asset-base/redhat/vscode-yaml/1.25.0/1000/Microsoft.VisualStudio.Services.VSIXPackage",
            get(|| async {
                (
                    [("content-type", "application/vnd.ms-vsix")],
                    b"verified marketplace VSIX".as_slice(),
                )
            }),
        )
        .route(
            "/redhat/vscode-yaml/1.25.0/extension",
            get(|| async {
                (
                    [("content-type", "application/json")],
                    r#"{"name":"vscode-yaml"}"#,
                )
            }),
        )
        .route(
            "/_gallery/redhat/vscode-yaml/latest",
            get(|| async {
                (
                    [("content-type", "application/vnd.ms-vsix")],
                    b"verified marketplace VSIX".as_slice(),
                )
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("gallery server");
    });
    (
        Url::parse(&format!("http://{address}")).expect("gallery URL"),
        address,
    )
}
