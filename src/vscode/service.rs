use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{Method, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::{
    SourcePool,
    audit::{AuditDetails, AuditSink},
    sources::{
        FetchOperation, SourcePoolError,
        pool::is_safe_configured_url,
        registry::{Adapter, AdapterFuture, AdapterRequest},
    },
};

#[derive(Debug)]
pub struct VscodeAdapter {
    service: Arc<VscodeService>,
}

#[derive(Debug)]
pub struct VscodeService {
    root_url: Url,
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

#[derive(Deserialize, Serialize)]
struct AssetCacheMetadata {
    content_type: Option<String>,
}

impl VscodeAdapter {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(VscodeService::new(root_url, data_dir)),
        }
    }

    pub fn service(&self) -> &VscodeService {
        &self.service
    }
}

impl VscodeService {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        Self {
            root_url,
            audit: AuditSink::new(data_dir.clone()),
            data_dir,
        }
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
                .query(entry_name, &request.body, pool)
                .await
                .map(|body| response(StatusCode::OK, "application/json", body)),
            Method::GET => {
                if let Some(asset_id) = parse_asset_id(&request.path) {
                    self.asset(entry_name, asset_id, pool)
                        .await
                        .map(asset_response)
                } else if let Some(resource) = parse_resource_request(&request.path) {
                    self.resource(entry_name, resource, pool)
                        .await
                        .map(asset_response)
                } else if let Some(extension) = parse_extension_request(&request.path) {
                    self.extension(entry_name, extension, pool)
                        .await
                        .map(asset_response)
                } else if let Some(asset_base) = parse_asset_base_request(&request.path) {
                    self.asset_base(entry_name, asset_base, pool)
                        .await
                        .map(asset_response)
                } else {
                    Err(VscodeError::MissingAsset(request.path))
                }
            }
            _ => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
        };

        match result {
            Ok(response) => response,
            Err(error) => vscode_error_response(&error),
        }
    }

    async fn query(
        &self,
        entry_name: &str,
        request_body: &[u8],
        pool: &SourcePool,
    ) -> Result<Vec<u8>, VscodeError> {
        serde_json::from_slice::<Value>(request_body)?;
        let response = pool
            .post_json(entry_name, "extensionquery", request_body)
            .await?;
        let rewritten = rewrite_gallery_query(
            &response.body,
            &self.root_url,
            entry_name,
            &response.source_name,
        )?;
        for (asset_id, descriptor) in rewritten.asset_descriptors {
            atomic_write(
                &self.asset_descriptor_path(entry_name, &asset_id),
                toml::to_string(&descriptor)?.as_bytes(),
            )?;
        }
        for (key, descriptor) in rewritten.resource_descriptors {
            atomic_write(
                &self.resource_descriptor_path(entry_name, &key),
                toml::to_string(&descriptor)?.as_bytes(),
            )?;
        }
        for (key, descriptor) in rewritten.extension_descriptors {
            atomic_write(
                &self.extension_descriptor_path(entry_name, &key),
                toml::to_string(&descriptor)?.as_bytes(),
            )?;
        }
        for (key, descriptor) in rewritten.asset_base_descriptors {
            atomic_write(
                &self.asset_base_descriptor_path(entry_name, &key),
                toml::to_string(&descriptor)?.as_bytes(),
            )?;
        }
        atomic_write(&self.query_path(entry_name, request_body), &rewritten.body)?;
        self.record(
            "vscode_marketplace_query_published",
            AuditDetails {
                ecosystem: Some("vscode"),
                entry_name: Some(entry_name),
                object_key: Some("extensionquery"),
                selected_proxy: Some(&response.source_name),
                object_type: Some("metadata"),
                cache_status: Some("published"),
                failure_category: None,
                count: None,
            },
        );
        Ok(rewritten.body)
    }

    async fn asset(
        &self,
        entry_name: &str,
        asset_id: String,
        pool: &SourcePool,
    ) -> Result<MarketplaceAsset, VscodeError> {
        let descriptor_path = self.asset_descriptor_path(entry_name, &asset_id);
        let descriptor_contents = fs::read_to_string(&descriptor_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                VscodeError::MissingAsset(asset_id.clone())
            } else {
                VscodeError::Storage(error)
            }
        })?;
        let descriptor: AssetDescriptor = toml::from_str(&descriptor_contents)?;
        if !pool.proxy_members(entry_name).is_some_and(|members| {
            members
                .iter()
                .any(|member| member.name == descriptor.source_name)
        }) {
            return Err(VscodeError::MissingAsset(asset_id));
        }
        self.fetch_bound_asset(
            entry_name,
            &asset_id,
            &descriptor.source_name,
            &descriptor.source_url,
            None,
            pool,
        )
        .await
    }

    async fn resource(
        &self,
        entry_name: &str,
        resource: ResourceRequest,
        pool: &SourcePool,
    ) -> Result<MarketplaceAsset, VscodeError> {
        let descriptor_path = self.resource_descriptor_path(entry_name, &resource.extension_key());
        let descriptor_contents = fs::read_to_string(&descriptor_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                VscodeError::MissingAsset(resource.path.clone())
            } else {
                VscodeError::Storage(error)
            }
        })?;
        let descriptor: ResourceDescriptor = toml::from_str(&descriptor_contents)?;
        let source = pool
            .proxy_members(entry_name)
            .and_then(|members| {
                members
                    .into_iter()
                    .find(|member| member.name == descriptor.source_name)
            })
            .ok_or_else(|| VscodeError::MissingAsset(resource.path.clone()))?;
        let scheme = source
            .base_url
            .as_ref()
            .expect("validated proxy source must have a base URL")
            .scheme();
        let source_url = Url::parse(&format!(
            "{}://{}.vscode-unpkg.net/{}/{}/{}/{}",
            scheme,
            resource.publisher,
            resource.publisher,
            resource.extension,
            resource.version,
            resource.path
        ))
        .expect("validated resource segments must produce a valid URL");
        if !is_safe_configured_url(&source_url) {
            return Err(VscodeError::InvalidGalleryResponse(
                "extension resource URL is unsafe".to_owned(),
            ));
        }
        let cache_id = sha256(source_url.as_str().as_bytes());
        self.fetch_bound_asset(
            entry_name,
            &cache_id,
            &descriptor.source_name,
            source_url.as_str(),
            None,
            pool,
        )
        .await
    }

    async fn extension(
        &self,
        entry_name: &str,
        extension: ExtensionRequest,
        pool: &SourcePool,
    ) -> Result<MarketplaceAsset, VscodeError> {
        let descriptor_path =
            self.extension_descriptor_path(entry_name, &extension.extension_key());
        let descriptor_contents = fs::read_to_string(&descriptor_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                VscodeError::MissingAsset(extension.name.clone())
            } else {
                VscodeError::Storage(error)
            }
        })?;
        let descriptor: ExtensionDescriptor = toml::from_str(&descriptor_contents)?;
        let source = pool
            .proxy_members(entry_name)
            .and_then(|members| {
                members
                    .into_iter()
                    .find(|member| member.name == descriptor.source_name)
            })
            .ok_or_else(|| VscodeError::MissingAsset(extension.name.clone()))?;
        let scheme = source
            .base_url
            .as_ref()
            .expect("validated proxy source must have a base URL")
            .scheme();
        let source_url = Url::parse(&format!(
            "{}://www.vscode-unpkg.net/_gallery/{}/{}/latest",
            scheme, extension.publisher, extension.name
        ))
        .expect("validated extension segments must produce a valid URL");
        if !is_safe_configured_url(&source_url) {
            return Err(VscodeError::InvalidGalleryResponse(
                "extension package URL is unsafe".to_owned(),
            ));
        }
        let cache_id = sha256(source_url.as_str().as_bytes());
        self.fetch_bound_asset(
            entry_name,
            &cache_id,
            &descriptor.source_name,
            source_url.as_str(),
            None,
            pool,
        )
        .await
    }

    async fn asset_base(
        &self,
        entry_name: &str,
        request: AssetBaseRequest,
        pool: &SourcePool,
    ) -> Result<MarketplaceAsset, VscodeError> {
        let descriptor_path = self.asset_base_descriptor_path(entry_name, &request.asset_base_id);
        let descriptor_contents = fs::read_to_string(&descriptor_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                VscodeError::MissingAsset(request.asset_base_id.clone())
            } else {
                VscodeError::Storage(error)
            }
        })?;
        let descriptor: AssetBaseDescriptor = toml::from_str(&descriptor_contents)?;
        if !pool.proxy_members(entry_name).is_some_and(|members| {
            members
                .iter()
                .any(|member| member.name == descriptor.source_name)
        }) {
            return Err(VscodeError::MissingAsset(request.asset_base_id));
        }
        let source_url = append_asset_type(&descriptor.asset_uri, &request.asset_type)?;
        let fallback_url = descriptor
            .fallback_asset_uri
            .as_deref()
            .map(|url| append_asset_type(url, &request.asset_type))
            .transpose()?;
        let cache_id = sha256(source_url.as_str().as_bytes());
        self.fetch_bound_asset(
            entry_name,
            &cache_id,
            &descriptor.source_name,
            source_url.as_str(),
            fallback_url.as_ref().map(Url::as_str),
            pool,
        )
        .await
    }

    async fn fetch_bound_asset(
        &self,
        entry_name: &str,
        cache_id: &str,
        source_name: &str,
        source_url: &str,
        fallback_url: Option<&str>,
        pool: &SourcePool,
    ) -> Result<MarketplaceAsset, VscodeError> {
        let cache_path = self.asset_path(source_name, cache_id);
        if let Some(bytes) = read_if_present(&cache_path)? {
            let content_type =
                read_asset_cache_metadata(&self.asset_cache_metadata_path(source_name, cache_id))?;
            if !is_stale_compressed_cache(&bytes, content_type.as_deref()) {
                let _ = crate::retention::note_access(&cache_path);
                self.record(
                    "vscode_marketplace_asset_read",
                    AuditDetails {
                        ecosystem: Some("vscode"),
                        entry_name: Some(entry_name),
                        object_key: Some(cache_id),
                        selected_proxy: Some(source_name),
                        object_type: Some("artifact"),
                        cache_status: Some("hit"),
                        failure_category: None,
                        count: None,
                    },
                );
                return Ok(MarketplaceAsset {
                    bytes,
                    content_type,
                });
            }
        }

        let source_url = Url::parse(source_url).map_err(|_| {
            VscodeError::InvalidGalleryResponse("stored asset URL is invalid".to_owned())
        })?;
        let response = match pool
            .fetch_configured_url(source_name, source_url, FetchOperation::Artifact)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let Some(fallback_url) = fallback_url else {
                    return Err(error.into());
                };
                let fallback_url = Url::parse(fallback_url).map_err(|_| {
                    VscodeError::InvalidGalleryResponse(
                        "stored fallback asset URL is invalid".to_owned(),
                    )
                })?;
                pool.fetch_configured_url(source_name, fallback_url, FetchOperation::Artifact)
                    .await?
            }
        };
        atomic_write(&cache_path, &response.body)?;
        atomic_write(
            &self.asset_cache_metadata_path(source_name, cache_id),
            toml::to_string(&AssetCacheMetadata {
                content_type: response.content_type.clone(),
            })?
            .as_bytes(),
        )?;
        self.record(
            "vscode_marketplace_asset_published",
            AuditDetails {
                ecosystem: Some("vscode"),
                entry_name: Some(entry_name),
                object_key: Some(cache_id),
                selected_proxy: Some(source_name),
                object_type: Some("artifact"),
                cache_status: Some("published"),
                failure_category: None,
                count: None,
            },
        );
        Ok(MarketplaceAsset {
            bytes: response.body,
            content_type: response.content_type,
        })
    }

    fn query_path(&self, entry_name: &str, request_body: &[u8]) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, "vscode")
            .join(entry_name)
            .join("marketplace")
            .join(format!("{}.json", sha256(request_body)))
    }

    fn asset_descriptor_path(&self, entry_name: &str, asset_id: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, "vscode")
            .join(entry_name)
            .join("marketplace")
            .join("assets")
            .join(format!("{asset_id}.toml"))
    }

    fn resource_descriptor_path(&self, entry_name: &str, extension_key: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, "vscode")
            .join(entry_name)
            .join("marketplace")
            .join("resources")
            .join(format!("{extension_key}.toml"))
    }

    fn extension_descriptor_path(&self, entry_name: &str, extension_key: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, "vscode")
            .join(entry_name)
            .join("marketplace")
            .join("extensions")
            .join(format!("{extension_key}.toml"))
    }

    fn asset_base_descriptor_path(&self, entry_name: &str, asset_base_id: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, "vscode")
            .join(entry_name)
            .join("marketplace")
            .join("asset-bases")
            .join(format!("{asset_base_id}.toml"))
    }

    fn asset_path(&self, source_name: &str, asset_id: &str) -> PathBuf {
        crate::layout::artifact_objects_root(&self.data_dir, "vscode", source_name)
            .join("marketplace")
            .join(asset_id)
    }

    fn asset_cache_metadata_path(&self, source_name: &str, asset_id: &str) -> PathBuf {
        self.asset_path(source_name, asset_id)
            .with_file_name(format!("{asset_id}.toml"))
    }

    fn record(&self, event: &str, details: AuditDetails<'_>) {
        if let Err(error) = self.audit.record_detailed(
            event,
            details.object_key.unwrap_or_default(),
            if details.failure_category.is_some() {
                "failure"
            } else {
                "success"
            },
            details,
        ) {
            eprintln!("Stacktory audit write failed: {error}");
        }
    }
}

