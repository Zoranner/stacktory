//! 数据目录布局的唯一来源。
//!
//! ```text
//! <data_dir>/stores/<ecosystem>/metadata/...     入口拥有的元数据与来源绑定
//! <data_dir>/stores/<ecosystem>/artifacts/<来源>/objects/...  实际来源拥有的对象
//! <data_dir>/stores/rustup/snapshots/<入口>/<channel>/...      Rustup 快照
//! <data_dir>/audit/events.jsonl                  全局审计事件
//! ```
//!
//! 按生态分片的数据全部在 `stores` 下，只有审计这类全局运维数据直接位于
//! `<data_dir>`。适配器只负责在 `metadata` 或对象根之下拼接协议对象键。

use std::path::{Path, PathBuf};

pub const STORES_DIR: &str = "stores";
pub const METADATA_DIR: &str = "metadata";
pub const ARTIFACTS_DIR: &str = "artifacts";
pub const OBJECTS_DIR: &str = "objects";
pub const SNAPSHOTS_DIR: &str = "snapshots";
pub const AUDIT_DIR: &str = "audit";

/// 所有生态存储的根：`<data_dir>/stores`。
pub fn stores_root(data_dir: &Path) -> PathBuf {
    data_dir.join(STORES_DIR)
}

/// 某个生态的存储根：`<data_dir>/stores/<ecosystem>`。
pub fn store_root(data_dir: &Path, ecosystem: &str) -> PathBuf {
    stores_root(data_dir).join(ecosystem)
}

/// 入口拥有的元数据根：`<data_dir>/stores/<ecosystem>/metadata`。
pub fn metadata_root(data_dir: &Path, ecosystem: &str) -> PathBuf {
    store_root(data_dir, ecosystem).join(METADATA_DIR)
}

/// Whether `relative` (relative to the ecosystem's metadata root) names a
/// directory that maps client artifacts to upstream identities.
///
/// Binding files are artifact state, not refreshable metadata. A metadata
/// refresh may rebuild them, but metadata retention must not expire them on
/// its own: doing so makes a still-valid metadata entry advertise artifacts
/// that the artifact handler can no longer resolve.
pub fn is_binding_dir(ecosystem: &str, relative: &Path) -> bool {
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    match ecosystem {
        "npm" | "upm" => name == "descriptors",
        "nuget" => name == "bindings",
        "docker" => name == "blobs",
        "vscode" => matches!(name, "assets" | "resources" | "extensions" | "asset-bases"),
        _ => false,
    }
}

/// 来源拥有的对象根：`<data_dir>/stores/<ecosystem>/artifacts/<source>/objects`。
pub fn artifact_objects_root(data_dir: &Path, ecosystem: &str, source_name: &str) -> PathBuf {
    store_root(data_dir, ecosystem)
        .join(ARTIFACTS_DIR)
        .join(source_name)
        .join(OBJECTS_DIR)
}

/// 快照根：`<data_dir>/stores/<ecosystem>/snapshots`。
pub fn snapshots_root(data_dir: &Path, ecosystem: &str) -> PathBuf {
    store_root(data_dir, ecosystem).join(SNAPSHOTS_DIR)
}

/// 全局审计目录：`<data_dir>/audit`。
pub fn audit_root(data_dir: &Path) -> PathBuf {
    data_dir.join(AUDIT_DIR)
}
