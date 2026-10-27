//! 通用镜像路由：`/mirror/{ecosystem}/{name}/...` 和 Docker 的标准 Registry 根路径。

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{OriginalUri, Path, State},
    http::{HeaderMap, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::IntoResponse,
    routing::any,
};

use crate::{
    http::public_base_url_for_request,
    upstream::registry::{AdapterRegistry, AdapterRequest},
};

#[derive(Clone)]
struct RegistryRouteState {
    registry: AdapterRegistry,
    docker_entry: Option<String>,
}

pub(super) fn registry_routes(registry: AdapterRegistry, docker_entry: Option<String>) -> Router {
    let mut router = Router::new()
        .route("/mirror/{ecosystem}/{name}/{*path}", any(named_source))
        .layer(middleware::from_fn(registry_cors));
    if docker_entry.is_some() {
        router = router
            .route("/v2/", any(docker_root_source))
            .route("/v2/{*path}", any(docker_root_source));
    }
    router.with_state(RegistryRouteState {
        registry,
        docker_entry,
    })
}

async fn named_source(
    State(state): State<RegistryRouteState>,
    Path((ecosystem, name, path)): Path<(String, String, String)>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    let Some(registered) = state.registry.get(&ecosystem) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if registered.pool.proxy_members(&name).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let body = match to_bytes(body, 1024 * 1024).await {
        Ok(body) => body.to_vec(),
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let prefix = format!("/mirror/{ecosystem}/{name}/");
    let raw_path = uri.path().strip_prefix(&prefix).unwrap_or(&path).to_owned();
    let base_url = public_base_url_for_request(&headers, registered.adapter.public_urls());
    registered
        .adapter
        .handle(
            name,
            AdapterRequest {
                base_url,
                method,
                path,
                raw_path,
                query: uri.query().map(str::to_owned),
                headers,
                body,
            },
            registered.pool.clone(),
        )
        .await
}

async fn docker_root_source(
    State(state): State<RegistryRouteState>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    let Some(entry_name) = state.docker_entry.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(registered) = state.registry.get("docker") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if registered.pool.proxy_members(entry_name).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let body = match to_bytes(body, 1024 * 1024).await {
        Ok(body) => body.to_vec(),
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let raw_path = uri
        .path()
        .strip_prefix('/')
        .unwrap_or(uri.path())
        .to_owned();
    let base_url = public_base_url_for_request(&headers, registered.adapter.public_urls());
    registered
        .adapter
        .handle(
            entry_name.to_owned(),
            AdapterRequest {
                base_url,
                method,
                path: raw_path.clone(),
                raw_path,
                query: uri.query().map(str::to_owned),
                headers,
                body,
            },
            registered.pool.clone(),
        )
        .await
}

async fn registry_cors(request: Request<Body>, next: Next) -> axum::response::Response {
    let origin = request.headers().get(header::ORIGIN).cloned();
    let requested_headers = request
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned();
    let mut response = if request.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        origin.unwrap_or_else(|| header::HeaderValue::from_static("*")),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        requested_headers.unwrap_or_else(|| {
            header::HeaderValue::from_static("Accept, Content-Type, X-Market-Client-Id")
        }),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        header::HeaderValue::from_static("true"),
    );
    headers.insert(
        header::HeaderName::from_static("access-control-allow-private-network"),
        header::HeaderValue::from_static("true"),
    );
    headers.insert(
        header::VARY,
        header::HeaderValue::from_static("Origin, Access-Control-Request-Headers"),
    );
    response
}