struct MarketplaceAsset {
    bytes: Vec<u8>,
    content_type: Option<String>,
}

fn asset_response(asset: MarketplaceAsset) -> Response {
    response(
        StatusCode::OK,
        asset
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream"),
        asset.bytes,
    )
}

struct ResourceRequest {
    publisher: String,
    extension: String,
    version: String,
    path: String,
}

impl ResourceRequest {
    fn extension_key(&self) -> String {
        sha256(format!("{}/{}/{}", self.publisher, self.extension, self.version).as_bytes())
    }
}

struct ExtensionRequest {
    publisher: String,
    name: String,
}

struct AssetBaseRequest {
    asset_base_id: String,
    asset_type: String,
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

impl Adapter for VscodeAdapter {
    fn ecosystem(&self) -> &str {
        "vscode"
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

fn rewrite_gallery_query(
    source: &[u8],
    root_url: &Url,
    entry_name: &str,
    source_name: &str,
) -> Result<RewrittenGalleryQuery, VscodeError> {
    let mut query: Value = serde_json::from_slice(source)?;
    let results = query
        .get_mut("results")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| VscodeError::InvalidGalleryResponse("results is missing".to_owned()))?;
    let mut asset_descriptors = BTreeMap::new();
    let mut resource_descriptors = BTreeMap::new();
    let mut extension_descriptors = BTreeMap::new();
    let mut asset_base_descriptors = BTreeMap::new();

    for result in results {
        let extensions = result
            .get_mut("extensions")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| {
                VscodeError::InvalidGalleryResponse("result extensions is missing".to_owned())
            })?;
        for extension in extensions {
            let publisher = extension
                .get("publisher")
                .and_then(Value::as_object)
                .and_then(|publisher| publisher.get("publisherName"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    VscodeError::InvalidGalleryResponse(
                        "extension publisher.publisherName is missing".to_owned(),
                    )
                })?
                .to_owned();
            let extension_name = extension
                .get("extensionName")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    VscodeError::InvalidGalleryResponse(
                        "extension extensionName is missing".to_owned(),
                    )
                })?
                .to_owned();
            if !is_safe_resource_segment(&publisher) || !is_safe_resource_segment(&extension_name) {
                return Err(VscodeError::InvalidGalleryResponse(
                    "extension identity is invalid".to_owned(),
                ));
            }
            let extension_key = sha256(format!("{publisher}/{extension_name}").as_bytes());
            extension_descriptors.insert(
                extension_key,
                ExtensionDescriptor {
                    source_name: source_name.to_owned(),
                },
            );
            let versions = extension
                .get_mut("versions")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| {
                    VscodeError::InvalidGalleryResponse("extension versions is missing".to_owned())
                })?;
            for version in versions {
                let version_object = version.as_object_mut().ok_or_else(|| {
                    VscodeError::InvalidGalleryResponse("extension version is invalid".to_owned())
                })?;
                let version_name = version_object
                    .get("version")
                    .and_then(Value::as_str)
                    .filter(|version| is_safe_resource_segment(version))
                    .ok_or_else(|| {
                        VscodeError::InvalidGalleryResponse(
                            "extension version is missing or invalid".to_owned(),
                        )
                    })?
                    .to_owned();
                let resource_key =
                    sha256(format!("{publisher}/{extension_name}/{version_name}").as_bytes());
                resource_descriptors.insert(
                    resource_key,
                    ResourceDescriptor {
                        source_name: source_name.to_owned(),
                    },
                );
                if let Some(asset_uri) = version_object.get("assetUri").and_then(Value::as_str) {
                    let asset_uri = Url::parse(asset_uri).map_err(|_| {
                        VscodeError::InvalidGalleryResponse(
                            "extension assetUri is invalid".to_owned(),
                        )
                    })?;
                    if !is_safe_configured_url(&asset_uri) {
                        return Err(VscodeError::InvalidGalleryResponse(
                            "extension assetUri is unsafe".to_owned(),
                        ));
                    }
                    let fallback_asset_uri = version_object
                        .get("fallbackAssetUri")
                        .and_then(Value::as_str)
                        .map(|fallback| {
                            let fallback = Url::parse(fallback).map_err(|_| {
                                VscodeError::InvalidGalleryResponse(
                                    "extension fallbackAssetUri is invalid".to_owned(),
                                )
                            })?;
                            if !is_safe_configured_url(&fallback) {
                                return Err(VscodeError::InvalidGalleryResponse(
                                    "extension fallbackAssetUri is unsafe".to_owned(),
                                ));
                            }
                            Ok(fallback.to_string())
                        })
                        .transpose()?;
                    let asset_base_id = sha256(asset_uri.as_str().as_bytes());
                    asset_base_descriptors.insert(
                        asset_base_id.clone(),
                        AssetBaseDescriptor {
                            source_name: source_name.to_owned(),
                            asset_uri: asset_uri.to_string(),
                            fallback_asset_uri,
                        },
                    );
                    version_object.insert(
                        "assetUri".to_owned(),
                        Value::String(local_asset_base_url(root_url, entry_name, &asset_base_id)),
                    );
                    if version_object.contains_key("fallbackAssetUri") {
                        version_object.insert(
                            "fallbackAssetUri".to_owned(),
                            Value::String(local_asset_base_url(
                                root_url,
                                entry_name,
                                &asset_base_id,
                            )),
                        );
                    }
                }
                let files = version_object
                    .get_mut("files")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| {
                        VscodeError::InvalidGalleryResponse("version files is missing".to_owned())
                    })?;
                for file in files {
                    let object = file.as_object_mut().ok_or_else(|| {
                        VscodeError::InvalidGalleryResponse("version file is invalid".to_owned())
                    })?;
                    let asset_type = object
                        .get("assetType")
                        .and_then(Value::as_str)
                        .filter(|asset_type| !asset_type.is_empty())
                        .ok_or_else(|| {
                            VscodeError::InvalidGalleryResponse(
                                "version file assetType is missing".to_owned(),
                            )
                        })?
                        .to_owned();
                    let source_url = object
                        .get("source")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            VscodeError::InvalidGalleryResponse(
                                "version file source is missing".to_owned(),
                            )
                        })?
                        .to_owned();
                    let source_url = Url::parse(&source_url).map_err(|_| {
                        VscodeError::InvalidGalleryResponse(
                            "version file source URL is invalid".to_owned(),
                        )
                    })?;
                    if !is_safe_configured_url(&source_url) {
                        return Err(VscodeError::InvalidGalleryResponse(
                            "version file source URL is unsafe".to_owned(),
                        ));
                    }
                    let asset_id = sha256(source_url.as_str().as_bytes());
                    asset_descriptors.insert(
                        asset_id.clone(),
                        AssetDescriptor {
                            source_name: source_name.to_owned(),
                            source_url: source_url.to_string(),
                            asset_type,
                        },
                    );
                    object.insert(
                        "source".to_owned(),
                        Value::String(local_asset_url(root_url, entry_name, &asset_id)),
                    );
                }
            }
        }
    }

    serde_json::to_vec(&query)
        .map(|body| RewrittenGalleryQuery {
            body,
            asset_descriptors,
            resource_descriptors,
            extension_descriptors,
            asset_base_descriptors,
        })
        .map_err(VscodeError::RequestJson)
}

