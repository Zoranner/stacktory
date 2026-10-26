use std::{
    fs,
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
    http::{
        Request, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::IntoResponse,
};
use sha2::{Digest, Sha512};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, ecosystems::npm::NpmAdapter,
    http::router_with_registry, upstream::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn serves_unscoped_and_scoped_metadata_with_standard_tarball_urls() {
    let package_body = b"verified npm tarball".to_vec();
    let artifact_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, _) = start_npm_server(
        "left-pad",
        package_body.clone(),
        Arc::new(AtomicUsize::new(0)),
        artifact_requests.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(vec![proxy("official", upstream.as_str())], data_dir.path());

    let metadata = router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("metadata response");
    assert_eq!(metadata.status(), StatusCode::OK);
    let metadata: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(metadata.into_body(), usize::MAX)
            .await
            .expect("metadata body"),
    )
    .expect("metadata JSON");
    let tarball = metadata["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .expect("standard tarball URL");
    assert_eq!(
        tarball,
        "http://127.0.0.1:18761/mirror/npm/official/left-pad/-/left-pad-1.0.0.tgz"
    );
    assert!(!tarball.contains("registry.example"));

    let tarball_path = Url::parse(tarball).expect("tarball URL").path().to_owned();
    let artifact = router
        .clone()
        .oneshot(get_request(&tarball_path))
        .await
        .expect("tarball response");
    assert_eq!(artifact.status(), StatusCode::OK);
    let content_length = artifact
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    assert_eq!(content_length, Some(package_body.len().to_string()));
    assert_eq!(
        axum::body::to_bytes(artifact.into_body(), usize::MAX)
            .await
            .expect("tarball body"),
        package_body
    );
    let cached = router
        .clone()
        .oneshot(get_request(&tarball_path))
        .await
        .expect("cached tarball response");
    assert_eq!(cached.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(cached.into_body(), usize::MAX)
            .await
            .expect("cached tarball body"),
        package_body
    );
    assert_eq!(artifact_requests.load(Ordering::SeqCst), 1);
    let temporary_files = std::fs::read_dir(data_dir.path().join("tmp"))
        .expect("temporary directory")
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("npm-") && name.ends_with(".part")
        })
        .count();
    assert_eq!(temporary_files, 0);

    let scoped = router
        .oneshot(get_request("/mirror/npm/official/@scope%2Fpackage"))
        .await
        .expect("scoped metadata response");
    assert_eq!(scoped.status(), StatusCode::OK);
}

