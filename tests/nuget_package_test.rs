use std::{
    net::SocketAddr,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{Router, body::Body, http::Request, routing::get};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha512};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, http::router_with_registry,
    nuget::NugetAdapter, sources::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn package_content_is_verified_and_reused_from_source_bound_cache() {
    let package = b"verified nupkg".to_vec();
    let package_hash = STANDARD.encode(Sha512::digest(&package));
    let package_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, address) = start_package_server(
        package.clone(),
        package_hash.clone(),
        package_requests.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    prime_registration(&router).await;
    let package_path = "/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg";
    let first = router
        .clone()
        .oneshot(get_request(package_path))
        .await
        .expect("package response");
    assert_eq!(first.status(), axum::http::StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("package body"),
        package
    );

    let second = router
        .oneshot(get_request(package_path))
        .await
        .expect("cached package response");
    assert_eq!(second.status(), axum::http::StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("cached package body"),
        b"verified nupkg".to_vec()
    );
    assert_eq!(package_requests.load(Ordering::SeqCst), 1);
    assert!(
        data_dir
            .path()
            .join("stores/nuget/artifacts/official/objects/package/foo/1.0.0/foo.1.0.0.nupkg")
            .exists()
    );
}

#[tokio::test]
async fn package_content_digest_failure_does_not_publish_or_return_the_bad_bytes() {
    let expected = b"expected nupkg".to_vec();
    let actual = b"tampered nupkg".to_vec();
    let package_hash = STANDARD.encode(Sha512::digest(&expected));
    let (upstream, address) =
        start_package_server(actual, package_hash, Arc::new(AtomicUsize::new(0))).await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    prime_registration(&router).await;
    let response = router
        .oneshot(get_request(
            "/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg",
        ))
        .await
        .expect("bad package response");

    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("stores/nuget/artifacts/official/objects/package/foo/1.0.0/foo.1.0.0.nupkg")
            .exists()
    );
}

#[tokio::test]
async fn package_content_without_upstream_digest_uses_local_cache_hash() {
    let package = valid_nupkg();
    let package_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, address) =
        start_package_server_without_hash(package.clone(), package_requests.clone()).await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    prime_registration(&router).await;
    let path = "/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg";
    let first = router
        .clone()
        .oneshot(get_request(path))
        .await
        .expect("package response");
    assert_eq!(first.status(), axum::http::StatusCode::OK);
    let cache = data_dir
        .path()
        .join("stores/nuget/artifacts/official/objects/package/foo/1.0.0/foo.1.0.0.nupkg");
    assert!(cache.exists());
    assert!(cache.with_file_name("foo.1.0.0.nupkg.meta.json").exists());

    let second = router
        .oneshot(get_request(path))
        .await
        .expect("cached package response");
    assert_eq!(second.status(), axum::http::StatusCode::OK);
    assert_eq!(package_requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("cached package body"),
        package
    );
}

fn valid_nupkg() -> Vec<u8> {
    vec![
        0x50, 0x4b, 0x05, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]
}

#[tokio::test]
async fn package_content_without_upstream_digest_rejects_non_nupkg_content() {
    let package_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, address) =
        start_package_server_without_hash(b"not a zip archive".to_vec(), package_requests).await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    prime_registration(&router).await;
    let response = router
        .oneshot(get_request(
            "/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg",
        ))
        .await
        .expect("bad package response");

    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("stores/nuget/artifacts/official/objects/package/foo/1.0.0/foo.1.0.0.nupkg")
            .exists()
    );
}

#[tokio::test]
async fn package_version_index_is_available_for_restore_clients() {
    let package = b"version index package".to_vec();
    let package_hash = STANDARD.encode(Sha512::digest(&package));
    let (upstream, address) =
        start_package_server(package, package_hash, Arc::new(AtomicUsize::new(0))).await;
    let router = nuget_router(upstream, address, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request("/mirror/nuget/official/package/foo/index.json"))
        .await
        .expect("package version index response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("package version index body"),
    )
    .expect("package version index JSON");
    assert_eq!(body["versions"], serde_json::json!(["1.0.0"]));
}

#[tokio::test]
async fn package_content_can_use_a_verified_package_version_index_binding() {
    let package = valid_nupkg();
    let package_hash = STANDARD.encode(Sha512::digest(&package));
    let (upstream, address) =
        start_package_server(package.clone(), package_hash, Arc::new(AtomicUsize::new(0))).await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    let index = router
        .clone()
        .oneshot(get_request("/mirror/nuget/official/package/foo/index.json"))
        .await
        .expect("package version index response");
    assert_eq!(index.status(), axum::http::StatusCode::OK);

    let response = router
        .oneshot(get_request(
            "/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg",
        ))
        .await
        .expect("package response");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("package body"),
        package
    );
}

