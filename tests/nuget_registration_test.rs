use std::{net::SocketAddr, path::Path, sync::Arc};

use axum::{Router, body::Body, http::Request, routing::get};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, http::router_with_registry,
    nuget::NugetAdapter, sources::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn registration_index_rewrites_leaf_and_package_content_relationships() {
    let (upstream, address) = start_registration_server(false, false).await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    let service_index = router
        .clone()
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");
    assert_eq!(service_index.status(), axum::http::StatusCode::OK);

    let response = router
        .clone()
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/index.json",
        ))
        .await
        .expect("registration response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("registration body");
    let registration: serde_json::Value = serde_json::from_slice(&body).expect("registration JSON");
    assert_eq!(
        registration["items"][0]["@id"],
        "http://127.0.0.1:18761/mirror/nuget/official/registration/foo/1.0.0.json"
    );
    assert_eq!(
        registration["items"][0]["packageContent"],
        "http://127.0.0.1:18761/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg"
    );
    assert!(!String::from_utf8_lossy(&body).contains("nuget.example"));
    assert!(
        data_dir
            .path()
            .join("stores/nuget/metadata/official/bindings/foo/1.0.0.json")
            .exists()
    );

    let cached = router
        .clone()
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/index.json",
        ))
        .await
        .expect("cached registration response");
    assert_eq!(cached.status(), axum::http::StatusCode::OK);
    let audit =
        std::fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(audit.contains(r#""event":"nuget_registration_read""#));
    assert!(audit.contains(r#""cache_status":"hit""#));

    let leaf = router
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/1.0.0.json",
        ))
        .await
        .expect("registration leaf response");
    assert_eq!(leaf.status(), axum::http::StatusCode::OK);
    let leaf: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(leaf.into_body(), usize::MAX)
            .await
            .expect("registration leaf body"),
    )
    .expect("registration leaf JSON");
    assert_eq!(
        leaf["items"][0]["packageContent"],
        "http://127.0.0.1:18761/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg"
    );
}

#[tokio::test]
async fn registration_cache_rebases_after_the_public_root_changes() {
    let (upstream, address) = start_registration_server(false, false).await;
    let data_dir = tempdir().expect("data directory");
    let first = nuget_router_with_root(
        upstream.clone(),
        address,
        data_dir.path(),
        Url::parse("http://127.0.0.1:18761").expect("first root URL"),
    );
    let second = nuget_router_with_root(
        upstream,
        address,
        data_dir.path(),
        Url::parse("https://pkg2.example").expect("second root URL"),
    );

    let service_index = first
        .clone()
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");
    assert_eq!(service_index.status(), axum::http::StatusCode::OK);
    let registration = first
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/index.json",
        ))
        .await
        .expect("first registration response");
    assert_eq!(registration.status(), axum::http::StatusCode::OK);

    let rebased = second
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/index.json",
        ))
        .await
        .expect("rebased registration response");
    assert_eq!(rebased.status(), axum::http::StatusCode::OK);
    let rebased: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(rebased.into_body(), usize::MAX)
            .await
            .expect("rebased registration body"),
    )
    .expect("rebased registration JSON");
    assert_eq!(
        rebased["items"][0]["@id"],
        "https://pkg2.example/mirror/nuget/official/registration/foo/1.0.0.json"
    );
    assert_eq!(
        rebased["items"][0]["packageContent"],
        "https://pkg2.example/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg"
    );
}

#[tokio::test]
async fn registration_rejects_package_content_outside_the_discovered_package_resource() {
    let (upstream, address) = start_registration_server(true, false).await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    let _ = router
        .clone()
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");
    let response = router
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/index.json",
        ))
        .await
        .expect("registration response");

    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("stores/nuget/metadata/official/bindings/foo/1.0.0.json")
            .exists()
    );
}

#[tokio::test]
async fn registration_leaf_rejects_a_catalog_version_different_from_the_request() {
    let (upstream, address) = start_registration_server(false, true).await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    let response = router
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/1.0.0.json",
        ))
        .await
        .expect("registration response");

    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("stores/nuget/metadata/official/bindings/foo/2.0.0.json")
            .exists()
    );
}

#[tokio::test]
async fn invalid_registration_paths_return_not_found() {
    let (upstream, address) = start_registration_server(false, false).await;
    let router = nuget_router(upstream, address, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/not-json",
        ))
        .await
        .expect("registration response");

    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn registration_index_accepts_the_official_document_shape() {
    let (upstream, address) = start_official_shape_server().await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, address, data_dir.path());

    let response = router
        .oneshot(get_request(
            "/mirror/nuget/official/registration/foo/index.json",
        ))
        .await
        .expect("registration response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("registration body");
    let registration: serde_json::Value = serde_json::from_slice(&body).expect("registration JSON");
    let page = &registration["items"][0];
    assert_eq!(
        page["@id"],
        "http://127.0.0.1:18761/mirror/nuget/official/registration/foo/page/0.json"
    );
    assert_eq!(
        page["parent"],
        "http://127.0.0.1:18761/mirror/nuget/official/registration/foo/index.json"
    );
    let leaf = &page["items"][0];
    assert_eq!(
        leaf["@id"],
        "http://127.0.0.1:18761/mirror/nuget/official/registration/foo/1.0.0.json"
    );
    assert_eq!(
        leaf["registration"],
        "http://127.0.0.1:18761/mirror/nuget/official/registration/foo/index.json"
    );
    assert_eq!(
        leaf["packageContent"],
        "http://127.0.0.1:18761/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg"
    );
    assert_eq!(
        leaf["catalogEntry"]["packageContent"],
        "http://127.0.0.1:18761/mirror/nuget/official/package/foo/1.0.0/foo.1.0.0.nupkg"
    );
    assert!(leaf["catalogEntry"]["@id"].is_null());
    assert_eq!(
        leaf["catalogEntry"]["dependencyGroups"][0]["targetFramework"],
        ".NETStandard2.0"
    );
    assert!(leaf["catalogEntry"]["dependencyGroups"][0]["@id"].is_null());
    assert_eq!(registration["@context"]["packageContent"]["@type"], "@id");
    assert!(!String::from_utf8_lossy(&body).contains("nuget.example"));
    assert!(
        data_dir
            .path()
            .join("stores/nuget/metadata/official/bindings/foo/1.0.0.json")
            .exists()
    );
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
    nuget_router_with_root(
        upstream,
        address,
        data_dir,
        Url::parse("http://127.0.0.1:18761").expect("root URL"),
    )
}

