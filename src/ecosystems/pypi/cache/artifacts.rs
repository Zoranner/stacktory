use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::Path,
};

use sha2::{Digest, Sha256, Sha384, Sha512};

use super::{ArtifactFile, PyPiArtifactKey, PyPiStorage, StorageError, validate_artifact_key};

impl PyPiStorage {
    pub(crate) fn read_verified_artifact(
        &self,
        key: &PyPiArtifactKey,
        hashes: &BTreeMap<String, String>,
    ) -> Result<Option<ArtifactFile>, StorageError> {
        validate_artifact_key(key)?;
        let path = self.artifact_path(key);
        let length = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        match verify_file_hashes(&path, hashes) {
            Ok(()) => {}
            Err(StorageError::IntegrityMismatch(_)) => {
                fs::remove_file(path)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        let _ = crate::storage::access::note_access(&path);
        Ok(Some(ArtifactFile { path, length }))
    }

    pub(crate) fn publish_verified_artifact_file(
        &self,
        key: &PyPiArtifactKey,
        hashes: &BTreeMap<String, String>,
        temporary_path: &Path,
    ) -> Result<ArtifactFile, StorageError> {
        validate_artifact_key(key)?;
        if let Err(error) = verify_file_hashes(temporary_path, hashes) {
            let _ = fs::remove_file(temporary_path);
            return Err(error);
        }
        let destination = self.artifact_path(key);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::rename(temporary_path, &destination) {
            Ok(()) => {}
            Err(_) if destination.exists() => {
                let _ = fs::remove_file(temporary_path);
                verify_file_hashes(&destination, hashes)?;
            }
            Err(error) => {
                let _ = fs::remove_file(temporary_path);
                return Err(StorageError::Io(error));
            }
        }
        let length = fs::metadata(&destination)?.len();
        Ok(ArtifactFile {
            path: destination,
            length,
        })
    }

    pub(crate) fn artifact_path(&self, key: &PyPiArtifactKey) -> std::path::PathBuf {
        crate::storage::layout::artifact_objects_root(self.data_dir(), "pypi", &key.selected_proxy)
            .join(&key.entry)
            .join(&key.project)
            .join(&key.filename)
    }
}

fn verify_file_hashes(path: &Path, hashes: &BTreeMap<String, String>) -> Result<(), StorageError> {
    if let Some(expected) = hashes.get("sha512") {
        return verify::<Sha512>(path, expected);
    }
    if let Some(expected) = hashes.get("sha384") {
        return verify::<Sha384>(path, expected);
    }
    if let Some(expected) = hashes.get("sha256") {
        return verify::<Sha256>(path, expected);
    }
    Ok(())
}

fn verify<D>(path: &Path, expected: &str) -> Result<(), StorageError>
where
    D: Digest,
{
    let mut file = fs::File::open(path)?;
    let mut hasher = D::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(StorageError::IntegrityMismatch(path.display().to_string()))
    }
}
