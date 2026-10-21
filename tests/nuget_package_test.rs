use std::{
    net::SocketAddr,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{Router, body::Body, http::Request, response::IntoResponse, routing::get};
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
    assert!(!has_part_files(data_dir.path()));
}

#[tokio::test]
async fn package_content_is_streamed_and_temporary_files_are_cleaned_up() {
    let comment = vec![b'x'; 60 * 1024];
    let package = valid_nupkg_with_comment(&comment);
    let package_hash = STANDARD.encode(Sha512::digest(&package));
    let (upstream, address) =
        start_package_server(package.clone(), package_hash, Arc::new(AtomicUsize::new(0))).await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    prime_registration(&router).await;
    let path = "/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg";
    let expected_length = package.len().to_string();
    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(get_request(path))
            .await
            .expect("streamed package response");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some(expected_length.as_str())
        );
        assert_eq!(
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("streamed package body"),
            package
        );
    }
    assert!(!has_part_files(data_dir.path()));
}

#[test]
fn constructor_removes_stale_nuget_temporary_files() {
    let data_dir = tempdir().expect("data directory");
    let temporary = data_dir.path().join("tmp");
    std::fs::create_dir_all(&temporary).expect("temporary directory");
    let stale_nuget = temporary.join("nuget-stale.part");
    let docker_temp = temporary.join("docker-stale.part");
    std::fs::write(&stale_nuget, b"stale").expect("stale NuGet temp file");
    std::fs::write(&docker_temp, b"docker").expect("Docker temp file");

    let _adapter = NugetAdapter::new(
        Url::parse("http://127.0.0.1:18761").expect("root URL"),
        data_dir.path(),
    );

    assert!(!stale_nuget.exists());
    assert!(docker_temp.exists());
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
    assert!(!has_part_files(data_dir.path()));
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

#[tokio::test]
async fn package_cache_without_upstream_digest_is_rebound_after_the_source_changes() {
    let first_package = valid_nupkg_with_comment(b"first");
    let second_package = valid_nupkg_with_comment(b"second");
    let first_requests = Arc::new(AtomicUsize::new(0));
    let second_requests = Arc::new(AtomicUsize::new(0));
    let (first_upstream, first_address) =
        start_package_server_without_hash(first_package.clone(), first_requests.clone()).await;
    let (second_upstream, second_address) =
        start_package_server_without_hash(second_package.clone(), second_requests.clone()).await;
    let data_dir = tempdir().expect("data directory");
    let path = "/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg";

    let first_router = nuget_router(first_upstream, first_address, data_dir.path());
    prime_registration(&first_router).await;
    let first = first_router
        .oneshot(get_request(path))
        .await
        .expect("first package response");
    assert_eq!(first.status(), axum::http::StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("first package body"),
        first_package
    );

    let second_router = nuget_router(second_upstream, second_address, data_dir.path());
    prime_registration(&second_router).await;
    let second = second_router
        .oneshot(get_request(path))
        .await
        .expect("second package response");
    assert_eq!(second.status(), axum::http::StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("second package body"),
        second_package
    );
    assert_eq!(first_requests.load(Ordering::SeqCst), 1);
    assert_eq!(second_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stale_package_binding_revalidates_republished_content() {
    let first_package = valid_nupkg_with_comment(b"first");
    let second_package = valid_nupkg_with_comment(b"second");
    let registration_hits = Arc::new(AtomicUsize::new(0));
    let package_hits = Arc::new(AtomicUsize::new(0));
    let (upstream, address) = start_republishing_package_server(
        first_package.clone(),
        second_package.clone(),
        registration_hits.clone(),
        package_hits.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());
    let path = "/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg";
    let binding_path = data_dir
        .path()
        .join("stores/nuget/metadata/official/bindings/foo/1.0.0.json");

    prime_registration(&router).await;
    let first = router
        .clone()
        .oneshot(get_request(path))
        .await
        .expect("first package response");
    assert_eq!(
        axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("first package body"),
        first_package
    );

    expire_metadata(&binding_path);
    let republished = router
        .oneshot(get_request(path))
        .await
        .expect("republished package response");
    assert_eq!(
        axum::body::to_bytes(republished.into_body(), usize::MAX)
            .await
            .expect("republished package body"),
        second_package
    );
    assert_eq!(registration_hits.load(Ordering::SeqCst), 2);
    assert_eq!(package_hits.load(Ordering::SeqCst), 2);
}

fn valid_nupkg() -> Vec<u8> {
    vec![
        0x50, 0x4b, 0x05, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]
}

fn has_part_files(data_dir: &Path) -> bool {
    std::fs::read_dir(data_dir.join("tmp"))
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
        })
        .unwrap_or(false)
}

fn valid_nupkg_with_comment(comment: &[u8]) -> Vec<u8> {
    let mut package = valid_nupkg();
    let comment_length = u16::try_from(comment.len()).expect("comment length");
    let length = package.len();
    package[length - 2..].copy_from_slice(&comment_length.to_le_bytes());
    package.extend_from_slice(comment);
    package
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
async fn stale_package_index_is_revalidated_and_keeps_the_last_good_versions() {
    let hits = Arc::new(AtomicUsize::new(0));
    let (upstream, address) = start_republishing_package_index_server(
        hits.clone(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());
    let request = || get_request("/mirror/nuget/official/package/foo/index.json");
    let descriptor_path = data_dir
        .path()
        .join("stores/nuget/metadata/official/package-index/foo.source");

    let first = router
        .clone()
        .oneshot(request())
        .await
        .expect("first package index response");
    assert_eq!(package_versions(first).await, vec!["1.0.0"]);

    expire_metadata(&descriptor_path);
    let republished = router
        .clone()
        .oneshot(request())
        .await
        .expect("republished package index response");
    assert_eq!(package_versions(republished).await, vec!["1.0.0", "2.0.0"]);

    expire_metadata(&descriptor_path);
    let stale = router
        .oneshot(request())
        .await
        .expect("stale package index response");
    assert_eq!(package_versions(stale).await, vec!["1.0.0", "2.0.0"]);
    assert_eq!(hits.load(Ordering::SeqCst), 3);

    let audit =
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(audit.contains(r#""event":"nuget_package_index_revalidated""#));
    assert!(audit.contains(r#""event":"nuget_package_index_stale""#));
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

async fn package_versions(response: axum::response::Response) -> Vec<String> {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("package index body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("package index JSON");
    body["versions"]
        .as_array()
        .expect("package versions")
        .iter()
        .map(|version| version.as_str().expect("package version string").to_owned())
        .collect()
}

fn expire_metadata(path: &Path) {
    let mut descriptor: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("metadata descriptor"))
            .expect("metadata descriptor JSON");
    descriptor["validated_at_unix_secs"] = serde_json::json!(0);
    std::fs::write(
        path,
        serde_json::to_vec(&descriptor).expect("descriptor JSON"),
    )
    .expect("expired descriptor");
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

async fn start_republishing_package_index_server(
    hits: Arc<AtomicUsize>,
    failure_status: axum::http::StatusCode,
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
    let router = Router::new()
        .route(
            "/v3/index.json",
            get(move || {
                let service_index = service_index.clone();
                async move { service_index }
            }),
        )
        .route(
            "/v3-flatcontainer/foo/index.json",
            get(move || {
                let hits = hits.clone();
                async move {
                    match hits.fetch_add(1, Ordering::SeqCst) {
                        0 => (axum::http::StatusCode::OK, r#"{"versions":["1.0.0"]}"#)
                            .into_response(),
                        1 => (
                            axum::http::StatusCode::OK,
                            r#"{"versions":["1.0.0","2.0.0"]}"#,
                        )
                            .into_response(),
                        _ => failure_status.into_response(),
                    }
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

async fn start_republishing_package_server(
    first_package: Vec<u8>,
    second_package: Vec<u8>,
    registration_hits: Arc<AtomicUsize>,
    package_hits: Arc<AtomicUsize>,
) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let first_hash = STANDARD.encode(Sha512::digest(&first_package));
    let second_hash = STANDARD.encode(Sha512::digest(&second_package));
    let service_index = format!(
        r#"{{"version":"3.0.0","resources":[
            {{"@id":"http://nuget.example:{}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
            {{"@id":"http://nuget.example:{}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
        ]}}"#,
        address.port(),
        address.port()
    );
    let registration = |hash: String| {
        format!(
            r#"{{
                "@id":"http://nuget.example:{}/v3/registration/foo/1.0.0.json",
                "catalogEntry":{{
                    "id":"foo",
                    "version":"1.0.0",
                    "packageHash":"{hash}",
                    "packageHashAlgorithm":"SHA512"
                }},
                "packageContent":"http://nuget.example:{}/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg"
            }}"#,
            address.port(),
            address.port()
        )
    };
    let first_registration = registration(first_hash);
    let second_registration = registration(second_hash);
    let router = Router::new()
        .route(
            "/v3/index.json",
            get(move || {
                let service_index = service_index.clone();
                async move { service_index }
            }),
        )
        .route(
            "/v3/registration/foo/1.0.0.json",
            get(move || {
                let registration_hits = registration_hits.clone();
                let first_registration = first_registration.clone();
                let second_registration = second_registration.clone();
                async move {
                    match registration_hits.fetch_add(1, Ordering::SeqCst) {
                        0 => (axum::http::StatusCode::OK, first_registration).into_response(),
                        _ => (axum::http::StatusCode::OK, second_registration).into_response(),
                    }
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
                let package_hits = package_hits.clone();
                let first_package = first_package.clone();
                let second_package = second_package.clone();
                async move {
                    match package_hits.fetch_add(1, Ordering::SeqCst) {
                        0 => (axum::http::StatusCode::OK, first_package).into_response(),
                        _ => (axum::http::StatusCode::OK, second_package).into_response(),
                    }
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
