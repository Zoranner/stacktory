use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::{
    audit::{AuditDetails, AuditSink},
    config::RetentionConfig,
    layout, logging,
    storage::SnapshotStore,
    timeformat,
};

const AUDIT_FILE: &str = "events.jsonl";
const AUDIT_ROTATED_PREFIX: &str = "events.";
const AUDIT_ROTATED_SUFFIX: &str = ".jsonl";
const LOG_ROTATED_PREFIX: &str = "stacktory.";
const LOG_ROTATED_SUFFIX: &str = ".log";
const SECS_PER_DAY: u64 = 86_400;
/// 平铺布局时期的生态目录特征：至少包含其中一种子目录。
const ECOSYSTEM_TREE_MARKERS: [&str; 3] = [
    layout::METADATA_DIR,
    layout::ARTIFACTS_DIR,
    layout::SNAPSHOTS_DIR,
];

#[derive(Debug, Default, Eq, PartialEq)]
pub struct SweepReport {
    pub removed_metadata_files: u64,
    pub failed_metadata_files: u64,
    pub removed_artifact_files: u64,
    pub failed_artifact_files: u64,
    pub removed_snapshot_revisions: u64,
    pub failed_snapshot_revisions: u64,
    pub rotated_audit_file: Option<String>,
    pub pruned_audit_files: u64,
    pub audit_failed: bool,
    pub rotated_log_file: Option<String>,
    pub pruned_log_files: u64,
    pub log_failed: bool,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct LegacyLayoutReport {
    pub removed_entries: u64,
    pub removed_bytes: u64,
    pub failed_entries: u64,
    pub dry_run: bool,
}

/// 启动后台清理任务：首次执行立即开始，之后按配置间隔重复。
pub fn spawn(data_dir: PathBuf, log_dir: PathBuf, config: RetentionConfig) {
    tokio::spawn(async move {
        let audit = AuditSink::new(data_dir.clone());
        let mut ticker = tokio::time::interval(Duration::from_secs(config.cleanup_interval_secs));
        loop {
            ticker.tick().await;
            sweep(&data_dir, &log_dir, &config, SystemTime::now(), &audit);
        }
    });
}

/// 按保留窗口清理生态存储、审计文件和运行日志。
///
/// metadata 按 `metadata_ttl_secs` 过期，制品按 `artifact_idle_secs` 淘汰，Rustup
/// 快照按 `rustup_snapshot_revisions` 保留，审计事件按天轮转并保留
/// `audit_retention_days` 天，运行日志按天轮转并保留 `log_retention_days` 天。
pub fn sweep(
    data_dir: &Path,
    log_dir: &Path,
    config: &RetentionConfig,
    now: SystemTime,
    audit: &AuditSink,
) -> SweepReport {
    let mut report = SweepReport::default();
    rotate_and_prune(
        &layout::audit_root(data_dir),
        AUDIT_FILE,
        AUDIT_ROTATED_PREFIX,
        AUDIT_ROTATED_SUFFIX,
        config.audit_retention_days,
        now,
        "audit",
        &mut report,
    );
    rotate_and_prune(
        log_dir,
        logging::LOG_FILE,
        LOG_ROTATED_PREFIX,
        LOG_ROTATED_SUFFIX,
        config.log_retention_days,
        now,
        "log",
        &mut report,
    );
    clean_metadata(data_dir, config, now, &mut report);
    evict_idle_artifacts(data_dir, config, now, &mut report);
    prune_rustup_snapshots(data_dir, config.rustup_snapshot_revisions, &mut report);
    record(audit, &report);
    report
}

/// 更新对象访问时间。闲置淘汰以该时间为准，命中缓存的读取路径调用它。
pub fn note_access(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_times(fs::FileTimes::new().set_modified(SystemTime::now()))
}

/// 删除旧数据布局留下的、当前代码不会再读取的缓存。
///
/// 规则：
/// 1. 平铺布局时期的生态目录（`<data_dir>/<ecosystem>/…`）：整棵删除。
/// 2. `<data_dir>/stores/<ecosystem>/artifacts/<来源>/` 下 `objects` 之外的条目：
///    删除，覆盖"来源目录直接存放对象"和 Docker 曾把仓库名放进对象路径的旧形态。
///
/// 该清理只在显式执行维护命令时运行，不属于服务启动和周期清理的一部分。
pub fn prune_legacy_layout(
    data_dir: &Path,
    dry_run: bool,
    audit: &AuditSink,
) -> LegacyLayoutReport {
    let mut report = LegacyLayoutReport {
        dry_run,
        ..LegacyLayoutReport::default()
    };
    for entry in read_dirs(data_dir) {
        if entry.file_name() == OsStr::new(layout::STORES_DIR)
            || entry.file_name() == OsStr::new(layout::AUDIT_DIR)
        {
            continue;
        }
        if !is_ecosystem_tree(&entry.path()) {
            continue;
        }
        remove_entry(&entry.path(), dry_run, &mut report);
    }
    for ecosystem in read_dirs(&layout::stores_root(data_dir)) {
        for source in read_dirs(&ecosystem.path().join(layout::ARTIFACTS_DIR)) {
            let mut removed_from_source = 0;
            for entry in read_dirs(&source.path()) {
                if entry.file_name() == OsStr::new(layout::OBJECTS_DIR) {
                    continue;
                }
                if remove_entry(&entry.path(), dry_run, &mut report) {
                    removed_from_source += 1;
                }
            }
            if !dry_run && removed_from_source > 0 && is_empty_dir(&source.path()) {
                let _ = fs::remove_dir(source.path());
            }
        }
    }
    record_legacy(audit, &report);
    report
}

fn read_dirs(path: &Path) -> Vec<fs::DirEntry> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect()
}

