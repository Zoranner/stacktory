use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};
use thiserror::Error;

use crate::fsutil::atomic_write;

use super::{metadata::NpmArtifactDescriptor, path::validate_package_name};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NpmCacheKey {
    pub ecosystem: String,
    pub entry: String,
    pub protocol_variant: String,
    pub package: String,
    pub version: String,
    pub representation: String,
    pub selected_proxy: String,
}

#[derive(Clone, Debug)]
pub struct StoredMetadata {
    pub key: NpmCacheKey,
    pub body: Vec<u8>,
    pub descriptors: Vec<NpmArtifactDescriptor>,
    pub validated_at_unix_secs: u64,
    pub content_digest: String,
    pub source_fingerprint: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IntegrityExpectation {
    pub integrity: Option<String>,
    pub shasum: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedArtifact {
    pub local_sha256: String,
    pub upstream_verified: bool,
}

#[derive(Clone, Debug)]
pub struct ArtifactFile {
    pub path: PathBuf,
    pub length: u64,
}

#[derive(Debug)]
pub struct NpmStorage {
    data_dir: PathBuf,
    ecosystem: String,
}

#[derive(Deserialize, Serialize)]
struct MetadataPointer {
    key: NpmCacheKey,
    #[serde(default)]
    canonical: bool,
    #[serde(default)]
    validated_at_unix_secs: u64,
    #[serde(default)]
    content_digest: String,
    #[serde(default)]
    source_fingerprint: String,
}

#[derive(Deserialize, Serialize)]
struct MetadataEnvelope {
    descriptors: Vec<NpmArtifactDescriptor>,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("npm storage I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("npm cache identity `{0}` is invalid")]
    InvalidIdentity(String),
    #[error("npm artifact key `{0}` is invalid")]
    InvalidArtifactKey(String),
    #[error("npm integrity value `{0}` is invalid")]
    InvalidIntegrity(String),
    #[error("npm artifact does not match its expected digest: {0}")]
    IntegrityMismatch(String),
}

impl NpmStorage {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self::new_for_ecosystem(data_dir, "npm")
    }

    pub fn new_for_ecosystem(data_dir: impl Into<PathBuf>, ecosystem: &str) -> Self {
        Self {
            data_dir: data_dir.into(),
            ecosystem: ecosystem.to_owned(),
        }
    }

    pub fn publish_verified_artifact(
        &self,
        key: &NpmCacheKey,
        artifact_key: &str,
        expectation: &IntegrityExpectation,
        contents: &[u8],
    ) -> Result<VerifiedArtifact, StorageError> {
        validate_key(key)?;
        if key.ecosystem != self.ecosystem {
            return Err(StorageError::InvalidIdentity(key.ecosystem.clone()));
        }
        validate_artifact_key(artifact_key)?;
        let verification = verify_contents(artifact_key, expectation, contents)?;
        let result = VerifiedArtifact {
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
        let _ = crate::retention::note_access(&path);
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
            validate_artifact_key(&descriptor.artifact_key)?;
            atomic_write(
                &self.descriptor_path(&key.entry, &descriptor.artifact_key),
                &serde_json::to_vec(descriptor)
                    .map_err(|error| StorageError::Io(io::Error::other(error)))?,
            )?;
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

    pub fn artifact_path(&self, key: &NpmCacheKey, artifact_key: &str) -> PathBuf {
        crate::layout::artifact_objects_root(
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

    fn metadata_body_path(&self, key: &NpmCacheKey) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, &self.ecosystem)
            .join(safe_component(&key.entry))
            .join(safe_component(&key.package))
            .join(safe_component(&key.selected_proxy))
            .join(format!("{}-{}.json", key.version, key.representation))
    }

    fn metadata_envelope_path(&self, key: &NpmCacheKey) -> PathBuf {
        self.metadata_body_path(key).with_extension("envelope.json")
    }

    fn metadata_pointer_path(&self, entry: &str, package: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, &self.ecosystem)
            .join(safe_component(entry))
            .join(safe_component(package))
            .join("current.toml")
    }

    fn descriptor_path(&self, entry: &str, artifact_key: &str) -> PathBuf {
        crate::layout::metadata_root(&self.data_dir, &self.ecosystem)
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

fn validate_key(key: &NpmCacheKey) -> Result<(), StorageError> {
    validate_package_name(&key.package)
        .map_err(|error| StorageError::InvalidIdentity(error.to_string()))?;
    for value in [
        key.ecosystem.as_str(),
        key.entry.as_str(),
        key.protocol_variant.as_str(),
        key.version.as_str(),
        key.representation.as_str(),
        key.selected_proxy.as_str(),
    ] {
        if value.is_empty()
            || value == "."
            || value == ".."
            || value.contains('\\')
            || value.contains('/')
            || value.chars().any(char::is_control)
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'_' | b'-')
            })
        {
            return Err(StorageError::InvalidIdentity(value.to_owned()));
        }
    }
    Ok(())
}

fn validate_metadata_key(key: &NpmCacheKey) -> Result<(), StorageError> {
    validate_key(key)?;
    if key.version != "catalog" || key.representation != "metadata" {
        return Err(StorageError::InvalidIdentity(key.version.clone()));
    }
    Ok(())
}

fn validate_artifact_key(value: &str) -> Result<(), StorageError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains(['/', '\\'])
        || value.chars().any(char::is_control)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(StorageError::InvalidArtifactKey(value.to_owned()));
    }
    Ok(())
}

