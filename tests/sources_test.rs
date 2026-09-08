use std::sync::Arc;

use axum::{Router, http::StatusCode, routing::get};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType,
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
    let pool = SourcePool::new(graph).expect("source pool");

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
    let fetcher = pool.fetcher();
    pool.fetch(entry_name, request_key, move |url| {
        let fetcher = fetcher.clone();
        async move { fetcher.fetch(url).await }
    })
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
