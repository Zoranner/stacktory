use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    audit::{AuditDetails, AuditSink},
    config::RetentionConfig,
    storage::SnapshotStore,
};

const AUDIT_FILE: &str = "events.jsonl";
const AUDIT_ROTATED_PREFIX: &str = "events.";
const AUDIT_ROTATED_SUFFIX: &str = ".jsonl";
const METADATA_DIR: &str = "metadata";
const SECS_PER_DAY: u64 = 86_400;
/// 只有 Rustup 和 Cargo 经历过"来源拥有的对象缓存"迁移，旧布局只可能出现在这两处。
const LEGACY_ARTIFACT_ECOSYSTEMS: [&str; 2] = ["rustup", "cargo"];

#[derive(Debug, Default, Eq, PartialEq)]
pub struct SweepReport {
    pub removed_metadata_files: u64,
    pub failed_metadata_files: u64,
    pub removed_snapshot_revisions: u64,
    pub failed_snapshot_revisions: u64,
    pub rotated_audit_file: Option<String>,
    pub pruned_audit_files: u64,
    pub audit_failed: bool,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct LegacyArtifactReport {
    pub removed_entries: u64,
    pub removed_bytes: u64,
    pub failed_entries: u64,
    pub dry_run: bool,
}

/// 启动后台清理任务：首次执行立即开始，之后按配置间隔重复。
pub fn spawn(data_dir: PathBuf, config: RetentionConfig) {
    tokio::spawn(async move {
        let audit = AuditSink::new(data_dir.clone());
        let mut ticker = tokio::time::interval(Duration::from_secs(config.cleanup_interval_secs));
        loop {
            ticker.tick().await;
            sweep(&data_dir, &config, SystemTime::now(), &audit);
        }
    });
}

/// 删除过期的生态 metadata 文件，轮转并清理审计事件文件。
///
/// 只处理 `<data_dir>/<ecosystem>/metadata` 下的缓存文件和 `<data_dir>/audit` 下的
/// 审计文件；制品和 Rustup 快照不在本次清理范围内。
pub fn sweep(
    data_dir: &Path,
    config: &RetentionConfig,
    now: SystemTime,
    audit: &AuditSink,
) -> SweepReport {
    let mut report = SweepReport::default();
    let audit_dir = data_dir.join("audit");
    if let Err(error) = rotate_audit(&audit_dir, now, &mut report) {
        report.audit_failed = true;
        eprintln!("Stacktory audit rotation failed: {error}");
    }
    prune_audit(&audit_dir, config, now, &mut report);
    clean_metadata(data_dir, config, now, &mut report);
    prune_rustup_snapshots(data_dir, config.rustup_snapshot_revisions, &mut report);
    record(audit, &report);
    report
}

/// 删除 Rustup 和 Cargo 来源目录下 `objects` 之外的旧布局子树。
///
/// 该清理只在显式执行维护命令时运行，不属于服务启动和周期清理的一部分。
pub fn prune_legacy_artifacts(
    data_dir: &Path,
    dry_run: bool,
    audit: &AuditSink,
) -> LegacyArtifactReport {
    let mut report = LegacyArtifactReport {
        dry_run,
        ..LegacyArtifactReport::default()
    };
    for ecosystem in LEGACY_ARTIFACT_ECOSYSTEMS {
        let root = data_dir.join(ecosystem).join("artifacts");
        let Ok(sources) = fs::read_dir(&root) else {
            continue;
        };
        for source in sources.flatten() {
            if !source.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(entries) = fs::read_dir(source.path()) else {
                continue;
            };
            let mut removed_from_source = 0;
            for entry in entries.flatten() {
                if entry.file_name() == OsStr::new("objects") {
                    continue;
                }
                let path = entry.path();
                let bytes = subtree_bytes(&path);
                if dry_run {
                    report.removed_entries += 1;
                    report.removed_bytes += bytes;
                    removed_from_source += 1;
                    continue;
                }
                let removed = match entry.file_type() {
                    Ok(kind) if kind.is_dir() => fs::remove_dir_all(&path),
                    Ok(_) => fs::remove_file(&path),
                    Err(error) => Err(error),
                };
                match removed {
                    Ok(()) => {
                        report.removed_entries += 1;
                        report.removed_bytes += bytes;
                        removed_from_source += 1;
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        report.failed_entries += 1;
                        eprintln!(
                            "Stacktory legacy artifact cleanup failed for `{}`: {error}",
                            path.display()
                        );
                    }
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

fn record_legacy(audit: &AuditSink, report: &LegacyArtifactReport) {
    if report.dry_run || (report.removed_entries == 0 && report.failed_entries == 0) {
        return;
    }
    let failed = report.failed_entries > 0;
    write_event(
        audit,
        "legacy_artifact_cleanup",
        "artifact",
        if failed { "partial" } else { "success" },
        AuditDetails {
            object_type: Some("artifact"),
            cache_status: Some("removed"),
            failure_category: failed.then_some("remove_failed"),
            count: Some(report.removed_entries),
            ..AuditDetails::default()
        },
    );
}

fn prune_rustup_snapshots(data_dir: &Path, keep_revisions: u64, report: &mut SweepReport) {
    let root = data_dir.join("rustup").join("snapshots");
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
                    eprintln!("Stacktory snapshot cleanup failed: {error}");
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
    let Ok(entries) = fs::read_dir(data_dir) else {
        return;
    };
    let Some(cutoff) = now.checked_sub(Duration::from_secs(config.metadata_ttl_secs)) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let root = entry.path().join(METADATA_DIR);
        if root.is_dir() {
            remove_expired(&root, cutoff, report);
        }
    }
}

fn remove_expired(dir: &Path, cutoff: SystemTime, report: &mut SweepReport) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            remove_expired(&path, cutoff, report);
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
            Ok(()) => report.removed_metadata_files += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                report.failed_metadata_files += 1;
                eprintln!(
                    "Stacktory metadata cleanup failed for `{}`: {error}",
                    path.display()
                );
            }
        }
    }
}

fn rotate_audit(audit_dir: &Path, now: SystemTime, report: &mut SweepReport) -> io::Result<()> {
    let current = audit_dir.join(AUDIT_FILE);
    let modified = match fs::metadata(&current) {
        Ok(metadata) => metadata.modified()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let Some(label) = date_label(modified) else {
        return Ok(());
    };
    if date_label(now).as_deref() == Some(label.as_str()) {
        return Ok(());
    }
    fs::rename(
        &current,
        audit_dir.join(format!(
            "{AUDIT_ROTATED_PREFIX}{label}{AUDIT_ROTATED_SUFFIX}"
        )),
    )?;
    report.rotated_audit_file = Some(format!(
        "{AUDIT_ROTATED_PREFIX}{label}{AUDIT_ROTATED_SUFFIX}"
    ));
    Ok(())
}

fn prune_audit(
    audit_dir: &Path,
    config: &RetentionConfig,
    now: SystemTime,
    report: &mut SweepReport,
) {
    let Ok(entries) = fs::read_dir(audit_dir) else {
        return;
    };
    let Some(cutoff) = now
        .checked_sub(Duration::from_secs(
            config.audit_retention_days.saturating_mul(SECS_PER_DAY),
        ))
        .and_then(date_label)
    else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(label) = name.to_str().and_then(rotated_label) else {
            continue;
        };
        if label >= cutoff {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => report.pruned_audit_files += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                report.audit_failed = true;
                eprintln!("Stacktory audit cleanup failed: {error}");
            }
        }
    }
}

