pub mod path;

use std::{path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use url::Url;

use crate::{
    SourcePool,
    sources::registry::{Adapter, AdapterFuture, AdapterRequest},
};

use self::path::{DockerRequest, parse_request};

#[derive(Debug)]
pub struct DockerAdapter {
    service: Arc<DockerService>,
}

#[derive(Debug)]
pub struct DockerService {
    _root_url: Url,
    _data_dir: PathBuf,
}

impl DockerAdapter {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(DockerService::new(root_url, data_dir)),
        }
    }

    pub fn service(&self) -> &DockerService {
        &self.service
    }
}

impl DockerService {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            _root_url: root_url,
            _data_dir: data_dir.into(),
        }
    }

    async fn handle_request(
        &self,
        _entry_name: &str,
        request: AdapterRequest,
        _pool: &SourcePool,
    ) -> Response {
        if request.method != Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        match parse_request(&request.raw_path) {
            Ok(DockerRequest::Probe) => (
                StatusCode::OK,
                [(
                    header::HeaderName::from_static("docker-distribution-api-version"),
                    "registry/2.0",
                )],
                Body::empty(),
            )
                .into_response(),
            Ok(DockerRequest::Manifest { .. } | DockerRequest::Blob { .. }) => {
                StatusCode::NOT_IMPLEMENTED.into_response()
            }
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        }
    }
}

impl Adapter for DockerAdapter {
    fn ecosystem(&self) -> &str {
        "docker"
    }

    fn handle(
        &self,
        source_name: String,
        request: AdapterRequest,
        pool: Arc<SourcePool>,
    ) -> AdapterFuture {
        let service = self.service.clone();
        Box::pin(async move { service.handle_request(&source_name, request, &pool).await })
    }
}
