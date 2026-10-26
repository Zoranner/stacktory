use std::{fs, io, path::PathBuf};

use super::{
    MetadataEnvelope, MetadataPointer, NpmCacheKey, NpmStorage, StorageError, StoredMetadata,
    now_unix_secs, remove_if_present, safe_component, sha256, validate_artifact_key,
    validate_metadata_key,
};
use crate::{ecosystems::npm::metadata::NpmArtifactDescriptor, storage::atomic::atomic_write};

impl NpmStorage {
    pub fn publish_metadata(
        &self,
        key: &NpmCacheKey,
        body: &[u8],
        descriptors: &[NpmArtifactDescriptor],
        source_fingerprint: &str,
    ) -> Result<(), StorageError> {
        validate_metadata_key(key)?;
        if key.ecosystem != self.ecosystem {
            return Err(StorageError::InvalidIdentity(key.ecosystem.clone()));
        }
        let previous_descriptors = self.stored_descriptors(&key.entry, &key.package);
        for descriptor in descriptors {
            self.write_descriptor(&key.entry, descriptor)?;
        }
        atomic_write(&self.metadata_body_path(key), body)?;
        atomic_write(
            &self.metadata_envelope_path(key),
            &serde_json::to_vec(&MetadataEnvelope {
                descriptors: descriptors.to_vec(),
            })
            .map_err(|error| StorageError::Io(io::Error::other(error)))?,
        )?;
        atomic_write(
            &self.metadata_pointer_path(&key.entry, &key.package),
            &toml::to_string(&MetadataPointer {
                key: key.clone(),
                canonical: true,
                validated_at_unix_secs: now_unix_secs(),
                content_digest: sha256(body),
                source_fingerprint: source_fingerprint.to_owned(),
            })
            .map_err(|error| StorageError::Io(io::Error::other(error)))?
            .into_bytes(),
        )?;
        if let Some(previous_descriptors) = previous_descriptors {
            for descriptor in previous_descriptors {
                if !descriptors
                    .iter()
                    .any(|next| next.artifact_key == descriptor.artifact_key)
                    && let Err(error) = remove_if_present(
                        &self.descriptor_path(&key.entry, &descriptor.artifact_key),
                    )
                {
                    crate::logging::warn(format!(
                        "npm metadata descriptor cleanup failed for `{}`: {error}",
                        descriptor.artifact_key
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn materialize_descriptors(
        &self,
        entry: &str,
        descriptors: &[NpmArtifactDescriptor],
    ) -> Result<(), StorageError> {
        for descriptor in descriptors {
            validate_artifact_key(&descriptor.artifact_key)?;
            let path = self.descriptor_path(entry, &descriptor.artifact_key);
            if path.exists() {
                continue;
            }
            self.write_descriptor(entry, descriptor)?;
        }
        Ok(())
    }

    fn write_descriptor(
        &self,
        entry: &str,
        descriptor: &NpmArtifactDescriptor,
    ) -> Result<(), StorageError> {
        validate_artifact_key(&descriptor.artifact_key)?;
        atomic_write(
            &self.descriptor_path(entry, &descriptor.artifact_key),
            &serde_json::to_vec(descriptor)
                .map_err(|error| StorageError::Io(io::Error::other(error)))?,
        )
        .map_err(StorageError::Io)
    }

    pub fn read_metadata(
        &self,
        entry: &str,
        package: &str,
    ) -> Result<Option<StoredMetadata>, StorageError> {
        let pointer_path = self.metadata_pointer_path(entry, package);
        let pointer_contents = match fs::read_to_string(&pointer_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        let pointer = toml::from_str::<MetadataPointer>(&pointer_contents)
            .map_err(|error| StorageError::Io(io::Error::other(error)));
        let pointer = match pointer {
            Ok(pointer) => pointer,
            Err(_) => {
                self.remove_metadata(entry, package)?;
                return Ok(None);
            }
        };
        if validate_metadata_key(&pointer.key).is_err() {
            self.remove_metadata(entry, package)?;
            return Ok(None);
        }
        if pointer.key.ecosystem != self.ecosystem
            || pointer.key.entry != entry
            || pointer.key.package != package
        {
            self.remove_metadata(entry, package)?;
            return Ok(None);
        }
        if !pointer.canonical
            || pointer.content_digest.is_empty()
            || pointer.source_fingerprint.is_empty()
        {
            self.remove_metadata(entry, package)?;
            return Ok(None);
        }
        let body = match fs::read(self.metadata_body_path(&pointer.key)) {
            Ok(body) => body,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.remove_metadata(entry, package)?;
                return Ok(None);
            }
            Err(error) => return Err(StorageError::Io(error)),
        };
        if sha256(&body) != pointer.content_digest {
            self.remove_metadata(entry, package)?;
            return Ok(None);
        }
        let envelope_bytes = match fs::read(self.metadata_envelope_path(&pointer.key)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.remove_metadata(entry, package)?;
                return Ok(None);
            }
            Err(error) => return Err(StorageError::Io(error)),
        };
        let envelope: MetadataEnvelope = match serde_json::from_slice(&envelope_bytes) {
            Ok(envelope) => envelope,
            Err(_) => {
                self.remove_metadata(entry, package)?;
                return Ok(None);
            }
        };
        Ok(Some(StoredMetadata {
            key: pointer.key,
            body,
            descriptors: envelope.descriptors,
            validated_at_unix_secs: pointer.validated_at_unix_secs,
            content_digest: pointer.content_digest,
            source_fingerprint: pointer.source_fingerprint,
        }))
    }

    pub fn touch_metadata(
        &self,
        entry: &str,
        package: &str,
        key: &NpmCacheKey,
        content_digest: &str,
        source_fingerprint: &str,
    ) -> Result<(), StorageError> {
        validate_metadata_key(key)?;
        if key.ecosystem != self.ecosystem || key.entry != entry || key.package != package {
            return Err(StorageError::InvalidIdentity(package.to_owned()));
        }
        Ok(atomic_write(
            &self.metadata_pointer_path(entry, package),
            &toml::to_string(&MetadataPointer {
                key: key.clone(),
                canonical: true,
                validated_at_unix_secs: now_unix_secs(),
                content_digest: content_digest.to_owned(),
                source_fingerprint: source_fingerprint.to_owned(),
            })
            .map_err(|error| StorageError::Io(io::Error::other(error)))?
            .into_bytes(),
        )?)
    }

    pub fn remove_metadata(&self, entry: &str, package: &str) -> Result<(), StorageError> {
        let pointer_path = self.metadata_pointer_path(entry, package);
        let pointer = fs::read_to_string(&pointer_path)
            .ok()
            .and_then(|contents| toml::from_str::<MetadataPointer>(&contents).ok());
        if let Some(pointer) = &pointer {
            if let Some(descriptors) = self.stored_descriptors(entry, package) {
                for descriptor in descriptors {
                    remove_if_present(&self.descriptor_path(entry, &descriptor.artifact_key))?;
                }
            }
            remove_if_present(&self.metadata_body_path(&pointer.key))?;
            remove_if_present(&self.metadata_envelope_path(&pointer.key))?;
        }
        remove_if_present(&pointer_path)
    }

    pub fn read_descriptor(
        &self,
        entry: &str,
        artifact_key: &str,
    ) -> Result<Option<NpmArtifactDescriptor>, StorageError> {
        validate_artifact_key(artifact_key)?;
        match fs::read(self.descriptor_path(entry, artifact_key)) {
            Ok(contents) => serde_json::from_slice(&contents)
                .map(Some)
                .map_err(|error| StorageError::Io(io::Error::other(error))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StorageError::Io(error)),
        }
    }

    fn metadata_body_path(&self, key: &NpmCacheKey) -> PathBuf {
        crate::storage::layout::metadata_root(&self.data_dir, &self.ecosystem)
            .join(safe_component(&key.entry))
            .join(safe_component(&key.package))
            .join(safe_component(&key.selected_proxy))
            .join(format!("{}-{}.json", key.version, key.representation))
    }

    fn metadata_envelope_path(&self, key: &NpmCacheKey) -> PathBuf {
        self.metadata_body_path(key).with_extension("envelope.json")
    }

    fn metadata_pointer_path(&self, entry: &str, package: &str) -> PathBuf {
        crate::storage::layout::metadata_root(&self.data_dir, &self.ecosystem)
            .join(safe_component(entry))
            .join(safe_component(package))
            .join("current.toml")
    }

    fn descriptor_path(&self, entry: &str, artifact_key: &str) -> PathBuf {
        crate::storage::layout::metadata_root(&self.data_dir, &self.ecosystem)
            .join(safe_component(entry))
            .join("descriptors")
            .join(format!("{artifact_key}.json"))
    }

    fn stored_descriptors(&self, entry: &str, package: &str) -> Option<Vec<NpmArtifactDescriptor>> {
        let pointer_path = self.metadata_pointer_path(entry, package);
        let pointer: MetadataPointer = fs::read_to_string(pointer_path)
            .ok()
            .and_then(|contents| toml::from_str(&contents).ok())?;
        let envelope: MetadataEnvelope =
            serde_json::from_slice(&fs::read(self.metadata_envelope_path(&pointer.key)).ok()?)
                .ok()?;
        Some(envelope.descriptors)
    }
}
