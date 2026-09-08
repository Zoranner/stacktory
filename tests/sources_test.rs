use std::sync::Arc;

use axum::{
    Router,
    http::{StatusCode, header::LOCATION},
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
            .fetch("primary", request_key, FetchOperation::Get)
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
        .fetch("primary", "artifact", FetchOperation::Get)
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
        .fetch("fallbacks", "artifact", FetchOperation::Get)
        .await
        .expect("fallback response");

    assert_eq!(response.body, b"from fallback source");
    assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn terminal_client_error_does_not_fallback() {
    let fallback_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first =
        start_server(Router::new().route("/artifact", get(|| async { StatusCode::NOT_FOUND })))
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
        .fetch("fallbacks", "artifact", FetchOperation::Get)
        .await;

    assert!(matches!(
        result,
        Err(SourcePoolError::TerminalSourceError { source_name, .. }) if source_name == "first"
    ));
    assert_eq!(fallback_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unauthorized_forbidden_and_missing_do_not_fallback() {
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

    for request_key in ["unauthorized", "forbidden", "missing"] {
        let result = pool
            .fetch("fallbacks", request_key, FetchOperation::Get)
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
            .fetch("fallbacks", request_key, FetchOperation::Get)
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
    pool.fetch(entry_name, request_key, FetchOperation::Get)
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
