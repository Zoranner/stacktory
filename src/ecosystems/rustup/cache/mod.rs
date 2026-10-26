//! Rustup 快照与制品的本地缓存。

mod artifacts;
mod snapshots;

use std::io;

pub use artifacts::{ArtifactCache, ArtifactFile};
pub use snapshots::{PrunedSnapshots, SnapshotStore, StoredSnapshot};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("storage I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("storage metadata cannot be parsed: {0}")]
    MetadataParse(#[from] toml::de::Error),
    #[error("storage metadata cannot be serialized: {0}")]
    MetadataSerialize(#[from] toml::ser::Error),
    #[error("rustup channel `{0}` is invalid")]
    InvalidChannel(String),
    #[error("rustup source name `{0}` is invalid")]
    InvalidSourceName(String),
    #[error("no published rustup snapshot exists for channel `{0}`")]
    MissingSnapshot(String),
    #[error("snapshot checksum `{0}` is invalid")]
    InvalidChecksum(String),
    #[error("current snapshot for channel `{0}` is inconsistent")]
    InconsistentSnapshot(String),
    #[error("current snapshot for channel `{0}` uses an incompatible artifact index")]
    IncompatibleSnapshot(String),
    #[error("artifact `{0}` does not match its expected SHA-256")]
    ArtifactHashMismatch(String),
}

fn validate_source_name(source_name: &str) -> Result<(), StorageError> {
    if !source_name.is_empty()
        && source_name != "."
        && source_name != ".."
        && source_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(StorageError::InvalidSourceName(source_name.to_owned()))
    }
}

fn validate_channel(channel: &str) -> Result<(), StorageError> {
    if channel.is_empty()
        || channel == "."
        || channel == ".."
        || !channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err(StorageError::InvalidChannel(channel.to_owned()));
    }
    Ok(())
}

fn validate_checksum(checksum: &str) -> Result<(), StorageError> {
    if checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(StorageError::InvalidChecksum(checksum.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_channel, validate_source_name};

    #[test]
    fn rejects_dot_path_segments() {
        assert!(validate_source_name(".").is_err());
        assert!(validate_source_name("..").is_err());
        assert!(validate_channel(".").is_err());
        assert!(validate_channel("..").is_err());
        assert!(validate_source_name("official").is_ok());
        assert!(validate_channel("stable").is_ok());
    }
}
