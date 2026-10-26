use std::{net::SocketAddr, path::Path, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::OriginalUri,
    http::{
        Request, StatusCode,
        header::{CONTENT_TYPE, LOCATION},
    },
    response::IntoResponse,
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha512};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, ecosystems::upm::UpmAdapter,
    http::router_with_registry, upstream::registry::AdapterRegistry,
};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn upm_serves_metadata_and_tarball_through_its_own_route() {
    let artifact = b"verified UPM package".to_vec();
    let (upstream, _) = start_upm_server(artifact.clone()).await;
    let data_dir = tempdir().expect("data directory");
    let router = upm_router(upstream, data_dir.path());

    let metadata = router
        .clone()
        .oneshot(get_request("/mirror/upm/group/com.company.package"))
        .await
        .expect("UPM metadata response");

    assert_eq!(metadata.status(), StatusCode::OK);
    let metadata: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(metadata.into_body(), usize::MAX)
            .await
            .expect("UPM metadata body"),
    )
    .expect("UPM metadata JSON");
    assert_eq!(metadata["name"], "com.company.package");
    let tarball = metadata["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .expect("UPM tarball URL");
    assert!(tarball.starts_with("http://127.0.0.1:18761/mirror/upm/group/tarballs/"));

    let tarball_path = Url::parse(tarball)
        .expect("UPM tarball URL")
        .path()
        .to_owned();
    let artifact_response = router
        .oneshot(get_request(&tarball_path))
        .await
        .expect("UPM tarball response");

    assert_eq!(artifact_response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(artifact_response.into_body(), usize::MAX)
            .await
            .expect("UPM tarball body"),
        artifact
    );
    assert!(
        data_dir
            .path()
            .join("stores/upm/artifacts/official/objects")
            .exists()
    );
    assert!(!data_dir.path().join("npm").exists());
}

#[tokio::test]
async fn upm_supports_search_version_dist_tags_and_ping_reads() {
    let (upstream, _) = start_upm_read_server().await;
    let router = upm_router(upstream, tempdir().expect("data directory").path());

    let search = router
        .clone()
        .oneshot(
            Request::get("/mirror/upm/group/-/v1/search?text=company&size=3")
                .body(Body::empty())
                .expect("UPM search request"),
        )
        .await
        .expect("UPM search response");
    assert_eq!(search.status(), StatusCode::OK);

    let version = router
        .clone()
        .oneshot(get_request("/mirror/upm/group/com.company.package/1.0.0"))
        .await
        .expect("UPM version response");
    let version: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(version.into_body(), usize::MAX)
            .await
            .expect("UPM version body"),
    )
    .expect("UPM version JSON");
    assert_eq!(version["version"], "1.0.0");
    assert!(
        version["dist"]["tarball"]
            .as_str()
            .expect("UPM internal tarball")
            .contains("/mirror/upm/group/tarballs/")
    );

    let tags = router
        .clone()
        .oneshot(get_request(
            "/mirror/upm/group/-/package/com.company.package/dist-tags",
        ))
        .await
        .expect("UPM dist-tags response");
    let tags: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(tags.into_body(), usize::MAX)
            .await
            .expect("UPM dist-tags body"),
    )
    .expect("UPM tags JSON");
    assert_eq!(tags["latest"], "1.0.0");

    let ping = router
        .oneshot(get_request("/mirror/upm/group/-/ping"))
        .await
        .expect("UPM ping response");
    assert_eq!(ping.status(), StatusCode::OK);
}

#[tokio::test]
async fn upm_follows_a_safe_tarball_redirect() {
    let artifact = b"redirected UPM package".to_vec();
    let (upstream, _) = start_redirecting_upm_server(artifact.clone()).await;
    let data_dir = tempdir().expect("data directory");
    let router = upm_router(upstream, data_dir.path());

    let metadata = router
        .clone()
        .oneshot(get_request("/mirror/upm/group/com.company.package"))
        .await
        .expect("UPM metadata response");
    assert_eq!(metadata.status(), StatusCode::OK);
    let metadata: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(metadata.into_body(), usize::MAX)
            .await
            .expect("UPM metadata body"),
    )
    .expect("UPM metadata JSON");
    let tarball_path = Url::parse(
        metadata["versions"]["1.0.0"]["dist"]["tarball"]
            .as_str()
            .expect("UPM tarball URL"),
    )
    .expect("UPM tarball URL")
    .path()
    .to_owned();

    let artifact_response = router
        .oneshot(get_request(&tarball_path))
        .await
        .expect("UPM tarball response");
    assert_eq!(artifact_response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(artifact_response.into_body(), usize::MAX)
            .await
            .expect("UPM tarball body"),
        artifact
    );
}

