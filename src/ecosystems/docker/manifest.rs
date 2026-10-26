use axum::{
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::Value;

use super::*;
use crate::storage::atomic::atomic_write;

pub(super) fn validate_manifest(
    repository: &str,
    reference: &ManifestReference,
    source: ManifestSource<'_>,
    content_type: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
    accept: Option<&str>,
) -> Result<ManifestMetadata, DockerError> {
    let manifest: Value = serde_json::from_slice(body)
        .map_err(|_| DockerError::InvalidManifest("body is not valid JSON".to_owned()))?;
    if !manifest.is_object() {
        return Err(DockerError::InvalidManifest(
            "body is not a JSON object".to_owned(),
        ));
    }
    if let Some(config) = manifest.get("config") {
        validate_blob_descriptor(config, "config")?;
    }
    if let Some(layers) = manifest.get("layers") {
        let layers = layers
            .as_array()
            .ok_or_else(|| DockerError::InvalidManifest("layers is not an array".to_owned()))?;
        for layer in layers {
            validate_blob_descriptor(layer, "layer")?;
        }
    }
    let actual_digest = sha256_digest(body);
    let expected_digest = match reference {
        ManifestReference::Tag(_) => headers
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok())
            .filter(|digest| is_sha256_digest(digest))
            .map(str::to_owned)
            .ok_or(DockerError::MissingDigest)?,
        ManifestReference::Digest(digest) => digest.clone(),
    };
    if expected_digest != actual_digest {
        return Err(DockerError::DigestMismatch {
            expected: expected_digest,
            actual: actual_digest,
        });
    }
    if let Some(header_digest) = headers
        .get("docker-content-digest")
        .and_then(|value| value.to_str().ok())
        .filter(|digest| is_sha256_digest(digest))
        && header_digest != actual_digest
    {
        return Err(DockerError::DigestMismatch {
            expected: header_digest.to_owned(),
            actual: actual_digest,
        });
    }
    let content_type = content_type
        .map(str::to_owned)
        .or_else(|| manifest_media_type(&manifest));
    Ok(ManifestMetadata {
        repository: repository.to_owned(),
        reference: reference.as_str().to_owned(),
        digest: actual_digest,
        selected_proxy: source.name.to_owned(),
        source_fingerprint: source.fingerprint.to_owned(),
        content_type,
        accept: accept.map(str::to_owned),
    })
}

pub(super) fn is_sha256_digest(value: &str) -> bool {
    value.len() == "sha256:".len() + 64
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_blob_descriptor(value: &Value, object_type: &str) -> Result<(), DockerError> {
    let digest = value
        .get("digest")
        .and_then(Value::as_str)
        .filter(|digest| is_sha256_digest(digest))
        .ok_or_else(|| DockerError::InvalidManifest(format!("{object_type} digest is invalid")))?;
    let has_media_type = value
        .get("mediaType")
        .and_then(Value::as_str)
        .is_some_and(|media_type| !media_type.is_empty());
    if !has_media_type {
        return Err(DockerError::InvalidManifest(format!(
            "{object_type} mediaType is missing for {digest}"
        )));
    }
    Ok(())
}

fn manifest_media_type(value: &Value) -> Option<String> {
    value
        .get("mediaType")
        .and_then(Value::as_str)
        .filter(|media_type| !media_type.is_empty())
        .map(str::to_owned)
}

pub(super) fn manifest_object_key(repository: &str, reference: &ManifestReference) -> String {
    format!("{repository}/manifests/{}", reference.as_str())
}

pub(super) fn manifest_object_type(content_type: Option<&str>) -> &'static str {
    let media_type = content_type
        .and_then(|value| value.split(';').next())
        .unwrap_or_default()
        .trim();
    if media_type.contains("manifest.list") || media_type.contains("image.index") {
        "image-index"
    } else {
        "manifest"
    }
}

pub(super) fn manifest_response(
    body: Vec<u8>,
    content_type: Option<&str>,
    digest: &str,
    head: bool,
) -> Response {
    let length = body.len();
    let response_body = if head { Vec::new() } else { body };
    let mut response = (StatusCode::OK, axum::body::Body::from(response_body)).into_response();
    if let Some(content_type) = content_type
        && let Ok(value) = header::HeaderValue::from_str(content_type)
    {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_str(&length.to_string()).expect("content length is valid"),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("docker-content-digest"),
        header::HeaderValue::from_str(digest).expect("SHA-256 digest is valid"),
    );
    response
}

