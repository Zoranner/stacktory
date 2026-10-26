use std::{fs, io};

use axum::{
    body::Body,
    http::{
        StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use tokio_util::io::ReaderStream;
use url::Url;

use super::{
    AssetBaseDescriptor, AssetBaseRequest, AssetDescriptor, ExtensionDescriptor, ExtensionRequest,
    MarketplaceAsset, ResourceDescriptor, ResourceRequest, VscodeError, VscodeService,
    cache::{is_stale_compressed_cache_file, publish_asset_file, temporary_asset_path},
    query::append_asset_type,
    sha256,
};
use crate::{
    SourcePool,
    audit::AuditDetails,
    upstream::{FetchOperation, pool::is_safe_configured_url},
};

impl VscodeService {
    pub(super) async fn asset(
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

    pub(super) async fn resource(
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

    pub(super) async fn extension(
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

    pub(super) async fn asset_base(
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
        let cached_metadata = match fs::metadata(&cache_path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(VscodeError::Storage(error)),
        };
        if let Some(metadata) = cached_metadata {
            let content_type = self.read_asset_content_type(source_name, cache_id)?;
            if !is_stale_compressed_cache_file(&cache_path, content_type.as_deref())? {
                let _ = crate::storage::access::note_access(&cache_path);
                let _ = crate::storage::access::note_access(
                    &self.asset_cache_metadata_path(source_name, cache_id),
                );
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
                    path: cache_path,
                    length: metadata.len(),
                    content_type,
                });
            }
        }
        let source_url = Url::parse(source_url).map_err(|_| {
            VscodeError::InvalidGalleryResponse("stored asset URL is invalid".to_owned())
        })?;
        let temporary_path = temporary_asset_path(&self.data_dir)?;
        let fetched = match pool
            .fetch_configured_url_following_safe_redirects_to_file(
                source_name,
                source_url,
                FetchOperation::Artifact,
                temporary_path,
            )
            .await
        {
            Ok(file) => file,
            Err(error) => {
                let Some(fallback_url) = fallback_url else {
                    return Err(error.into());
                };
                let fallback_url = Url::parse(fallback_url).map_err(|_| {
                    VscodeError::InvalidGalleryResponse(
                        "stored fallback asset URL is invalid".to_owned(),
                    )
                })?;
                let temporary_path = temporary_asset_path(&self.data_dir)?;
                pool.fetch_configured_url_following_safe_redirects_to_file(
                    source_name,
                    fallback_url,
                    FetchOperation::Artifact,
                    temporary_path,
                )
                .await?
            }
        };
        let (path, length) = publish_asset_file(&fetched.path, &cache_path)?;
        self.write_asset_cache_metadata(source_name, cache_id, fetched.content_type.clone())?;
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
            path,
            length,
            content_type: fetched.content_type,
        })
    }

    pub(super) fn record(&self, event: &str, details: AuditDetails<'_>) {
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
            crate::logging::error(format!("audit write failed: {error}"));
        }
    }
}

pub(super) async fn asset_response(asset: MarketplaceAsset) -> Response {
    let handle = match tokio::fs::File::open(&asset.path).await {
        Ok(file) => file,
        Err(error) => return super::vscode_error_response(&VscodeError::Storage(error)),
    };
    let mut response =
        (StatusCode::OK, Body::from_stream(ReaderStream::new(handle))).into_response();
    let content_type = asset
        .content_type
        .as_deref()
        .and_then(|value| axum::http::HeaderValue::from_str(value).ok())
        .unwrap_or_else(|| axum::http::HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    if let Ok(length) = axum::http::HeaderValue::from_str(&asset.length.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
}
