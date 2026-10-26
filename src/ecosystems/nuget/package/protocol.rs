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

use super::super::service_index::NugetError;
use super::PackageFile;

pub(super) fn parse_package_path(path: &str) -> Result<(String, String), NugetError> {
    let relative = path
        .strip_prefix("package/")
        .ok_or_else(|| NugetError::InvalidPackage("package path is invalid".to_owned()))?;
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.len() != 3
        || parts[..2]
            .iter()
            .any(|part| !is_safe_segment(part) || matches!(*part, "." | ".."))
    {
        return Err(NugetError::InvalidPackage(
            "package path contains unsafe segments".to_owned(),
        ));
    }
    let package_id = parts[0].to_owned();
    let version = parts[1].to_owned();
    let expected_file = format!(
        "{}.{}.nupkg",
        package_id.to_ascii_lowercase(),
        version.to_ascii_lowercase()
    );
    if !parts[2].eq_ignore_ascii_case(&expected_file) {
        return Err(NugetError::InvalidPackage(
            "package filename does not match ID and version".to_owned(),
        ));
    }
    Ok((package_id, version))
}

pub(super) fn parse_package_index_path(path: &str) -> Result<String, NugetError> {
    let relative = path
        .strip_prefix("package/")
        .ok_or_else(|| NugetError::InvalidPackage("package path is invalid".to_owned()))?;
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || !is_safe_segment(parts[0])
        || matches!(parts[0], "." | "..")
        || parts[1] != "index.json"
    {
        return Err(NugetError::InvalidPackage(
            "package version index path is invalid".to_owned(),
        ));
    }
    Ok(parts[0].to_ascii_lowercase())
}

pub(super) fn valid_package_index(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("versions")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .is_some_and(|versions| {
            !versions.is_empty()
                && versions
                    .iter()
                    .all(|version| version.as_str().is_some_and(is_safe_segment))
        })
}

pub(super) fn append_path(base_url: &Url, relative: &str) -> Url {
    let mut url = base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/{relative}"));
    url
}

fn is_safe_segment(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('%')
        && !value.contains('\\')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

pub(super) async fn package_file_response(file: PackageFile) -> Response {
    let handle = match tokio::fs::File::open(&file.path).await {
        Ok(handle) => handle,
        Err(error) => return super::package_error_response(&NugetError::Storage(error)),
    };
    let mut response =
        (StatusCode::OK, Body::from_stream(ReaderStream::new(handle))).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(length) = axum::http::HeaderValue::from_str(&file.length.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
}

pub(super) fn response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response {
    let mut response = (status, Body::from(body)).into_response();
    let content_type = axum::http::HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response
}
