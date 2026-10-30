use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::{NugetError, PackageBinding, RegistrationDescriptor};
use crate::storage::atomic::atomic_write;

pub(super) fn registration_path(data_dir: &Path, entry_name: &str, relative: &str) -> PathBuf {
    crate::storage::layout::metadata_root(data_dir, "nuget")
        .join(entry_name)
        .join("registration")
        .join(relative)
}

pub(super) fn registration_descriptor_path(
    data_dir: &Path,
    entry_name: &str,
    relative: &str,
) -> PathBuf {
    let path = registration_path(data_dir, entry_name, relative);
    path.with_file_name(format!(
        "{}.source",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ))
}

fn binding_path(data_dir: &Path, entry_name: &str, package_id: &str, version: &str) -> PathBuf {
    crate::storage::layout::metadata_root(data_dir, "nuget")
        .join(entry_name)
        .join("bindings")
        .join(package_id.to_ascii_lowercase())
        .join(format!("{}.json", version.to_ascii_lowercase()))
}

pub(crate) fn persist_binding(
    data_dir: &Path,
    entry_name: &str,
    binding: &PackageBinding,
) -> Result<(), NugetError> {
    atomic_write(
        &binding_path(data_dir, entry_name, &binding.package_id, &binding.version),
        &serde_json::to_vec(binding)?,
    )?;
    Ok(())
}

pub(crate) fn read_binding(
    data_dir: &Path,
    entry_name: &str,
    package_id: &str,
    version: &str,
) -> Result<Option<PackageBinding>, NugetError> {
    let path = binding_path(data_dir, entry_name, package_id, version);
    let Some(contents) = read_if_present(&path)? else {
        return Ok(None);
    };
    let binding = serde_json::from_slice(&contents).map_err(NugetError::Serialization)?;
    let _ = crate::storage::access::note_access(&path);
    Ok(Some(binding))
}

pub(super) fn binding_key(package_id: &str, version: &str) -> String {
    format!(
        "{}\0{}",
        package_id.to_ascii_lowercase(),
        version.to_ascii_lowercase()
    )
}

pub(super) fn read_descriptor(path: &Path) -> Result<Option<RegistrationDescriptor>, NugetError> {
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
