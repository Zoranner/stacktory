use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;
use crate::storage::atomic::atomic_write;

pub(super) fn metadata_file_key(reference: &str) -> String {
    reference.replace(':', "-")
}

pub(super) fn representation_file_key(accept: Option<&str>) -> String {
    let value = accept.unwrap_or("default");
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub(super) fn write_blob_descriptors(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    selected_proxy: &str,
    body: &[u8],
    only_missing: bool,
) -> Result<(), DockerError> {
    let manifest: Value = serde_json::from_slice(body).map_err(DockerError::Serialization)?;
    if let Some(config) = manifest.get("config") {
        write_blob_descriptor(
            data_dir,
            entry_name,
            repository,
            selected_proxy,
            config,
            "config",
            only_missing,
        )?;
    }
    if let Some(layers) = manifest.get("layers").and_then(Value::as_array) {
        for layer in layers {
            write_blob_descriptor(
                data_dir,
                entry_name,
                repository,
                selected_proxy,
                layer,
                "layer",
                only_missing,
            )?;
        }
    }
    Ok(())
}

fn write_blob_descriptor(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    selected_proxy: &str,
    descriptor: &Value,
    object_type: &str,
    only_missing: bool,
) -> Result<(), DockerError> {
    let digest = descriptor
        .get("digest")
        .and_then(Value::as_str)
        .filter(|value| super::is_sha256_digest(value))
        .ok_or_else(|| DockerError::UnknownBlob(object_type.to_owned()))?;
    let content_type = descriptor
        .get("mediaType")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DockerError::UnknownBlob(digest.to_owned()))?;
    if only_missing && blob_metadata_path(data_dir, entry_name, repository, digest).exists() {
        return Ok(());
    }
    let metadata = BlobMetadata {
        repository: repository.to_owned(),
        digest: digest.to_owned(),
        object_type: object_type.to_owned(),
        content_type: content_type.to_owned(),
        selected_proxy: selected_proxy.to_owned(),
    };
    write_blob_metadata(data_dir, entry_name, repository, &metadata)
}

pub(super) fn write_blob_metadata(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    metadata: &BlobMetadata,
) -> Result<(), DockerError> {
    let path = blob_metadata_path(data_dir, entry_name, repository, &metadata.digest);
    atomic_write(&path, &serde_json::to_vec(metadata)?).map_err(DockerError::Storage)
}

pub(super) fn read_blob_metadata(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    digest: &str,
) -> Result<Option<BlobMetadata>, DockerError> {
    let path = blob_metadata_path(data_dir, entry_name, repository, digest);
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(DockerError::Serialization)
}

pub(super) fn read_shared_blob_metadata(
    data_dir: &Path,
    selected_proxy: &str,
    repository: &str,
    digest: &str,
) -> Result<Option<BlobMetadata>, DockerError> {
    let root = crate::storage::layout::metadata_root(data_dir, "docker");
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DockerError::Storage(error)),
    };
    for entry in entries {
        let entry = entry.map_err(DockerError::Storage)?;
        let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(metadata) = read_blob_metadata(data_dir, &entry_name, repository, digest)?
            && metadata.selected_proxy == selected_proxy
        {
            return Ok(Some(metadata));
        }
    }
    Ok(None)
}

pub(super) fn read_manifest_metadata(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    reference: &str,
    accept: Option<&str>,
) -> Result<Option<ManifestMetadata>, DockerError> {
    let path = crate::storage::layout::metadata_root(data_dir, "docker")
        .join(entry_name)
        .join(repository)
        .join("manifests")
        .join(format!(
            "{}-{}.json",
            metadata_file_key(reference),
            representation_file_key(accept)
        ));
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(DockerError::Serialization)
}

pub(super) fn read_query_metadata(
    data_dir: &Path,
    entry_name: &str,
    object_type: &str,
    namespace: &str,
    request_key: &str,
    query: Option<&str>,
) -> Result<Option<QueryMetadata>, DockerError> {
    let path = query_metadata_path(
        data_dir,
        entry_name,
        object_type,
        namespace,
        request_key,
        query,
    );
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(DockerError::Serialization)
}

pub(super) fn write_query_metadata(
    data_dir: &Path,
    entry_name: &str,
    metadata: &QueryMetadata,
) -> Result<(), DockerError> {
    let path = query_metadata_path(
        data_dir,
        entry_name,
        &metadata.object_type,
        &metadata.namespace,
        &metadata.request_key,
        metadata.query.as_deref(),
    );
    atomic_write(&path, &serde_json::to_vec(metadata)?).map_err(DockerError::Storage)
}

