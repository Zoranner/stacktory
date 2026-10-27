use std::{collections::BTreeMap, io, path::PathBuf, sync::Arc};

use crate::{
    PublicUrls, SourcePool,
    audit::AuditSink,
    upstream::{
        SourcePoolError,
        registry::{Adapter, AdapterFuture, AdapterRequest},
    },
};
use axum::{
    body::Body,
    http::{Method, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

mod assets;
mod cache;
mod query;

use cache::cleanup_temporary_assets;

#[derive(Debug)]
pub struct VscodeAdapter {
    service: Arc<VscodeService>,
}

#[derive(Debug)]
pub struct VscodeService {
    public_urls: PublicUrls,
    data_dir: PathBuf,
    audit: AuditSink,
}

#[derive(Debug, Error)]
pub enum VscodeError {
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error("VS Code Marketplace request JSON is invalid: {0}")]
    RequestJson(#[from] serde_json::Error),
    #[error("VS Code Marketplace response is invalid: {0}")]
    InvalidGalleryResponse(String),
    #[error("VS Code Marketplace asset `{0}` was not found")]
    MissingAsset(String),
    #[error("VS Code Marketplace storage I/O failed: {0}")]
    Storage(#[from] io::Error),
    #[error("VS Code Marketplace descriptor is invalid: {0}")]
    DescriptorParse(#[from] toml::de::Error),
    #[error("VS Code Marketplace descriptor cannot be serialized: {0}")]
    DescriptorSerialize(#[from] toml::ser::Error),
}

#[derive(Deserialize, Serialize)]
struct AssetDescriptor {
    source_name: String,
    source_url: String,
    asset_type: String,
}

#[derive(Deserialize, Serialize)]
struct ResourceDescriptor {
    source_name: String,
}

#[derive(Deserialize, Serialize)]
struct ExtensionDescriptor {
    source_name: String,
}

#[derive(Deserialize, Serialize)]
struct AssetBaseDescriptor {
    source_name: String,
    asset_uri: String,
    fallback_asset_uri: Option<String>,
}

struct ResourceRequest {
    publisher: String,
    extension: String,
    version: String,
    path: String,
}

struct ExtensionRequest {
    publisher: String,
    name: String,
}

struct AssetBaseRequest {
    asset_base_id: String,
    asset_type: String,
}

impl ResourceRequest {
    fn extension_key(&self) -> String {
        sha256(format!("{}/{}/{}", self.publisher, self.extension, self.version).as_bytes())
    }
}

impl ExtensionRequest {
    fn extension_key(&self) -> String {
        sha256(format!("{}/{}", self.publisher, self.name).as_bytes())
    }
}

struct RewrittenGalleryQuery {
    body: Vec<u8>,
    asset_descriptors: BTreeMap<String, AssetDescriptor>,
    resource_descriptors: BTreeMap<String, ResourceDescriptor>,
    extension_descriptors: BTreeMap<String, ExtensionDescriptor>,
    asset_base_descriptors: BTreeMap<String, AssetBaseDescriptor>,
}

struct MarketplaceAsset {
    path: PathBuf,
    length: u64,
    content_type: Option<String>,
}

impl VscodeAdapter {
    pub fn new(public_urls: impl Into<PublicUrls>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(VscodeService::new(public_urls, data_dir)),
        }
    }

    pub fn service(&self) -> &VscodeService {
        &self.service
    }
}

impl VscodeService {
    pub fn new(public_urls: impl Into<PublicUrls>, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        cleanup_temporary_assets(&data_dir);
        Self {
            public_urls: public_urls.into(),
            audit: AuditSink::new(data_dir.clone()),
            data_dir,
        }
    }

    pub fn public_urls(&self) -> &PublicUrls {
        &self.public_urls
    }

    async fn handle_request(
        &self,
        entry_name: &str,
        mut request: AdapterRequest,
        pool: &SourcePool,
    ) -> Response {
        request.path = request.path.trim_start_matches('/').to_owned();
        if request.path == "extensionquery" && request.method != Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        if request.path.starts_with("asset/") && request.method != Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        if request.path.starts_with("resource/") && request.method != Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        if request.path.starts_with("extension/") && request.method != Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        if request.path.starts_with("asset-base/") && request.method != Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let result = match request.method {
            Method::POST if request.path == "extensionquery" => self
                .query(entry_name, &request.body, pool, &request.base_url)
                .await
                .map(|body| response(StatusCode::OK, "application/json", body)),
            Method::GET => match self.requested_asset(entry_name, &request.path, pool).await {
                Ok(asset) => Ok(assets::asset_response(asset).await),
                Err(error) => Err(error),
            },
            _ => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
        };
        match result {
            Ok(response) => response,
            Err(error) => vscode_error_response(&error),
        }
    }

    async fn requested_asset(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
    ) -> Result<MarketplaceAsset, VscodeError> {
        if let Some(asset_id) = query::parse_asset_id(path) {
            self.asset(entry_name, asset_id, pool).await
        } else if let Some(resource) = query::parse_resource_request(path) {
            self.resource(entry_name, resource, pool).await
        } else if let Some(extension) = query::parse_extension_request(path) {
            self.extension(entry_name, extension, pool).await
        } else if let Some(asset_base) = query::parse_asset_base_request(path) {
            self.asset_base(entry_name, asset_base, pool).await
        } else {
            Err(VscodeError::MissingAsset(path.to_owned()))
        }
    }
}

impl Adapter for VscodeAdapter {
    fn ecosystem(&self) -> &str {
        "vscode"
    }

    fn public_urls(&self) -> &PublicUrls {
        self.service.public_urls()
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

fn vscode_error_response(error: &VscodeError) -> Response {
    let status = match error {
        VscodeError::MissingAsset(_) => StatusCode::NOT_FOUND,
        VscodeError::RequestJson(_) => StatusCode::BAD_REQUEST,
        VscodeError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        VscodeError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        VscodeError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        VscodeError::Source(_) | VscodeError::InvalidGalleryResponse(_) => StatusCode::BAD_GATEWAY,
        VscodeError::Storage(_)
        | VscodeError::DescriptorParse(_)
        | VscodeError::DescriptorSerialize(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response {
    let mut response = (status, Body::from(body)).into_response();
    let content_type = axum::http::HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response
}

fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}
