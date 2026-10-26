use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use super::super::{ArtifactFile, DistPath, ServiceError, StorageError};
use super::RustupService;
use crate::upstream::FetchOperation;

impl RustupService {
    pub async fn artifact_for_request(
        &self,
        entry_name: &str,
        channel: &str,
        path: &DistPath,
    ) -> Result<Vec<u8>, ServiceError> {
        let file = self
            .artifact_file_for_request(entry_name, channel, path)
            .await?;
        fs::read(file.path)
            .map_err(StorageError::Io)
            .map_err(Into::into)
    }

    pub async fn artifact_file_for_request(
        &self,
        entry_name: &str,
        channel: &str,
        path: &DistPath,
    ) -> Result<ArtifactFile, ServiceError> {
        self.ensure_entry(entry_name)?;
        self.ensure_channel(channel)?;
        let snapshot = self.snapshots.current(entry_name, channel)?;
        let artifact_source = snapshot
            .artifact_sources
            .get(path)
            .cloned()
            .ok_or_else(|| ServiceError::UnknownArtifact(path.clone()))?;
        let _artifact_guard = self
            .artifact_locks
            .lock(&format!(
                "{}\0{}",
                artifact_source.source_name,
                path.as_str()
            ))
            .await;
        let snapshot = self.snapshots.current(entry_name, channel)?;
        let artifact_source = snapshot
            .artifact_sources
            .get(path)
            .cloned()
            .ok_or_else(|| ServiceError::UnknownArtifact(path.clone()))?;
        let expected_checksum = artifact_source.checksum;
        let selected_source = artifact_source.source_name;
        if let Some(file) =
            self.cache
                .get_verified_file(&selected_source, path, &expected_checksum)?
        {
            self.record_audit("artifact_cache_hit", path.as_str(), "success");
            return Ok(file);
        }

        let temporary_path = temporary_artifact_path(&self.data_dir).map_err(StorageError::Io)?;
        let fetched = match self
            .sources
            .fetch_proxy_path_to_file(
                &selected_source,
                &format!("dist/{}", path.as_str()),
                FetchOperation::Artifact,
                temporary_path,
            )
            .await
        {
            Ok(file) => file,
            Err(error) => {
                self.record_audit("artifact_fetch_failed", path.as_str(), "upstream_failed");
                return Err(error.into());
            }
        };
        let file = match self.cache.publish_verified_file(
            &selected_source,
            path,
            &fetched.path,
            &expected_checksum,
        ) {
            Ok(file) => file,
            Err(StorageError::ArtifactHashMismatch(_)) => {
                self.record_audit("artifact_fetch_failed", path.as_str(), "hash_mismatch");
                return Err(ServiceError::Storage(StorageError::ArtifactHashMismatch(
                    path.to_string(),
                )));
            }
            Err(error) => return Err(error.into()),
        };
        self.record_audit("artifact_cached", path.as_str(), "success");
        Ok(file)
    }
}

pub(super) fn temporary_artifact_path(data_dir: &Path) -> Result<PathBuf, io::Error> {
    let directory = data_dir.join("tmp");
    fs::create_dir_all(&directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(directory.join(format!("rustup-{}-{nonce}.part", std::process::id())))
}

pub(super) fn cleanup_temporary_artifacts(data_dir: &Path) {
    let directory = data_dir.join("tmp");
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("rustup-") && name.ends_with(".part") {
            let _ = fs::remove_file(entry.path());
        }
    }
}