fn parse_asset_id(path: &str) -> Option<String> {
    let asset_id = path.strip_prefix("asset/")?;
    (asset_id.len() == 64
        && asset_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !asset_id.contains('/'))
    .then_some(asset_id.to_owned())
}

fn parse_resource_request(path: &str) -> Option<ResourceRequest> {
    let mut segments = path.strip_prefix("resource/")?.split('/');
    let publisher = segments.next()?.to_owned();
    let extension = segments.next()?.to_owned();
    let version = segments.next()?.to_owned();
    let resource_path = segments.collect::<Vec<_>>().join("/");
    (!resource_path.is_empty()
        && [publisher.as_str(), extension.as_str(), version.as_str()]
            .into_iter()
            .all(is_safe_resource_segment)
        && is_safe_resource_path(&resource_path))
    .then_some(ResourceRequest {
        publisher,
        extension,
        version,
        path: resource_path,
    })
}

fn parse_extension_request(path: &str) -> Option<ExtensionRequest> {
    let parts = path
        .strip_prefix("extension/")?
        .split('/')
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [publisher, name, "latest"]
            if is_safe_resource_segment(publisher) && is_safe_resource_segment(name) =>
        {
            Some(ExtensionRequest {
                publisher: (*publisher).to_owned(),
                name: (*name).to_owned(),
            })
        }
        _ => None,
    }
}