async fn prime_registration(router: &axum::Router) {
    let service_index = router
        .clone()
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");
    assert_eq!(service_index.status(), axum::http::StatusCode::OK);
    let registration = router
        .clone()
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/1.0.0.json",
        ))
        .await
        .expect("registration response");
    assert_eq!(registration.status(), axum::http::StatusCode::OK);
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("request")
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

fn nuget_router(upstream: Url, address: SocketAddr, data_dir: &Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "nuget",
            Arc::new(
                SourcePool::new_with_resolver(
                    "nuget",
                    SourceGraph::new(vec![proxy("official", upstream.as_str())])
                        .expect("source graph"),
                    "nuget.example",
                    address,
                )
                .expect("source pool"),
            ),
            Arc::new(NugetAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("registry");
    router_with_registry(registry)
}

async fn start_package_server(
    package: Vec<u8>,
    package_hash: String,
    package_requests: Arc<AtomicUsize>,
) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let service_index = format!(
        r#"{{"version":"3.0.0","resources":[
            {{"@id":"http://nuget.example:{}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
            {{"@id":"http://nuget.example:{}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
        ]}}"#,
        address.port(),
        address.port()
    );
    let registration = format!(
        r#"{{
            "@id":"http://nuget.example:{}/v3/registration/foo/1.0.0.json",
            "catalogEntry":{{
                "id":"foo",
                "version":"1.0.0",
                "packageHash":"{}",
                "packageHashAlgorithm":"SHA512"
            }},
            "packageContent":"http://nuget.example:{}/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg"
        }}"#,
        address.port(),
        package_hash,
        address.port()
    );
    let router = Router::new()
        .route(
            "/v3/index.json",
            get({
                let service_index = service_index.clone();
                move || {
                    let service_index = service_index.clone();
                    async move { service_index }
                }
            }),
        )
        .route(
            "/v3/registration/foo/1.0.0.json",
            get({
                let registration = registration.clone();
                move || {
                    let registration = registration.clone();
                    async move { registration }
                }
            }),
        )
        .route(
            "/v3-flatcontainer/foo/index.json",
            get(|| async { r#"{"versions":["1.0.0"]}"# }),
        )
        .route(
            "/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg",
            get(move || {
                let package = package.clone();
                let package_requests = package_requests.clone();
                async move {
                    package_requests.fetch_add(1, Ordering::SeqCst);
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                        package,
                    )
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    (
        Url::parse(&format!(
            "http://nuget.example:{}/v3/index.json",
            address.port()
        ))
        .expect("service index URL"),
        address,
    )
}

async fn start_package_server_without_hash(
    package: Vec<u8>,
    package_requests: Arc<AtomicUsize>,
) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let service_index = format!(
        r#"{{"version":"3.0.0","resources":[
            {{"@id":"http://nuget.example:{}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
            {{"@id":"http://nuget.example:{}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
        ]}}"#,
        address.port(),
        address.port()
    );
    let registration = format!(
        r#"{{
            "@id":"http://nuget.example:{}/v3/registration/foo/1.0.0.json",
            "catalogEntry":{{"id":"foo","version":"1.0.0"}},
            "packageContent":"http://nuget.example:{}/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg"
        }}"#,
        address.port(),
        address.port()
    );
    let router = Router::new()
        .route(
            "/v3/index.json",
            get({
                let service_index = service_index.clone();
                move || {
                    let service_index = service_index.clone();
                    async move { service_index }
                }
            }),
        )
        .route(
            "/v3/registration/foo/1.0.0.json",
            get({
                let registration = registration.clone();
                move || {
                    let registration = registration.clone();
                    async move { registration }
                }
            }),
        )
        .route(
            "/v3-flatcontainer/foo/index.json",
            get(|| async { r#"{"versions":["1.0.0"]}"# }),
        )
        .route(
            "/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg",
            get(move || {
                let package = package.clone();
                let package_requests = package_requests.clone();
                async move {
                    package_requests.fetch_add(1, Ordering::SeqCst);
                    package
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    (
        Url::parse(&format!(
            "http://nuget.example:{}/v3/index.json",
            address.port()
        ))
        .expect("service index URL"),
        address,
    )
}
