use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use stacktory::http::router;
use tower::ServiceExt;

#[tokio::test]
async fn health_route_reports_service_availability() {
    let response = router()
        .oneshot(
            Request::get("/healthz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
}