/// 平铺布局的生态目录特征：直接包含 `metadata`、`artifacts` 或 `snapshots`。
fn is_ecosystem_tree(path: &Path) -> bool {
    ECOSYSTEM_TREE_MARKERS
        .iter()
        .any(|marker| path.join(marker).is_dir())
}

/// 删除条目并记录到报告；返回是否删除成功（`--dry-run` 视为已处理）。
fn remove_entry(path: &Path, dry_run: bool, report: &mut LegacyLayoutReport) -> bool {
    let bytes = subtree_bytes(path);
    if dry_run {
        report.removed_entries += 1;
        report.removed_bytes += bytes;
        return true;
    }
    let removed = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match removed {
        Ok(()) => {
            report.removed_entries += 1;
            report.removed_bytes += bytes;
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            report.failed_entries += 1;
            logging::error(format!(
                "legacy layout cleanup failed for `{}`: {error}",
                path.display()
            ));
            false
        }
    }
}

fn is_empty_dir(path: &Path) -> bool {
    fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

fn subtree_bytes(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| subtree_bytes(&entry.path()))
        .sum()
}

fn record_legacy(audit: &AuditSink, report: &LegacyLayoutReport) {
    if report.dry_run || (report.removed_entries == 0 && report.failed_entries == 0) {
        return;
    }
    let failed = report.failed_entries > 0;
    write_event(
        audit,
        "legacy_layout_cleanup",
        "layout",
        if failed { "partial" } else { "success" },
        AuditDetails {
            object_type: Some("layout"),
            cache_status: Some("removed"),
            failure_category: failed.then_some("remove_failed"),
            count: Some(report.removed_entries),
            ..AuditDetails::default()
        },
    );
}

fn prune_rustup_snapshots(data_dir: &Path, keep_revisions: u64, report: &mut SweepReport) {
    let root = layout::snapshots_root(data_dir, "rustup");
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    let store = SnapshotStore::new(data_dir);
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
                    report.removed_snapshot_revisions += pruned.removed_revisions;
                    report.failed_snapshot_revisions += pruned.failed_revisions;
                }
                Err(error) => {
                    report.failed_snapshot_revisions += 1;
                    logging::error(format!("snapshot cleanup failed: {error}"));
                }
            }
        }
    }
}

