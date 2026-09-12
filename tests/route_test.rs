use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    response::Response,
};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType,
    http::router_with_registry,
    sources::registry::{Adapter, AdapterRegistry, AdapterRequest, RegistryError},
};
use tower::ServiceExt;
use url::Url;

#[tokio::test]
async fn route_uses_ecosystem_and_source_name() {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "demo",
            pool("demo"),
            Arc::new(EchoAdapter { ecosystem: "demo" }),
        )
        .expect("adapter registration");

    let response = router_with_registry(registry)
        .oneshot(
            Request::get("/mirror/demo/internal/path/to/object")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body"),
        "internal:path/to/object"
    );
}

#[tokio::test]
async fn unknown_ecosystem_or_source_returns_not_found() {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "demo",
            pool("demo"),
            Arc::new(EchoAdapter { ecosystem: "demo" }),
        )
        .expect("adapter registration");
    let router = router_with_registry(registry);

    let unknown_ecosystem = router
        .clone()
        .oneshot(
            Request::get("/mirror/missing/internal/object")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let unknown_source = router
        .oneshot(
            Request::get("/mirror/demo/missing/object")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(unknown_ecosystem.status(), StatusCode::NOT_FOUND);
    assert_eq!(unknown_source.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn route_forwards_post_method_and_body_to_the_registered_adapter() {
    let observed = Arc::new(Mutex::new(None));
    let registry = AdapterRegistry::new()
        .with_adapter(
            "demo",
            pool("demo"),
            Arc::new(RecordingAdapter {
                ecosystem: "demo",
                observed: observed.clone(),
            }),
        )
        .expect("adapter registration");
    let response = router_with_registry(registry)
        .oneshot(
            Request::post("/mirror/demo/internal/extensionquery")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"filters":[]}"#))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        *observed.lock().expect("observation lock"),
        Some((
            Method::POST,
            "extensionquery".to_owned(),
            br#"{"filters":[]}"#.to_vec()
        ))
    );
}

#[tokio::test]
async fn route_rejects_bodies_larger_than_one_mebibyte() {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "demo",
            pool("demo"),
            Arc::new(EchoAdapter { ecosystem: "demo" }),
        )
        .expect("adapter registration");
    let response = router_with_registry(registry)
        .oneshot(
            Request::post("/mirror/demo/internal/extensionquery")
                .body(Body::from(vec![b'x'; 1024 * 1024 + 1]))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn registry_route_answers_cors_preflight_without_calling_the_adapter() {
    let observed = Arc::new(Mutex::new(None));
    let registry = AdapterRegistry::new()
        .with_adapter(
            "demo",
            pool("demo"),
            Arc::new(RecordingAdapter {
                ecosystem: "demo",
                observed: observed.clone(),
            }),
        )
        .expect("adapter registration");
    let response = router_with_registry(registry)
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/mirror/demo/internal/extensionquery")
                .header("origin", "vscode-file://vscode-app")
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    "content-type, x-client-name, x-client-version",
                )
                .body(Body::empty())
                .expect("preflight request"),
        )
        .await
        .expect("preflight response");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("vscode-file://vscode-app")
    );
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-methods")
            .and_then(|value| value.to_str().ok()),
        Some("GET, POST, OPTIONS")
    );
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-headers")
            .and_then(|value| value.to_str().ok()),
        Some("content-type, x-client-name, x-client-version")
    );
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-private-network")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    assert_eq!(*observed.lock().expect("observation lock"), None);
}

#[test]
fn duplicate_ecosystem_registration_returns_error() {
    let registry = AdapterRegistry::new()
        .with_adapter(
            "demo",
            pool("demo"),
            Arc::new(EchoAdapter { ecosystem: "demo" }),
        )
        .expect("first registration");
    let result = registry.with_adapter(
        "demo",
        pool("demo"),
        Arc::new(EchoAdapter { ecosystem: "demo" }),
    );

    assert!(matches!(
        result,
        Err(RegistryError::DuplicateEcosystem(name)) if name == "demo"
    ));
}

#[test]
fn registry_rejects_ecosystem_identity_mismatch() {
    let result = AdapterRegistry::new().with_adapter(
        "demo",
        pool("cargo"),
        Arc::new(EchoAdapter { ecosystem: "demo" }),
    );

    assert!(matches!(
        result,
        Err(RegistryError::EcosystemMismatch { ecosystem, pool, adapter })
            if ecosystem == "demo" && pool == "cargo" && adapter == "demo"
    ));
}

fn pool(ecosystem: &str) -> Arc<SourcePool> {
    Arc::new(
        SourcePool::new(
            ecosystem,
            SourceGraph::new(vec![SourceConfig {
                name: "internal".to_owned(),
                source_type: SourceType::Proxy,
                base_url: Some(Url::parse("https://source.example.internal").expect("base URL")),
                require_https: true,
                members: vec![],
            }])
            .expect("source graph"),
        )
        .expect("source pool"),
    )
}

struct EchoAdapter {
    ecosystem: &'static str,
}

type ObservedRequest = Arc<Mutex<Option<(Method, String, Vec<u8>)>>>;

impl Adapter for EchoAdapter {
    fn ecosystem(&self) -> &str {
        self.ecosystem
    }

    fn handle(
        &self,
        source_name: String,
        request: AdapterRequest,
        _pool: Arc<SourcePool>,
    ) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        Box::pin(
            async move { Response::new(Body::from(format!("{source_name}:{}", request.path))) },
        )
    }
}

struct RecordingAdapter {
    ecosystem: &'static str,
    observed: ObservedRequest,
}

impl Adapter for RecordingAdapter {
    fn ecosystem(&self) -> &str {
        self.ecosystem
    }

    fn handle(
        &self,
        _source_name: String,
        request: AdapterRequest,
        _pool: Arc<SourcePool>,
    ) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let observed = self.observed.clone();
        Box::pin(async move {
            *observed.lock().expect("observation lock") =
                Some((request.method, request.path, request.body));
            Response::new(Body::from("recorded"))
        })
    }
}
