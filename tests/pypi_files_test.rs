mod pypi_support;

use std::{
    fs,
    path::{Path, PathBuf},
};

use axum::http::{StatusCode, header::CONTENT_LENGTH};
use pypi_support::{
    TestResponse, anchor, body_bytes, file_url_with_hash, get, has_part_file, project_html,
    pypi_router, read_binding, send, start_upstream,
};
use tempfile::tempdir;

const FILENAME: &str = "friendly_bard-1.0.0-py3-none-any.whl";

#[tokio::test]
async fn file_download_is_hash_verified_and_cached() {
    let contents = b"verified wheel bytes";
    let upstream = upstream_with_file(contents, contents).await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let index = send(
        router.clone(),
        get("/mirror/pypi/official/simple/friendly-bard/"),
    )
    .await;
    assert_eq!(index.status(), StatusCode::OK);

    let response = send(
        router,
        get(&format!(
            "/mirror/pypi/official/files/friendly-bard/{FILENAME}"
        )),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()),
        Some(contents.len().to_string().as_str())
    );
    assert_eq!(body_bytes(response).await, contents);
    assert_eq!(upstream.request_count(&format!("/packages/{FILENAME}")), 1);
    assert!(artifact_path(data_dir.path(), "official", FILENAME).is_file());
    assert!(!has_part_file(data_dir.path()));
    assert_eq!(
        read_binding(data_dir.path(), "official", "friendly-bard", FILENAME)["selected_proxy"],
        "official"
    );
}

#[tokio::test]
async fn hash_mismatch_returns_502_and_leaves_no_artifact_or_temp_file() {
    let declared = b"expected wheel bytes";
    let tampered = b"tampered wheel bytes";
    let upstream = upstream_with_file(declared, tampered).await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let _ = send(
        router.clone(),
        get("/mirror/pypi/official/simple/friendly-bard/"),
    )
    .await;
    let response = send(
        router,
        get(&format!(
            "/mirror/pypi/official/files/friendly-bard/{FILENAME}"
        )),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(!artifact_path(data_dir.path(), "official", FILENAME).exists());
    assert!(!has_part_file(data_dir.path()));
    let audit = fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(
        audit.contains(r#""event":"pypi_artifact_fetch_failed""#),
        "{audit}"
    );
    assert!(
        audit.contains(r#""failure_category":"integrity_mismatch""#),
        "{audit}"
    );
}

#[tokio::test]
async fn unindexed_file_is_rejected_without_contacting_upstream() {
    let contents = b"verified wheel bytes";
    let upstream = upstream_with_file(contents, contents).await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let _ = send(
        router.clone(),
        get("/mirror/pypi/official/simple/friendly-bard/"),
    )
    .await;
    let response = send(
        router,
        get("/mirror/pypi/official/files/friendly-bard/other-1.0.whl"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(upstream.request_count("/packages/other-1.0.whl"), 0);
    assert!(!artifact_path(data_dir.path(), "official", "other-1.0.whl").exists());
}

#[tokio::test]
async fn absolute_and_encoded_file_paths_are_rejected() {
    let upstream = start_upstream(|_| Vec::new()).await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    for path in [
        "/mirror/pypi/official/files/friendly-bard/https:%2F%2Fevil.example%2Fx.whl",
        "/mirror/pypi/official/files/friendly-bard/%2e%2e%2fsecret",
        "/mirror/pypi/official/files/friendly-bard/..%5csecret",
        "/mirror/pypi/official/files/https://evil.example/x.whl",
    ] {
        let response = send(router.clone(), get(path)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    assert!(upstream.requests().is_empty());
}

#[tokio::test]
async fn upstream_links_to_private_hosts_are_rejected() {
    let upstream = start_upstream(|_| {
        let href = format!("http://127.0.0.2:9/packages/{FILENAME}");
        vec![(
            "/simple/friendly-bard/",
            TestResponse::html(project_html("friendly-bard", &[anchor(FILENAME, &href)])),
        )]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let response = send(router, get("/mirror/pypi/official/simple/friendly-bard/")).await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(upstream.request_count(&format!("/packages/{FILENAME}")), 0);
}

#[tokio::test]
async fn artifact_cache_hit_avoids_a_second_upstream_download() {
    let contents = b"verified wheel bytes";
    let upstream = upstream_with_file(contents, contents).await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let _ = send(
        router.clone(),
        get("/mirror/pypi/official/simple/friendly-bard/"),
    )
    .await;
    let path = format!("/mirror/pypi/official/files/friendly-bard/{FILENAME}");
    let first = send(router.clone(), get(&path)).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(body_bytes(first).await, contents);

    let second = send(router, get(&path)).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(body_bytes(second).await, contents);
    assert_eq!(upstream.request_count(&format!("/packages/{FILENAME}")), 1);
}

#[tokio::test]
async fn artifact_without_upstream_digest_records_audit_state() {
    let contents = b"unverified wheel bytes";
    let upstream = start_upstream(|base| {
        let href = base
            .join(&format!("packages/{FILENAME}"))
            .expect("file URL");
        vec![
            (
                "/simple/friendly-bard/",
                TestResponse::html(project_html(
                    "friendly-bard",
                    &[anchor(FILENAME, href.as_str())],
                )),
            ),
            (
                Box::leak(format!("/packages/{FILENAME}").into_boxed_str()) as &'static str,
                TestResponse::bytes("application/octet-stream", contents.to_vec()),
            ),
        ]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    assert_eq!(
        send(
            router.clone(),
            get("/mirror/pypi/official/simple/friendly-bard/")
        )
        .await
        .status(),
        StatusCode::OK
    );
    let response = send(
        router,
        get(&format!(
            "/mirror/pypi/official/files/friendly-bard/{FILENAME}"
        )),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await, contents);
    let audit = fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(
        audit.contains("pypi_artifact_published_without_digest"),
        "{audit}"
    );
}

async fn upstream_with_file(declared: &[u8], served: &[u8]) -> pypi_support::Upstream {
    let declared = declared.to_vec();
    let served = served.to_vec();
    start_upstream(move |base| {
        let href = file_url_with_hash(base, FILENAME, &declared);
        let index = project_html("friendly-bard", &[anchor(FILENAME, &href)]);
        vec![
            ("/simple/friendly-bard/", TestResponse::html(index)),
            ("/packages/", TestResponse::status(StatusCode::NOT_FOUND)),
        ]
        .into_iter()
        .chain(std::iter::once((
            Box::leak(format!("/packages/{FILENAME}").into_boxed_str()) as &'static str,
            TestResponse::bytes("application/octet-stream", served.clone()),
        )))
        .collect()
    })
    .await
}

fn artifact_path(data_dir: &Path, proxy: &str, filename: &str) -> PathBuf {
    data_dir
        .join("stores/pypi/artifacts")
        .join(proxy)
        .join("objects")
        .join("official")
        .join("friendly-bard")
        .join(filename)
}