fn nuget_router_with_root(
    upstream: Url,
    address: SocketAddr,
    data_dir: &Path,
    root_url: Url,
) -> axum::Router {
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
            Arc::new(NugetAdapter::new(root_url, data_dir)),
        )
        .expect("registry");
    router_with_registry(registry)
}

async fn start_official_shape_server() -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let service_index = format!(
        r#"{{
            "version": "3.0.0",
            "resources": [
                {{"@id":"http://nuget.example:{}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                {{"@id":"http://nuget.example:{}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
            ]
        }}"#,
        address.port(),
        address.port()
    );
    let registration_index = format!(
        r#"{{
            "@id":"http://nuget.example:{port}/v3/registration/foo/index.json",
            "@type":["catalog:CatalogRoot","PackageRegistration","catalog:Permalink"],
            "@context":{{
                "@vocab":"http://schema.nuget.org/schema#",
                "catalog":"http://schema.nuget.org/catalog#",
                "packageContent":{{"@type":"@id"}},
                "registration":{{"@type":"@id"}}
            }},
            "count":1,
            "items":[{{
                "@id":"http://nuget.example:{port}/v3/registration/foo/page/0.json",
                "@type":"catalog:Permalink",
                "count":1,
                "lower":"1.0.0",
                "upper":"1.0.0",
                "parent":"http://nuget.example:{port}/v3/registration/foo/index.json",
                "items":[{{
                    "@id":"http://nuget.example:{port}/v3/registration/foo/1.0.0.json",
                    "@type":"Package",
                    "catalogEntry":{{
                        "@id":"http://nuget.example:{port}/v3/catalog0/data/foo.1.0.0.json",
                        "id":"foo",
                        "version":"1.0.0",
                        "packageContent":"http://nuget.example:{port}/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg",
                        "dependencyGroups":[{{
                            "@id":"http://nuget.example:{port}/v3/catalog0/data/foo.1.0.0.json#dependencygroup/.netstandard2.0",
                            "targetFramework":".NETStandard2.0"
                        }}]
                    }},
                    "packageContent":"http://nuget.example:{port}/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg",
                    "registration":"http://nuget.example:{port}/v3/registration/foo/index.json"
                }}]
            }}]
        }}"#,
        port = address.port()
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
            "/v3/registration/foo/index.json",
            get({
                let registration_index = registration_index.clone();
                move || {
                    let registration_index = registration_index.clone();
                    async move { registration_index }
                }
            }),
        )
        .route(
            "/v3/registration/foo/page/0.json",
            get({
                let registration_index = registration_index.clone();
                move || {
                    let registration_index = registration_index.clone();
                    async move { registration_index }
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

async fn start_registration_server(
    external_content: bool,
    mismatched_version: bool,
) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let content_version = if mismatched_version { "2.0.0" } else { "1.0.0" };
    let content_url = if external_content {
        format!(
            "http://evil.example/v3-flatcontainer/foo/{content_version}/foo.{content_version}.nupkg"
        )
    } else {
        format!(
            "http://nuget.example:{}/v3-flatcontainer/foo/{content_version}/foo.{content_version}.nupkg",
            address.port(),
        )
    };
    let service_index = format!(
        r#"{{
            "version": "3.0.0",
            "resources": [
                {{"@id":"http://nuget.example:{}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                {{"@id":"http://nuget.example:{}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
            ]
        }}"#,
        address.port(),
        address.port()
    );
    let registration_index = format!(
        r#"{{
            "@id":"http://nuget.example:{}/v3/registration/foo/index.json",
            "count":1,
            "items":[{{
                "@id":"http://nuget.example:{}/v3/registration/foo/1.0.0.json",
                "catalogEntry":{{"id":"foo","version":"{}"}},
                "packageContent":"{}"
            }}]
        }}"#,
        address.port(),
        address.port(),
        content_version,
        content_url
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
            "/v3/registration/foo/index.json",
            get({
                let registration_index = registration_index.clone();
                move || {
                    let registration_index = registration_index.clone();
                    async move { registration_index }
                }
            }),
        )
        .route(
            "/v3/registration/foo/1.0.0.json",
            get({
                let registration_index = registration_index.clone();
                move || {
                    let registration_index = registration_index.clone();
                    async move { registration_index }
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
