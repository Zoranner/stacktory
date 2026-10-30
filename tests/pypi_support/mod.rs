#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Body,
    extract::{OriginalUri, State},
    http::{
        HeaderMap, HeaderValue, Request, StatusCode,
        header::{ACCEPT, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use stacktory::{
    SourceConfig, SourceGraph, SourcePool, SourceType, ecosystems::pypi::PyPiAdapter,
    http::router_with_registry, upstream::registry::AdapterRegistry,
};
use tower::ServiceExt;
use url::Url;

#[derive(Clone, Debug)]
pub struct TestResponse {
    pub status: StatusCode,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl TestResponse {
    pub fn html(body: impl Into<String>) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/html; charset=utf-8",
            body: body.into().into_bytes(),
        }
    }

    pub fn json(body: impl Into<String>) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "application/vnd.pypi.simple.v1+json",
            body: body.into().into_bytes(),
        }
    }

    pub fn bytes(content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: StatusCode::OK,
            content_type,
            body: body.into(),
        }
    }

    pub fn status(status: StatusCode) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
        }
    }

    fn not_found() -> Self {
        Self::status(StatusCode::NOT_FOUND)
    }
}

struct UpstreamState {
    routes: Mutex<BTreeMap<String, TestResponse>>,
    requests: Mutex<Vec<(String, Option<String>)>>,
}

#[derive(Clone)]
pub struct Upstream {
    pub base_url: Url,
    state: Arc<UpstreamState>,
}

impl Upstream {
    pub fn request_count(&self, path: &str) -> usize {
        self.state
            .requests
            .lock()
            .expect("request log lock")
            .iter()
            .filter(|(request_path, _)| request_path == path)
            .count()
    }

    pub fn requests(&self) -> Vec<(String, Option<String>)> {
        self.state
            .requests
            .lock()
            .expect("request log lock")
            .clone()
    }

    pub fn accept_for(&self, path: &str) -> Option<String> {
        self.state
            .requests
            .lock()
            .expect("request log lock")
            .iter()
            .find(|(request_path, _)| request_path == path)
            .and_then(|(_, accept)| accept.clone())
    }

    pub fn set_route(&self, path: &str, response: TestResponse) {
        self.state
            .routes
            .lock()
            .expect("route lock")
            .insert(path.to_owned(), response);
    }
}

pub async fn start_upstream(
    build: impl FnOnce(&Url) -> Vec<(&'static str, TestResponse)>,
) -> Upstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("upstream listener");
    let address = listener.local_addr().expect("upstream address");
    let base_url = Url::parse(&format!("http://{address}")).expect("upstream URL");
    let routes = build(&base_url)
        .into_iter()
        .map(|(path, response)| (path.to_owned(), response))
        .collect();
    let state = Arc::new(UpstreamState {
        routes: Mutex::new(routes),
        requests: Mutex::new(Vec::new()),
    });
    let router = Router::new()
        .fallback(upstream_handler)
        .with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("upstream server");
    });
    Upstream { base_url, state }
}

async fn upstream_handler(
    State(state): State<Arc<UpstreamState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let path = uri.path().to_owned();
    let accept = headers
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    state
        .requests
        .lock()
        .expect("request log lock")
        .push((path.clone(), accept));
    let response = state
        .routes
        .lock()
        .expect("route lock")
        .get(&path)
        .cloned()
        .unwrap_or_else(TestResponse::not_found);
    let content_type = response.content_type;
    let mut http_response = (response.status, Body::from(response.body)).into_response();
    http_response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    http_response
}

pub fn pypi_router(
    upstreams: &[(&str, &Url)],
    group: Option<(&str, &[&str])>,
    data_dir: &Path,
) -> Router {
    let mut sources = upstreams
        .iter()
        .map(|(name, base_url)| proxy(name, base_url.as_str()))
        .collect::<Vec<_>>();
    if let Some((name, members)) = group {
        sources.push(SourceConfig {
            name: name.to_owned(),
            source_type: SourceType::Group,
            base_url: None,
            require_https: false,
            members: members.iter().map(|member| (*member).to_owned()).collect(),
        });
    }
    let registry = AdapterRegistry::new()
        .with_adapter(
            "pypi",
            Arc::new(
                SourcePool::new(
                    "pypi",
                    SourceGraph::new(sources).expect("PyPI source graph"),
                )
                .expect("PyPI source pool"),
            ),
            Arc::new(PyPiAdapter::new(
                Url::parse("http://127.0.0.1:18761").expect("root URL"),
                data_dir,
            )),
        )
        .expect("PyPI adapter");
    router_with_registry(registry)
}

