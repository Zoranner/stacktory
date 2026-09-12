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
async fn registration_falls_back_without_merging_sources() {
    let (port, address) = start_group_server(false, Arc::new(AtomicUsize::new(0))).await;
    let data_dir = tempdir().expect("data directory");
    let router = group_router(port, address, data_dir.path());

    let index = router
        .clone()
        .oneshot(get_request("/mirror/nuget/group/v3/index.json"))
        .await
        .expect("service index response");
    assert_eq!(index.status(), axum::http::StatusCode::OK);

    let response = router
        .oneshot(get_request(
            "/mirror/nuget/group/registration/foo/index.json",
        ))
        .await
        .expect("registration response");
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("registration body");
    assert!(String::from_utf8_lossy(&body).contains("\"marker\":\"second\""));
    assert!(!String::from_utf8_lossy(&body).contains("\"marker\":\"first\""));
    let source: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            data_dir
                .path()
                .join("nuget/metadata/group/service-index.source"),
        )
        .expect("service index source"),
    )
    .expect("service index source JSON");
    assert_eq!(source["selected_proxy"], "second");
    let audit =
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(audit.contains(r#""event":"nuget_registration_fallback""#));
    assert!(audit.contains(r#""selected_proxy":"second""#));
}

#[tokio::test]
async fn package_integrity_failure_falls_back_and_updates_binding_source() {
    let package = b"package from second".to_vec();
    let package_hash = STANDARD.encode(Sha512::digest(&package));
    let package_requests = Arc::new(AtomicUsize::new(0));
    let (port, address) = start_group_server(true, package_requests.clone()).await;
    let data_dir = tempdir().expect("data directory");
    let router = group_router(port, address, data_dir.path());

    let _ = router
        .clone()
        .oneshot(get_request("/mirror/nuget/group/v3/index.json"))
        .await
        .expect("service index response");
    let registration = router
        .clone()
        .oneshot(get_request(
            "/mirror/nuget/group/registration/foo/1.0.0.json",
        ))
        .await
        .expect("registration response");
    assert_eq!(registration.status(), axum::http::StatusCode::OK);

    let response = router
        .oneshot(get_request(
            "/mirror/nuget/group/package/foo/1.0.0/foo.1.0.0.nupkg",
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
    assert_eq!(package_requests.load(Ordering::SeqCst), 2);
    let binding: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            data_dir
                .path()
                .join("nuget/metadata/group/bindings/foo/1.0.0.json"),
        )
        .expect("binding"),
    )
    .expect("binding JSON");
    assert_eq!(binding["selected_proxy"], "second");
    assert_eq!(package_hash.len(), 88);
    let audit =
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(audit.contains(r#""event":"nuget_package_fallback""#));
    assert!(audit.contains(r#""failure_category":"integrity_mismatch""#));
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

fn group_router(port: u16, address: SocketAddr, data_dir: &Path) -> axum::Router {
    let first = format!("http://first.example:{port}/first/v3/index.json");
    let second = format!("http://second.example:{port}/second/v3/index.json");
    let registry = AdapterRegistry::new()
        .with_adapter(
            "nuget",
            Arc::new(
                SourcePool::new_with_resolvers(
                    "nuget",
                    SourceGraph::new(vec![
                        proxy("first", &first),
                        proxy("second", &second),
                        SourceConfig {
                            name: "group".to_owned(),
                            source_type: SourceType::Group,
                            base_url: None,
                            require_https: false,
                            members: vec!["first".to_owned(), "second".to_owned()],
                        },
                    ])
                    .expect("source graph"),
                    &[("first.example", address), ("second.example", address)],
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

async fn start_group_server(
    package_mode: bool,
    package_requests: Arc<AtomicUsize>,
) -> (u16, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let port = address.port();
    let package_hash = STANDARD.encode(Sha512::digest(b"package from second"));
    let router = Router::new()
        .route(
            "/first/v3/index.json",
            get(move || async move {
                format!(
                    r#"{{"version":"3.0.0","resources":[
                        {{"@id":"http://first.example:{port}/first/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                        {{"@id":"http://first.example:{port}/first/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
                    ]}}"#
                )
            }),
        )
        .route(
            "/second/v3/index.json",
            get(move || async move {
                format!(
                    r#"{{"version":"3.0.0","resources":[
                        {{"@id":"http://second.example:{port}/second/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                        {{"@id":"http://second.example:{port}/second/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
                    ]}}"#
                )
            }),
        )
        .route(
            "/first/v3/registration/foo/index.json",
            get({
                let package_hash = package_hash.clone();
                move || {
                    let package_hash = package_hash.clone();
                    async move {
                        if package_mode {
                            valid_registration(port, "first", "foo", &package_hash)
                        } else {
                            valid_registration(port, "first", "wrong", &package_hash)
                        }
                    }
                }
            }),
        )
        .route(
            "/second/v3/registration/foo/index.json",
            get({
                let package_hash = package_hash.clone();
                move || {
                    let package_hash = package_hash.clone();
                    async move { valid_registration(port, "second", "foo", &package_hash) }
                }
            }),
        )
        .route(
            "/first/v3/registration/foo/1.0.0.json",
            get({
                let package_hash = package_hash.clone();
                move || {
                    let package_hash = package_hash.clone();
                    async move {
                        valid_registration(port, "first", "foo", &package_hash)
                    }
                }
            }),
        )
        .route(
            "/second/v3/registration/foo/1.0.0.json",
            get({
                let package_hash = package_hash.clone();
                move || {
                    let package_hash = package_hash.clone();
                    async move { valid_registration(port, "second", "foo", &package_hash) }
                }
            }),
        )
        .route(
            "/first/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg",
            get({
                let package_requests = package_requests.clone();
                move || {
                    let package_requests = package_requests.clone();
                    async move {
                        package_requests.fetch_add(1, Ordering::SeqCst);
                        b"tampered package".to_vec()
                    }
                }
            }),
        )
        .route(
            "/second/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg",
            get({
                let package_requests = package_requests.clone();
                move || {
                    let package_requests = package_requests.clone();
                    async move {
                        package_requests.fetch_add(1, Ordering::SeqCst);
                        b"package from second".to_vec()
                    }
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    (port, address)
}

fn valid_registration(port: u16, source: &str, package_name: &str, package_hash: &str) -> String {
    format!(
        r#"{{"marker":"{source}","@id":"http://{source}.example:{port}/{source}/v3/registration/foo/1.0.0.json","catalogEntry":{{"id":"foo","version":"1.0.0","packageHash":"{package_hash}","packageHashAlgorithm":"SHA512"}},"packageContent":"http://{source}.example:{port}/{source}/v3-flatcontainer/{package_name}/1.0.0/{package_name}.1.0.0.nupkg"}}"#
    )
}
