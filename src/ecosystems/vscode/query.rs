use serde_json::Value;
use url::Url;

use super::{
    AssetBaseDescriptor, AssetBaseRequest, AssetDescriptor, ExtensionDescriptor, ExtensionRequest,
    ResourceDescriptor, ResourceRequest, RewrittenGalleryQuery, VscodeError, VscodeService, sha256,
};
use crate::{
    SourcePool,
    audit::AuditDetails,
    upstream::{
        GroupRun, pool::is_safe_configured_url, source_error_is_group_fallback,
        source_error_is_not_found,
    },
};

impl VscodeService {
    pub(super) async fn query(
        &self,
        entry_name: &str,
        request_body: &[u8],
        pool: &SourcePool,
    ) -> Result<Vec<u8>, VscodeError> {
        serde_json::from_slice::<Value>(request_body)?;
        let mut run = GroupRun::new(pool, entry_name, None)?;
        let is_group = run.is_group();
        while let Some(member) = run.next_member() {
            let response = match pool
                .post_json_to_source(&member.name, "extensionquery", request_body)
                .await
            {
                Ok(response) => response,
                Err(error) if is_group && source_error_is_group_fallback(&error) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let rewritten = match rewrite_gallery_query(
                &response.body,
                &self.root_url,
                entry_name,
                &response.source_name,
            ) {
                Ok(rewritten) => rewritten,
                Err(error @ VscodeError::InvalidGalleryResponse(_)) if is_group => {
                    run.record_failure(&member.name, &error.to_string(), false);
                    continue;
                }
                Err(error) => return Err(error),
            };
            for (asset_id, descriptor) in rewritten.asset_descriptors {
                self.write_descriptor(
                    &self.asset_descriptor_path(entry_name, &asset_id),
                    &descriptor,
                )?;
            }
            for (key, descriptor) in rewritten.resource_descriptors {
                self.write_descriptor(
                    &self.resource_descriptor_path(entry_name, &key),
                    &descriptor,
                )?;
            }
            for (key, descriptor) in rewritten.extension_descriptors {
                self.write_descriptor(
                    &self.extension_descriptor_path(entry_name, &key),
                    &descriptor,
                )?;
            }
            for (key, descriptor) in rewritten.asset_base_descriptors {
                self.write_descriptor(
                    &self.asset_base_descriptor_path(entry_name, &key),
                    &descriptor,
                )?;
            }
            crate::storage::atomic::atomic_write(
                &self.query_path(entry_name, request_body),
                &rewritten.body,
            )?;
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
            return Ok(rewritten.body);
        }
        Err(VscodeError::Source(
            crate::upstream::SourcePoolError::AllSourcesFailed {
                entry_name: entry_name.to_owned(),
                request_key: "extensionquery".to_owned(),
                failures: run.failure_summary(),
                status: (run.has_failures() && run.all_not_found()).then_some(404),
            },
        ))
    }
}

fn rewrite_gallery_query(
    source: &[u8],
    root_url: &Url,
    entry_name: &str,
    source_name: &str,
) -> Result<RewrittenGalleryQuery, VscodeError> {
    let mut query: Value = serde_json::from_slice(source).map_err(|error| {
        VscodeError::InvalidGalleryResponse(format!("response JSON is invalid: {error}"))
    })?;
    let results = query
        .get_mut("results")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| VscodeError::InvalidGalleryResponse("results is missing".to_owned()))?;
    let mut asset_descriptors = std::collections::BTreeMap::new();
    let mut resource_descriptors = std::collections::BTreeMap::new();
    let mut extension_descriptors = std::collections::BTreeMap::new();
    let mut asset_base_descriptors = std::collections::BTreeMap::new();
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
        .map_err(|error| {
            VscodeError::InvalidGalleryResponse(format!("rewritten response is invalid: {error}"))
        })
}

pub(super) fn parse_asset_id(path: &str) -> Option<String> {
    let asset_id = path.strip_prefix("asset/")?;
    (asset_id.len() == 64
        && asset_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !asset_id.contains('/'))
    .then_some(asset_id.to_owned())
}

pub(super) fn parse_resource_request(path: &str) -> Option<ResourceRequest> {
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

pub(super) fn parse_extension_request(path: &str) -> Option<ExtensionRequest> {
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

pub(super) fn parse_asset_base_request(path: &str) -> Option<AssetBaseRequest> {
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

pub(super) fn append_asset_type(base_url: &str, asset_type: &str) -> Result<Url, VscodeError> {
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
