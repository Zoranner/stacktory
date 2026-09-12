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
    let (upstream, address) = start_registration_server(false).await;
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
            .join("nuget/metadata/official/bindings/foo/1.0.0.json")
            .exists()
    );

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
async fn registration_rejects_package_content_outside_the_discovered_package_resource() {
    let (upstream, address) = start_registration_server(true).await;
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
            .join("nuget/metadata/official/bindings/foo/1.0.0.json")
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

async fn start_registration_server(external_content: bool) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let content_url = if external_content {
        "http://evil.example/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg".to_owned()
    } else {
        format!(
            "http://nuget.example:{}/v3-flatcontainer/foo/1.0.0/foo.1.0.0.nupkg",
            address.port()
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
                "catalogEntry":{{"id":"foo","version":"1.0.0"}},
                "packageContent":"{}"
            }}]
        }}"#,
        address.port(),
        address.port(),
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