impl DockerService {
    pub(super) async fn manifest(
        &self,
        entry_name: &str,
        repository: &str,
        reference: &ManifestReference,
        request_headers: &HeaderMap,
        pool: &SourcePool,
        head: bool,
    ) -> Result<Response, DockerError> {
        let client_accept = request_headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        // A digest identifies one immutable manifest. A client that sends only
        // the Docker v2 type must not make the upstream reject an OCI
        // manifest, so negotiate the full supported set. For a tag the
        // representation can depend on the client's preference (index vs
        // platform manifest), so preserve it.
        let accept = match reference {
            ManifestReference::Digest(_) => MANIFEST_ACCEPT.to_owned(),
            ManifestReference::Tag(_) => client_accept
                .clone()
                .unwrap_or_else(|| MANIFEST_ACCEPT.to_owned()),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_str(&accept)
                .unwrap_or_else(|_| HeaderValue::from_static(MANIFEST_ACCEPT)),
        );
        let accept = Some(accept);
        let bound = read_manifest_metadata(
            &self.data_dir,
            entry_name,
            repository,
            reference.as_str(),
            accept.as_deref(),
        )?
        .filter(|metadata| {
            source_is_current(
                pool,
                entry_name,
                &metadata.selected_proxy,
                &metadata.source_fingerprint,
            )
        });
        if let Some(metadata) = &bound
            && let Some(body) = read_verified_artifact(
                &self.data_dir,
                &metadata.selected_proxy,
                manifest_object_type(metadata.content_type.as_deref()),
                &metadata.digest,
            )?
        {
            // A manifest response is the client's only source of config and
            // layer descriptors. Re-materialize them even on a cache hit so
            // an older or partially cleaned binding set cannot make the
            // following blob requests fail.
            write_blob_descriptors(
                &self.data_dir,
                entry_name,
                &metadata.repository,
                &metadata.selected_proxy,
                &body,
                true,
            )?;
            self.record(
                "docker_manifest_read",
                DockerAuditDetails {
                    entry_name,
                    object_key: &manifest_object_key(repository, reference),
                    selected_proxy: &metadata.selected_proxy,
                    object_type: manifest_object_type(metadata.content_type.as_deref()),
                    cache_status: "hit",
                    failure_category: None,
                },
            );
            return Ok(manifest_response(
                body,
                metadata.content_type.as_deref(),
                &metadata.digest,
                head,
            ));
        }

        if let ManifestReference::Digest(digest) = reference {
            let mut cached_run = GroupRun::new(pool, entry_name, None)?;
            while let Some(member) = cached_run.next_member() {
                let source_name = member.name;
                if let Some(cached) = read_cached_manifest(&self.data_dir, &source_name, digest)? {
                    let source_fingerprint = pool
                        .source_fingerprint(entry_name, &source_name)
                        .unwrap_or_default();
                    let metadata = ManifestMetadata {
                        repository: repository.to_owned(),
                        reference: digest.clone(),
                        digest: digest.clone(),
                        selected_proxy: source_name,
                        source_fingerprint,
                        content_type: cached.content_type,
                        accept: accept.clone(),
                    };
                    self.write_manifest_metadata(entry_name, &metadata, &cached.body)?;
                    self.record(
                        "docker_manifest_read",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &manifest_object_key(repository, reference),
                            selected_proxy: &metadata.selected_proxy,
                            object_type: manifest_object_type(metadata.content_type.as_deref()),
                            cache_status: "hit",
                            failure_category: None,
                        },
                    );
                    return Ok(manifest_response(
                        cached.body,
                        metadata.content_type.as_deref(),
                        digest,
                        head,
                    ));
                }
            }
        }