fn rotated_label(name: &str) -> Option<String> {
    let label = name
        .strip_prefix(AUDIT_ROTATED_PREFIX)?
        .strip_suffix(AUDIT_ROTATED_SUFFIX)?;
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
}

fn write_event(
    audit: &AuditSink,
    event: &str,
    target: &str,
    outcome: &str,
    details: AuditDetails<'_>,
) {
    if let Err(error) = audit.record_detailed(event, target, outcome, details) {
        eprintln!("Stacktory audit write failed: {error}");
    }
}

fn date_label(time: SystemTime) -> Option<String> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let (year, month, day) = civil_from_days((seconds / SECS_PER_DAY) as i64);
    Some(format!("{year:04}{month:02}{day:02}"))
}

/// 把 1970-01-01 起的天数转换为公历日期（Howard Hinnant 的 civil_from_days）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, date_label, rotated_label};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn converts_epoch_days_to_calendar_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_654), (2026, 7, 20));
    }

    #[test]
    fn formats_dates_in_utc() {
        let time = UNIX_EPOCH + Duration::from_secs(1_784_000_000);
        assert_eq!(date_label(time).as_deref(), Some("20260714"));
    }

    #[test]
    fn accepts_only_dated_audit_names() {
        assert_eq!(
            rotated_label("events.20260913.jsonl").as_deref(),
            Some("20260913")
        );
        assert_eq!(rotated_label("events.jsonl"), None);
        assert_eq!(rotated_label("events.2026.jsonl"), None);
    }
}
