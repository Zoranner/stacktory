use std::{future::Future, pin::Pin, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType,
    http::router_with_registry,
    sources::registry::{Adapter, AdapterRegistry, RegistryError},
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

impl Adapter for EchoAdapter {
    fn ecosystem(&self) -> &str {
        self.ecosystem
    }

    fn handle(
        &self,
        source_name: String,
        path: String,
        _pool: Arc<SourcePool>,
    ) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        Box::pin(async move { Response::new(Body::from(format!("{source_name}:{path}"))) })
    }
}
