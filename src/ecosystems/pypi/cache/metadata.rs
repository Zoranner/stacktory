use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

use super::{
    PyPiStorage, StorageError, now_unix_secs, sha256_hex, validate_component, validate_digest,
};
use crate::ecosystems::pypi::{
    index::{PyPiFile, PyPiIndex},
    path::is_safe_file_name,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PyPiMetadataKey {
    pub(crate) entry: String,
    pub(crate) project: String,
    pub(crate) selected_proxy: String,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredProjectIndex {
    pub(crate) key: PyPiMetadataKey,
    pub(crate) body: Vec<u8>,
    pub(crate) index: PyPiIndex,
    pub(crate) validated_at_unix_secs: u64,
    pub(crate) canonical_digest: String,
    pub(crate) source_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PyPiFileBinding {
    pub(crate) project: String,
    pub(crate) filename: String,
    pub(crate) selected_proxy: String,
    #[serde(default)]
    pub(crate) source_fingerprint: String,
    pub(crate) upstream_url: String,
    #[serde(default)]
    pub(crate) hashes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requires_python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) yanked: Option<String>,
}

impl PyPiFileBinding {
    pub(crate) fn from_file(
        project: &str,
        selected_proxy: &str,
        source_fingerprint: &str,
        file: &PyPiFile,
    ) -> Self {
        Self {
            project: project.to_owned(),
            filename: file.filename.clone(),
            selected_proxy: selected_proxy.to_owned(),
            source_fingerprint: source_fingerprint.to_owned(),
            upstream_url: file.upstream_url.clone(),
            hashes: file.hashes.clone(),
            requires_python: file.requires_python.clone(),
            yanked: file.yanked.clone(),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct MetadataPointer {
    entry: String,
    project: String,
    selected_proxy: String,
    source_fingerprint: String,
    canonical_digest: String,
    validated_at_unix_secs: u64,
}

impl PyPiStorage {
    pub(crate) fn read_metadata(
        &self,
        entry: &str,
        project: &str,
    ) -> Result<Option<StoredProjectIndex>, StorageError> {
        validate_component(entry)?;
        validate_component(project)?;
        let pointer_path = self.pointer_path(entry, project);
        let pointer = match fs::read_to_string(&pointer_path) {
            Ok(contents) => match toml::from_str::<MetadataPointer>(&contents) {
                Ok(pointer) => pointer,
                Err(_) => {
                    self.remove_metadata(entry, project)?;
                    return Ok(None);
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        if pointer.entry != entry
            || pointer.project != project
            || validate_component(&pointer.selected_proxy).is_err()
            || validate_digest(&pointer.canonical_digest, 64).is_err()
            || pointer.source_fingerprint.is_empty()
        {
            self.remove_metadata(entry, project)?;
            return Ok(None);
        }
        let body = match fs::read(self.index_path(entry, project)) {
            Ok(body) => body,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.remove_metadata(entry, project)?;
                return Ok(None);
            }
            Err(error) => return Err(StorageError::Io(error)),
        };
        if sha256_hex(&body) != pointer.canonical_digest {
            self.remove_metadata(entry, project)?;
            return Ok(None);
        }
        let index: PyPiIndex = match serde_json::from_slice(&body) {
            Ok(index) => index,
            Err(_) => {
                self.remove_metadata(entry, project)?;
                return Ok(None);
            }
        };
        if index.name != project {
            self.remove_metadata(entry, project)?;
            return Ok(None);
        }
        Ok(Some(StoredProjectIndex {
            key: PyPiMetadataKey {
                entry: entry.to_owned(),
                project: project.to_owned(),
                selected_proxy: pointer.selected_proxy,
            },
            body,
            index,
            validated_at_unix_secs: pointer.validated_at_unix_secs,
            canonical_digest: pointer.canonical_digest,
            source_fingerprint: pointer.source_fingerprint,
        }))
    }
}
impl PyPiStorage {
    pub(crate) fn publish_metadata(
        &self,
        key: &PyPiMetadataKey,
        body: &[u8],
        index: &PyPiIndex,
        source_fingerprint: &str,
    ) -> Result<(), StorageError> {
        validate_metadata_key(key)?;
        if source_fingerprint.is_empty() {
            return Err(StorageError::InvalidIdentity(source_fingerprint.to_owned()));
        }
        let filenames = index
            .files
            .iter()
            .map(|file| file.filename.clone())
            .collect::<BTreeSet<_>>();
        for file in &index.files {
            if !is_safe_file_name(&file.filename) {
                return Err(StorageError::InvalidIdentity(file.filename.clone()));
            }
            let binding = PyPiFileBinding::from_file(
                &key.project,
                &key.selected_proxy,
                source_fingerprint,
                file,
            );
            self.write_binding(&key.entry, &binding)?;
        }
        self.remove_stale_bindings(&key.entry, &key.project, &filenames)?;
        crate::storage::atomic::atomic_write(&self.index_path(&key.entry, &key.project), body)?;
        crate::storage::atomic::atomic_write(
            &self.pointer_path(&key.entry, &key.project),
            toml::to_string(&MetadataPointer {
                entry: key.entry.clone(),
                project: key.project.clone(),
                selected_proxy: key.selected_proxy.clone(),
                source_fingerprint: source_fingerprint.to_owned(),
                canonical_digest: sha256_hex(body),
                validated_at_unix_secs: now_unix_secs(),
            })
            .map_err(|error| StorageError::Io(io::Error::other(error)))?
            .as_bytes(),
        )?;
        Ok(())
    }

    pub(crate) fn touch_metadata(
        &self,
        key: &PyPiMetadataKey,
        canonical_digest: &str,
        source_fingerprint: &str,
    ) -> Result<(), StorageError> {
        validate_metadata_key(key)?;
        validate_digest(canonical_digest, 64)?;
        if source_fingerprint.is_empty() {
            return Err(StorageError::InvalidIdentity(source_fingerprint.to_owned()));
        }
        crate::storage::atomic::atomic_write(
            &self.pointer_path(&key.entry, &key.project),
            toml::to_string(&MetadataPointer {
                entry: key.entry.clone(),
                project: key.project.clone(),
                selected_proxy: key.selected_proxy.clone(),
                source_fingerprint: source_fingerprint.to_owned(),
                canonical_digest: canonical_digest.to_owned(),
                validated_at_unix_secs: now_unix_secs(),
            })
            .map_err(|error| StorageError::Io(io::Error::other(error)))?
            .as_bytes(),
        )?;
        Ok(())
    }

    pub(crate) fn remove_metadata(&self, entry: &str, project: &str) -> Result<(), StorageError> {
        validate_component(entry)?;
        validate_component(project)?;
        match fs::remove_dir_all(self.project_dir(entry, project)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(StorageError::Io(error)),
        }
    }

    pub(crate) fn read_binding(
        &self,
        entry: &str,
        project: &str,
        filename: &str,
    ) -> Result<Option<PyPiFileBinding>, StorageError> {
        validate_component(entry)?;
        validate_component(project)?;
        if !is_safe_file_name(filename) {
            return Ok(None);
        }
        let path = self.binding_path(entry, project, filename);
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        let binding = match serde_json::from_slice::<PyPiFileBinding>(&contents) {
            Ok(binding) => binding,
            Err(_) => {
                let _ = fs::remove_file(path);
                return Ok(None);
            }
        };
        if binding.project != project || binding.filename != filename {
            let _ = fs::remove_file(self.binding_path(entry, project, filename));
            return Ok(None);
        }
        let _ = crate::storage::access::note_access(&path);
        Ok(Some(binding))
    }
}
impl PyPiStorage {
    pub(crate) fn write_binding(
        &self,
        entry: &str,
        binding: &PyPiFileBinding,
    ) -> Result<(), StorageError> {
        validate_component(entry)?;
        validate_component(&binding.project)?;
        validate_component(&binding.selected_proxy)?;
        if !is_safe_file_name(&binding.filename) || binding.source_fingerprint.is_empty() {
            return Err(StorageError::InvalidIdentity(binding.filename.clone()));
        }
        crate::storage::atomic::atomic_write(
            &self.binding_path(entry, &binding.project, &binding.filename),
            &serde_json::to_vec(binding)
                .map_err(|error| StorageError::Io(io::Error::other(error)))?,
        )?;
        Ok(())
    }

    fn remove_stale_bindings(
        &self,
        entry: &str,
        project: &str,
        filenames: &BTreeSet<String>,
    ) -> Result<(), StorageError> {
        let directory = self.files_dir(entry, project);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StorageError::Io(error)),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(filename) = name.strip_suffix(".json") else {
                continue;
            };
            if !filenames.contains(filename) {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    fn project_dir(&self, entry: &str, project: &str) -> PathBuf {
        crate::storage::layout::metadata_root(self.data_dir(), "pypi")
            .join(entry)
            .join(project)
    }

    fn pointer_path(&self, entry: &str, project: &str) -> PathBuf {
        self.project_dir(entry, project).join("current.toml")
    }

    fn index_path(&self, entry: &str, project: &str) -> PathBuf {
        self.project_dir(entry, project).join("index.json")
    }

    fn files_dir(&self, entry: &str, project: &str) -> PathBuf {
        self.project_dir(entry, project).join("files")
    }

    fn binding_path(&self, entry: &str, project: &str, filename: &str) -> PathBuf {
        self.files_dir(entry, project)
            .join(format!("{filename}.json"))
    }
}

fn validate_metadata_key(key: &PyPiMetadataKey) -> Result<(), StorageError> {
    validate_component(&key.entry)?;
    validate_component(&key.project)?;
    validate_component(&key.selected_proxy)?;
    Ok(())
}