        let mut run = GroupRun::new(
            pool,
            entry_name,
            bound.as_ref().map(|value| value.selected_proxy.as_str()),
        )?;
        let request_key = format!("v2/{repository}/manifests/{}", reference.as_str());
        while let Some(member) = run.next_member() {
            let source_name = member.name;
            let source_fingerprint = pool
                .source_fingerprint(entry_name, &source_name)
                .unwrap_or_default();
            match pool
                .fetch_proxy_registry_path(
                    &source_name,
                    &request_key,
                    Method::GET,
                    headers.clone(),
                    FetchOperation::Metadata,
                )
                .await
            {
                Ok(upstream) => match validate_manifest(
                    repository,
                    reference,
                    ManifestSource {
                        name: &source_name,
                        fingerprint: &source_fingerprint,
                    },
                    upstream.content_type.as_deref(),
                    &upstream.headers,
                    &upstream.body,
                    accept.as_deref(),
                ) {
                    Ok(metadata) => {
                        let object_type = manifest_object_type(metadata.content_type.as_deref());
                        write_verified_artifact(
                            &self.data_dir,
                            &metadata.selected_proxy,
                            object_type,
                            &metadata.digest,
                            &upstream.body,
                        )?;
                        self.write_manifest_metadata(entry_name, &metadata, &upstream.body)?;
                        self.record(
                            "docker_manifest_read",
                            DockerAuditDetails {
                                entry_name,
                                object_key: &manifest_object_key(repository, reference),
                                selected_proxy: &metadata.selected_proxy,
                                object_type,
                                cache_status: "miss",
                                failure_category: None,
                            },
                        );
                        return Ok(manifest_response(
                            upstream.body,
                            metadata.content_type.as_deref(),
                            &metadata.digest,
                            head,
                        ));
                    }
                    Err(error) if run.is_group() && is_fallback_validation_error(&error) => {
                        let category = failure_category(&error);
                        self.record(
                            "docker_manifest_fetch_failed",
                            DockerAuditDetails {
                                entry_name,
                                object_key: &manifest_object_key(repository, reference),
                                selected_proxy: &source_name,
                                object_type: "manifest",
                                cache_status: "miss",
                                failure_category: Some(category),
                            },
                        );
                        run.record_failure(&source_name, &error.to_string(), false);
                    }
                    Err(error) => {
                        self.record(
                            "docker_manifest_fetch_failed",
                            DockerAuditDetails {
                                entry_name,
                                object_key: &manifest_object_key(repository, reference),
                                selected_proxy: &source_name,
                                object_type: "manifest",
                                cache_status: "miss",
                                failure_category: Some(failure_category(&error)),
                            },
                        );
                        return Err(error);
                    }
                },
                Err(error) if run.is_group() && is_fallback_source_error(&error) => {
                    run.record_failure(
                        &source_name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    self.record(
                        "docker_manifest_fetch_failed",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &manifest_object_key(repository, reference),
                            selected_proxy: &source_name,
                            object_type: "manifest",
                            cache_status: "miss",
                            failure_category: Some(if source_error_is_not_found(&error) {
                                "not_found"
                            } else {
                                "source_failed"
                            }),
                        },
                    );
                }
                Err(error) => {
                    self.record(
                        "docker_manifest_fetch_failed",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &manifest_object_key(repository, reference),
                            selected_proxy: &source_name,
                            object_type: "manifest",
                            cache_status: "miss",
                            failure_category: Some(if source_error_is_not_found(&error) {
                                "not_found"
                            } else {
                                "source_failed"
                            }),
                        },
                    );
                    return Err(error.into());
                }
            }
        }
        Err(DockerError::Source(run.all_sources_failed(request_key)))
    }

    fn write_manifest_metadata(
        &self,
        entry_name: &str,
        metadata: &ManifestMetadata,
        body: &[u8],
    ) -> Result<(), DockerError> {
        let path = crate::storage::layout::metadata_root(&self.data_dir, "docker")
            .join(entry_name)
            .join(&metadata.repository)
            .join("manifests")
            .join(format!(
                "{}-{}.json",
                metadata_file_key(&metadata.reference),
                representation_file_key(metadata.accept.as_deref())
            ));
        atomic_write(&path, &serde_json::to_vec(&metadata)?).map_err(DockerError::Storage)?;
        write_blob_descriptors(
            &self.data_dir,
            entry_name,
            &metadata.repository,
            &metadata.selected_proxy,
            body,
            false,
        )?;
        Ok(())
    }
}
