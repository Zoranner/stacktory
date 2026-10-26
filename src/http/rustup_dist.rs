//! Rustup 分发入口：manifest、checksum 和制品下载。

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
use tokio_util::io::ReaderStream;

use crate::{
    ecosystems::rustup::{ArtifactFile, DistPath, RustupService, ServiceError, StorageError},
    upstream::SourcePoolError,
};

pub(super) async fn dist(
    State(service): State<Arc<RustupService>>,
    Path((name, path)): Path<(String, String)>,
) -> impl IntoResponse {
    let Some(path) = path.strip_prefix("dist/") else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Some((channel, checksum)) = channel_request(path) {
        return match service.manifest_for_request(&name, channel).await {
            Ok(snapshot) if checksum => {
                format!("{}  channel-rust-{channel}.toml\n", snapshot.checksum).into_response()
            }
            Ok(snapshot) => snapshot.manifest.into_response(),
            Err(error) => service_error_response(&error),
        };
    }

    let Some((channel, artifact_path)) = path.split_once('/') else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = match DistPath::parse(artifact_path) {
        Ok(path) => path,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    match service
        .artifact_file_for_request(&name, channel, &path)
        .await
    {
        Ok(file) => artifact_response(file).await,
        Err(error) => service_error_response(&error),
    }
}

async fn artifact_response(file: ArtifactFile) -> axum::response::Response {
    let handle = match tokio::fs::File::open(&file.path).await {
        Ok(handle) => handle,
        Err(error) => {
            return service_error_response(&ServiceError::Storage(StorageError::Io(error)));
        }
    };
    let mut response =
        (StatusCode::OK, Body::from_stream(ReaderStream::new(handle))).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(length) = axum::http::HeaderValue::from_str(&file.length.to_string()) {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, length);
    }
    response
}

fn service_error_response(error: &ServiceError) -> axum::response::Response {
    let status = match error {
        ServiceError::UnconfiguredChannel(_) | ServiceError::UnknownArtifact(_) => {
            StatusCode::NOT_FOUND
        }
        ServiceError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        ServiceError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        ServiceError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        ServiceError::Storage(StorageError::MissingSnapshot(_)) => StatusCode::SERVICE_UNAVAILABLE,
        ServiceError::Source(_)
        | ServiceError::Rewrite(_)
        | ServiceError::Storage(StorageError::ArtifactHashMismatch(_)) => StatusCode::BAD_GATEWAY,
        ServiceError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn channel_request(path: &str) -> Option<(&str, bool)> {
    let value = path.strip_prefix("channel-rust-")?;
    if let Some(channel) = value.strip_suffix(".toml.sha256") {
        return Some((channel, true));
    }
    value.strip_suffix(".toml").map(|channel| (channel, false))
}
