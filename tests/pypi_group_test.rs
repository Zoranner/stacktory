mod pypi_support;

use std::fs;

use axum::http::StatusCode;
use pypi_support::{
    TestResponse, Upstream, anchor, body_bytes, file_url_with_hash, get, project_html, pypi_router,
    read_binding, read_current, send, start_upstream,
};
use tempfile::tempdir;

const FILENAME: &str = "friendly_bard-1.0.0-py3-none-any.whl";
const GOOD: &[u8] = b"wheel from the second source";
const BAD: &[u8] = b"tampered wheel from the first source";

#[tokio::test]
async fn project_index_falls_back_and_records_the_selected_proxy() {
    let first = start_upstream(|_| {
        vec![(
            "/simple/friendly-bard/",
            TestResponse::status(StatusCode::INTERNAL_SERVER_ERROR),
        )]
    })
    .await;
    let second = index_source(b"second wheel").await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(
        &[("first", &first.base_url), ("second", &second.base_url)],
        Some(("group", &["first", "second"])),
        data_dir.path(),
    );

    let response = send(router, get("/mirror/pypi/group/simple/friendly-bard/")).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(first.request_count("/simple/friendly-bard/"), 1);
    assert_eq!(second.request_count("/simple/friendly-bard/"), 1);
    let current = read_current(data_dir.path(), "group", "friendly-bard");
    assert!(
        current.contains(r#"selected_proxy = "second""#),
        "{current}"
    );
    let audit = fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(
        audit.contains(r#""event":"pypi_metadata_source_failed""#),
        "{audit}"
    );
    assert!(audit.contains(r#""selected_proxy":"second""#), "{audit}");
}

#[tokio::test]
async fn root_index_falls_back_across_group_members() {
    let first = start_upstream(|_| {
        vec![(
            "/simple/",
            TestResponse::status(StatusCode::INTERNAL_SERVER_ERROR),
        )]
    })
    .await;
    let second = start_upstream(|_| {
        vec![(
            "/simple/",
            TestResponse::html(
                "<!DOCTYPE html><html><body><a href=\"/simple/friendly-bard/\">friendly-bard</a></body></html>",
            ),
        )]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(
        &[("first", &first.base_url), ("second", &second.base_url)],
        Some(("group", &["first", "second"])),
        data_dir.path(),
    );

    let response = send(router, get("/mirror/pypi/group/simple/")).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(body_bytes(response).await).expect("UTF-8 root index");
    assert!(
        body.contains(r#"href="/mirror/pypi/group/simple/friendly-bard/""#),
        "{body}"
    );
    assert_eq!(first.request_count("/simple/"), 1);
    assert_eq!(second.request_count("/simple/"), 1);
}

#[tokio::test]
async fn artifact_hash_failure_falls_back_and_updates_the_binding() {
    let first = start_upstream(move |base| {
        let href = file_url_with_hash(base, FILENAME, GOOD);
        vec![
            (
                "/simple/friendly-bard/",
                TestResponse::html(project_html("friendly-bard", &[anchor(FILENAME, &href)])),
            ),
            (
                Box::leak(format!("/packages/{FILENAME}").into_boxed_str()) as &'static str,
                TestResponse::bytes("application/octet-stream", BAD.to_vec()),
            ),
        ]
    })
    .await;
    let second = start_upstream(move |base| {
        let href = file_url_with_hash(base, FILENAME, GOOD);
        vec![
            (
                "/simple/friendly-bard/",
                TestResponse::html(project_html("friendly-bard", &[anchor(FILENAME, &href)])),
            ),
            (
                Box::leak(format!("/packages/{FILENAME}").into_boxed_str()) as &'static str,
                TestResponse::bytes("application/octet-stream", GOOD.to_vec()),
            ),
        ]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(
        &[("first", &first.base_url), ("second", &second.base_url)],
        Some(("group", &["first", "second"])),
        data_dir.path(),
    );

    let index = send(
        router.clone(),
        get("/mirror/pypi/group/simple/friendly-bard/"),
    )
    .await;
    assert_eq!(index.status(), StatusCode::OK);
    assert!(
        read_current(data_dir.path(), "group", "friendly-bard")
            .contains(r#"selected_proxy = "first""#)
    );

    let file = send(
        router,
        get(&format!(
            "/mirror/pypi/group/files/friendly-bard/{FILENAME}"
        )),
    )
    .await;
    assert_eq!(file.status(), StatusCode::OK);
    assert_eq!(body_bytes(file).await, GOOD);
    assert_eq!(first.request_count(&format!("/packages/{FILENAME}")), 1);
    assert_eq!(second.request_count("/simple/friendly-bard/"), 1);
    assert_eq!(second.request_count(&format!("/packages/{FILENAME}")), 1);
    let binding = read_binding(data_dir.path(), "group", "friendly-bard", FILENAME);
    assert_eq!(binding["selected_proxy"], "second");
    let audit = fs::read_to_string(data_dir.path().join("audit/events.jsonl")).expect("audit log");
    assert!(
        audit.contains(r#""event":"pypi_artifact_fallback""#),
        "{audit}"
    );
    assert!(
        audit.contains(r#""failure_category":"integrity_mismatch""#),
        "{audit}"
    );
}

#[tokio::test]
async fn all_sources_failing_returns_502() {
    let first = failing_source().await;
    let second = failing_source().await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(
        &[("first", &first.base_url), ("second", &second.base_url)],
        Some(("group", &["first", "second"])),
        data_dir.path(),
    );

    let response = send(router, get("/mirror/pypi/group/simple/friendly-bard/")).await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(first.request_count("/simple/friendly-bard/"), 1);
    assert_eq!(second.request_count("/simple/friendly-bard/"), 1);
}

#[tokio::test]
async fn unknown_project_in_a_group_returns_404() {
    let first = start_upstream(|_| {
        vec![(
            "/simple/friendly-bard/",
            TestResponse::status(StatusCode::NOT_FOUND),
        )]
    })
    .await;
    let second = start_upstream(|_| {
        vec![(
            "/simple/friendly-bard/",
            TestResponse::status(StatusCode::NOT_FOUND),
        )]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(
        &[("first", &first.base_url), ("second", &second.base_url)],
        Some(("group", &["first", "second"])),
        data_dir.path(),
    );

    let response = send(router, get("/mirror/pypi/group/simple/friendly-bard/")).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn index_source(contents: &[u8]) -> Upstream {
    let contents = contents.to_vec();
    start_upstream(move |base| {
        let href = file_url_with_hash(base, FILENAME, &contents);
        vec![(
            "/simple/friendly-bard/",
            TestResponse::html(project_html("friendly-bard", &[anchor(FILENAME, &href)])),
        )]
    })
    .await
}

async fn failing_source() -> Upstream {
    start_upstream(|_| {
        vec![(
            "/simple/friendly-bard/",
            TestResponse::status(StatusCode::INTERNAL_SERVER_ERROR),
        )]
    })
    .await
}
