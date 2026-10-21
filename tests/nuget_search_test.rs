use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::OriginalUri,
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
async fn search_resource_is_exposed_and_query_is_forwarded_safely() {
    let (upstream, address) = start_search_server().await;
    let data_dir = tempdir().expect("data directory");
    let router = search_router(upstream, address, data_dir.path());

    let index = router
        .clone()
        .oneshot(get_request("/mirror/nuget/official/v3/index.json"))
        .await
        .expect("service index response");
    assert_eq!(index.status(), StatusCode::OK);
    let index: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(index.into_body(), usize::MAX)
            .await
            .expect("service index body"),
    )
    .expect("service index JSON");
    assert_eq!(
        index["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .find(|resource| {
                resource["@type"]
                    .as_str()
                    .is_some_and(|resource_type| resource_type.starts_with("SearchQueryService"))
            })
            .expect("search resource")["@id"],
        "http://127.0.0.1:18761/mirror/nuget/official/search/query"
    );
    assert_eq!(
        index["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .find(|resource| {
                resource["@type"]
                    .as_str()
                    .is_some_and(|resource_type| resource_type.starts_with("SearchQueryService"))
            })
            .expect("search resource")["@type"],
        "SearchQueryService/Versioned"
    );

    let response = router
        .oneshot(get_request(
            "/mirror/nuget/official/search/query?q=Newtonsoft.Json&take=5",
        ))
        .await
        .expect("search response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("search body");
    let search: serde_json::Value = serde_json::from_slice(&body).expect("search JSON");
    assert_eq!(search["data"][0]["id"], "Newtonsoft.Json");
    assert_eq!(
        search["data"][0]["registration"],
        "http://127.0.0.1:18761/mirror/nuget/official/registration/newtonsoft.json/index.json"
    );
    assert_eq!(
        search["@context"]["@base"],
        "http://127.0.0.1:18761/mirror/nuget/official/registration/"
    );
    assert!(search["data"][0]["iconUrl"].is_null());
    assert!(!String::from_utf8_lossy(&body).contains("azuresearch"));
    assert!(!String::from_utf8_lossy(&body).contains("nuget.example"));
}

#[tokio::test]
async fn search_request_errors_and_upstream_response_errors_use_distinct_statuses() {
    let (upstream, address) = start_search_server().await;
    let data_dir = tempdir().expect("data directory");
    let router = search_router(upstream, address, data_dir.path());

    let invalid_query = "a".repeat(16 * 1024 + 1);
    let request_error = router
        .clone()
        .oneshot(get_request(&format!(
            "/mirror/nuget/official/search/query?q={invalid_query}"
        )))
        .await
        .expect("invalid search request response");
    assert_eq!(request_error.status(), StatusCode::BAD_REQUEST);

    let upstream_error = router
        .oneshot(get_request("/mirror/nuget/official/search/query?q=invalid"))
        .await
        .expect("invalid search response");
    assert_eq!(upstream_error.status(), StatusCode::BAD_GATEWAY);
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

fn search_router(upstream: Url, address: SocketAddr, data_dir: &std::path::Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "nuget",
            Arc::new(
                SourcePool::new_with_resolvers(
                    "nuget",
                    SourceGraph::new(vec![proxy("official", upstream.as_str())])
                        .expect("source graph"),
                    &[("nuget.example", address), ("azuresearch.example", address)],
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

async fn start_search_server() -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let service_index = format!(
        r#"{{
            "version":"3.0.0",
            "resources":[
                {{"@id":"http://nuget.example:{}/v3/registration/","@type":"RegistrationsBaseUrl/3.6.0"}},
                {{"@id":"http://nuget.example:{}/v3-flatcontainer/","@type":"PackageBaseAddress/3.0.0"}},
                {{"@id":"http://azuresearch.example:{}/legacy-query","@type":"SearchQueryService"}},
                {{"@id":"http://azuresearch.example:{}/query","@type":"SearchQueryService/3.5.0"}}
            ]
        }}"#,
        address.port(),
        address.port(),
        address.port(),
        address.port()
    );
    let registration =
        r#"{"@id":"http://nuget.example/v3/registration/newtonsoft.json/index.json","items":[]}"#;
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
            "/v3/registration/newtonsoft.json/index.json",
            get(move || async move { registration }),
        )
        .route(
            "/query",
            get(move |OriginalUri(uri): OriginalUri| async move {
                if uri.query() == Some("q=invalid") {
                    return (StatusCode::OK, "not-json").into_response();
                }
                if uri.query() != Some("q=Newtonsoft.Json&take=5") {
                    return (StatusCode::BAD_REQUEST, "unexpected query").into_response();
                }
                axum::Json(serde_json::json!({
                    "@context": {
                        "@base": format!(
                            "http://nuget.example:{}/v3/registration/",
                            address.port()
                        )
                    },
                    "totalHits": 1,
                    "data": [{
                        "@id": format!(
                            "http://nuget.example:{}/v3/registration/newtonsoft.json/index.json",
                            address.port()
                        ),
                        "id": "Newtonsoft.Json",
                        "version": "13.0.3",
                        "iconUrl": "http://nuget.example/icon",
                        "registration": format!(
                            "http://nuget.example:{}/v3/registration/newtonsoft.json/index.json",
                            address.port()
                        )
                    }]
                }))
                .into_response()
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
