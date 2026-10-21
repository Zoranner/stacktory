use std::{
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
async fn service_index_rebases_cached_public_urls_without_refetching() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let body = format!(
        r#"{{"version":"3.0.0","resources":[
            {{"@id":"http://nuget.example:{}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
            {{"@id":"http://nuget.example:{}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0.0"}}
        ]}}"#,
        address.port(),
        address.port()
    );
    let router = Router::new().route(
        "/v3/index.json",
        get(move || {
            let body = body.clone();
            let hits = hits.clone();
            async move {
                if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                    body.into_response()
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("service index server");
    });
    let upstream = Url::parse(&format!("http://{address}/v3/index.json")).expect("upstream URL");
    let data_dir = tempdir().expect("data directory");

    let first = nuget_router_with_root(
        upstream.clone(),
        data_dir.path(),
        Url::parse("http://127.0.0.1:18761").expect("first root URL"),
    )
    .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
    .await
    .expect("first service index response");
    assert_eq!(first.status(), axum::http::StatusCode::OK);
    let first: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("first service index body"),
    )
    .expect("first service index JSON");
    let first_registration = first["resources"]
        .as_array()
        .expect("first resources")
        .iter()
        .find(|resource| {
            resource["@type"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("RegistrationsBaseUrl"))
        })
        .expect("first registration resource");
    assert_eq!(
        first_registration["@id"],
        "http://127.0.0.1:18761/mirror/nuget/official/registration/"
    );

    let second = nuget_router_with_root(
        upstream,
        data_dir.path(),
        Url::parse("https://pkg2.example").expect("second root URL"),
    )
    .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
    .await
    .expect("second service index response");
    assert_eq!(second.status(), axum::http::StatusCode::OK);
    let second: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("second service index body"),
    )
    .expect("second service index JSON");
    let second_registration = second["resources"]
        .as_array()
        .expect("second resources")
        .iter()
        .find(|resource| {
            resource["@type"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("RegistrationsBaseUrl"))
        })
        .expect("second registration resource");
    assert_eq!(
        second_registration["@id"],
        "https://pkg2.example/mirror/nuget/official/registration/"
    );
}

#[tokio::test]
async fn service_index_refetches_when_the_source_target_changes() {
    let first = start_service_index_with_body(|port| {
        format!(
            r#"{{"version":"3.0.0","resources":[
                {{"@id":"http://127.0.0.1:{port}/first/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                {{"@id":"http://127.0.0.1:{port}/first/packages/","@type":"PackageBaseAddress/3.0.0"}}
            ]}}"#
        )
    })
    .await;
    let second = start_service_index_with_body(|port| {
        format!(
            r#"{{"version":"3.0.0","resources":[
                {{"@id":"http://127.0.0.1:{port}/second/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                {{"@id":"http://127.0.0.1:{port}/second/packages/","@type":"PackageBaseAddress/3.0.0"}}
            ]}}"#
        )
    })
    .await;
    let data_dir = tempdir().expect("data directory");

    let first_response = nuget_router(first, data_dir.path())
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("first service index response");
    assert_eq!(first_response.status(), axum::http::StatusCode::OK);

    let second_response = nuget_router(second.clone(), data_dir.path())
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("second service index response");
    assert_eq!(second_response.status(), axum::http::StatusCode::OK);

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
        descriptor["source_fingerprint"],
        format!("official|{}|false", second.as_str())
    );
    assert_eq!(
        descriptor["registration_url"],
        format!(
            "http://127.0.0.1:{}/second/registration/",
            second.port().expect("second port")
        )
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
        br#"{"selected_proxy":"official","source_fingerprint":"official|http://127.0.0.1:9/v3/index.json|false","public_base_url":"http://127.0.0.1:18761","registration_url":"http://127.0.0.1:9/v3/registration/","package_base_url":"http://127.0.0.1:9/v3-flatcontainer/"}"#,
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
    nuget_router_with_root(
        upstream,
        data_dir,
        Url::parse("http://127.0.0.1:18761").expect("root URL"),
    )
}

fn nuget_router_with_root(upstream: Url, data_dir: &Path, root_url: Url) -> axum::Router {
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
            Arc::new(NugetAdapter::new(root_url, data_dir)),
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
