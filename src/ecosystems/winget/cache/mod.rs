use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::WingetError;
use crate::storage::atomic::atomic_write;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct InstallerBinding {
    pub(crate) package_id: String,
    pub(crate) version: String,
    pub(crate) installer_token: String,
    pub(crate) installer_sha256: String,
    pub(crate) installer_type: String,
    pub(crate) architecture: String,
    pub(crate) scope: String,
    pub(crate) installer_locale: String,
    pub(crate) installer_index: usize,
    pub(crate) request_query: Option<String>,
    pub(crate) selected_proxy: String,
    pub(crate) source_fingerprint: String,
    pub(crate) upstream_url: String,
    pub(crate) content_type: Option<String>,
    pub(crate) validated_at_unix_secs: u64,
}

pub(crate) fn binding_path(data_dir: &Path, entry_name: &str, token: &str) -> PathBuf {
    crate::storage::layout::metadata_root(data_dir, "winget")
        .join(entry_name)
        .join("installers")
        .join(format!("{token}.json"))
}

pub(crate) fn read_binding(
    data_dir: &Path,
    entry_name: &str,
    token: &str,
) -> Result<Option<InstallerBinding>, WingetError> {
    let path = binding_path(data_dir, entry_name, token);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(WingetError::Storage(error)),
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(|error| WingetError::InvalidBinding(error.to_string()))
}

pub(crate) fn write_binding(
    data_dir: &Path,
    entry_name: &str,
    binding: &InstallerBinding,
) -> Result<(), WingetError> {
    atomic_write(
        &binding_path(data_dir, entry_name, &binding.installer_token),
        &serde_json::to_vec(binding)?,
    )?;
    Ok(())
}

pub(crate) fn artifact_path(
    data_dir: &Path,
    selected_proxy: &str,
    entry_name: &str,
    package_id: &str,
    version: &str,
    token: &str,
) -> PathBuf {
    crate::storage::layout::artifact_objects_root(data_dir, "winget", selected_proxy)
        .join(entry_name)
        .join(package_id)
        .join(version)
        .join(token)
}

pub(crate) fn temporary_path(data_dir: &Path) -> Result<PathBuf, WingetError> {
    let directory = data_dir.join("tmp");
    fs::create_dir_all(&directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(directory.join(format!("winget-{}-{nonce}.part", std::process::id())))
}

pub(crate) fn verify_file_sha256(path: &Path, expected_sha256: &str) -> Result<bool, WingetError> {
    Ok(sha256_file(path)?.eq_ignore_ascii_case(expected_sha256))
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, WingetError> {
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

pub(crate) fn publish_verified_file(
    temporary: &Path,
    destination: &Path,
) -> Result<u64, WingetError> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("Winget artifact cache has no parent directory"))?;
    if destination.exists() {
        fs::remove_file(destination)?;
    }
    if let Err(error) = fs::create_dir_all(parent).and_then(|()| fs::rename(temporary, destination))
    {
        let _ = fs::remove_file(temporary);
        return Err(WingetError::Storage(error));
    }
    Ok(fs::metadata(destination)?.len())
}

pub(crate) fn remove_if_present(path: &Path) -> Result<(), WingetError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(WingetError::Storage(error)),
    }
}
