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
async fn service_index_rewrites_restore_resources_and_omits_unsupported_resources() {
    let upstream = start_service_index_server().await;
    let data_dir = tempdir().expect("data directory");
    let registry = AdapterRegistry::new()
        .with_adapter(
            "nuget",
            Arc::new(
                SourcePool::new_with_resolver(
                    "nuget",
                    SourceGraph::new(vec![proxy("official", upstream.as_str())])
                        .expect("source graph"),
                    "nuget.example",
                    upstream_socket(&upstream),
                )
                .expect("source pool"),
            ),
            Arc::new(NugetAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir.path(),
            )),
        )
        .expect("registry");

    let response = router_with_registry(registry)
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("service index body");
    let index: serde_json::Value = serde_json::from_slice(&body).expect("service index JSON");
    let resources = index["resources"].as_array().expect("resources");
    assert_eq!(resources.len(), 4);

    let registration = resources
        .iter()
        .find(|resource| resource["@type"] == "RegistrationsBaseUrl/3.6.0")
        .expect("registration resource")["@id"]
        .as_str()
        .expect("registration URL");
    assert_eq!(
        registration,
        "http://127.0.0.1:18761/mirror/nuget/official/registration/"
    );

    let package_base = resources
        .iter()
        .find(|resource| resource["@type"] == "PackageBaseAddress/3.0.0.0")
        .expect("package base resource")["@id"]
        .as_str()
        .expect("package base URL");
    assert_eq!(
        package_base,
        "http://127.0.0.1:18761/mirror/nuget/official/package/"
    );
    assert!(String::from_utf8_lossy(&body).contains("SearchQueryService"));
    assert!(!String::from_utf8_lossy(&body).contains("evil.example"));

    let vulnerability = resources
        .iter()
        .find(|resource| resource["@type"] == "VulnerabilityInfo/6.7.0")
        .expect("vulnerability resource")["@id"]
        .as_str()
        .expect("vulnerability URL");
    assert_eq!(
        vulnerability,
        "http://127.0.0.1:18761/mirror/nuget/official/vulnerability/index.json"
    );

    assert!(
        data_dir
            .path()
            .join("stores/nuget/metadata/official/service-index.json")
            .exists()
    );
    assert!(
        data_dir
            .path()
            .join("stores/nuget/metadata/official/service-index.source")
            .exists()
    );
}

#[tokio::test]
async fn service_index_rejects_unknown_resource_relationships() {
    let upstream = start_server(Router::new().route(
        "/v3/index.json",
        get(|| async {
            r#"{"version":"3.0.0","resources":[{"@id":"http://127.0.0.2/resource","@type":"PackageBaseAddress/3.0.0.0"}]}"#
        }),
    ))
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream.clone(), data_dir.path());

    let response = router
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("unsafe service index response");

    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("stores/nuget/metadata/official/service-index.json")
            .exists()
    );
}

