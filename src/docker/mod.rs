pub mod path;

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::{
    SourcePool,
    sources::registry::{Adapter, AdapterFuture, AdapterRequest},
    sources::{FetchOperation, SourcePoolError},
};

use self::path::{DockerRequest, ManifestReference, parse_request};

#[derive(Debug)]
pub struct DockerAdapter {
    service: Arc<DockerService>,
}

#[derive(Debug)]
pub struct DockerService {
    _root_url: Url,
    data_dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum DockerError {
    #[error(transparent)]
    Path(#[from] path::DockerPathError),
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error("Docker manifest digest header is missing or invalid")]
    MissingDigest,
    #[error("Docker manifest digest mismatch: expected `{expected}`, got `{actual}`")]
    DigestMismatch { expected: String, actual: String },
    #[error("Docker metadata storage failed: {0}")]
    Storage(#[from] io::Error),
    #[error("Docker metadata serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManifestMetadata {
    repository: String,
    reference: String,
    digest: String,
    selected_proxy: String,
    content_type: Option<String>,
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
            data_dir: data_dir.into(),
        }
    }

    async fn handle_request(
        &self,
        entry_name: &str,
        request: AdapterRequest,
        pool: &SourcePool,
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
            Ok(DockerRequest::Manifest {
                repository,
                reference,
            }) => match self
                .manifest(entry_name, &repository, &reference, &request.headers, pool)
                .await
            {
                Ok(response) => response,
                Err(error) => docker_error_response(&error),
            },
            Ok(DockerRequest::Blob { .. }) => StatusCode::NOT_IMPLEMENTED.into_response(),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn manifest(
        &self,
        entry_name: &str,
        repository: &str,
        reference: &ManifestReference,
        request_headers: &HeaderMap,
        pool: &SourcePool,
    ) -> Result<Response, DockerError> {
        let source_name = pool
            .proxy_members(entry_name)
            .and_then(|members| members.into_iter().next())
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?
            .name;
        let request_key = format!("v2/{repository}/manifests/{}", reference.as_str());
        let mut headers = HeaderMap::new();
        if let Some(accept) = request_headers.get(header::ACCEPT) {
            headers.insert(header::ACCEPT, accept.clone());
        }
        let upstream = pool
            .fetch_proxy_path_with_headers(
                &source_name,
                &request_key,
                headers,
                FetchOperation::Metadata,
            )
            .await?;
        let actual_digest = sha256_digest(&upstream.body);
        let expected_digest = match reference {
            ManifestReference::Tag(_) => upstream
                .headers
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
        if let Some(header_digest) = upstream
            .headers
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
        self.write_manifest_metadata(
            entry_name,
            repository,
            reference,
            &actual_digest,
            &source_name,
            upstream.content_type.as_deref(),
        )?;
        let mut response = (StatusCode::OK, Body::from(upstream.body)).into_response();
        if let Some(content_type) = upstream.content_type
            && let Ok(value) = header::HeaderValue::from_str(&content_type)
        {
            response.headers_mut().insert(header::CONTENT_TYPE, value);
        }
        response.headers_mut().insert(
            header::HeaderName::from_static("docker-content-digest"),
            header::HeaderValue::from_str(&actual_digest).expect("SHA-256 digest is valid"),
        );
        Ok(response)
    }

    fn write_manifest_metadata(
        &self,
        entry_name: &str,
        repository: &str,
        reference: &ManifestReference,
        digest: &str,
        selected_proxy: &str,
        content_type: Option<&str>,
    ) -> Result<(), DockerError> {
        let metadata = ManifestMetadata {
            repository: repository.to_owned(),
            reference: reference.as_str().to_owned(),
            digest: digest.to_owned(),
            selected_proxy: selected_proxy.to_owned(),
            content_type: content_type.map(str::to_owned),
        };
        let path = self
            .data_dir
            .join("docker")
            .join("metadata")
            .join(entry_name)
            .join(repository)
            .join("manifests")
            .join(format!("{}.json", metadata_file_key(reference.as_str())));
        atomic_write(&path, &serde_json::to_vec(&metadata)?).map_err(DockerError::Storage)
    }
}

impl ManifestReference {
    fn as_str(&self) -> &str {
        match self {
            Self::Tag(value) | Self::Digest(value) => value,
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

fn docker_error_response(error: &DockerError) -> Response {
    let status = match error {
        DockerError::Path(_) => StatusCode::NOT_FOUND,
        DockerError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        DockerError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        DockerError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        DockerError::Source(_)
        | DockerError::MissingDigest
        | DockerError::DigestMismatch { .. } => StatusCode::BAD_GATEWAY,
        DockerError::Storage(_) | DockerError::Serialization(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    status.into_response()
}

fn sha256_digest(contents: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(contents))
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == "sha256:".len() + 64
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn metadata_file_key(reference: &str) -> String {
    reference.replace(':', "-")
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Docker metadata path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Docker metadata path has no file name"))?;
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
