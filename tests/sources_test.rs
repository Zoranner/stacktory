use std::sync::Arc;

use axum::{
    Router,
    http::{HeaderMap, StatusCode, header::LOCATION},
    routing::get,
};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType,
    sources::FetchOperation,
    sources::pool::{SourcePoolError, SourceResponse},
};
use tokio::net::TcpListener;
use url::Url;

#[test]
fn group_resolves_members_in_declared_order() {
    let graph = SourceGraph::new(vec![
        proxy("first", "https://first.example.internal"),
        proxy("second", "https://second.example.internal"),
        group("fallbacks", &["second", "first"]),
    ])
    .expect("source graph");
    let pool = SourcePool::new("rustup", graph).expect("source pool");

    let members = pool
        .proxy_members("fallbacks")
        .expect("group members")
        .into_iter()
        .map(|source| source.name)
        .collect::<Vec<_>>();

    assert_eq!(members, ["second", "first"]);
}

#[tokio::test]
async fn upstream_requests_identify_stacktory() {
    let upstream = start_server(Router::new().route(
        "/artifact",
        get(|headers: HeaderMap| async move {
            (headers
                .get("user-agent")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),)
        }),
    ))
    .await;
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![proxy("primary", upstream.as_str())]).expect("source graph"),
    )
    .expect("source pool");

    let response = pool
        .fetch("primary", "artifact", FetchOperation::Metadata)
        .await
        .expect("source response");

    assert_eq!(
        response.body,
        format!("Stacktory/{}", env!("CARGO_PKG_VERSION")).into_bytes()
    );
}

#[tokio::test]
async fn marketplace_query_posts_json_with_fixed_gallery_headers() {
    let upstream = start_server(Router::new().route(
        "/extensionquery",
        axum::routing::post(|headers: HeaderMap, body: String| async move {
            let accept = headers
                .get("accept")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let content_type = headers
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let market_client = headers
                .get("x-market-client-id")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            format!("{accept}|{content_type}|{market_client}|{body}")
        }),
    ))
    .await;
    let pool = SourcePool::new(
        "vscode",
        SourceGraph::new(vec![proxy("official", upstream.as_str())]).expect("source graph"),
    )
    .expect("source pool");

    let response = pool
        .post_json("official", "extensionquery", br#"{"filters":[]}"#)
        .await
        .expect("query response");

    assert_eq!(
        response.body,
        b"application/json;api-version=3.0-preview.1|application/json|VSCode|{\"filters\":[]}"
    );
}

#[tokio::test]
async fn fetcher_decodes_gzip_response_before_returning_body() {
    let upstream = start_server(Router::new().route(
        "/manifest",
        get(|| async {
            (
                [
                    ("content-type", "application/json"),
                    ("content-encoding", "gzip"),
                ],
                [
                    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0xab, 0x56, 0xca,
                    0xcf, 0x56, 0xb2, 0x2a, 0x29, 0x2a, 0x4d, 0xad, 0x05, 0x00, 0x90, 0x5f, 0xd4,
                    0xa7, 0x0b, 0x00, 0x00, 0x00,
                ],
            )
        }),
    ))
    .await;
    let pool = SourcePool::new(
        "vscode",
        SourceGraph::new(vec![proxy("official", upstream.as_str())]).expect("source graph"),
    )
    .expect("source pool");

    let response = pool
        .fetch("official", "manifest", FetchOperation::Artifact)
        .await
        .expect("manifest response");

    assert_eq!(response.body, br#"{"ok":true}"#);
    assert_eq!(response.content_type.as_deref(), Some("application/json"));
}

#[tokio::test]
async fn proxy_entry_does_not_fallback_to_sibling_proxy() {
    let first_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let second_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first = start_server({
        let first_hits = first_hits.clone();
        Router::new().route(
            "/artifact",
            get(move || {
                let first_hits = first_hits.clone();
                async move {
                    first_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    StatusCode::BAD_GATEWAY
                }
            }),
        )
    })
    .await;
    let second = start_server({
        let second_hits = second_hits.clone();
        Router::new().route(
            "/artifact",
            get(move || {
                let second_hits = second_hits.clone();
                async move {
                    second_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    "sibling"
                }
            }),
        )
    })
    .await;
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![
            proxy("primary", first.as_str()),
            proxy("sibling", second.as_str()),
        ])
        .expect("source graph"),
    )
    .expect("source pool");

    let result = fetch_from_pool(&pool, "primary", "artifact").await;

    assert!(result.is_err());
    assert_eq!(first_hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(second_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn group_falls_back_after_upstream_failure() {
    let unavailable =
        start_server(Router::new().route("/{*path}", get(|| async { StatusCode::BAD_GATEWAY })))
            .await;
    let available =
        start_server(Router::new().route("/artifact", get(|| async { "from second source" })))
            .await;
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![
            proxy("first", unavailable.as_str()),
            proxy("second", available.as_str()),
            group("fallbacks", &["first", "second"]),
        ])
        .expect("source graph"),
    )
    .expect("source pool");

    let response = fetch_from_pool(&pool, "fallbacks", "artifact")
        .await
        .expect("fallback response");

    assert_eq!(response.body, b"from second source");
    assert_eq!(response.source_name, "second");
}

#[tokio::test]
async fn request_key_rejects_absolute_and_abnormal_segments() {
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![proxy("primary", "https://source.example.internal")])
            .expect("source graph"),
    )
    .expect("source pool");

    for request_key in [
        "/artifact",
        "artifact//part",
        ".",
        "artifact/./part",
        "..",
        "artifact/../part",
        "artifact\\part",
        "artifact%2Fpart",
        "artifact?query",
        "artifact#fragment",
        "artifact\u{0000}part",
    ] {
        let result = pool
            .fetch("primary", request_key, FetchOperation::Metadata)
            .await;
        if request_key == "/artifact" {
            assert!(matches!(
                result,
                Err(SourcePoolError::InvalidRequestKey(value)) if value == request_key
            ));
        } else {
            assert!(
                result.is_err(),
                "request key should be rejected: {request_key:?}"
            );
        }
    }
}