#[tokio::test]
async fn service_index_rejects_unsafe_vulnerability_resources() {
    let upstream = start_service_index_with_body(|port| {
        format!(
            r#"{{"version":"3.0.0","resources":[
                {{"@id":"http://127.0.0.1:{port}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                {{"@id":"http://127.0.0.1:{port}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}},
                {{"@id":"http://127.0.0.2/v3/vulnerabilities/index.json","@type":"VulnerabilityInfo/6.7.0"}}
            ]}}"#
        )
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("unsafe vulnerability response");

    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
    assert!(
        !data_dir
            .path()
            .join("stores/nuget/metadata/official/service-index.json")
            .exists()
    );
}

#[tokio::test]
async fn service_index_returns_internal_error_when_audit_storage_fails() {
    let data_dir = tempdir().expect("data directory");
    let metadata_dir = data_dir.path().join("stores/nuget/metadata/official");
    std::fs::create_dir_all(&metadata_dir).expect("metadata directory");
    std::fs::write(
        metadata_dir.join("service-index.json"),
        br#"{"version":"3.0.0","resources":[
            {"@id":"http://127.0.0.1:9/mirror/nuget/official/registration/","@type":"RegistrationsBaseUrl/3.6.0"},
            {"@id":"http://127.0.0.1:9/mirror/nuget/official/package/","@type":"PackageBaseAddress/3.0.0"}
        ]}"#,
    )
    .expect("service index cache");
    std::fs::write(
        metadata_dir.join("service-index.source"),
        br#"{"selected_proxy":"official","registration_url":"http://127.0.0.1:9/v3/registration/","package_base_url":"http://127.0.0.1:9/v3-flatcontainer/"}"#,
    )
    .expect("service index sidecar");
    std::fs::write(data_dir.path().join("audit"), b"not a directory").expect("audit blocker");
    let upstream = Url::parse("http://127.0.0.1:9/v3/index.json").expect("unused upstream URL");
    let router = nuget_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");

    assert_eq!(
        response.status(),
        axum::http::StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn service_index_rejects_incomplete_restore_resources() {
    let upstream = start_service_index_with_body(|port| {
        format!(
            r#"{{"version":"3.0.0","resources":[{{"@id":"http://127.0.0.1:{port}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}}]}}"#
        )
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");

    assert_eq!(response.status(), axum::http::StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn service_index_selects_one_restore_resource_from_duplicate_aliases() {
    let upstream = start_service_index_with_body(|port| {
        format!(
            r#"{{"version":"3.0.0","resources":[
                {{"@id":"http://127.0.0.1:{port}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                {{"@id":"http://127.0.0.1:{port}/v3/registration2/","@type":"RegistrationsBaseUrl/3.4.0"}},
                {{"@id":"http://127.0.0.1:{port}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
            ]}}"#
        )
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("service index body");
    let index: serde_json::Value = serde_json::from_slice(&body).expect("service index JSON");
    assert_eq!(index["resources"].as_array().expect("resources").len(), 2);
}

#[tokio::test]
async fn service_index_prefers_the_most_capable_registration_resource() {
    let upstream = start_service_index_with_body(|port| {
        format!(
            r#"{{"version":"3.0.0","resources":[
                {{"@id":"http://127.0.0.1:{port}/v3/registration5-semver1/","@type":"RegistrationsBaseUrl"}},
                {{"@id":"http://127.0.0.1:{port}/v3/registration5-gz-semver2/","@type":"RegistrationsBaseUrl/Versioned"}},
                {{"@id":"http://127.0.0.1:{port}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
            ]}}"#
        )
    })
    .await;
    let port = upstream.port().expect("port");
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let descriptor: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            data_dir
                .path()
                .join("stores/nuget/metadata/official/service-index.source"),
        )
        .expect("service index source"),
    )
    .expect("service index source JSON");
    assert_eq!(
        descriptor["registration_url"],
        format!("http://127.0.0.1:{port}/v3/registration5-gz-semver2/")
    );
}

#[tokio::test]
async fn service_index_accepts_a_declared_public_cross_origin_package_resource() {
    let upstream = start_service_index_with_body(|port| {
        format!(
            r#"{{"version":"3.0.0","resources":[
                {{"@id":"http://127.0.0.1:{port}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                {{"@id":"https://api.nuget.org/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0","comment":"https://api.nuget.org/v3-flatcontainer/{{id}}"}}
            ]}}"#
        )
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = nuget_router(upstream, data_dir.path());

    let response = router
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("service index body");
    assert!(String::from_utf8_lossy(&body).contains("/mirror/nuget/official/package/"));
    assert!(!String::from_utf8_lossy(&body).contains("api.nuget.org"));
}

#[tokio::test]
async fn service_index_follows_a_safe_public_redirect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let body = format!(
        r#"{{"version":"3.0.0","resources":[
            {{"@id":"http://redirect-target.example:{}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
            {{"@id":"http://redirect-target.example:{}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}}
        ]}}"#,
        address.port(),
        address.port()
    );
    let router = Router::new()
        .route(
            "/v3/index.json",
            get({
                let body = body.clone();
                move || {
                    let body = body.clone();
                    async move { body }
                }
            }),
        )
        .route(
            "/redirect/v3/index.json",
            get(|| async {
                (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, "/v3/index.json")],
                )
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    let base = Url::parse(&format!(
        "http://redirect-source.example:{}/redirect/v3/index.json",
        address.port()
    ))
    .expect("source URL");
    let data_dir = tempdir().expect("data directory");
    let registry = AdapterRegistry::new()
        .with_adapter(
            "nuget",
            Arc::new(
                SourcePool::new_with_resolvers(
                    "nuget",
                    SourceGraph::new(vec![proxy("official", base.as_str())]).expect("source graph"),
                    &[
                        ("redirect-source.example", address),
                        ("redirect-target.example", address),
                    ],
                )
                .expect("source pool"),
            ),
            Arc::new(NugetAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir.path(),
            )),
        )
        .expect("registry");

    let response = router_with_registry(registry)
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
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

fn nuget_router(upstream: Url, data_dir: &Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "nuget",
            Arc::new(
                SourcePool::new(
                    "nuget",
                    SourceGraph::new(vec![proxy("official", upstream.as_str())])
                        .expect("source graph"),
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

async fn start_service_index_server() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let body = format!(
        r#"{{
            "version": "3.0.0",
            "resources": [
                {{
                    "@id": "http://nuget.example:{}/v3/registration5-semver1/",
                    "@type": "RegistrationsBaseUrl/3.6.0"
                }},
                {{
                    "@id": "http://nuget.example:{}/v3-flatcontainer/",
                    "@type": "PackageBaseAddress/3.0.0.0"
                }},
                {{
                    "@id": "http://nuget.example:{}/query",
                    "@type": "SearchQueryService/3.0.0"
                }},
                {{
                    "@id": "http://nuget.example:{}/v3/vulnerabilities/index.json",
                    "@type": "VulnerabilityInfo/6.7.0"
                }},
                {{
                    "@id": "http://evil.example/unknown",
                    "@type": "UnknownResource/1.0.0"
                }}
            ]
        }}"#,
        address.port(),
        address.port(),
        address.port(),
        address.port()
    );
    let router = Router::new().route(
        "/v3/index.json",
        get(move || {
            let body = body.clone();
            async move { body }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    Url::parse(&format!(
        "http://nuget.example:{address_port}/v3/index.json",
        address_port = address.port()
    ))
    .expect("service index URL")
}

async fn start_server(router: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    Url::parse(&format!("http://{address}/v3/index.json")).expect("service index URL")
}

async fn start_service_index_with_body<F>(build_body: F) -> Url
where
    F: FnOnce(u16) -> String,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let body = build_body(address.port());
    let router = Router::new().route(
        "/v3/index.json",
        get(move || {
            let body = body.clone();
            async move { body }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    Url::parse(&format!("http://{address}/v3/index.json")).expect("service index URL")
}

fn upstream_socket(url: &Url) -> SocketAddr {
    format!("127.0.0.1:{}", url.port().expect("upstream port"))
        .parse()
        .expect("upstream socket")
}
