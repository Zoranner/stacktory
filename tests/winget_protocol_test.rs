use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::Value;
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, ecosystems::winget::WingetAdapter,
    http::router_with_registry, upstream::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn information_is_pinned_to_contract_1_0_and_keeps_source_metadata() {
    let (port, address) = start_source_server("1.0.0", false).await;
    let router = winget_router(port, address, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request("/mirror/winget/official/information"))
        .await
        .expect("information response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("information body"),
    )
    .expect("information JSON");

    assert_eq!(
        body["Data"]["SourceIdentifier"],
        "stacktory-winget:official"
    );
    assert_eq!(
        body["Data"]["ServerSupportedVersions"],
        serde_json::json!(["1.0.0"])
    );
    assert_eq!(
        body["Data"]["SourceAgreements"]["AgreementsIdentifier"],
        "agreement-v1"
    );
    assert_eq!(
        body["Data"]["SourceAgreements"]["Agreements"][0]["AgreementLabel"],
        "EULA"
    );
    assert_eq!(
        body["Data"]["RequiredPackageMatchFields"],
        serde_json::json!(["Market"])
    );
    assert_eq!(body["Data"]["UpstreamOnlyField"], "kept");
}

#[tokio::test]
async fn manifest_search_forwards_the_winget_request_and_keeps_the_envelope() {
    let (port, address) = start_source_server("1.0.0", false).await;
    let router = winget_router(port, address, tempdir().expect("data directory").path());
    let request_body = r#"{"Query":{"KeyWord":"tool","MatchType":"Substring"},"MaximumResults":5}"#;

    let response = router
        .oneshot(
            Request::post("/mirror/winget/official/manifestSearch")
                .header("content-type", "application/json")
                .body(Body::from(request_body))
                .expect("search request"),
        )
        .await
        .expect("search response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("search body"),
    )
    .expect("search JSON");
    assert_eq!(body["Data"][0]["PackageIdentifier"], "Test.Package");
    assert_eq!(body["Data"][0]["UpstreamOnlyField"], "kept");
}

#[tokio::test]
async fn malformed_manifest_search_body_is_a_client_error() {
    let (port, address) = start_source_server("1.0.0", false).await;
    let router = winget_router(port, address, tempdir().expect("data directory").path());

    let response = router
        .oneshot(
            Request::post("/mirror/winget/official/manifestSearch")
                .header("content-type", "application/json")
                .body(Body::from("not-json"))
                .expect("search request"),
        )
        .await
        .expect("search response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn upstream_contract_without_1_0_is_rejected() {
    let (port, address) = start_source_server("2.0.0", false).await;
    let router = winget_router(port, address, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request("/mirror/winget/official/information"))
        .await
        .expect("information response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn upstream_authentication_is_not_faked() {
    let (port, address) = start_source_server("1.0.0", true).await;
    let router = winget_router(port, address, tempdir().expect("data directory").path());

    let response = router
        .oneshot(get_request("/mirror/winget/official/information"))
        .await
        .expect("information response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn invalid_paths_unknown_entries_and_unregistered_ecosystems_return_404() {
    let (port, address) = start_source_server("1.0.0", false).await;
    let router = winget_router(port, address, tempdir().expect("data directory").path());

    for path in [
        "/mirror/winget/official/packageManifests/bad%2Fid",
        "/mirror/winget/official/packageManifests/..",
        "/mirror/winget/official/installers/not-a-token",
        "/mirror/winget/official/installers/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bad-ext!",
        "/mirror/winget/missing/information",
        "/mirror/pypi/official/simple/",
    ] {
        let response = router
            .clone()
            .oneshot(get_request(path))
            .await
            .expect("path response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

fn winget_router(port: u16, address: SocketAddr, data_dir: &std::path::Path) -> Router {
    let source = format!("http://winget.example:{port}/api");
    let graph = SourceGraph::new(vec![SourceConfig {
        name: "official".to_owned(),
        source_type: SourceType::Proxy,
        base_url: Some(Url::parse(&source).expect("source URL")),
        require_https: false,
        members: Vec::new(),
    }])
    .expect("source graph");
    let pool = SourcePool::new_with_resolver("winget", graph, "winget.example", address)
        .expect("source pool");
    let hosts = BTreeMap::from([(
        "official".to_owned(),
        BTreeSet::from([format!("127.0.0.1:{port}")]),
    )]);
    let registry = AdapterRegistry::new()
        .with_adapter(
            "winget",
            std::sync::Arc::new(pool),
            std::sync::Arc::new(WingetAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("public URL"),
                data_dir,
                hosts,
            )),
        )
        .expect("adapter registry");
    router_with_registry(registry)
}

async fn start_source_server(contract: &str, authentication: bool) -> (u16, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("source listener");
    let address = listener.local_addr().expect("source address");
    let port = address.port();
    let contract = contract.to_owned();
    let information = if authentication {
        serde_json::json!({
            "Data": {
                "SourceIdentifier": "upstream",
                "ServerSupportedVersions": [contract],
                "Authentication": {
                    "AuthenticationType": "microsoftEntraId",
                    "MicrosoftEntraIdAuthenticationInfo": {
                        "Resource": "resource",
                        "Scope": "scope"
                    }
                }
            }
        })
    } else {
        serde_json::json!({
            "Data": {
                "SourceIdentifier": "upstream",
                "ServerSupportedVersions": [contract, "1.1.0"],
                "SourceAgreements": {
                    "AgreementsIdentifier": "agreement-v1",
                    "Agreements": [{
                        "AgreementLabel": "EULA",
                        "Agreement": "terms",
                        "AgreementUrl": "https://agreement.example/eula"
                    }]
                },
                "RequiredPackageMatchFields": ["Market"],
                "UpstreamOnlyField": "kept"
            }
        })
    };
    let information = information.to_string();
    let router = Router::new()
        .route(
            "/api/information",
            get({
                let information = information.clone();
                move || {
                    let information = information.clone();
                    async move { (StatusCode::OK, information) }
                }
            }),
        )
        .route(
            "/api/manifestSearch",
            post(|| async {
                (
                    StatusCode::OK,
                    r#"{"Data":[{"PackageIdentifier":"Test.Package","PackageName":"Test","Publisher":"Test","Versions":[{"PackageVersion":"1.0.0"}],"UpstreamOnlyField":"kept"}]}"#,
                )
                    .into_response()
            }),
        )
        .route(
            "/api/packageManifests/Test.Package",
            get(move || async move {
                let installer_url = format!("http://127.0.0.1:{port}/download/Test.Package.bin");
                format!(
                    r#"{{"Data":{{"PackageIdentifier":"Test.Package","Versions":[{{"PackageVersion":"1.0.0","DefaultLocale":{{"PackageLocale":"en-US","Publisher":"Test","PackageName":"Test","ShortDescription":"test"}},"Installers":[{{"InstallerUrl":"{installer_url}","InstallerSha256":"{}","InstallerType":"exe","Architecture":"x64","Scope":"user","InstallerLocale":"en-US"}}],"UnknownVersionField":{{"kept":true}}}}],"UnknownDataField":"kept"}}}}"#,
                    "0".repeat(64)
                )
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("source server");
    });
    (port, address)
}

fn get_request(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET request")
}
