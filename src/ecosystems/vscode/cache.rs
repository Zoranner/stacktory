use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{VscodeError, VscodeService, sha256};
use crate::storage::atomic::atomic_write;

#[derive(Deserialize, Serialize)]
pub(super) struct AssetCacheMetadata {
    pub(super) content_type: Option<String>,
}

impl VscodeService {
    fn marketplace_root(&self, entry_name: &str) -> PathBuf {
        crate::storage::layout::metadata_root(&self.data_dir, "vscode")
            .join(entry_name)
            .join("marketplace")
    }

    pub(super) fn query_path(&self, entry_name: &str, request_body: &[u8]) -> PathBuf {
        self.marketplace_root(entry_name)
            .join(format!("{}.json", sha256(request_body)))
    }

    pub(super) fn asset_descriptor_path(&self, entry_name: &str, asset_id: &str) -> PathBuf {
        self.marketplace_root(entry_name)
            .join("assets")
            .join(format!("{asset_id}.toml"))
    }

    pub(super) fn resource_descriptor_path(
        &self,
        entry_name: &str,
        extension_key: &str,
    ) -> PathBuf {
        self.marketplace_root(entry_name)
            .join("resources")
            .join(format!("{extension_key}.toml"))
    }

    pub(super) fn extension_descriptor_path(
        &self,
        entry_name: &str,
        extension_key: &str,
    ) -> PathBuf {
        self.marketplace_root(entry_name)
            .join("extensions")
            .join(format!("{extension_key}.toml"))
    }

    pub(super) fn asset_base_descriptor_path(
        &self,
        entry_name: &str,
        asset_base_id: &str,
    ) -> PathBuf {
        self.marketplace_root(entry_name)
            .join("asset-bases")
            .join(format!("{asset_base_id}.toml"))
    }

    pub(super) fn asset_path(&self, source_name: &str, asset_id: &str) -> PathBuf {
        crate::storage::layout::artifact_objects_root(&self.data_dir, "vscode", source_name)
            .join("marketplace")
            .join(asset_id)
    }

    pub(super) fn asset_cache_metadata_path(&self, source_name: &str, asset_id: &str) -> PathBuf {
        self.asset_path(source_name, asset_id)
            .with_file_name(format!("{asset_id}.toml"))
    }

    pub(super) fn write_descriptor<T: Serialize>(
        &self,
        path: &Path,
        descriptor: &T,
    ) -> Result<(), VscodeError> {
        atomic_write(path, toml::to_string(descriptor)?.as_bytes())?;
        Ok(())
    }

    pub(super) fn read_asset_content_type(
        &self,
        source_name: &str,
        asset_id: &str,
    ) -> Result<Option<String>, VscodeError> {
        let path = self.asset_cache_metadata_path(source_name, asset_id);
        let Some(contents) = read_if_present(&path)? else {
            return Ok(None);
        };
        Ok(toml::from_str::<AssetCacheMetadata>(&String::from_utf8_lossy(&contents))?.content_type)
    }

    pub(super) fn write_asset_cache_metadata(
        &self,
        source_name: &str,
        asset_id: &str,
        content_type: Option<String>,
    ) -> Result<(), VscodeError> {
        atomic_write(
            &self.asset_cache_metadata_path(source_name, asset_id),
            toml::to_string(&AssetCacheMetadata { content_type })?.as_bytes(),
        )?;
        Ok(())
    }
}

pub(super) fn publish_asset_file(
    temporary_path: &Path,
    destination: &Path,
) -> Result<(PathBuf, u64), io::Error> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("VS Code asset cache has no parent directory"))?;
    if let Err(error) =
        fs::create_dir_all(parent).and_then(|()| fs::rename(temporary_path, destination))
    {
        let _ = fs::remove_file(temporary_path);
        return Err(error);
    }
    Ok((destination.to_path_buf(), fs::metadata(destination)?.len()))
}

pub(super) fn temporary_asset_path(data_dir: &Path) -> Result<PathBuf, io::Error> {
    let directory = data_dir.join("tmp");
    fs::create_dir_all(&directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(directory.join(format!("vscode-{}-{nonce}.part", std::process::id())))
}

pub(super) fn cleanup_temporary_assets(data_dir: &Path) {
    let directory = data_dir.join("tmp");
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("vscode-") && name.ends_with(".part") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn is_stale_compressed_cache_file(
    path: &Path,
    content_type: Option<&str>,
) -> Result<bool, io::Error> {
    use std::io::Read;

    let mut file = fs::File::open(path)?;
    let mut prefix = [0_u8; 3];
    let mut read = 0;
    while read < prefix.len() {
        let count = file.read(&mut prefix[read..])?;
        if count == 0 {
            break;
        }
        read += count;
    }
    Ok(is_stale_compressed_cache(&prefix[..read], content_type))
}

pub(super) fn is_stale_compressed_cache(bytes: &[u8], content_type: Option<&str>) -> bool {
    if !bytes.starts_with(&[0x1f, 0x8b, 0x08]) {
        return false;
    }
    let Some(content_type) = content_type else {
        return true;
    };
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    !matches!(media_type, "application/gzip" | "application/x-gzip")
}

#[cfg(test)]
mod tests {
    use super::is_stale_compressed_cache;

    #[test]
    fn gzip_bytes_with_json_content_type_are_stale() {
        assert!(is_stale_compressed_cache(
            &[0x1f, 0x8b, 0x08],
            Some("application/json")
        ));
    }

    #[test]
    fn gzip_bytes_with_gzip_content_type_are_valid() {
        assert!(!is_stale_compressed_cache(
            &[0x1f, 0x8b, 0x08],
            Some("application/gzip")
        ));
    }
}
