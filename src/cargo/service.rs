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
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

use crate::{
    SourcePool,
    sources::registry::{Adapter, AdapterFuture},
    sources::{FetchOperation, SourcePoolError},
};

use super::{
    index::{
        CargoIndexError, download_template, parse_index_lines, rewrite_index_lines,
        sparse_path_for_request,
    },
    path::{CargoPath, CargoPathError, normalize_crate_name, validate_version},
};

#[derive(Debug)]
pub struct CargoAdapter {
    service: Arc<CargoService>,
}

#[derive(Debug)]
pub struct CargoService {
    root_url: Url,
    data_dir: PathBuf,
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
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(CargoService::new(root_url, data_dir)),
        }
    }

    pub fn service(&self) -> &CargoService {
        &self.service
    }
}

impl CargoService {
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
            Ok((content_type, body)) => response(StatusCode::OK, content_type, body),
            Err(error) => cargo_error_response(&error),
        }
    }

    async fn serve(
        &self,
        entry_name: &str,
        path: &str,
        pool: &SourcePool,
    ) -> Result<(&'static str, Vec<u8>), CargoError> {
        if pool.proxy_members(entry_name).is_none() {
            return Err(SourcePoolError::UnknownEntry(entry_name.to_owned()).into());
        }
        if path == "config.json" {
            return Ok(("application/json", self.config(entry_name)?));
        }
        if let Ok((crate_name, _)) = sparse_path_for_request(path) {
            let body = self.index(entry_name, path, &crate_name, pool).await?;
            return Ok(("application/json", body));
        }
        if let Some(crate_path) = path.strip_prefix("crates/") {
            let (crate_name, version) = parse_crate_request(crate_path)?;
            let body = self
                .crate_file(entry_name, &crate_name, &version, pool)
                .await?;
            return Ok(("application/octet-stream", body));
        }
        Err(CargoPathError::Invalid(path.to_owned()).into())
    }

    fn config(&self, entry_name: &str) -> Result<Vec<u8>, CargoError> {
        let body = json!({
            "dl": download_template(&self.root_url, entry_name),
        });
        Ok(serde_json::to_vec(&body).expect("static cargo config is serializable"))
    }

    async fn index(
        &self,
        entry_name: &str,
        index_path: &str,
        crate_name: &str,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, CargoError> {
        let cache_path = self.metadata_path(entry_name, index_path);
        if let Some(contents) = read_if_present(&cache_path)? {
            let valid_cache = std::str::from_utf8(&contents)
                .map_err(|_| CargoIndexError::InvalidRecord {
                    line: 0,
                    error: "index is not UTF-8".to_owned(),
                })
                .and_then(|contents| parse_index_lines(contents).map(|_| ()));
            if valid_cache.is_ok() {
                return Ok(contents);
            }
            fs::remove_file(&cache_path)?;
        }

        let request_key = index_path.to_owned();
        let mut failures = Vec::new();
        for member in pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?
        {
            match pool
                .fetch(&member.name, &request_key, FetchOperation::Get)
                .await
            {
                Ok(response) => {
                    let source = std::str::from_utf8(&response.body).map_err(|_| {
                        CargoIndexError::InvalidRecord {
                            line: 0,
                            error: "index is not UTF-8".to_owned(),
                        }
                    });
                    match source
                        .and_then(|source| rewrite_index_lines(source, &self.root_url, entry_name))
                    {
                        Ok(contents) => {
                            let records =
                                parse_index_lines(std::str::from_utf8(&contents).map_err(
                                    |_| CargoIndexError::InvalidRecord {
                                        line: 0,
                                        error: "index is not UTF-8".to_owned(),
                                    },
                                )?)?;
                            if !records.iter().any(|record| {
                                normalize_crate_name(&record.name)
                                    .is_ok_and(|name| name == crate_name)
                            }) {
                                failures.push(format!(
                                    "{}: index does not contain the requested crate",
                                    member.name
                                ));
                                continue;
                            }
                            atomic_write(&cache_path, &contents)?;
                            return Ok(contents);
                        }
                        Err(error) => failures.push(format!("{}: {error}", member.name)),
                    }
                }
                Err(error) if is_retryable(&error) => {
                    failures.push(format!("{}: {error}", member.name));
                }
                Err(error) => return Err(error.into()),
            }
        }

        Err(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key,
            failures: failures.join("; "),
        }
        .into())
    }

    async fn crate_file(
        &self,
        entry_name: &str,
        crate_name: &str,
        version: &str,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, CargoError> {
        let index_path = super::path::sparse_index_path(crate_name)?;
        let index = self
            .index(entry_name, &index_path, crate_name, pool)
            .await?;
        let records = parse_index_lines(std::str::from_utf8(&index).map_err(|_| {
            CargoIndexError::InvalidRecord {
                line: 0,
                error: "index is not UTF-8".to_owned(),
            }
        })?)?;
        let expected = records
            .iter()
            .find(|record| {
                normalize_crate_name(&record.name).is_ok_and(|name| name == crate_name)
                    && record.version == version
            })
            .map(|record| record.checksum.clone())
            .ok_or_else(|| CargoError::MissingVersion {
                name: crate_name.to_owned(),
                version: version.to_owned(),
            })?;

        let cache_path = self.artifact_path(entry_name, crate_name, version);
        if let Some(contents) = read_if_present(&cache_path)? {
            if checksum(&contents).eq_ignore_ascii_case(&expected) {
                return Ok(contents);
            }
            fs::remove_file(&cache_path)?;
        }

        let request_key = format!("crates/{crate_name}/{version}.crate");
        let mut failures = Vec::new();
        for member in pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?
        {
            match pool
                .fetch(&member.name, &request_key, FetchOperation::Get)
                .await
            {
                Ok(response) if checksum(&response.body).eq_ignore_ascii_case(&expected) => {
                    atomic_write(&cache_path, &response.body)?;
                    return Ok(response.body);
                }
                Ok(_) => failures.push(format!(
                    "{}: checksum mismatch for {crate_name} {version}",
                    member.name
                )),
                Err(error) if is_retryable(&error) => {
                    failures.push(format!("{}: {error}", member.name));
                }
                Err(error) => return Err(error.into()),
            }
        }

        Err(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key,
            failures: failures.join("; "),
        }
        .into())
    }

    fn metadata_path(&self, entry_name: &str, object_key: &str) -> PathBuf {
        self.data_dir
            .join("cargo")
            .join("metadata")
            .join(entry_name)
            .join("index")
            .join(object_key)
    }

    fn artifact_path(&self, entry_name: &str, crate_name: &str, version: &str) -> PathBuf {
        self.data_dir
            .join("cargo")
            .join("artifacts")
            .join(entry_name)
            .join("crates")
            .join(crate_name)
            .join(format!("{version}.crate"))
    }
}

impl Adapter for CargoAdapter {
    fn ecosystem(&self) -> &str {
        "cargo"
    }

    fn handle(&self, source_name: String, path: String, pool: Arc<SourcePool>) -> AdapterFuture {
        let service = self.service.clone();
        Box::pin(async move { service.handle_request(&source_name, &path, &pool).await })
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
        CargoError::Source(_) | CargoError::Index(_) => StatusCode::BAD_GATEWAY,
        CargoError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    (status, [(CONTENT_TYPE, content_type)], Body::from(body)).into_response()
}

fn is_retryable(error: &SourcePoolError) -> bool {
    matches!(error, SourcePoolError::AllSourcesFailed { .. })
}

fn checksum(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
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
        .ok_or_else(|| io::Error::other("cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("cache path has no file name"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));
    fs::write(&temporary, contents)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}