fn parse_asset_base_request(path: &str) -> Option<AssetBaseRequest> {
    let parts = path
        .strip_prefix("asset-base/")?
        .split('/')
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [asset_base_id, asset_type]
            if asset_base_id.len() == 64
                && asset_base_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                && is_safe_resource_segment(asset_type) =>
        {
            Some(AssetBaseRequest {
                asset_base_id: (*asset_base_id).to_owned(),
                asset_type: (*asset_type).to_owned(),
            })
        }
        _ => None,
    }
}

fn is_safe_resource_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

fn is_safe_resource_path(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\\')
        && !value.contains('%')
        && !value.contains('?')
        && !value.contains('#')
        && value
            .split('/')
            .all(|segment| is_safe_resource_segment(segment) && segment != "." && segment != "..")
}

fn local_asset_url(root_url: &Url, entry_name: &str, asset_id: &str) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/mirror/vscode/{entry_name}/asset/{asset_id}"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn local_asset_base_url(root_url: &Url, entry_name: &str, asset_base_id: &str) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/mirror/vscode/{entry_name}/asset-base/{asset_base_id}"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn append_asset_type(base_url: &str, asset_type: &str) -> Result<Url, VscodeError> {
    let mut url = Url::parse(base_url).map_err(|_| {
        VscodeError::InvalidGalleryResponse("stored asset base URL is invalid".to_owned())
    })?;
    if !is_safe_configured_url(&url) || !is_safe_resource_segment(asset_type) {
        return Err(VscodeError::InvalidGalleryResponse(
            "stored asset base URL is unsafe".to_owned(),
        ));
    }
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/{asset_type}"));
    Ok(url)
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

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_asset_cache_metadata(path: &Path) -> Result<Option<String>, VscodeError> {
    let Some(contents) = read_if_present(path)? else {
        return Ok(None);
    };
    Ok(toml::from_str::<AssetCacheMetadata>(&String::from_utf8_lossy(&contents))?.content_type)
}

fn is_stale_compressed_cache(bytes: &[u8], content_type: Option<&str>) -> bool {
    if !bytes.starts_with(&[0x1f, 0x8b, 0x08]) {
        return false;
    }

    let Some(content_type) = content_type else {
        return true;
    };
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    !matches!(media_type, "application/gzip" | "application/x-gzip")
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("VS Code storage path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("VS Code storage path has no file name"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));
    fs::write(&temporary, contents)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(error)
        }
    }
}

fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

#[cfg(test)]
mod tests {
    use super::is_stale_compressed_cache;

    #[test]
    fn gzip_bytes_with_json_content_type_are_stale() {
        assert!(is_stale_compressed_cache(
            &[0x1f, 0x8b, 0x08],
            Some("application/json")
        ));
    }

    #[test]
    fn gzip_bytes_with_gzip_content_type_are_valid() {
        assert!(!is_stale_compressed_cache(
            &[0x1f, 0x8b, 0x08],
            Some("application/gzip")
        ));
    }
}