pub async fn send(router: Router, request: Request<Body>) -> Response {
    router.oneshot(request).await.expect("router response")
}

pub fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).expect("GET request")
}

pub fn get_accept(path: &str, accept: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header(ACCEPT, accept)
        .body(Body::empty())
        .expect("GET request with Accept")
}

pub async fn body_bytes(response: Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body")
        .to_vec()
}

pub async fn body_text(response: Response) -> String {
    String::from_utf8(body_bytes(response).await).expect("UTF-8 response body")
}

pub async fn body_json(response: Response) -> Value {
    serde_json::from_slice(&body_bytes(response).await).expect("JSON response body")
}

pub fn sha256_hex(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

pub fn file_url(base_url: &Url, filename: &str) -> String {
    format!("{base_url}packages/{filename}")
}

pub fn file_url_with_hash(base_url: &Url, filename: &str, contents: &[u8]) -> String {
    format!(
        "{}#sha256={}",
        file_url(base_url, filename),
        sha256_hex(contents)
    )
}

pub fn anchor(filename: &str, href: &str) -> String {
    format!(r#"<a href="{href}">{filename}</a><br />"#)
}

pub fn anchor_with_attrs(filename: &str, href: &str, attributes: &str) -> String {
    format!(r#"<a href="{href}" {attributes}>{filename}</a><br />"#)
}

pub fn project_html(project: &str, anchors: &[String]) -> String {
    let mut html = format!(
        "<!DOCTYPE html><html><head><title>Links for {project}</title></head><body>\
         <h1>Links for {project}</h1>"
    );
    for anchor in anchors {
        html.push_str(anchor);
        html.push('\n');
    }
    html.push_str("</body></html>");
    html
}

pub fn root_html(projects: &[&str]) -> String {
    let mut html = String::from("<!DOCTYPE html><html><body>");
    for project in projects {
        html.push_str(&format!(
            r#"<a href="/simple/{project}/">{project}</a><br />"#
        ));
    }
    html.push_str("</body></html>");
    html
}

pub fn metadata_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("stores/pypi/metadata")
}

pub fn read_current(data_dir: &Path, entry: &str, project: &str) -> String {
    fs::read_to_string(
        metadata_dir(data_dir)
            .join(entry)
            .join(project)
            .join("current.toml"),
    )
    .expect("current PyPI metadata pointer")
}

pub fn set_metadata_validated_at(
    data_dir: &Path,
    entry: &str,
    project: &str,
    validated_at_unix_secs: u64,
) {
    let path = metadata_dir(data_dir)
        .join(entry)
        .join(project)
        .join("current.toml");
    let contents = fs::read_to_string(&path).expect("current metadata pointer");
    let mut updated = String::new();
    for line in contents.lines() {
        if line.starts_with("validated_at_unix_secs =") {
            updated.push_str(&format!(
                "validated_at_unix_secs = {validated_at_unix_secs}\n"
            ));
        } else {
            updated.push_str(line);
            updated.push('\n');
        }
    }
    fs::write(path, updated).expect("updated metadata pointer");
}

pub fn read_binding(data_dir: &Path, entry: &str, project: &str, filename: &str) -> Value {
    serde_json::from_slice(
        &fs::read(
            metadata_dir(data_dir)
                .join(entry)
                .join(project)
                .join("files")
                .join(format!("{filename}.json")),
        )
        .expect("PyPI file binding"),
    )
    .expect("PyPI file binding JSON")
}

pub fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files_into(root, &mut files);
    files.sort();
    files
}

fn collect_files_into(root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_files_into(&path, files);
        } else if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            files.push(path);
        }
    }
}

pub fn has_part_file(data_dir: &Path) -> bool {
    collect_files(data_dir).iter().any(|path| {
        path.extension()
            .is_some_and(|extension| extension == "part")
    })
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
