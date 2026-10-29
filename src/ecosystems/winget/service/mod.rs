mod artifact;
mod source;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{ACCEPT, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};

use crate::{PublicUrls, SourcePool, audit::AuditSink, upstream::registry::AdapterRequest};

use super::{
    path::{WingetRoute, parse_route, validate_query},
    winget_error_response,
};

#[derive(Debug)]
pub struct WingetService {
    pub(crate) public_urls: PublicUrls,
    pub(crate) data_dir: PathBuf,
    pub(crate) audit: AuditSink,
    pub(crate) installer_hosts: BTreeMap<String, BTreeSet<String>>,
}

impl WingetService {
    pub(crate) async fn handle_request(
        &self,
        entry_name: &str,
        request: AdapterRequest,
        pool: &SourcePool,
    ) -> Response {
        let route = match parse_route(&request.path, &request.raw_path) {
            Ok(route) => route,
            Err(error) => return winget_error_response(&error),
        };
        let result = match route {
            WingetRoute::Information => {
                if request.method != Method::GET {
                    return StatusCode::METHOD_NOT_ALLOWED.into_response();
                }
                self.information(entry_name, pool)
                    .await
                    .map(|body| json_response(StatusCode::OK, body))
            }
            WingetRoute::ManifestSearch => {
                if request.method != Method::POST {
                    return StatusCode::METHOD_NOT_ALLOWED.into_response();
                }
                self.search(entry_name, &request.body, pool)
                    .await
                    .map(|body| json_response(StatusCode::OK, body))
            }
            WingetRoute::PackageManifest { package_id } => {
                if request.method != Method::GET {
                    return StatusCode::METHOD_NOT_ALLOWED.into_response();
                }
                match validate_query(request.query.as_deref()) {
                    Ok(query) => self
                        .manifest(
                            entry_name,
                            &package_id,
                            query.as_deref(),
                            &request.base_url,
                            pool,
                        )
                        .await
                        .map(|body| json_response(StatusCode::OK, body)),
                    Err(error) => Err(error),
                }
            }
            WingetRoute::Installer { token } => {
                if request.method != Method::GET {
                    return StatusCode::METHOD_NOT_ALLOWED.into_response();
                }
                match self
                    .installer(entry_name, &token, &request.base_url, pool)
                    .await
                {
                    Ok(file) => Ok(artifact::installer_response(file).await),
                    Err(error) => Err(error),
                }
            }
        };
        match result {
            Ok(response) => response,
            Err(error) => winget_error_response(&error),
        }
    }
}

pub(super) fn json_response(status: StatusCode, body: Vec<u8>) -> Response {
    let mut response = (status, Body::from(body)).into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

pub(super) fn metadata_headers(include_version: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if include_version {
        headers.insert(
            axum::http::HeaderName::from_static("version"),
            HeaderValue::from_static(super::path::CONTRACT_VERSION),
        );
    }
    headers
}

pub(super) fn search_headers() -> HeaderMap {
    let mut headers = metadata_headers(true);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}