fn verify_contents(
    artifact_key: &str,
    expectation: &IntegrityExpectation,
    contents: &[u8],
) -> Result<VerificationResult, StorageError> {
    let plan = VerificationPlan::new(expectation)?;
    let mut contents = io::Cursor::new(contents);
    let digests = hash_reader(&mut contents, &plan)?;
    verify_digests(artifact_key, &plan, digests)
}

fn verify_file(
    artifact_key: &str,
    expectation: &IntegrityExpectation,
    path: &Path,
) -> Result<VerificationResult, StorageError> {
    let plan = VerificationPlan::new(expectation)?;
    let mut file = fs::File::open(path)?;
    let digests = hash_reader(&mut file, &plan)?;
    verify_digests(artifact_key, &plan, digests)
}

struct VerificationResult {
    local_sha256: String,
    upstream_verified: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum IntegrityAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

struct ExpectedIntegrity {
    algorithm: IntegrityAlgorithm,
    digest: Vec<u8>,
}

struct VerificationPlan {
    integrity: Vec<ExpectedIntegrity>,
    shasum: Option<String>,
}

impl VerificationPlan {
    fn new(expectation: &IntegrityExpectation) -> Result<Self, StorageError> {
        if let Some(integrity) = expectation.integrity.as_deref() {
            let mut expected = Vec::new();
            for token in integrity.split_whitespace() {
                let Some((algorithm, encoded)) = token.split_once('-') else {
                    continue;
                };
                let digest = STANDARD
                    .decode(encoded)
                    .map_err(|_| StorageError::InvalidIntegrity(integrity.to_owned()))?;
                let algorithm = match algorithm {
                    "sha512" => IntegrityAlgorithm::Sha512,
                    "sha384" => IntegrityAlgorithm::Sha384,
                    "sha256" => IntegrityAlgorithm::Sha256,
                    _ => continue,
                };
                expected.push(ExpectedIntegrity { algorithm, digest });
            }
            if expected.is_empty() {
                return Err(StorageError::InvalidIntegrity(integrity.to_owned()));
            }
            return Ok(Self {
                integrity: expected,
                shasum: None,
            });
        }
        if let Some(shasum) = expectation.shasum.as_deref() {
            if shasum.len() != 40 || !shasum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(StorageError::InvalidIntegrity(shasum.to_owned()));
            }
            return Ok(Self {
                integrity: Vec::new(),
                shasum: Some(shasum.to_owned()),
            });
        }
        Ok(Self {
            integrity: Vec::new(),
            shasum: None,
        })
    }

    fn needs(&self, algorithm: IntegrityAlgorithm) -> bool {
        self.integrity
            .iter()
            .any(|expected| expected.algorithm == algorithm)
    }
}

struct FileDigests {
    sha256: Vec<u8>,
    sha1: Option<Vec<u8>>,
    sha384: Option<Vec<u8>>,
    sha512: Option<Vec<u8>>,
}

fn hash_reader<R: Read>(
    reader: &mut R,
    plan: &VerificationPlan,
) -> Result<FileDigests, StorageError> {
    let mut sha256 = Sha256::new();
    let mut sha1 = plan.shasum.as_ref().map(|_| Sha1::new());
    let mut sha384 = plan.needs(IntegrityAlgorithm::Sha384).then(Sha384::new);
    let mut sha512 = plan.needs(IntegrityAlgorithm::Sha512).then(Sha512::new);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        sha256.update(chunk);
        if let Some(hasher) = &mut sha1 {
            hasher.update(chunk);
        }
        if let Some(hasher) = &mut sha384 {
            hasher.update(chunk);
        }
        if let Some(hasher) = &mut sha512 {
            hasher.update(chunk);
        }
    }
    Ok(FileDigests {
        sha256: sha256.finalize().to_vec(),
        sha1: sha1.map(|hasher| hasher.finalize().to_vec()),
        sha384: sha384.map(|hasher| hasher.finalize().to_vec()),
        sha512: sha512.map(|hasher| hasher.finalize().to_vec()),
    })
}

fn verify_digests(
    artifact_key: &str,
    plan: &VerificationPlan,
    digests: FileDigests,
) -> Result<VerificationResult, StorageError> {
    if !plan.integrity.is_empty() {
        for expected in &plan.integrity {
            let actual = match expected.algorithm {
                IntegrityAlgorithm::Sha256 => &digests.sha256,
                IntegrityAlgorithm::Sha384 => digests
                    .sha384
                    .as_ref()
                    .expect("SHA-384 digest is computed when requested"),
                IntegrityAlgorithm::Sha512 => digests
                    .sha512
                    .as_ref()
                    .expect("SHA-512 digest is computed when requested"),
            };
            if actual == &expected.digest {
                return Ok(VerificationResult {
                    local_sha256: hex(&digests.sha256),
                    upstream_verified: true,
                });
            }
        }
        return Err(StorageError::IntegrityMismatch(artifact_key.to_owned()));
    }
    if let Some(shasum) = plan.shasum.as_deref() {
        let actual = hex(digests
            .sha1
            .as_ref()
            .expect("SHA-1 digest is computed when requested"));
        if !actual.eq_ignore_ascii_case(shasum) {
            return Err(StorageError::IntegrityMismatch(artifact_key.to_owned()));
        }
        return Ok(VerificationResult {
            local_sha256: hex(&digests.sha256),
            upstream_verified: true,
        });
    }
    Ok(VerificationResult {
        local_sha256: hex(&digests.sha256),
        upstream_verified: false,
    })
}

fn safe_component(value: &str) -> String {
    value.replace('@', "_at_").replace(['/', '\\'], "_slash_")
}

fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn hex(contents: &[u8]) -> String {
    contents.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn remove_if_present(path: &std::path::Path) -> Result<(), StorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StorageError::Io(error)),
    }
}
