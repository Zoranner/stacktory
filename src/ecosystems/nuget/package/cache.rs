use std::{
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512};

use super::super::registration::PackageBinding;
use super::super::service_index::NugetError;
use super::{PackageCacheMetadata, PackageFile, PackageIndexDescriptor};
use crate::storage::atomic::atomic_write;

pub(super) fn verify_package_file(
    path: &Path,
    binding: &PackageBinding,
) -> Result<bool, NugetError> {
    let Some(expected) = binding.package_hash.as_deref() else {
        validate_nupkg_file(path)?;
        return Ok(false);
    };
    let Some(algorithm) = binding.package_hash_algorithm.as_deref() else {
        return Err(NugetError::InvalidPackage(
            "package hash algorithm is missing".to_owned(),
        ));
    };
    let expected = STANDARD
        .decode(expected)
        .map_err(|_| NugetError::InvalidPackage("package hash is not valid base64".to_owned()))?;
    let actual = match algorithm.to_ascii_uppercase().as_str() {
        "SHA512" => hash_file::<Sha512>(path)?,
        "SHA256" => hash_file::<Sha256>(path)?,
        _ => {
            return Err(NugetError::InvalidPackage(format!(
                "unsupported package hash algorithm `{algorithm}`"
            )));
        }
    };
    if actual != expected {
        return Err(NugetError::PackageIntegrityFailure(
            "package content digest does not match registration".to_owned(),
        ));
    }
    Ok(true)
}

fn hash_file<D: Digest>(path: &Path) -> Result<Vec<u8>, NugetError> {
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
    Ok(hasher.finalize().to_vec())
}

fn hash_file_sha256(path: &Path) -> Result<String, NugetError> {
    Ok(hash_file::<Sha256>(path)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_nupkg_file(path: &Path) -> Result<(), NugetError> {
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length < 22 {
        return Err(NugetError::PackageIntegrityFailure(
            "package content is not a ZIP archive".to_owned(),
        ));
    }
    let mut header = [0_u8; 4];
    file.read_exact(&mut header)?;
    if header != *b"PK\x03\x04" && header != *b"PK\x05\x06" {
        return Err(NugetError::PackageIntegrityFailure(
            "package content is not a ZIP archive".to_owned(),
        ));
    }
    let tail_length = usize::try_from(length.min(65_557)).expect("ZIP tail length fits usize");
    file.seek(SeekFrom::End(-(tail_length as i64)))?;
    let mut tail = vec![0_u8; tail_length];
    file.read_exact(&mut tail)?;
    let eocd = tail
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .ok_or_else(|| {
            NugetError::PackageIntegrityFailure(
                "package content has no ZIP end-of-central-directory record".to_owned(),
            )
        })?;
    if eocd + 22 > tail.len() {
        return Err(NugetError::PackageIntegrityFailure(
            "package ZIP end record is truncated".to_owned(),
        ));
    }
    let comment_length = u16::from_le_bytes([tail[eocd + 20], tail[eocd + 21]]) as usize;
    if eocd + 22 + comment_length > tail.len() {
        return Err(NugetError::PackageIntegrityFailure(
            "package ZIP end record is truncated".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn cache_is_valid(
    cache_path: &Path,
    binding: &PackageBinding,
) -> Result<bool, NugetError> {
    match verify_package_file(cache_path, binding) {
        Ok(true) => Ok(true),
        Ok(false) => {
            let Some(metadata) =
                read_json::<PackageCacheMetadata>(&package_cache_metadata_path(cache_path))?
            else {
                return Ok(false);
            };
            Ok(metadata.source_fingerprint == binding.source_fingerprint
                && metadata.local_sha256 == hash_file_sha256(cache_path)?)
        }
        Err(_) => Ok(false),
    }
}

pub(super) fn package_file(path: &Path) -> Result<PackageFile, NugetError> {
    Ok(PackageFile {
        path: path.to_path_buf(),
        length: fs::metadata(path)?.len(),
    })
}

pub(super) fn temporary_package_path(data_dir: &Path) -> Result<PathBuf, NugetError> {
    let directory = data_dir.join("tmp");
    fs::create_dir_all(&directory)?;
    Ok(directory.join(format!(
        "nuget-{}-{}.part",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    )))
}

pub(super) fn publish_verified_file(
    path: &Path,
    temporary: &Path,
    binding: &PackageBinding,
    upstream_verified: bool,
) -> Result<(), NugetError> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("NuGet package cache has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::rename(temporary, path).map_err(|error| {
        let _ = fs::remove_file(temporary);
        io::Error::other(error)
    })?;
    let local_sha256 = hash_file_sha256(path)?;
    if let Err(error) = atomic_write(
        &package_cache_metadata_path(path),
        &serde_json::to_vec(&PackageCacheMetadata {
            local_sha256,
            upstream_verified,
            source_fingerprint: binding.source_fingerprint.clone(),
        })?,
    ) {
        let _ = fs::remove_file(path);
        return Err(error.into());
    }
    Ok(())
}

pub(super) fn package_cache_path(
    data_dir: &Path,
    selected_proxy: &str,
    package_id: &str,
    version: &str,
) -> PathBuf {
    crate::storage::layout::artifact_objects_root(data_dir, "nuget", selected_proxy)
        .join("package")
        .join(package_id.to_ascii_lowercase())
        .join(version.to_ascii_lowercase())
        .join(format!(
            "{}.{}.nupkg",
            package_id.to_ascii_lowercase(),
            version.to_ascii_lowercase()
        ))
}

pub(super) fn package_index_path(data_dir: &Path, entry_name: &str, package_id: &str) -> PathBuf {
    crate::storage::layout::metadata_root(data_dir, "nuget")
        .join(entry_name)
        .join("package-index")
        .join(format!("{package_id}.json"))
}

pub(super) fn package_index_descriptor_path(
    data_dir: &Path,
    entry_name: &str,
    package_id: &str,
) -> PathBuf {
    package_index_path(data_dir, entry_name, package_id)
        .with_file_name(format!("{package_id}.source"))
}

pub(super) fn read_package_index_descriptor(
    path: &Path,
) -> Result<Option<PackageIndexDescriptor>, NugetError> {
    let Some(contents) = read_if_present(path)? else {
        return Ok(None);
    };
    Ok(serde_json::from_slice(&contents).ok())
}

pub(super) fn package_cache_metadata_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.meta.json",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ))
}

pub(super) fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, NugetError> {
    let Some(contents) = read_if_present(path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(NugetError::Serialization)
}

pub(super) fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