fn query_metadata_path(
    data_dir: &Path,
    entry_name: &str,
    object_type: &str,
    namespace: &str,
    request_key: &str,
    query: Option<&str>,
) -> PathBuf {
    let identity = format!(
        "docker\0{entry_name}\0{object_type}\0{namespace}\0{request_key}\0{}",
        query.unwrap_or_default()
    );
    crate::storage::layout::metadata_root(data_dir, "docker")
        .join(entry_name)
        .join("queries")
        .join(format!("{}.json", super::sha256_hex(identity.as_bytes())))
}

pub(super) fn read_cached_manifest(
    data_dir: &Path,
    selected_proxy: &str,
    digest: &str,
) -> Result<Option<CachedManifest>, DockerError> {
    for object_type in ["manifest", "image-index"] {
        if let Some(body) = read_verified_artifact(data_dir, selected_proxy, object_type, digest)? {
            let content_type = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("mediaType")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            return Ok(Some(CachedManifest { body, content_type }));
        }
    }
    Ok(None)
}

fn blob_metadata_path(
    data_dir: &Path,
    entry_name: &str,
    repository: &str,
    digest: &str,
) -> PathBuf {
    crate::storage::layout::metadata_root(data_dir, "docker")
        .join(entry_name)
        .join(repository)
        .join("blobs")
        .join(format!("{}.json", metadata_file_key(digest)))
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn artifact_path(
    data_dir: &Path,
    selected_proxy: &str,
    object_type: &str,
    digest: &str,
) -> PathBuf {
    crate::storage::layout::artifact_objects_root(data_dir, "docker", selected_proxy)
        .join(object_type)
        .join(metadata_file_key(digest))
}

// Manifest and query objects are small enough to keep in memory; blob objects
// use the file-based helpers below.
pub(super) fn read_verified_artifact(
    data_dir: &Path,
    selected_proxy: &str,
    object_type: &str,
    digest: &str,
) -> Result<Option<Vec<u8>>, DockerError> {
    let path = artifact_path(data_dir, selected_proxy, object_type, digest);
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    if super::sha256_digest(&contents) == digest {
        let _ = crate::storage::access::note_access(&path);
        Ok(Some(contents))
    } else {
        fs::remove_file(path)?;
        Ok(None)
    }
}

pub(super) fn write_verified_artifact(
    data_dir: &Path,
    selected_proxy: &str,
    object_type: &str,
    digest: &str,
    contents: &[u8],
) -> Result<(), DockerError> {
    let actual_digest = super::sha256_digest(contents);
    if actual_digest != digest {
        return Err(DockerError::DigestMismatch {
            expected: digest.to_owned(),
            actual: actual_digest,
        });
    }
    atomic_write(
        &artifact_path(data_dir, selected_proxy, object_type, digest),
        contents,
    )
    .map_err(DockerError::Storage)
}

pub(super) async fn read_verified_artifact_path(
    data_dir: &Path,
    selected_proxy: &str,
    object_type: &str,
    digest: &str,
) -> Result<Option<(PathBuf, u64)>, DockerError> {
    let path = artifact_path(data_dir, selected_proxy, object_type, digest);
    let digest = digest.to_owned();
    tokio::task::spawn_blocking(move || {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(DockerError::Storage(error)),
        };
        let mut file = fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = std::io::Read::read(&mut file, &mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if format!("sha256:{:x}", hasher.finalize()) == digest {
            let _ = crate::storage::access::note_access(&path);
            Ok(Some((path, metadata.len())))
        } else {
            fs::remove_file(&path)?;
            Ok(None)
        }
    })
    .await
    .map_err(|error| DockerError::Storage(io::Error::other(error)))?
}

pub(super) async fn publish_verified_file(
    data_dir: &Path,
    selected_proxy: &str,
    object_type: &str,
    digest: &str,
    temporary: &Path,
) -> Result<PathBuf, DockerError> {
    let destination = artifact_path(data_dir, selected_proxy, object_type, digest);
    let temporary = temporary.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let result = (|| -> Result<(), io::Error> {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&temporary, &destination)
        })();
        match result {
            Ok(()) => Ok(destination),
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(DockerError::Storage(error))
            }
        }
    })
    .await
    .map_err(|error| DockerError::Storage(io::Error::other(error)))?
}
