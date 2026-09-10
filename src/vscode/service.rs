use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use thiserror::Error;
use url::Url;

use crate::{
    SourcePool,
    sources::{
        FetchOperation, SourcePoolError,
        registry::{Adapter, AdapterFuture},
    },
};

use super::path::{VscodePath, VscodePathError};

#[derive(Debug)]
pub struct VscodeAdapter {
    service: Arc<VscodeService>,
}

#[derive(Debug)]
pub struct VscodeService {
    root_url: Url,
    data_dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum VscodeError {
    #[error(transparent)]
    Path(#[from] VscodePathError),
    #[error(transparent)]
    Source(#[from] SourcePoolError),
    #[error("VS Code metadata JSON is invalid: {0}")]
    MetadataJson(#[from] serde_json::Error),
    #[error("VS Code metadata is invalid: {0}")]
    InvalidMetadata(String),
    #[error("VS Code storage I/O failed: {0}")]
    Storage(#[from] io::Error),
    #[error("VSIX file serving is not implemented yet")]
    ArtifactNotImplemented,
}

impl VscodeAdapter {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(VscodeService::new(root_url, data_dir)),
        }
    }

    pub fn service(&self) -> &VscodeService {
        &self.service
    }
}

impl VscodeService {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_url,
            data_dir: data_dir.into(),
        }
    }

    pub async fn handle_request(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
    ) -> Response {
        match self.serve(entry_name, path, pool).await {
            Ok(body) => response(StatusCode::OK, "application/json", body),
            Err(error) => vscode_error_response(&error),
        }
    }

    async fn serve(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, VscodeError> {
        if pool.proxy_members(entry_name).is_none() {
            return Err(SourcePoolError::UnknownEntry(entry_name.to_owned()).into());
        }

        match VscodePath::parse(path)? {
            metadata @ VscodePath::Metadata { .. } => {
                self.metadata(entry_name, metadata, pool).await
            }
            VscodePath::File { .. } => Err(VscodeError::ArtifactNotImplemented),
        }
    }

    async fn metadata(
        &self,
        entry_name: &str,
        path: VscodePath,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, VscodeError> {
        let cache_path = self.metadata_path(entry_name, &path);
        if let Some(contents) = read_if_present(&cache_path)? {
            if validate_cached_metadata(&contents).is_ok() {
                return Ok(contents);
            }
            fs::remove_file(&cache_path)?;
        }

        let response = pool
            .fetch(entry_name, &path.request_key(), FetchOperation::Get)
            .await?;
        let contents = rewrite_metadata(&response.body, &self.root_url, entry_name, &path)?;
        atomic_write(&cache_path, &contents)?;
        atomic_write(
            &self.metadata_sidecar_path(entry_name, &path),
            format!("{}\n", response.source_name).as_bytes(),
        )?;
        Ok(contents)
    }

    fn metadata_path(&self, entry_name: &str, path: &VscodePath) -> PathBuf {
        self.data_dir
            .join("vscode")
            .join("metadata")
            .join(entry_name)
            .join(path.metadata_key())
    }

    fn metadata_sidecar_path(&self, entry_name: &str, path: &VscodePath) -> PathBuf {
        let path = self.metadata_path(entry_name, path);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("VS Code metadata path must have a file name");
        path.with_file_name(format!("{file_name}.source"))
    }
}

impl Adapter for VscodeAdapter {
    fn ecosystem(&self) -> &str {
        "vscode"
    }

    fn handle(&self, source_name: String, path: String, pool: Arc<SourcePool>) -> AdapterFuture {
        let service = self.service.clone();
        Box::pin(async move { service.handle_request(&source_name, &path, &pool).await })
    }
}

fn rewrite_metadata(
    body: &[u8],
    root_url: &Url,
    entry_name: &str,
    path: &VscodePath,
) -> Result<Vec<u8>, VscodeError> {
    let mut metadata: Value = serde_json::from_slice(body)?;
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata must be a JSON object".to_owned()))?;
    let publisher = path.publisher();
    let extension = path.extension();
    if object.get("namespace").and_then(Value::as_str) != Some(publisher)
        || object.get("name").and_then(Value::as_str) != Some(extension)
    {
        return Err(VscodeError::InvalidMetadata(
            "metadata namespace/name does not match the request".to_owned(),
        ));
    }

    let version = object
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata version is missing".to_owned()))?
        .to_owned();
    if path.version().is_some_and(|requested| requested != version) {
        return Err(VscodeError::InvalidMetadata(
            "metadata version does not match the request".to_owned(),
        ));
    }
    semver::Version::parse(&version)
        .map_err(|_| VscodeError::InvalidMetadata("metadata version is invalid".to_owned()))?;

    let files = object
        .get_mut("files")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata files are missing".to_owned()))?;
    let download = files
        .get("download")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VscodeError::InvalidMetadata("metadata download URL is missing".to_owned())
        })?;
    let expected_filename = format!("{publisher}.{extension}-{version}.vsix");
    validate_download_url(download, &expected_filename)?;
    let checksum = files
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata SHA-256 is missing".to_owned()))?;
    validate_checksum(checksum)?;

    files.insert(
        "download".to_owned(),
        Value::String(local_download_url(
            root_url,
            entry_name,
            publisher,
            extension,
            &version,
            &expected_filename,
        )),
    );
    serde_json::to_vec(&metadata).map_err(VscodeError::MetadataJson)
}

fn validate_cached_metadata(body: &[u8]) -> Result<(), VscodeError> {
    let metadata: Value = serde_json::from_slice(body)?;
    let files = metadata
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata files are missing".to_owned()))?;
    let download = files
        .get("download")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VscodeError::InvalidMetadata("metadata download URL is missing".to_owned())
        })?;
    let checksum = files
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| VscodeError::InvalidMetadata("metadata SHA-256 is missing".to_owned()))?;
    Url::parse(download)
        .map_err(|_| VscodeError::InvalidMetadata("cached download URL is invalid".to_owned()))?;
    validate_checksum(checksum)
}