fn clean_metadata(
    data_dir: &Path,
    config: &RetentionConfig,
    now: SystemTime,
    report: &mut SweepReport,
) {
    let Ok(entries) = fs::read_dir(layout::stores_root(data_dir)) else {
        return;
    };
    let Some(cutoff) = now.checked_sub(Duration::from_secs(config.metadata_ttl_secs)) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let root = entry.path().join(layout::METADATA_DIR);
        if root.is_dir() {
            remove_expired_files(
                &root,
                cutoff,
                "metadata",
                &mut report.removed_metadata_files,
                &mut report.failed_metadata_files,
            );
        }
    }
}

fn evict_idle_artifacts(
    data_dir: &Path,
    config: &RetentionConfig,
    now: SystemTime,
    report: &mut SweepReport,
) {
    if config.artifact_idle_secs == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(layout::stores_root(data_dir)) else {
        return;
    };
    let Some(cutoff) = now.checked_sub(Duration::from_secs(config.artifact_idle_secs)) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let root = entry.path().join(layout::ARTIFACTS_DIR);
        if root.is_dir() {
            remove_expired_files(
                &root,
                cutoff,
                "artifact",
                &mut report.removed_artifact_files,
                &mut report.failed_artifact_files,
            );
        }
    }
}

fn remove_expired_files(
    dir: &Path,
    cutoff: SystemTime,
    label: &str,
    removed: &mut u64,
    failed: &mut u64,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            remove_expired_files(&path, cutoff, label, removed, failed);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        if modified >= cutoff {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => *removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                *failed += 1;
                logging::error(format!(
                    "{label} cleanup failed for `{}`: {error}",
                    path.display()
                ));
            }
        }
    }
}

/// 跨天后把当前文件轮转为 `<前缀><日期><后缀>`，并删除超过保留期的轮转文件。
#[allow(clippy::too_many_arguments)]
fn rotate_and_prune(
    dir: &Path,
    file_name: &str,
    rotated_prefix: &str,
    rotated_suffix: &str,
    retention_days: u64,
    now: SystemTime,
    kind: &str,
    report: &mut SweepReport,
) {
    let (rotated, failed) = rotate_file(dir, file_name, rotated_prefix, rotated_suffix, now, kind);
    let pruned = prune_rotated_files(
        dir,
        rotated_prefix,
        rotated_suffix,
        retention_days,
        now,
        kind,
    );
    let rotated_failed = rotated.is_none() && failed;
    match kind {
        "audit" => {
            report.rotated_audit_file = rotated;
            report.pruned_audit_files = pruned;
            report.audit_failed = rotated_failed;
        }
        _ => {
            report.rotated_log_file = rotated;
            report.pruned_log_files = pruned;
            report.log_failed = rotated_failed;
        }
    }
}

fn rotate_file(
    dir: &Path,
    file_name: &str,
    rotated_prefix: &str,
    rotated_suffix: &str,
    now: SystemTime,
    kind: &str,
) -> (Option<String>, bool) {
    let current = dir.join(file_name);
    let modified = match fs::metadata(&current) {
        Ok(metadata) => metadata.modified(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (None, false),
        Err(error) => {
            logging::error(format!("{kind} rotation failed: {error}"));
            return (None, true);
        }
    };
    let Ok(modified) = modified else {
        return (None, false);
    };
    let Some(label) = timeformat::date_label(modified) else {
        return (None, false);
    };
    if timeformat::date_label(now).as_deref() == Some(label.as_str()) {
        return (None, false);
    }
    let rotated = format!("{rotated_prefix}{label}{rotated_suffix}");
    match fs::rename(&current, dir.join(&rotated)) {
        Ok(()) => (Some(rotated), false),
        Err(error) => {
            logging::error(format!("{kind} rotation failed: {error}"));
            (None, true)
        }
    }
}

fn prune_rotated_files(
    dir: &Path,
    rotated_prefix: &str,
    rotated_suffix: &str,
    retention_days: u64,
    now: SystemTime,
    kind: &str,
) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let Some(cutoff) = now
        .checked_sub(Duration::from_secs(
            retention_days.saturating_mul(SECS_PER_DAY),
        ))
        .and_then(timeformat::date_label)
    else {
        return 0;
    };
    let mut pruned = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(label) = name
            .to_str()
            .and_then(|name| rotated_label(name, rotated_prefix, rotated_suffix))
        else {
            continue;
        };
        if label >= cutoff {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => pruned += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => logging::error(format!("{kind} cleanup failed: {error}")),
        }
    }
    pruned
}

