use std::sync::Arc;

use axum::{body::Body, http::Request};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType,
    ecosystems::docker::{
        DockerAdapter,
        path::{DockerRequest, ManifestReference, parse_repository, parse_request},
    },
    http::router_with_registry,
    upstream::registry::AdapterRegistry,
};
use tower::ServiceExt;
use url::Url;

#[test]
fn parses_registry_v2_probe() {
    assert_eq!(
        parse_request("v2/").expect("v2 probe"),
        DockerRequest::Probe
    );
}

#[test]
fn parses_manifest_tag_and_digest_requests() {
    assert_eq!(
        parse_request("v2/library/alpine/manifests/latest").expect("tag manifest"),
        DockerRequest::Manifest {
            repository: "library/alpine".to_owned(),
            reference: ManifestReference::Tag("latest".to_owned()),
        }
    );

    let digest = format!("sha256:{}", "a".repeat(64));
    assert_eq!(
        parse_request(&format!("v2/library/alpine/manifests/{digest}")).expect("digest manifest"),
        DockerRequest::Manifest {
            repository: "library/alpine".to_owned(),
            reference: ManifestReference::Digest(digest),
        }
    );
}

#[test]
fn parses_blob_digest_requests() {
    let digest = format!("sha256:{}", "b".repeat(64));

    assert_eq!(
        parse_request(&format!("v2/library/alpine/blobs/{digest}")).expect("blob"),
        DockerRequest::Blob {
            repository: "library/alpine".to_owned(),
            digest,
        }
    );
}

#[test]
fn parses_catalog_and_tag_list_requests() {
    assert_eq!(
        parse_request("v2/_catalog").expect("catalog"),
        DockerRequest::Catalog
    );
    assert_eq!(
        parse_request("v2/library/alpine/tags/list").expect("tag list"),
        DockerRequest::Tags {
            repository: "library/alpine".to_owned(),
        }
    );
}

#[test]
fn parses_stacktory_search_request_without_treating_it_as_registry_v1() {
    assert_eq!(
        parse_request("search").expect("Stacktory search"),
        DockerRequest::Search
    );
    assert!(parse_request("v1/search").is_err());
    assert!(parse_request("v2/search").is_err());
}

#[test]
fn validates_repository_segments_without_decoding_external_paths() {
    assert_eq!(
        parse_repository("team/service"),
        Ok("team/service".to_owned())
    );

    for value in [
        "",
        "/team/service",
        "team//service",
        "team/./service",
        "team/../service",
        "team\\service",
        "team%2Fservice",
        "team/service?",
        "team/service#fragment",
    ] {
        assert!(
            parse_repository(value).is_err(),
            "invalid repository: {value}"
        );
    }
}

#[test]
fn rejects_v1_and_internal_v2_namespace_paths() {
    for value in [
        "v1/repositories/library/alpine/tags/latest",
        "v2",
        "v2/library/alpine",
        "library/alpine/manifests/latest",
    ] {
        assert!(
            parse_request(value).is_err(),
            "invalid Docker path: {value}"
        );
    }
}

#[test]
fn rejects_invalid_references_and_digests() {
    for value in [
        "v2/library/alpine/manifests/",
        "v2/library/alpine/manifests/sha256:",
        "v2/library/alpine/manifests/sha256:short",
        "v2/library/alpine/manifests/md5:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "v2/library/alpine/blobs/sha256:short",
        "v2/library/alpine/blobs/latest",
    ] {
        assert!(
            parse_request(value).is_err(),
            "invalid Docker request: {value}"
        );
    }
}

#[tokio::test]
async fn serves_registry_v2_probe_without_exposing_an_upstream_url() {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "docker",
            Arc::new(
                SourcePool::new(
                    "docker",
                    SourceGraph::new(vec![SourceConfig {
                        name: "official".to_owned(),
                        source_type: SourceType::Proxy,
                        base_url: Some(
                            Url::parse("https://registry.example.invalid").expect("source URL"),
                        ),
                        require_https: true,
                        members: vec![],
                    }])
                    .expect("source graph"),
                )
                .expect("source pool"),
            ),
            Arc::new(DockerAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                tempfile::tempdir().expect("data directory").path(),
            )),
        )
        .expect("adapter registration");

    let response = router_with_registry(registry)
        .oneshot(
            Request::get("/mirror/docker/official/v2/")
                .body(Body::empty())
                .expect("probe request"),
        )
        .await
        .expect("probe response");

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("docker-distribution-api-version")
            .and_then(|value| value.to_str().ok()),
        Some("registry/2.0")
    );
    assert!(!response.headers().get("location").is_some_and(|value| {
        value
            .to_str()
            .unwrap_or_default()
            .contains("registry.example")
    }));
}