fn upm_router(upstream: Url, data_dir: &Path) -> axum::Router {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "upm",
            Arc::new(
                SourcePool::new(
                    "upm",
                    SourceGraph::new(vec![
                        proxy("official", upstream.as_str()),
                        group("group", &["official"]),
                    ])
                    .expect("UPM source graph"),
                )
                .expect("UPM source pool"),
            ),
            Arc::new(UpmAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("UPM adapter registration");
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

async fn start_upm_server(artifact: Vec<u8>) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let tarball_url = format!("http://{address}/com.company.package/-/package-1.0.0.tgz");
    let integrity = format!(
        "sha512-{}",
        STANDARD.encode(Sha512::digest(b"verified UPM package"))
    );
    let router = Router::new().fallback(move |OriginalUri(uri): OriginalUri| {
        let tarball_url = tarball_url.clone();
        let integrity = integrity.clone();
        let artifact = artifact.clone();
        async move {
            if uri.path().ends_with(".tgz") {
                return ([(CONTENT_TYPE, "application/octet-stream")], artifact).into_response();
            }
            (
                [(CONTENT_TYPE, "application/json")],
                format!(
                    r#"{{
                            "name": "com.company.package",
                            "dist-tags": {{"latest":"1.0.0"}},
                            "versions": {{
                                "1.0.0": {{
                                    "name": "com.company.package",
                                    "version": "1.0.0",
                                    "dist": {{
                                        "tarball": "{tarball_url}",
                                        "integrity": "{integrity}"
                                    }}
                                }}
                            }}
                        }}"#
                ),
            )
                .into_response()
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("UPM server");
    });
    (
        Url::parse(&format!("http://{address}")).expect("UPM upstream URL"),
        address,
    )
}

async fn start_upm_read_server() -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let tarball_url = format!("http://{address}/com.company.package/-/package-1.0.0.tgz");
    let router = Router::new()
        .route(
            "/-/v1/search",
            get(|OriginalUri(uri): OriginalUri| async move {
                assert_eq!(uri.query(), Some("text=company&size=3"));
                (
                    [(CONTENT_TYPE, "application/json")],
                    r#"{"objects":[{"package":{"name":"com.company.package","version":"1.0.0"}}],"total":1}"#,
                )
            }),
        )
        .route(
            "/com.company.package",
            get(move || {
                let tarball_url = tarball_url.clone();
                async move {
                    (
                        [(CONTENT_TYPE, "application/json")],
                        format!(
                            r#"{{"name":"com.company.package","dist-tags":{{"latest":"1.0.0"}},"versions":{{"1.0.0":{{"name":"com.company.package","version":"1.0.0","dist":{{"tarball":"{tarball_url}","integrity":"sha512-YWJj"}}}}}}}}"#
                        ),
                    )
                }
            }),
        )
        .route(
            "/-/ping",
            get(|| async {
                (
                    [(CONTENT_TYPE, "application/json")],
                    r#"{"ping":"pong"}"#,
                )
            }),
        )
        .fallback(|| async { StatusCode::NOT_FOUND.into_response() });
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("UPM read server");
    });
    (
        Url::parse(&format!("http://{address}")).expect("UPM read upstream URL"),
        address,
    )
}

async fn start_redirecting_upm_server(artifact: Vec<u8>) -> (Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let base_url = format!("http://{address}");
    let tarball_url = format!("{base_url}/redirect.tgz");
    let integrity = format!(
        "sha512-{}",
        STANDARD.encode(Sha512::digest(b"redirected UPM package"))
    );
    let upstream_url = base_url.clone();
    let router = Router::new().fallback(move |OriginalUri(uri): OriginalUri| {
        let base_url = base_url.clone();
        let tarball_url = tarball_url.clone();
        let integrity = integrity.clone();
        let artifact = artifact.clone();
        async move {
            if uri.path() == "/redirect.tgz" {
                return (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(LOCATION, format!("{base_url}/artifact.tgz"))],
                )
                    .into_response();
            }
            if uri.path() == "/artifact.tgz" {
                return ([(CONTENT_TYPE, "application/octet-stream")], artifact).into_response();
            }
            (
                [(CONTENT_TYPE, "application/json")],
                format!(
                    r#"{{
                            "name": "com.company.package",
                            "dist-tags": {{"latest":"1.0.0"}},
                            "versions": {{
                                "1.0.0": {{
                                    "name": "com.company.package",
                                    "version": "1.0.0",
                                    "dist": {{
                                        "tarball": "{tarball_url}",
                                        "integrity": "{integrity}"
                                    }}
                                }}
                            }}
                        }}"#
                ),
            )
                .into_response()
        }
    });
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("redirecting UPM server");
    });
    (
        Url::parse(&upstream_url).expect("UPM upstream URL"),
        address,
    )
}
