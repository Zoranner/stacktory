use std::io::SeekFrom;

use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use super::*;

pub(super) async fn blob_file_response(
    path: &std::path::Path,
    full_length: u64,
    content_type: &str,
    digest: &str,
    requested_range: Option<&str>,
    head: bool,
) -> Result<Response, DockerError> {
    let (status, start, response_length, content_range) = if let Some(range) = requested_range {
        let (start, end) =
            parse_range(range, full_length).ok_or(DockerError::InvalidRange(full_length))?;
        (
            StatusCode::PARTIAL_CONTENT,
            Some(start),
            end - start + 1,
            Some(format!("bytes {start}-{end}/{full_length}")),
        )
    } else {
        (StatusCode::OK, None, full_length, None)
    };
    let body = if head {
        Body::empty()
    } else {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(DockerError::Storage)?;
        if let Some(start) = start {
            let mut file = file;
            file.seek(SeekFrom::Start(start))
                .await
                .map_err(DockerError::Storage)?;
            Body::from_stream(ReaderStream::new(file.take(response_length)))
        } else {
            Body::from_stream(ReaderStream::new(file))
        }
    };
    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(content_type)
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("accept-ranges"),
        header::HeaderValue::from_static("bytes"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from_str(&response_length.to_string())
            .expect("content length is valid"),
    );
    if let Some(content_range) = content_range {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            header::HeaderValue::from_str(&content_range).expect("content range is valid"),
        );
    }
    response.headers_mut().insert(
        header::HeaderName::from_static("docker-content-digest"),
        header::HeaderValue::from_str(digest).expect("SHA-256 digest is valid"),
    );
    Ok(response)
}

fn parse_range(value: &str, length: u64) -> Option<(u64, u64)> {
    if length == 0 || !value.starts_with("bytes=") || value[6..].contains(',') {
        return None;
    }
    let (start, end) = value[6..].split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some((length.saturating_sub(suffix), length - 1));
    }
    let start = start.parse::<u64>().ok()?;
    if start >= length {
        return None;
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().ok()?.min(length - 1)
    };
    (start <= end).then_some((start, end))
}

