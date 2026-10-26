use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    ArtifactFile, NpmCacheKey, NpmStorage, StorageError,
    integrity::{IntegrityExpectation, verify_contents, verify_file},
    safe_component, validate_artifact_key, validate_key,
};
use crate::storage::atomic::atomic_write;

impl NpmStorage {
    pub fn publish_verified_artifact(
        &self,
        key: &NpmCacheKey,
        artifact_key: &str,
        expectation: &IntegrityExpectation,
        contents: &[u8],
    ) -> Result<super::VerifiedArtifact, StorageError> {
        validate_key(key)?;
        if key.ecosystem != self.ecosystem {
            return Err(StorageError::InvalidIdentity(key.ecosystem.clone()));
        }
        validate_artifact_key(artifact_key)?;
        let verification = verify_contents(artifact_key, expectation, contents)?;
        let result = super::VerifiedArtifact {
            local_sha256: verification.local_sha256,
            upstream_verified: verification.upstream_verified,
        };
        atomic_write(&self.artifact_path(key, artifact_key), contents)?;
        Ok(result)
    }

    pub fn publish_verified_artifact_file(
        &self,
        key: &NpmCacheKey,
        artifact_key: &str,
        expectation: &IntegrityExpectation,
        temporary_path: &Path,
    ) -> Result<ArtifactFile, StorageError> {
        validate_key(key)?;
        if key.ecosystem != self.ecosystem {
            return Err(StorageError::InvalidIdentity(key.ecosystem.clone()));
        }
        validate_artifact_key(artifact_key)?;
        if let Err(error) = verify_file(artifact_key, expectation, temporary_path) {
            let _ = fs::remove_file(temporary_path);
            return Err(error);
        }
        let destination = self.artifact_path(key, artifact_key);
        let parent = destination
            .parent()
            .ok_or_else(|| io::Error::other("npm artifact cache has no parent directory"))?;
        if let Err(error) =
            fs::create_dir_all(parent).and_then(|()| fs::rename(temporary_path, &destination))
        {
            let _ = fs::remove_file(temporary_path);
            return Err(StorageError::Io(error));
        }
        let length = fs::metadata(&destination)?.len();
        Ok(ArtifactFile {
            path: destination,
            length,
        })
    }

    pub fn read_verified_artifact(
        &self,
        key: &NpmCacheKey,
        artifact_key: &str,
        expectation: &IntegrityExpectation,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(file) = self.read_verified_artifact_file(key, artifact_key, expectation)? else {
            return Ok(None);
        };
        fs::read(file.path).map(Some).map_err(StorageError::Io)
    }

    pub fn read_verified_artifact_file(
        &self,
        key: &NpmCacheKey,
        artifact_key: &str,
        expectation: &IntegrityExpectation,
    ) -> Result<Option<ArtifactFile>, StorageError> {
        validate_key(key)?;
        if key.ecosystem != self.ecosystem {
            return Err(StorageError::InvalidIdentity(key.ecosystem.clone()));
        }
        validate_artifact_key(artifact_key)?;
        let path = self.artifact_path(key, artifact_key);
        let length = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        match verify_file(artifact_key, expectation, &path) {
            Ok(_) => {}
            Err(StorageError::IntegrityMismatch(_)) => {
                fs::remove_file(path)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        let _ = crate::storage::access::note_access(&path);
        Ok(Some(ArtifactFile { path, length }))
    }

    pub fn temporary_artifact_path(&self) -> Result<PathBuf, StorageError> {
        let directory = self.data_dir.join("tmp");
        fs::create_dir_all(&directory)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        Ok(directory.join(format!(
            "{}-{}-{nonce}.part",
            safe_component(&self.ecosystem),
            std::process::id()
        )))
    }

    pub fn cleanup_temporary_artifacts(&self) {
        let directory = self.data_dir.join("tmp");
        let prefix = format!("{}-", safe_component(&self.ecosystem));
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) && name.ends_with(".part") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    pub fn artifact_path(&self, key: &NpmCacheKey, artifact_key: &str) -> PathBuf {
        crate::storage::layout::artifact_objects_root(
            &self.data_dir,
            &self.ecosystem,
            &safe_component(&key.selected_proxy),
        )
        .join(safe_component(&key.entry))
        .join(safe_component(&key.package))
        .join(safe_component(&key.version))
        .join(safe_component(&key.representation))
        .join(artifact_key)
    }
}
