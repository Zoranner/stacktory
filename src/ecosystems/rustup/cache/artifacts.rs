use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::{StorageError, validate_checksum, validate_source_name};
use crate::{ecosystems::rustup::DistPath, storage::atomic::atomic_write};

#[derive(Debug)]
pub struct ArtifactCache {
    data_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ArtifactFile {
    pub path: PathBuf,
    pub length: u64,
}

impl ArtifactCache {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub fn publish_verified(
        &self,
        source_name: &str,
        path: &DistPath,
        contents: &[u8],
        expected_checksum: &str,
    ) -> Result<(), StorageError> {
        validate_source_name(source_name)?;
        validate_checksum(expected_checksum)?;
        if checksum(contents) != expected_checksum {
            return Err(StorageError::ArtifactHashMismatch(path.to_string()));
        }
        Ok(atomic_write(
            &self.artifact_path(source_name, path),
            contents,
        )?)
    }

    pub fn get_verified(
        &self,
        source_name: &str,
        path: &DistPath,
        expected_checksum: &str,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(file) = self.get_verified_file(source_name, path, expected_checksum)? else {
            return Ok(None);
        };
        fs::read(file.path).map(Some).map_err(StorageError::Io)
    }

    pub fn get_verified_file(
        &self,
        source_name: &str,
        path: &DistPath,
        expected_checksum: &str,
    ) -> Result<Option<ArtifactFile>, StorageError> {
        validate_source_name(source_name)?;
        validate_checksum(expected_checksum)?;
        let artifact_path = self.artifact_path(source_name, path);
        let length = match fs::metadata(&artifact_path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        if checksum_file(&artifact_path)? == expected_checksum {
            let _ = crate::storage::access::note_access(&artifact_path);
            Ok(Some(ArtifactFile {
                path: artifact_path,
                length,
            }))
        } else {
            fs::remove_file(artifact_path)?;
            Ok(None)
        }
    }

    pub fn publish_verified_file(
        &self,
        source_name: &str,
        path: &DistPath,
        temporary_path: &Path,
        expected_checksum: &str,
    ) -> Result<ArtifactFile, StorageError> {
        validate_source_name(source_name)?;
        validate_checksum(expected_checksum)?;
        if checksum_file(temporary_path)? != expected_checksum {
            let _ = fs::remove_file(temporary_path);
            return Err(StorageError::ArtifactHashMismatch(path.to_string()));
        }
        let destination = self.artifact_path(source_name, path);
        let parent = destination
            .parent()
            .ok_or_else(|| io::Error::other("rustup artifact cache has no parent directory"))?;
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

    fn artifact_path(&self, source_name: &str, path: &DistPath) -> PathBuf {
        crate::storage::layout::artifact_objects_root(&self.data_dir, "rustup", source_name)
            .join(path.as_str())
    }
}

fn checksum(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn checksum_file(path: &Path) -> Result<String, StorageError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