fn rotated_label(name: &str, rotated_prefix: &str, rotated_suffix: &str) -> Option<String> {
    let label = name
        .strip_prefix(rotated_prefix)?
        .strip_suffix(rotated_suffix)?;
    (label.len() == 8 && label.bytes().all(|byte| byte.is_ascii_digit())).then(|| label.to_owned())
}

fn record(audit: &AuditSink, report: &SweepReport) {
    if report.removed_metadata_files > 0 || report.failed_metadata_files > 0 {
        let failed = report.failed_metadata_files > 0;
        write_event(
            audit,
            "metadata_cleanup",
            "metadata",
            if failed { "partial" } else { "success" },
            AuditDetails {
                object_type: Some("metadata"),
                cache_status: Some("expired"),
                failure_category: failed.then_some("remove_failed"),
                count: Some(report.removed_metadata_files),
                ..AuditDetails::default()
            },
        );
    }
    if report.rotated_audit_file.is_some() || report.pruned_audit_files > 0 || report.audit_failed {
        write_event(
            audit,
            "audit_cleanup",
            "audit",
            if report.audit_failed {
                "partial"
            } else {
                "success"
            },
            AuditDetails {
                object_key: report.rotated_audit_file.as_deref(),
                object_type: Some("audit"),
                cache_status: Some("rotated"),
                failure_category: report.audit_failed.then_some("cleanup_failed"),
                count: Some(report.pruned_audit_files),
                ..AuditDetails::default()
            },
        );
    }
    if report.removed_snapshot_revisions > 0 || report.failed_snapshot_revisions > 0 {
        let failed = report.failed_snapshot_revisions > 0;
        write_event(
            audit,
            "snapshot_cleanup",
            "snapshot",
            if failed { "partial" } else { "success" },
            AuditDetails {
                object_type: Some("snapshot"),
                cache_status: Some("expired"),
                failure_category: failed.then_some("remove_failed"),
                count: Some(report.removed_snapshot_revisions),
                ..AuditDetails::default()
            },
        );
    }
    if report.removed_artifact_files > 0 || report.failed_artifact_files > 0 {
        let failed = report.failed_artifact_files > 0;
        write_event(
            audit,
            "artifact_cleanup",
            "artifact",
            if failed { "partial" } else { "success" },
            AuditDetails {
                object_type: Some("artifact"),
                cache_status: Some("idle"),
                failure_category: failed.then_some("remove_failed"),
                count: Some(report.removed_artifact_files),
                ..AuditDetails::default()
            },
        );
    }
    if report.rotated_log_file.is_some() || report.pruned_log_files > 0 || report.log_failed {
        write_event(
            audit,
            "log_cleanup",
            "log",
            if report.log_failed {
                "partial"
            } else {
                "success"
            },
            AuditDetails {
                object_key: report.rotated_log_file.as_deref(),
                object_type: Some("log"),
                cache_status: Some("rotated"),
                failure_category: report.log_failed.then_some("cleanup_failed"),
                count: Some(report.pruned_log_files),
                ..AuditDetails::default()
            },
        );
    }
}

fn write_event(
    audit: &AuditSink,
    event: &str,
    target: &str,
    outcome: &str,
    details: AuditDetails<'_>,
) {
    if let Err(error) = audit.record_detailed(event, target, outcome, details) {
        logging::error(format!("audit write failed: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::rotated_label;

    #[test]
    fn accepts_only_dated_audit_names() {
        assert_eq!(
            rotated_label("events.20260913.jsonl", "events.", ".jsonl").as_deref(),
            Some("20260913")
        );
        assert_eq!(rotated_label("events.jsonl", "events.", ".jsonl"), None);
        assert_eq!(
            rotated_label("events.2026.jsonl", "events.", ".jsonl"),
            None
        );
        assert_eq!(
            rotated_label("stacktory.20260913.log", "stacktory.", ".log").as_deref(),
            Some("20260913")
        );
    }
}
