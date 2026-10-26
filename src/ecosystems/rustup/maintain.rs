//! Rustup 快照保留入口，供启动装配注入给 `retention`。

use std::{fs, path::Path};

use super::{SnapshotStore, StorageError};

/// 按入口和 channel 清理超出保留窗口的快照修订。
pub fn prune_snapshots(data_dir: &Path, keep_revisions: u64) -> Result<(u64, u64), StorageError> {
    let root = crate::storage::layout::snapshots_root(data_dir, "rustup");
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok((0, 0));
    };
    let store = SnapshotStore::new(data_dir);
    let mut removed = 0;
    let mut failed = 0;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let entry_name = entry.file_name();
        let Some(entry_name) = entry_name.to_str() else {
            continue;
        };
        let Ok(channels) = fs::read_dir(entry.path()) else {
            continue;
        };
        for channel in channels.flatten() {
            if !channel.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let channel_name = channel.file_name();
            let Some(channel_name) = channel_name.to_str() else {
                continue;
            };
            match store.prune(entry_name, channel_name, keep_revisions) {
                Ok(pruned) => {
                    removed += pruned.removed_revisions;
                    failed += pruned.failed_revisions;
                }
                Err(error) => {
                    failed += 1;
                    crate::logging::error(format!("snapshot cleanup failed: {error}"));
                }
            }
        }
    }
    Ok((removed, failed))
}
