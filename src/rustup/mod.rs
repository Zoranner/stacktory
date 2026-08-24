mod manifest;
mod path;

pub use manifest::{ManifestPolicy, RewrittenManifest, rewrite_manifest};
pub use path::DistPath;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RustupError {
    #[error("rustup dist path `{0}` is invalid")]
    InvalidDistPath(String),
    #[error("rustup manifest is invalid TOML: {0}")]
    ManifestParse(#[from] toml::de::Error),
    #[error("rewritten rustup manifest cannot be serialized: {0}")]
    ManifestSerialize(#[from] toml::ser::Error),
    #[error("rustup manifest URL `{0}` is not in a configured upstream dist path")]
    UnconfiguredManifestUrl(String),
    #[error("rustup manifest URL field `{0}` is not a string")]
    InvalidUrlField(String),
    #[error("rustup manifest URL field `{0}` has no matching SHA-256 field `{1}")]
    MissingArtifactHash(String, String),
    #[error("rustup manifest hash field `{0}` is not a SHA-256 hex value")]
    InvalidArtifactHash(String),
    #[error("rustup manifest assigns conflicting SHA-256 values to `{0}`")]
    ConflictingArtifactHash(String),
}
