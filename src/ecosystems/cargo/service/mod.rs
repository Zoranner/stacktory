use std::{io, path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;
use url::Url;

use crate::{
    PublicUrls, SourcePool,
    audit::{AuditDetails, AuditSink},
    upstream::{
        SourcePoolError,
        registry::{Adapter, AdapterFuture, AdapterRequest},
    },
};

use super::{
    index::{CargoIndexError, download_template, sparse_path_for_request},
    path::{CargoPath, CargoPathError, normalize_crate_name, validate_version},
};

mod artifact;
mod index;

use artifact::{cleanup_temporary_artifacts, crate_file_response};

#[derive(Debug)]
pub struct CargoAdapter {
    service: Arc<CargoService>,
}

#[derive(Debug)]
pub struct CargoService {
    public_urls: PublicUrls,
    data_dir: PathBuf,
    audit: AuditSink,
}

#[derive(Debug)]
pub(super) struct IndexSnapshot {
    pub(super) body: Vec<u8>,
    pub(super) selected_proxy: String,
}

pub(super) struct FetchedIndex {
    pub(super) body: Vec<u8>,
    pub(super) source_name: String,
}

pub(super) struct CrateFile {
    pub(super) path: PathBuf,
    pub(super) length: u64,
}

enum CargoPayload {
    Bytes(&'static str, Vec<u8>),
    File(CrateFile),
}

#[derive(Debug, Error)]
pub enum CargoError {
    #[error(transparent)]
    Path(#[from] CargoPathError),
    #[error(transparent)]
    Index(#[from] CargoIndexError),
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error("cargo crate `{name}` version `{version}` was not found in the index")]
    MissingVersion { name: String, version: String },
    #[error("cargo storage I/O failed: {0}")]
    Storage(#[from] io::Error),
}

impl CargoAdapter {
    pub fn new(public_urls: impl Into<PublicUrls>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(CargoService::new(public_urls, data_dir)),
        }
    }

    pub fn service(&self) -> &CargoService {
        &self.service
    }
}

impl CargoService {
    pub fn new(public_urls: impl Into<PublicUrls>, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        cleanup_temporary_artifacts(&data_dir);
        Self {
            public_urls: public_urls.into(),
            audit: AuditSink::new(data_dir.clone()),
            data_dir,
        }
    }

    pub fn public_urls(&self) -> &PublicUrls {
        &self.public_urls
    }

    pub async fn handle_request(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
        base_url: &Url,
    ) -> Response {
        match self.serve(entry_name, path, pool, base_url).await {
            Ok(CargoPayload::Bytes(content_type, body)) => {
                response(StatusCode::OK, content_type, body)
            }
            Ok(CargoPayload::File(file)) => crate_file_response(file).await,
            Err(error) => cargo_error_response(&error),
        }
    }

    async fn serve(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
        base_url: &Url,
    ) -> Result<CargoPayload, CargoError> {
        if pool.proxy_members(entry_name).is_none() {
            return Err(SourcePoolError::UnknownEntry(entry_name.to_owned()).into());
        }
        if path == "config.json" {
            return Ok(CargoPayload::Bytes(
                "application/json",
                self.config(entry_name, base_url)?,
            ));
        }
        if let Ok((crate_name, _)) = sparse_path_for_request(path) {
            let body = self
                .index(entry_name, path, &crate_name, pool, base_url)
                .await?;
            return Ok(CargoPayload::Bytes("application/json", body.body));
        }
        if let Some(crate_path) = path.strip_prefix("crates/") {
            let (crate_name, version) = parse_crate_request(crate_path)?;
            let file = self
                .crate_file(entry_name, &crate_name, &version, pool, base_url)
                .await?;
            return Ok(CargoPayload::File(file));
        }
        Err(CargoPathError::Invalid(path.to_owned()).into())
    }

    fn config(&self, entry_name: &str, base_url: &Url) -> Result<Vec<u8>, CargoError> {
        let body = json!({
            "dl": download_template(base_url, entry_name),
        });
        Ok(serde_json::to_vec(&body).expect("static cargo config is serializable"))
    }

    fn metadata_path(&self, entry_name: &str, object_key: &str) -> PathBuf {
        crate::storage::layout::metadata_root(&self.data_dir, "cargo")
            .join(entry_name)
            .join("index")
            .join(object_key)
    }

    fn metadata_sidecar_path(&self, entry_name: &str, object_key: &str) -> PathBuf {
        let path = self.metadata_path(entry_name, object_key);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("metadata path must have a file name");
        path.with_file_name(format!("{file_name}.source"))
    }

    fn record_cargo(
        &self,
        event: &str,
        entry_name: &str,
        object_key: &str,
        details: CargoAuditDetails<'_>,
    ) {
        if let Err(error) = self.audit.record_detailed(
            event,
            object_key,
            if details.failure_category.is_some() {
                "failure"
            } else {
                "success"
            },
            AuditDetails {
                ecosystem: Some("cargo"),
                entry_name: Some(entry_name),
                object_key: Some(object_key),
                selected_proxy: details.selected_proxy,
                object_type: Some(details.object_type),
                cache_status: Some(details.cache_status),
                failure_category: details.failure_category,
                count: None,
            },
        ) {
            crate::logging::error(format!("audit write failed: {error}"));
        }
    }
}

struct CargoAuditDetails<'a> {
    selected_proxy: Option<&'a str>,
    object_type: &'a str,
    cache_status: &'a str,
    failure_category: Option<&'a str>,
}

impl Adapter for CargoAdapter {
    fn ecosystem(&self) -> &str {
        "cargo"
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
        Box::pin(async move {
            if request.method != axum::http::Method::GET {
                return axum::http::StatusCode::METHOD_NOT_ALLOWED.into_response();
            }
            service
                .handle_request(&source_name, &request.path, &pool, &request.base_url)
                .await
        })
    }
}

fn parse_crate_request(path: &str) -> Result<(String, String), CargoPathError> {
    let path = CargoPath::parse(path)?;
    let mut parts = path.as_str().split('/');
    let crate_name = normalize_crate_name(
        parts
            .next()
            .ok_or_else(|| CargoPathError::Invalid(path.to_string()))?,
    )?;
    let version_file = parts
        .next()
        .ok_or_else(|| CargoPathError::Invalid(path.to_string()))?;
    if parts.next().is_some() {
        return Err(CargoPathError::Invalid(path.to_string()));
    }
    let version = version_file
        .strip_suffix(".crate")
        .ok_or_else(|| CargoPathError::Invalid(path.to_string()))?;
    validate_version(version)?;
    Ok((crate_name, version.to_owned()))
}

fn cargo_error_response(error: &CargoError) -> Response {
    let status = match error {
        CargoError::Path(_)
        | CargoError::MissingVersion { .. }
        | CargoError::Index(CargoIndexError::Path(_)) => StatusCode::NOT_FOUND,
        CargoError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        CargoError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        CargoError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        CargoError::Source(_) | CargoError::Index(_) => StatusCode::BAD_GATEWAY,
        CargoError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    (status, [(CONTENT_TYPE, content_type)], Body::from(body)).into_response()
}

fn terminal_status(error: &SourcePoolError) -> Option<u16> {
    match error {
        SourcePoolError::TerminalSourceError { status, .. } => Some(*status),
        SourcePoolError::AllSourcesFailed { status, .. } => *status,
        _ => None,
    }
}

fn is_not_found_cargo_error(error: &CargoError) -> bool {
    matches!(
        error,
        CargoError::Source(SourcePoolError::TerminalSourceError { status: 404, .. })
            | CargoError::Source(SourcePoolError::AllSourcesFailed {
                status: Some(404),
                ..
            })
    )
}
