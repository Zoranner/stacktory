//! Winget REST source 只读适配器。

mod cache;
mod manifest;
mod path;
mod service;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::PathBuf,
    sync::Arc,
};

use axum::http::StatusCode;
use thiserror::Error;

use crate::{
    PublicUrls, SourcePool,
    audit::{AuditDetails, AuditError, AuditSink},
    upstream::{
        SourcePoolError,
        registry::{Adapter, AdapterFuture, AdapterRequest},
        source_error_is_group_fallback, source_failure_category,
    },
};

pub use service::WingetService;

#[derive(Debug)]
pub struct WingetAdapter {
    service: Arc<WingetService>,
}

#[derive(Debug, Error)]
pub enum WingetError {
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error("Winget path is invalid: {0}")]
    InvalidPath(String),
    #[error("Winget request is invalid: {0}")]
    InvalidRequest(String),
    #[error("Winget information response is invalid: {0}")]
    InvalidInformation(String),
    #[error("Winget search response is invalid: {0}")]
    InvalidSearch(String),
    #[error("Winget manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("Winget package `{package_id}` version `{version}` was not found")]
    VersionNotFound { package_id: String, version: String },
    #[error("Winget installer host is not allowed: {0}")]
    InstallerPolicy(String),
    #[error("Winget installer is not indexed: {0}")]
    InstallerNotIndexed(String),
    #[error("Winget installer integrity check failed: {0}")]
    InstallerIntegrityFailure(String),
    #[error("Winget source authentication is not supported: {0}")]
    UnsupportedAuthentication(String),
    #[error("Winget binding is invalid: {0}")]
    InvalidBinding(String),
    #[error("Winget storage I/O failed: {0}")]
    Storage(#[from] io::Error),
    #[error("Winget JSON serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Winget audit storage failed: {0}")]
    Audit(#[from] AuditError),
}

pub(crate) struct WingetAuditDetails<'a> {
    pub(crate) object_key: &'a str,
    pub(crate) entry_name: &'a str,
    pub(crate) selected_proxy: &'a str,
    pub(crate) object_type: &'a str,
    pub(crate) cache_status: &'a str,
    pub(crate) failure_category: Option<&'a str>,
}

impl WingetAdapter {
    pub fn new(
        public_urls: impl Into<PublicUrls>,
        data_dir: impl Into<PathBuf>,
        installer_hosts: BTreeMap<String, BTreeSet<String>>,
    ) -> Self {
        Self {
            service: Arc::new(WingetService::new(public_urls, data_dir, installer_hosts)),
        }
    }

    pub fn service(&self) -> &WingetService {
        &self.service
    }
}

impl WingetService {
    pub fn new(
        public_urls: impl Into<PublicUrls>,
        data_dir: impl Into<PathBuf>,
        installer_hosts: BTreeMap<String, BTreeSet<String>>,
    ) -> Self {
        let data_dir = data_dir.into();
        cleanup_temporary_files(&data_dir);
        Self {
            public_urls: public_urls.into(),
            audit: AuditSink::new(data_dir.clone()),
            data_dir,
            installer_hosts,
        }
    }

    pub fn public_urls(&self) -> &PublicUrls {
        &self.public_urls
    }

    pub(crate) fn installer_hosts(&self, source_name: &str) -> Option<&BTreeSet<String>> {
        self.installer_hosts.get(source_name)
    }

    pub(crate) fn record_object(
        &self,
        event: &str,
        details: WingetAuditDetails<'_>,
    ) -> Result<(), WingetError> {
        self.audit.record_detailed(
            event,
            details.object_key,
            if details.failure_category.is_some() {
                "failed"
            } else {
                "success"
            },
            AuditDetails {
                ecosystem: Some("winget"),
                entry_name: Some(details.entry_name),
                object_key: Some(details.object_key),
                selected_proxy: (!details.selected_proxy.is_empty())
                    .then_some(details.selected_proxy),
                object_type: Some(details.object_type),
                cache_status: Some(details.cache_status),
                failure_category: details.failure_category,
                count: None,
            },
        )?;
        Ok(())
    }

    pub(crate) fn record_source_failure(
        &self,
        event: &str,
        object_key: &str,
        entry_name: &str,
        source_name: &str,
        object_type: &str,
        error: &WingetError,
    ) -> Result<(), WingetError> {
        self.record_object(
            event,
            WingetAuditDetails {
                object_key,
                entry_name,
                selected_proxy: source_name,
                object_type,
                cache_status: "failed",
                failure_category: Some(failure_category(error)),
            },
        )
    }
}

pub(crate) fn failure_category(error: &WingetError) -> &'static str {
    match error {
        WingetError::Source(error) => source_failure_category(error),
        WingetError::InvalidPath(_) | WingetError::InstallerNotIndexed(_) => "not_found",
        WingetError::VersionNotFound { .. } => "not_found",
        WingetError::InvalidRequest(_) => "invalid_request",
        WingetError::InvalidInformation(_) => "invalid_information",
        WingetError::InvalidSearch(_) => "invalid_search",
        WingetError::InvalidManifest(_) => "invalid_manifest",
        WingetError::InstallerPolicy(_) => "installer_policy",
        WingetError::InstallerIntegrityFailure(_) => "integrity_mismatch",
        WingetError::UnsupportedAuthentication(_) => "authentication_required",
        WingetError::InvalidBinding(_) => "invalid_binding",
        WingetError::Storage(_) => "storage_failed",
        WingetError::Serialization(_) => "serialization_failed",
        WingetError::Audit(_) => "audit_failed",
    }
}

impl Adapter for WingetAdapter {
    fn ecosystem(&self) -> &str {
        "winget"
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
        Box::pin(async move { service.handle_request(&source_name, request, &pool).await })
    }
}

pub(crate) fn group_fallback_source(
    error: &SourcePoolError,
    pool: &SourcePool,
    entry_name: &str,
) -> bool {
    pool.is_group(entry_name) && source_error_is_group_fallback(error)
}

pub(crate) fn winget_error_response(error: &WingetError) -> axum::response::Response {
    let status = match error {
        WingetError::InvalidPath(_)
        | WingetError::InstallerNotIndexed(_)
        | WingetError::VersionNotFound { .. } => StatusCode::NOT_FOUND,
        WingetError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        WingetError::Source(SourcePoolError::TerminalSourceError { status: 404, .. })
        | WingetError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(404), ..
        }) => StatusCode::NOT_FOUND,
        WingetError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
        WingetError::InvalidInformation(_)
        | WingetError::InvalidSearch(_)
        | WingetError::InvalidManifest(_)
        | WingetError::InstallerPolicy(_)
        | WingetError::InstallerIntegrityFailure(_)
        | WingetError::UnsupportedAuthentication(_)
        | WingetError::Source(_) => StatusCode::BAD_GATEWAY,
        WingetError::InvalidBinding(_)
        | WingetError::Storage(_)
        | WingetError::Serialization(_)
        | WingetError::Audit(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn cleanup_temporary_files(data_dir: &std::path::Path) {
    let directory = data_dir.join("tmp");
    let _ = fs::create_dir_all(&directory);
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("winget-") && name.ends_with(".part") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

use axum::response::IntoResponse;