fn validate_download_url(value: &str, expected_filename: &str) -> Result<(), VscodeError> {
    let url = Url::parse(value)
        .map_err(|_| VscodeError::InvalidMetadata("metadata download URL is invalid".to_owned()))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().ends_with(&format!("/file/{expected_filename}"))
    {
        return Err(VscodeError::InvalidMetadata(
            "metadata download URL is outside the Open VSX-compatible file path".to_owned(),
        ));
    }
    Ok(())
}

fn validate_checksum(value: &str) -> Result<(), VscodeError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(VscodeError::InvalidMetadata(
            "metadata SHA-256 must be 64 hexadecimal characters".to_owned(),
        ))
    }
}

fn local_download_url(
    root_url: &Url,
    entry_name: &str,
    publisher: &str,
    extension: &str,
    version: &str,
    filename: &str,
) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/mirror/vscode/{entry_name}/api/{publisher}/{extension}/{version}/file/{filename}"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn vscode_error_response(error: &VscodeError) -> Response {
    let status = match error {
        VscodeError::Path(_) => StatusCode::NOT_FOUND,
        VscodeError::Source(SourcePoolError::UnknownEntry(_)) => StatusCode::NOT_FOUND,
        VscodeError::Source(SourcePoolError::TerminalSourceError { status, .. }) => {
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        VscodeError::Source(SourcePoolError::AllSourcesFailed {
            status: Some(status),
            ..
        }) => StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
        VscodeError::Source(_) | VscodeError::MetadataJson(_) | VscodeError::InvalidMetadata(_) => {
            StatusCode::BAD_GATEWAY
        }
        VscodeError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        VscodeError::ArtifactNotImplemented => StatusCode::NOT_FOUND,
    };
    status.into_response()
}

fn response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    (status, [(CONTENT_TYPE, content_type)], Body::from(body)).into_response()
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), io::Error> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("VS Code cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("VS Code cache path has no file name"))?;
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

trait VscodePathParts {
    fn publisher(&self) -> &str;
    fn extension(&self) -> &str;
    fn version(&self) -> Option<&str>;
}

impl VscodePathParts for VscodePath {
    fn publisher(&self) -> &str {
        match self {
            VscodePath::Metadata { publisher, .. } | VscodePath::File { publisher, .. } => {
                publisher
            }
        }
    }

    fn extension(&self) -> &str {
        match self {
            VscodePath::Metadata { extension, .. } | VscodePath::File { extension, .. } => {
                extension
            }
        }
    }

    fn version(&self) -> Option<&str> {
        match self {
            VscodePath::Metadata { version, .. } => version.as_deref(),
            VscodePath::File { version, .. } => Some(version),
        }
    }
}
