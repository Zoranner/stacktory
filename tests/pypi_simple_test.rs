mod pypi_support;

use std::fs;

use axum::http::{StatusCode, header::CONTENT_TYPE};
use pypi_support::{
    TestResponse, anchor, anchor_with_attrs, body_json, body_text, collect_files,
    file_url_with_hash, get, get_accept, metadata_dir, project_html, pypi_router, root_html, send,
    sha256_hex, start_upstream,
};
use tempfile::tempdir;

#[tokio::test]
async fn normalized_project_aliases_share_one_metadata_object() {
    let contents = b"friendly wheel";
    let filename = "friendly_bard-1.0.0-py3-none-any.whl";
    let upstream = start_upstream(|base| {
        vec![(
            "/simple/friendly-bard/",
            TestResponse::html(project_html(
                "friendly-bard",
                &[anchor(
                    filename,
                    &file_url_with_hash(base, filename, contents),
                )],
            )),
        )]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    for alias in ["Friendly_Bard", "friendly-bard", "friendly.bard"] {
        let response = send(
            router.clone(),
            get(&format!("/mirror/pypi/official/simple/{alias}/")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{alias}");
        assert!(body_text(response).await.contains(filename), "{alias}");
    }

    assert_eq!(upstream.request_count("/simple/friendly-bard/"), 1);
    let projects = fs::read_dir(metadata_dir(data_dir.path()).join("official"))
        .expect("official metadata directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("metadata entries");
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].file_name().to_string_lossy(), "friendly-bard");
}

#[tokio::test]
async fn accept_negotiation_selects_html_json_or_406() {
    let contents = b"friendly wheel";
    let filename = "friendly_bard-1.0.0-py3-none-any.whl";
    let upstream = start_upstream(|base| {
        vec![(
            "/simple/friendly-bard/",
            TestResponse::html(project_html(
                "friendly-bard",
                &[anchor(
                    filename,
                    &file_url_with_hash(base, filename, contents),
                )],
            )),
        )]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let plain = send(
        router.clone(),
        get("/mirror/pypi/official/simple/friendly-bard/"),
    )
    .await;
    assert_eq!(plain.status(), StatusCode::OK);
    assert_eq!(
        plain
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );

    let wildcard = send(
        router.clone(),
        get_accept("/mirror/pypi/official/simple/friendly-bard/", "*/*"),
    )
    .await;
    assert_eq!(wildcard.status(), StatusCode::OK);
    assert_eq!(
        wildcard
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );

    let html = send(
        router.clone(),
        get_accept("/mirror/pypi/official/simple/friendly-bard/", "text/html"),
    )
    .await;
    assert_eq!(html.status(), StatusCode::OK);
    assert_eq!(
        html.headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );

    let json = send(
        router.clone(),
        get_accept(
            "/mirror/pypi/official/simple/friendly-bard/",
            "application/vnd.pypi.simple.v1+json",
        ),
    )
    .await;
    assert_eq!(json.status(), StatusCode::OK);
    assert_eq!(
        json.headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.pypi.simple.v1+json")
    );

    let unsupported = send(
        router.clone(),
        get_accept(
            "/mirror/pypi/official/simple/friendly-bard/",
            "application/json",
        ),
    )
    .await;
    assert_eq!(unsupported.status(), StatusCode::NOT_ACCEPTABLE);

    let rejected = send(
        router,
        get_accept(
            "/mirror/pypi/official/simple/friendly-bard/",
            "text/html;q=0, application/vnd.pypi.simple.v1+json;q=0",
        ),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::NOT_ACCEPTABLE);

    assert_eq!(upstream.request_count("/simple/friendly-bard/"), 1);
}

#[tokio::test]
async fn html_and_json_representations_use_the_same_project_object() {
    let contents = b"friendly wheel";
    let filename = "friendly_bard-1.0.0-py3-none-any.whl";
    let hash = sha256_hex(contents);
    let upstream = start_upstream(|base| {
        vec![(
            "/simple/friendly-bard/",
            TestResponse::html(project_html(
                "friendly-bard",
                &[anchor(
                    filename,
                    &file_url_with_hash(base, filename, contents),
                )],
            )),
        )]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let json_response = send(
        router.clone(),
        get_accept(
            "/mirror/pypi/official/simple/friendly-bard/",
            "application/vnd.pypi.simple.v1+json",
        ),
    )
    .await;
    assert_eq!(json_response.status(), StatusCode::OK);
    let json = body_json(json_response).await;
    assert_eq!(json["name"], "friendly-bard");
    assert_eq!(json["files"][0]["filename"], filename);
    assert_eq!(json["files"][0]["hashes"]["sha256"], hash);

    let html_response = send(
        router,
        get_accept("/mirror/pypi/official/simple/friendly-bard/", "text/html"),
    )
    .await;
    assert_eq!(html_response.status(), StatusCode::OK);
    let html = body_text(html_response).await;
    assert!(html.contains(filename));
    assert!(html.contains(&format!("#sha256={hash}")));

    assert_eq!(upstream.request_count("/simple/friendly-bard/"), 1);
    let representation_files = collect_files(&metadata_dir(data_dir.path()))
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".body"))
        })
        .collect::<Vec<_>>();
    assert_eq!(representation_files.len(), 2, "{representation_files:?}");
    assert!(
        representation_files
            .iter()
            .any(|path| path.to_string_lossy().contains("html"))
    );
    assert!(
        representation_files
            .iter()
            .any(|path| path.to_string_lossy().contains("json"))
    );
}

#[tokio::test]
async fn project_indices_do_not_advertise_unserved_metadata_sidecars() {
    let contents = b"friendly wheel";
    let filename = "friendly_bard-1.0.0-py3-none-any.whl";
    let upstream = start_upstream(|base| {
        let attributes = r#"data-core-metadata="sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" data-dist-info-metadata="true""#;
        vec![(
            "/simple/friendly-bard/",
            TestResponse::html(project_html(
                "friendly-bard",
                &[anchor_with_attrs(
                    filename,
                    &file_url_with_hash(base, filename, contents),
                    attributes,
                )],
            )),
        )]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let html = body_text(
        send(
            router.clone(),
            get_accept("/mirror/pypi/official/simple/friendly-bard/", "text/html"),
        )
        .await,
    )
    .await;
    assert!(!html.contains("core-metadata"), "{html}");
    assert!(!html.contains("dist-info-metadata"), "{html}");

    let json = body_json(
        send(
            router,
            get_accept(
                "/mirror/pypi/official/simple/friendly-bard/",
                "application/vnd.pypi.simple.v1+json",
            ),
        )
        .await,
    )
    .await;
    assert!(json["files"][0].get("core-metadata").is_none());
    assert!(json["files"][0].get("dist-info-metadata").is_none());
}

#[tokio::test]
async fn project_links_are_rewritten_to_the_current_entry() {
    let contents = b"friendly wheel";
    let filename = "friendly_bard-1.0.0-py3-none-any.whl";
    let hash = sha256_hex(contents);
    let upstream = start_upstream(|base| {
        vec![(
            "/simple/friendly-bard/",
            TestResponse::html(project_html(
                "friendly-bard",
                &[anchor(
                    filename,
                    &file_url_with_hash(base, filename, contents),
                )],
            )),
        )]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let html = body_text(
        send(
            router.clone(),
            get_accept("/mirror/pypi/official/simple/friendly-bard/", "text/html"),
        )
        .await,
    )
    .await;
    assert!(
        html.contains(&format!(
            "/mirror/pypi/official/files/friendly-bard/{filename}#sha256={hash}"
        )),
        "{html}"
    );
    assert!(!html.contains(upstream.base_url.as_str()), "{html}");

    let json = body_json(
        send(
            router,
            get_accept(
                "/mirror/pypi/official/simple/friendly-bard/",
                "application/vnd.pypi.simple.v1+json",
            ),
        )
        .await,
    )
    .await;
    let url = json["files"][0]["url"].as_str().expect("JSON file URL");
    assert!(
        url.contains(&format!(
            "/mirror/pypi/official/files/friendly-bard/{filename}"
        )),
        "{url}"
    );
    assert!(!url.contains(upstream.base_url.as_str()), "{url}");
}

#[tokio::test]
async fn root_index_streams_and_rewrites_project_links() {
    let projects = (0..2000)
        .map(|index| format!("project-{index}"))
        .collect::<Vec<_>>();
    let project_refs = projects.iter().map(String::as_str).collect::<Vec<_>>();
    let upstream = start_upstream({
        let body = root_html(&project_refs);
        move |_| vec![("/simple/", TestResponse::html(body.clone()))]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let response = send(router, get("/mirror/pypi/official/simple/")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(
        html.contains(r#"href="/mirror/pypi/official/simple/project-0/""#),
        "first project link missing"
    );
    assert!(
        html.contains(r#"href="/mirror/pypi/official/simple/project-1999/""#),
        "last project link missing"
    );
    assert!(!html.contains(r#"href="/simple/"#));
    assert_eq!(upstream.request_count("/simple/"), 1);
}

#[tokio::test]
async fn root_json_negotiation_returns_upstream_json() {
    let body = r#"{"meta":{"api-version":"1.0"},"projects":[{"name":"friendly-bard"}]}"#;
    let upstream = start_upstream({
        let body = body.to_owned();
        move |_| vec![("/simple/", TestResponse::json(body.clone()))]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let response = send(
        router,
        get_accept(
            "/mirror/pypi/official/simple/",
            "application/vnd.pypi.simple.v1+json",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.pypi.simple.v1+json")
    );
    let json = body_json(response).await;
    assert_eq!(json["projects"][0]["name"], "friendly-bard");
}

#[tokio::test]
async fn invalid_project_paths_and_encoded_traversal_are_rejected() {
    let upstream = start_upstream(|_| Vec::new()).await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    for path in [
        "/mirror/pypi/official/simple/%2e%2e/",
        "/mirror/pypi/official/simple/a%2fb/",
        "/mirror/pypi/official/simple/a%5cb/",
        "/mirror/pypi/official/simple//",
        "/mirror/pypi/official/simple/a/b/",
        "/mirror/pypi/official/simple/-foo/",
        "/mirror/pypi/official/simple/foo-/",
        "/mirror/pypi/official/files/friendly-bard/%2e%2e%2fsecret",
        "/mirror/pypi/official/files/friendly-bard/..%5csecret",
        "/mirror/pypi/official/files/https:%2F%2Fevil.example%2Fx.whl",
    ] {
        let response = send(router.clone(), get(path)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    assert!(upstream.requests().is_empty(), "{:?}", upstream.requests());
}

#[tokio::test]
async fn unregistered_ecosystems_and_unknown_entries_return_404() {
    let upstream = start_upstream(|_| Vec::new()).await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    for path in [
        "/mirror/npm/official/left-pad",
        "/mirror/pypi/missing/simple/friendly-bard/",
        "/mirror/pypi/official/unknown",
    ] {
        let response = send(router.clone(), get(path)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn project_path_requires_a_trailing_slash() {
    let contents = b"friendly wheel";
    let filename = "friendly_bard-1.0.0-py3-none-any.whl";
    let upstream = start_upstream(|base| {
        vec![(
            "/simple/friendly-bard/",
            TestResponse::html(project_html(
                "friendly-bard",
                &[anchor(
                    filename,
                    &file_url_with_hash(base, filename, contents),
                )],
            )),
        )]
    })
    .await;
    let data_dir = tempdir().expect("data directory");
    let router = pypi_router(&[("official", &upstream.base_url)], None, data_dir.path());

    let without_slash = send(
        router.clone(),
        get("/mirror/pypi/official/simple/friendly-bard"),
    )
    .await;
    assert_eq!(without_slash.status(), StatusCode::NOT_FOUND);

    let with_slash = send(router, get("/mirror/pypi/official/simple/friendly-bard/")).await;
    assert_eq!(with_slash.status(), StatusCode::OK);
    assert_eq!(upstream.request_count("/simple/friendly-bard/"), 1);
}