impl DockerService {
    pub(super) async fn blob(
        &self,
        entry_name: &str,
        repository: &str,
        digest: &str,
        request_headers: &HeaderMap,
        pool: &SourcePool,
        head: bool,
    ) -> Result<Response, DockerError> {
        let _blob_guard = self
            .blob_locks
            .lock(&format!("{repository}\0{digest}"))
            .await;
        let entry_metadata = read_blob_metadata(&self.data_dir, entry_name, repository, digest)?;
        let mut run = GroupRun::new(
            pool,
            entry_name,
            entry_metadata
                .as_ref()
                .map(|metadata| metadata.selected_proxy.as_str()),
        )?;
        let (metadata, cold_miss) = if let Some(metadata) = entry_metadata {
            (metadata, false)
        } else {
            let mut lookup_run = run.clone();
            let mut shared = None;
            while let Some(member) = lookup_run.next_member() {
                if let Some(metadata) =
                    read_shared_blob_metadata(&self.data_dir, &member.name, repository, digest)?
                {
                    shared = Some(metadata);
                    break;
                }
            }
            match shared {
                Some(metadata) => {
                    write_blob_metadata(&self.data_dir, entry_name, repository, &metadata)?;
                    (metadata, false)
                }
                None => (
                    // Docker can request a blob before the platform
                    // manifest descriptor is available (for example right
                    // after a cache clear). Do not turn that race into a
                    // permanent 404; fetch it from the group and refine the
                    // object/content type after the download.
                    BlobMetadata {
                        repository: repository.to_owned(),
                        digest: digest.to_owned(),
                        object_type: "layer".to_owned(),
                        content_type: "application/octet-stream".to_owned(),
                        selected_proxy: String::new(),
                    },
                    true,
                ),
            }
        };
        let cached = if source_is_member(pool, entry_name, &metadata.selected_proxy) {
            read_verified_artifact_path(
                &self.data_dir,
                &metadata.selected_proxy,
                &metadata.object_type,
                digest,
            )
            .await?
        } else {
            None
        };
        if let Some((path, length)) = cached {
            self.record(
                "docker_blob_read",
                DockerAuditDetails {
                    entry_name,
                    object_key: &format!("{repository}/blobs/{digest}"),
                    selected_proxy: &metadata.selected_proxy,
                    object_type: &metadata.object_type,
                    cache_status: "hit",
                    failure_category: None,
                },
            );
            return blob_file_response(
                &path,
                length,
                &metadata.content_type,
                digest,
                request_headers
                    .get(header::RANGE)
                    .and_then(|value| value.to_str().ok()),
                head,
            )
            .await;
        }
        let headers = HeaderMap::new();
        let requested_range = request_headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok());
        let request_key = format!("v2/{repository}/blobs/{digest}");
        while let Some(member) = run.next_member() {
            let source_name = member.name;
            let temporary_path = self.temporary_blob_path();
            match pool
                .fetch_proxy_registry_path_to_file(
                    &source_name,
                    &request_key,
                    Method::GET,
                    headers.clone(),
                    FetchOperation::Artifact,
                    temporary_path,
                )
                .await
            {
                Ok(upstream) => {
                    if upstream.sha256 != digest {
                        let error = DockerError::DigestMismatch {
                            expected: digest.to_owned(),
                            actual: upstream.sha256,
                        };
                        let _ = fs::remove_file(&upstream.path);
                        if run.is_group() {
                            self.record(
                                "docker_blob_fetch_failed",
                                DockerAuditDetails {
                                    entry_name,
                                    object_key: &format!("{repository}/blobs/{digest}"),
                                    selected_proxy: &source_name,
                                    object_type: &metadata.object_type,
                                    cache_status: "miss",
                                    failure_category: Some("digest_mismatch"),
                                },
                            );
                            run.record_failure(&source_name, &error.to_string(), false);
                            continue;
                        }
                        self.record(
                            "docker_blob_fetch_failed",
                            DockerAuditDetails {
                                entry_name,
                                object_key: &format!("{repository}/blobs/{digest}"),
                                selected_proxy: &source_name,
                                object_type: &metadata.object_type,
                                cache_status: "miss",
                                failure_category: Some("digest_mismatch"),
                            },
                        );
                        return Err(error);
                    }
                    let object_type = if cold_miss {
                        if upstream
                            .content_type
                            .as_deref()
                            .is_some_and(|content_type| content_type.contains("config"))
                        {
                            "config"
                        } else {
                            "layer"
                        }
                    } else {
                        metadata.object_type.as_str()
                    };
                    let path = publish_verified_file(
                        &self.data_dir,
                        &source_name,
                        object_type,
                        digest,
                        &upstream.path,
                    )
                    .await?;
                    let mut selected_metadata = metadata.clone();
                    selected_metadata.selected_proxy = source_name.clone();
                    selected_metadata.object_type = object_type.to_owned();
                    if let Some(content_type) = upstream.content_type.clone() {
                        selected_metadata.content_type = content_type;
                    }
                    write_blob_metadata(
                        &self.data_dir,
                        entry_name,
                        repository,
                        &selected_metadata,
                    )?;
                    self.record(
                        "docker_blob_read",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &format!("{repository}/blobs/{digest}"),
                            selected_proxy: &source_name,
                            object_type,
                            cache_status: "miss",
                            failure_category: None,
                        },
                    );
                    return blob_file_response(
                        &path,
                        upstream.length,
                        &selected_metadata.content_type,
                        digest,
                        requested_range,
                        head,
                    )
                    .await;
                }
                Err(error) if run.is_group() && is_fallback_source_error(&error) => {
                    run.record_failure(
                        &source_name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    self.record(
                        "docker_blob_fetch_failed",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &format!("{repository}/blobs/{digest}"),
                            selected_proxy: &source_name,
                            object_type: &metadata.object_type,
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
                        "docker_blob_fetch_failed",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &format!("{repository}/blobs/{digest}"),
                            selected_proxy: &source_name,
                            object_type: &metadata.object_type,
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

    fn temporary_blob_path(&self) -> PathBuf {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.data_dir
            .join("tmp")
            .join(format!("docker-{}-{counter}.part", std::process::id()))
    }
}