#[tokio::test]
async fn serves_standard_tarball_paths_without_prior_metadata_requests() {
    let package_body = b"verified npm tarball".to_vec();
    let metadata_requests = Arc::new(AtomicUsize::new(0));
    let artifact_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, _) = start_npm_server(
        "left-pad",
        package_body.clone(),
        metadata_requests.clone(),
        artifact_requests.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(vec![proxy("official", upstream.as_str())], data_dir.path());

    for path in [
        "/mirror/npm/official/left-pad/-/left-pad-1.0.0.tgz",
        "/mirror/npm/official/@scope/package/-/package-1.0.0.tgz",
    ] {
        let response = router
            .clone()
            .oneshot(get_request(path))
            .await
            .expect("standard tarball response");
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("standard tarball body"),
            package_body
        );
    }
    assert_eq!(metadata_requests.load(Ordering::SeqCst), 2);
    assert_eq!(artifact_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cached_metadata_repairs_a_missing_tarball_binding() {
    let package_body = b"verified npm tarball".to_vec();
    let metadata_requests = Arc::new(AtomicUsize::new(0));
    let artifact_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, _) = start_npm_server(
        "left-pad",
        package_body.clone(),
        metadata_requests.clone(),
        artifact_requests.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(vec![proxy("official", upstream.as_str())], data_dir.path());

    let first = router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("metadata response");
    assert_eq!(first.status(), StatusCode::OK);
    let first: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("metadata body"),
    )
    .expect("metadata JSON");
    let tarball_path = Url::parse(
        first["versions"]["1.0.0"]["dist"]["tarball"]
            .as_str()
            .expect("tarball URL"),
    )
    .expect("tarball URL")
    .path()
    .to_owned();
    let descriptor_dir = data_dir
        .path()
        .join("stores/npm/metadata/official/descriptors");
    let descriptor = fs::read_dir(&descriptor_dir)
        .expect("descriptor directory")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .expect("metadata descriptor");
    assert!(descriptor.exists());
    fs::remove_file(&descriptor).expect("remove descriptor");

    let second = router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("cached metadata response");
    assert_eq!(second.status(), StatusCode::OK);
    assert!(descriptor.exists(), "metadata hit must restore the binding");
    assert_eq!(metadata_requests.load(Ordering::SeqCst), 1);

    let artifact = router
        .oneshot(get_request(&tarball_path))
        .await
        .expect("tarball response");
    assert_eq!(artifact.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(artifact.into_body(), usize::MAX)
            .await
            .expect("tarball body"),
        package_body
    );
    assert_eq!(artifact_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn falls_back_after_tarball_integrity_failure_and_keeps_metadata_source_binding() {
    let expected_body = b"verified npm tarball".to_vec();
    let first_metadata_requests = Arc::new(AtomicUsize::new(0));
    let first_artifact_requests = Arc::new(AtomicUsize::new(0));
    let second_metadata_requests = Arc::new(AtomicUsize::new(0));
    let second_artifact_requests = Arc::new(AtomicUsize::new(0));
    let (first, _) = start_npm_server(
        "left-pad",
        b"tampered".to_vec(),
        first_metadata_requests.clone(),
        first_artifact_requests.clone(),
    )
    .await;
    let (second, _) = start_npm_server(
        "left-pad",
        expected_body.clone(),
        second_metadata_requests.clone(),
        second_artifact_requests.clone(),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(
        vec![
            proxy("first", first.as_str()),
            proxy("second", second.as_str()),
            group("group", &["first", "second"]),
        ],
        data_dir.path(),
    );

    let metadata = router
        .clone()
        .oneshot(get_request("/mirror/npm/group/left-pad"))
        .await
        .expect("metadata response");
    assert_eq!(metadata.status(), StatusCode::OK);
    let metadata: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(metadata.into_body(), usize::MAX)
            .await
            .expect("metadata body"),
    )
    .expect("metadata JSON");
    let tarball_path = Url::parse(
        metadata["versions"]["1.0.0"]["dist"]["tarball"]
            .as_str()
            .expect("tarball URL"),
    )
    .expect("tarball URL")
    .path()
    .to_owned();

    let artifact = router
        .oneshot(get_request(&tarball_path))
        .await
        .expect("fallback tarball response");
    assert_eq!(artifact.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(artifact.into_body(), usize::MAX)
            .await
            .expect("fallback body"),
        expected_body
    );
    assert_eq!(first_metadata_requests.load(Ordering::SeqCst), 1);
    assert_eq!(first_artifact_requests.load(Ordering::SeqCst), 1);
    assert_eq!(second_metadata_requests.load(Ordering::SeqCst), 1);
    assert_eq!(second_artifact_requests.load(Ordering::SeqCst), 1);
    assert!(
        data_dir
            .path()
            .join("stores/npm/artifacts/second/objects")
            .exists()
    );
}

#[tokio::test]
async fn rejects_unknown_and_legacy_artifact_paths() {
    let (upstream, _) = start_npm_server(
        "left-pad",
        b"verified npm tarball".to_vec(),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let router = npm_router(
        vec![proxy("official", upstream.as_str())],
        tempdir().expect("data directory").path(),
    );

    let unbound = router
        .clone()
        .oneshot(get_request(&format!(
            "/mirror/npm/official/tarballs/{}",
            "a".repeat(64)
        )))
        .await
        .expect("unbound artifact response");
    let decoded_scope = router
        .oneshot(get_request("/mirror/npm/official/@scope/package"))
        .await
        .expect("decoded scope response");

    assert_eq!(unbound.status(), StatusCode::NOT_FOUND);
    assert_eq!(decoded_scope.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn falls_back_for_forbidden_upstream() {
    let first_hits = Arc::new(AtomicUsize::new(0));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let first = start_status_server(StatusCode::FORBIDDEN, first_hits.clone()).await;
    let second = start_npm_server(
        "left-pad",
        b"verified npm tarball".to_vec(),
        second_hits.clone(),
        Arc::new(AtomicUsize::new(0)),
    )
    .await
    .0;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(
        vec![
            proxy("first", first.as_str()),
            proxy("second", second.as_str()),
            group("group", &["first", "second"]),
        ],
        data_dir.path(),
    );

    let response = router
        .oneshot(get_request("/mirror/npm/group/left-pad"))
        .await
        .expect("terminal response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(Ordering::SeqCst), 1);
    let audit =
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(audit.contains(r#""event":"npm_metadata_source_failed""#));
    assert!(audit.contains(r#""selected_proxy":"first""#));
    assert!(audit.contains(r#""failure_category":"source_failed""#));
}

#[tokio::test]
async fn re_renders_metadata_for_a_new_public_root_without_upstream_refetch() {
    let metadata_requests = Arc::new(AtomicUsize::new(0));
    let artifact_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, _) = start_npm_server(
        "left-pad",
        b"verified npm tarball".to_vec(),
        metadata_requests.clone(),
        artifact_requests,
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let sources = vec![proxy("official", upstream.as_str())];
    let first_router = npm_router_with_root(
        Url::parse("http://127.0.0.1:18761").expect("first root URL"),
        sources.clone(),
        data_dir.path(),
    );
    let second_router = npm_router_with_root(
        Url::parse("https://pkg2.example").expect("second root URL"),
        sources,
        data_dir.path(),
    );

    let first = first_router
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("first metadata response");
    assert_eq!(first.status(), StatusCode::OK);
    let first: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("first metadata body"),
    )
    .expect("first metadata JSON");
    assert!(
        first["versions"]["1.0.0"]["dist"]["tarball"]
            .as_str()
            .expect("first tarball URL")
            .starts_with("http://127.0.0.1:18761/")
    );

    let second = second_router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("second metadata response");
    assert_eq!(second.status(), StatusCode::OK);
    let second: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("second metadata body"),
    )
    .expect("second metadata JSON");
    assert!(
        second["versions"]["1.0.0"]["dist"]["tarball"]
            .as_str()
            .expect("second tarball URL")
            .starts_with("https://pkg2.example/")
    );
    assert_eq!(metadata_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn corrupt_metadata_cache_is_removed_and_refetched() {
    let metadata_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, _) = start_npm_server(
        "left-pad",
        b"verified npm tarball".to_vec(),
        metadata_requests.clone(),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(vec![proxy("official", upstream.as_str())], data_dir.path());

    let first = router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("first metadata response");
    assert_eq!(first.status(), StatusCode::OK);

    let body_path = data_dir
        .path()
        .join("stores/npm/metadata/official/left-pad/official/catalog-metadata.json");
    std::fs::write(&body_path, b"not canonical metadata").expect("corrupt metadata cache");

    let second = router
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("second metadata response");

    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(metadata_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn stale_metadata_is_revalidated_before_being_served() {
    let metadata_requests = Arc::new(AtomicUsize::new(0));
    let (upstream, _) = start_npm_server(
        "left-pad",
        b"verified npm tarball".to_vec(),
        metadata_requests.clone(),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(vec![proxy("official", upstream.as_str())], data_dir.path());

    let first = router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("first metadata response");
    assert_eq!(first.status(), StatusCode::OK);
    age_metadata_pointer(data_dir.path(), "npm", "official", "left-pad");

    let second = router
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("revalidated metadata response");

    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(metadata_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn deleted_upstream_metadata_removes_the_cached_entry() {
    let metadata_requests = Arc::new(AtomicUsize::new(0));
    let upstream = start_metadata_then_not_found_server(metadata_requests.clone()).await;
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(vec![proxy("official", upstream.as_str())], data_dir.path());

    let first = router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("first metadata response");
    assert_eq!(first.status(), StatusCode::OK);
    age_metadata_pointer(data_dir.path(), "npm", "official", "left-pad");

    let second = router
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("deleted metadata response");

    assert_eq!(second.status(), StatusCode::NOT_FOUND);
    assert_eq!(metadata_requests.load(Ordering::SeqCst), 2);
    assert!(
        !data_dir
            .path()
            .join("stores/npm/metadata/official/left-pad/current.toml")
            .exists()
    );
}

#[tokio::test]
async fn revalidated_metadata_replaces_a_republished_version() {
    let metadata_requests = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let first_upstream_tarball = format!("http://{address}/left-pad/-/left-pad-1.0.0.tgz");
    let second_upstream_tarball = format!("http://{address}/left-pad/-/left-pad-1.0.1.tgz");
    let integrity = format!(
        "sha512-{}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            Sha512::digest(b"verified npm tarball")
        )
    );
    let first_body = metadata_body("left-pad", &first_upstream_tarball, &integrity);
    let second_body = metadata_body("left-pad", &second_upstream_tarball, &integrity);
    let router = Router::new().fallback(move || {
        let metadata_requests = metadata_requests.clone();
        let first_body = first_body.clone();
        let second_body = second_body.clone();
        async move {
            let request = metadata_requests.fetch_add(1, Ordering::SeqCst);
            let body = if request == 0 {
                first_body
            } else {
                second_body
            };
            ([(CONTENT_TYPE, "application/json")], body).into_response()
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("republish server");
    });
    let upstream = Url::parse(&format!("http://{address}")).expect("upstream URL");
    let data_dir = tempdir().expect("data directory");
    let router = npm_router(vec![proxy("official", upstream.as_str())], data_dir.path());

    let first = router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("first metadata response");
    assert_eq!(first.status(), StatusCode::OK);
    let first: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("first metadata body"),
    )
    .expect("first metadata JSON");
    let first_tarball = first["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .expect("first tarball URL")
        .to_owned();
    let first_descriptors = descriptor_file_names(data_dir.path());
    age_metadata_pointer(data_dir.path(), "npm", "official", "left-pad");

    let second = router
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("republished metadata response");
    assert_eq!(second.status(), StatusCode::OK);
    let second: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("republished metadata body"),
    )
    .expect("republished metadata JSON");
    let tarball = second["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .expect("republished tarball URL");
    assert_eq!(
        tarball,
        "http://127.0.0.1:18761/mirror/npm/official/left-pad/-/left-pad-1.0.0.tgz"
    );
    assert_eq!(tarball, first_tarball);
    assert_ne!(descriptor_file_names(data_dir.path()), first_descriptors);
}

#[tokio::test]
async fn changed_source_base_url_does_not_reuse_canonical_metadata() {
    let first_requests = Arc::new(AtomicUsize::new(0));
    let second_requests = Arc::new(AtomicUsize::new(0));
    let (first_upstream, _) = start_npm_server(
        "left-pad",
        b"verified npm tarball".to_vec(),
        first_requests.clone(),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let (second_upstream, _) = start_npm_server(
        "left-pad",
        b"verified npm tarball".to_vec(),
        second_requests.clone(),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let data_dir = tempdir().expect("data directory");
    let first_router = npm_router(
        vec![proxy("official", first_upstream.as_str())],
        data_dir.path(),
    );
    let second_router = npm_router(
        vec![proxy("official", second_upstream.as_str())],
        data_dir.path(),
    );

    let first = first_router
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("first metadata response");
    assert_eq!(first.status(), StatusCode::OK);
    let first: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("first metadata body"),
    )
    .expect("first metadata JSON");
    let first_tarball = first["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .expect("first tarball URL")
        .to_owned();

    let second = second_router
        .clone()
        .oneshot(get_request("/mirror/npm/official/left-pad"))
        .await
        .expect("second metadata response");
    assert_eq!(second.status(), StatusCode::OK);
    let second: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("second metadata body"),
    )
    .expect("second metadata JSON");
    let second_tarball = second["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .expect("second tarball URL");

    assert_eq!(first_tarball, second_tarball);
    assert_eq!(first_requests.load(Ordering::SeqCst), 1);
    assert_eq!(second_requests.load(Ordering::SeqCst), 1);

    let tarball_path = Url::parse(second_tarball)
        .expect("tarball URL")
        .path()
        .to_owned();
    let tarball = second_router
        .oneshot(get_request(&tarball_path))
        .await
        .expect("tarball response");
    assert_eq!(tarball.status(), StatusCode::OK);
}

async fn start_metadata_then_not_found_server(metadata_requests: Arc<AtomicUsize>) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let tarball = format!("http://{address}/left-pad/-/left-pad-1.0.0.tgz");
    let router = Router::new().fallback(move || {
        let metadata_requests = metadata_requests.clone();
        let tarball = tarball.clone();
        async move {
            if metadata_requests.fetch_add(1, Ordering::SeqCst) > 0 {
                return StatusCode::NOT_FOUND.into_response();
            }
            let integrity = format!(
                "sha512-{}",
                base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    Sha512::digest(b"verified npm tarball")
                )
            );
            (
                [(CONTENT_TYPE, "application/json")],
                metadata_body("left-pad", &tarball, &integrity),
            )
                .into_response()
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("metadata sequence server");
    });
    Url::parse(&format!("http://{address}")).expect("metadata sequence URL")
}

fn age_metadata_pointer(data_dir: &Path, ecosystem: &str, entry: &str, package: &str) {
    let path = data_dir
        .join("stores")
        .join(ecosystem)
        .join("metadata")
        .join(entry)
        .join(package)
        .join("current.toml");
    let contents = std::fs::read_to_string(&path).expect("metadata pointer");
    let mut updated = String::new();
    for line in contents.lines() {
        if line.trim_start().starts_with("validated_at_unix_secs =") {
            updated.push_str("validated_at_unix_secs = 0\n");
        } else {
            updated.push_str(line);
            updated.push('\n');
        }
    }
    std::fs::write(path, updated).expect("age metadata pointer");
}

fn npm_router(sources: Vec<SourceConfig>, data_dir: &Path) -> axum::Router {
    npm_router_with_root(
        Url::parse("http://127.0.0.1:18761").expect("root URL"),
        sources,
        data_dir,
    )
}

fn npm_router_with_root(
    root_url: Url,
    sources: Vec<SourceConfig>,
    data_dir: &Path,
) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "npm",
            Arc::new(
                SourcePool::new("npm", SourceGraph::new(sources).expect("source graph"))
                    .expect("source pool"),
            ),
            Arc::new(NpmAdapter::new(root_url, data_dir)),
        )
        .expect("registry");
    router_with_registry(registry)
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET request")
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

fn descriptor_file_names(data_dir: &Path) -> Vec<String> {
    let directory = data_dir.join("stores/npm/metadata/official/descriptors");
    let mut names = fs::read_dir(directory)
        .expect("descriptor directory")
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect::<Vec<_>>();
    names.sort();
    names
}

async fn start_npm_server(
    package: &str,
    artifact_body: Vec<u8>,
    metadata_requests: Arc<AtomicUsize>,
    artifact_requests: Arc<AtomicUsize>,
) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let metadata_url = format!("http://{address}/left-pad/-/left-pad-1.0.0.tgz");
    let scoped_metadata_url = format!("http://{address}/@scope/package/-/package-1.0.0.tgz");
    let package = package.to_owned();
    let expected_integrity = format!(
        "sha512-{}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            Sha512::digest(b"verified npm tarball")
        )
    );
    let router = Router::new().fallback(move |OriginalUri(uri): OriginalUri| {
        let metadata_url = metadata_url.clone();
        let scoped_metadata_url = scoped_metadata_url.clone();
        let package = package.clone();
        let artifact_body = artifact_body.clone();
        let expected_integrity = expected_integrity.clone();
        let metadata_requests = metadata_requests.clone();
        let artifact_requests = artifact_requests.clone();
        async move {
            if uri.path().ends_with(".tgz") {
                artifact_requests.fetch_add(1, Ordering::SeqCst);
                return ([(CONTENT_TYPE, "application/octet-stream")], artifact_body)
                    .into_response();
            }
            metadata_requests.fetch_add(1, Ordering::SeqCst);
            let (name, tarball) = if uri.path().contains("@scope") {
                ("@scope/package", scoped_metadata_url)
            } else {
                (package.as_str(), metadata_url)
            };
            (
                [(CONTENT_TYPE, "application/json")],
                metadata_body(name, &tarball, &expected_integrity),
            )
                .into_response()
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("npm server");
    });
    (
        Url::parse(&format!("http://{address}")).expect("upstream URL"),
        address,
    )
}

async fn start_status_server(status: StatusCode, hits: Arc<AtomicUsize>) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new().fallback(move || {
        let hits = hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            status
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("status server");
    });
    Url::parse(&format!("http://{address}")).expect("status URL")
}

fn metadata_body(package: &str, tarball: &str, integrity: &str) -> String {
    format!(
        r#"{{
            "name": "{package}",
            "dist-tags": {{"latest":"1.0.0"}},
            "versions": {{
                "1.0.0": {{
                    "name": "{package}",
                    "version": "1.0.0",
                    "dependencies": {{}},
                    "dist": {{
                        "tarball": "{tarball}",
                        "integrity": "{integrity}"
                    }}
                }}
            }}
        }}"#
    )
}