#[tokio::test]
async fn request_key_preserves_configured_base_path() {
    let upstream = start_server(Router::new().route(
        "/repository/artifact",
        get(|| async { "from repository path" }),
    ))
    .await;
    let base_url = upstream.join("repository/").expect("base URL");
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![proxy("primary", base_url.as_str())]).expect("source graph"),
    )
    .expect("source pool");

    let response = pool
        .fetch("primary", "artifact", FetchOperation::Metadata)
        .await
        .expect("base path response");

    assert_eq!(response.body, b"from repository path");
}

#[tokio::test]
async fn redirect_is_not_followed_and_can_fallback() {
    let target_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let redirect = start_server({
        let target_hits = target_hits.clone();
        Router::new()
            .route(
                "/artifact",
                get(|| async { (StatusCode::FOUND, [(LOCATION, "/target")]) }),
            )
            .route(
                "/target",
                get(move || {
                    let target_hits = target_hits.clone();
                    async move {
                        target_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        "must not be followed"
                    }
                }),
            )
    })
    .await;
    let available =
        start_server(Router::new().route("/artifact", get(|| async { "from fallback source" })))
            .await;
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![
            proxy("redirect", redirect.as_str()),
            proxy("fallback", available.as_str()),
            group("fallbacks", &["redirect", "fallback"]),
        ])
        .expect("source graph"),
    )
    .expect("source pool");

    let response = pool
        .fetch("fallbacks", "artifact", FetchOperation::Metadata)
        .await
        .expect("fallback response");

    assert_eq!(response.body, b"from fallback source");
    assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn terminal_client_error_does_not_fallback() {
    let fallback_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first =
        start_server(Router::new().route("/artifact", get(|| async { StatusCode::FORBIDDEN })))
            .await;
    let second = start_server({
        let fallback_hits = fallback_hits.clone();
        Router::new().route(
            "/artifact",
            get(move || {
                let fallback_hits = fallback_hits.clone();
                async move {
                    fallback_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    "must not be requested"
                }
            }),
        )
    })
    .await;
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![
            proxy("first", first.as_str()),
            proxy("second", second.as_str()),
            group("fallbacks", &["first", "second"]),
        ])
        .expect("source graph"),
    )
    .expect("source pool");

    let result = pool
        .fetch("fallbacks", "artifact", FetchOperation::Metadata)
        .await;

    assert!(matches!(
        result,
        Err(SourcePoolError::TerminalSourceError { source_name, .. }) if source_name == "first"
    ));
    assert_eq!(fallback_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unauthorized_and_forbidden_do_not_fallback() {
    let fallback_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first = start_server(
        Router::new()
            .route("/unauthorized", get(|| async { StatusCode::UNAUTHORIZED }))
            .route("/forbidden", get(|| async { StatusCode::FORBIDDEN }))
            .route("/missing", get(|| async { StatusCode::NOT_FOUND })),
    )
    .await;
    let second = start_server({
        let fallback_hits = fallback_hits.clone();
        Router::new().route(
            "/{*path}",
            get(move || {
                let fallback_hits = fallback_hits.clone();
                async move {
                    fallback_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    "must not be requested"
                }
            }),
        )
    })
    .await;
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![
            proxy("first", first.as_str()),
            proxy("second", second.as_str()),
            group("fallbacks", &["first", "second"]),
        ])
        .expect("source graph"),
    )
    .expect("source pool");

    for request_key in ["unauthorized", "forbidden"] {
        let result = pool
            .fetch("fallbacks", request_key, FetchOperation::Metadata)
            .await;
        assert!(matches!(
            result,
            Err(SourcePoolError::TerminalSourceError { source_name, .. })
                if source_name == "first"
        ));
    }
    assert_eq!(fallback_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn group_falls_back_after_not_found_and_reports_not_found_when_all_missing() {
    let first =
        start_server(Router::new().route("/missing", get(|| async { StatusCode::NOT_FOUND })))
            .await;
    let second =
        start_server(Router::new().route("/missing", get(|| async { "from second source" }))).await;
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![
            proxy("first", first.as_str()),
            proxy("second", second.as_str()),
            group("fallbacks", &["first", "second"]),
        ])
        .expect("source graph"),
    )
    .expect("source pool");

    let response = fetch_from_pool(&pool, "fallbacks", "missing")
        .await
        .expect("404 fallback response");
    assert_eq!(response.body, b"from second source");

    let only_missing =
        start_server(Router::new().route("/missing", get(|| async { StatusCode::NOT_FOUND })))
            .await;
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![
            proxy("only", only_missing.as_str()),
            group("group", &["only"]),
        ])
        .expect("source graph"),
    )
    .expect("source pool");
    let error = fetch_from_pool(&pool, "group", "missing")
        .await
        .expect_err("all missing should fail");
    assert!(matches!(
        error,
        SourcePoolError::AllSourcesFailed {
            status: Some(404),
            ..
        }
    ));
}

#[tokio::test]
async fn request_timeout_and_rate_limit_do_not_fallback() {
    let fallback_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first = start_server(
        Router::new()
            .route("/timeout", get(|| async { StatusCode::REQUEST_TIMEOUT }))
            .route(
                "/rate-limit",
                get(|| async { StatusCode::TOO_MANY_REQUESTS }),
            ),
    )
    .await;
    let second = start_server({
        let fallback_hits = fallback_hits.clone();
        Router::new().route(
            "/{*path}",
            get(move || {
                let fallback_hits = fallback_hits.clone();
                async move {
                    fallback_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    "must not be requested"
                }
            }),
        )
    })
    .await;
    let pool = SourcePool::new(
        "rustup",
        SourceGraph::new(vec![
            proxy("first", first.as_str()),
            proxy("second", second.as_str()),
            group("fallbacks", &["first", "second"]),
        ])
        .expect("source graph"),
    )
    .expect("source pool");

    for request_key in ["timeout", "rate-limit"] {
        let result = pool
            .fetch("fallbacks", request_key, FetchOperation::Metadata)
            .await;
        assert!(matches!(
            result,
            Err(SourcePoolError::TerminalSourceError { source_name, .. })
                if source_name == "first"
        ));
    }
    assert_eq!(fallback_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
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

async fn fetch_from_pool(
    pool: &SourcePool,
    entry_name: &str,
    request_key: &str,
) -> Result<SourceResponse, SourcePoolError> {
    pool.fetch(entry_name, request_key, FetchOperation::Metadata)
        .await
}

async fn start_server(router: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    Url::parse(&format!("http://{address}")).expect("server URL")
}
