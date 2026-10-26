use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use crate::{
    audit::{AuditDetails, AuditSink},
    config::RetentionConfig,
    logging::{self, LogFile, rotation},
    storage::layout,
};

const AUDIT_FILE: &str = "events.jsonl";
const AUDIT_ROTATED_PREFIX: &str = "events.";
const AUDIT_ROTATED_SUFFIX: &str = ".jsonl";
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

/// 各生态注册给保留清理的快照维护入口。
///
/// 回调返回 `(removed, failed)`；`retention` 不需要认识任何生态的缓存格式。
#[derive(Clone, Default)]
pub struct Maintainer(Option<Arc<MaintainerFn>>);

type MaintainerFn = dyn Fn(&Path, u64) -> Result<(u64, u64), String> + Send + Sync;

impl Maintainer {
    pub fn new(
        maintain: impl Fn(&Path, u64) -> Result<(u64, u64), String> + Send + Sync + 'static,
    ) -> Self {
        Self(Some(Arc::new(maintain)))
    }

    fn prune(&self, data_dir: &Path, keep_revisions: u64) -> Result<(u64, u64), String> {
        match &self.0 {
            Some(maintain) => maintain(data_dir, keep_revisions),
            None => Ok((0, 0)),
        }
    }
}

/// 启动后台清理任务：首次执行立即开始，之后按配置间隔重复。
pub fn spawn(
    data_dir: PathBuf,
    log_file: std::sync::Arc<LogFile>,
    config: RetentionConfig,
    maintainer: Maintainer,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(config.cleanup_interval_secs));
        loop {
            ticker.tick().await;
            let data_dir = data_dir.clone();
            let log_file = log_file.clone();
            let maintainer = maintainer.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || {
                let audit = AuditSink::new(data_dir.clone());
                sweep_with_maintainer(
                    &data_dir,
                    &log_file,
                    &config,
                    SystemTime::now(),
                    &audit,
                    &maintainer,
                );
            })
            .await
            {
                logging::error(format!("retention task failed: {error}"));
            }
        }
    });
}

/// 按保留窗口清理生态存储、审计文件和运行日志。
///
/// metadata 按 `metadata_retention_secs` 过期，制品按 `artifact_idle_secs` 淘汰，Rustup
/// 快照按 `rustup_snapshot_revisions` 保留，审计事件按天轮转并保留
/// `audit_retention_days` 天，运行日志按天轮转并保留 `log_retention_days` 天。
pub fn sweep(
    data_dir: &Path,
    log_file: &LogFile,
    config: &RetentionConfig,
    now: SystemTime,
    audit: &AuditSink,
) -> SweepReport {
    sweep_with_maintainer(
        data_dir,
        log_file,
        config,
        now,
        audit,
        &Maintainer::default(),
    )
}

/// 与 [`sweep`] 相同，并调用注入的快照维护入口。
pub fn sweep_with_maintainer(
    data_dir: &Path,
    log_file: &LogFile,
    config: &RetentionConfig,
    now: SystemTime,
    audit: &AuditSink,
    maintainer: &Maintainer,
) -> SweepReport {
    let mut report = SweepReport::default();
    rotate_and_prune_audit(
        &layout::audit_root(data_dir),
        config.audit_retention_days,
        now,
        &mut report,
    );
    let log_outcome = log_file.maintain(now, config.log_retention_days);
    report.rotated_log_file = log_outcome.rotated;
    report.pruned_log_files = log_outcome.pruned;
    report.log_failed = log_outcome.failed;
    clean_metadata(data_dir, config, now, &mut report);
    evict_idle_artifacts(data_dir, config, now, &mut report);
    match maintainer.prune(data_dir, config.rustup_snapshot_revisions) {
        Ok((removed, failed)) => {
            report.removed_snapshot_revisions += removed;
            report.failed_snapshot_revisions += failed;
        }
        Err(error) => {
            report.failed_snapshot_revisions += 1;
            logging::error(format!("snapshot cleanup failed: {error}"));
        }
    }
    record(audit, &report);
    report
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

fn clean_metadata(
    data_dir: &Path,
    config: &RetentionConfig,
    now: SystemTime,
    report: &mut SweepReport,
) {
    let Ok(entries) = fs::read_dir(layout::stores_root(data_dir)) else {
        return;
    };
    let Some(cutoff) = now.checked_sub(Duration::from_secs(config.metadata_retention_secs)) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let ecosystem = entry.file_name().to_string_lossy().into_owned();
        let root = entry.path().join(layout::METADATA_DIR);
        if root.is_dir() {
            // Binding files map artifacts to upstream identities. They are
            // artifact state and must outlive the metadata that references
            // them, so metadata retention deliberately leaves those
            // subdirectories untouched.
            let mut cleanup = Cleanup {
                ecosystem: &ecosystem,
                cutoff,
                label: "metadata",
                preserve_bindings: true,
                removed: &mut report.removed_metadata_files,
                failed: &mut report.failed_metadata_files,
            };
            remove_expired_files(&root, Path::new(""), &mut cleanup);
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
            let mut cleanup = Cleanup {
                ecosystem: "",
                cutoff,
                label: "artifact",
                preserve_bindings: false,
                removed: &mut report.removed_artifact_files,
                failed: &mut report.failed_artifact_files,
            };
            remove_expired_files(&root, Path::new(""), &mut cleanup);
        }
    }
}

struct Cleanup<'a> {
    ecosystem: &'a str,
    cutoff: SystemTime,
    label: &'a str,
    preserve_bindings: bool,
    removed: &'a mut u64,
    failed: &'a mut u64,
}

fn remove_expired_files(dir: &Path, relative: &Path, cleanup: &mut Cleanup<'_>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let relative = relative.join(entry.file_name());
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if cleanup.preserve_bindings && layout::is_binding_dir(cleanup.ecosystem, &relative) {
                continue;
            }
            remove_expired_files(&path, &relative, cleanup);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        if modified >= cleanup.cutoff {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => *cleanup.removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                *cleanup.failed += 1;
                logging::error(format!(
                    "{} cleanup failed for `{}`: {error}",
                    cleanup.label,
                    path.display()
                ));
            }
        }
    }
}

/// 跨天后把当前文件轮转为 `<前缀><日期><后缀>`，并删除超过保留期的轮转文件。
fn rotate_and_prune_audit(
    audit_dir: &Path,
    retention_days: u64,
    now: SystemTime,
    report: &mut SweepReport,
) {
    let (rotated, failed) = rotate_file(
        audit_dir,
        AUDIT_FILE,
        AUDIT_ROTATED_PREFIX,
        AUDIT_ROTATED_SUFFIX,
        now,
        "audit",
    );
    report.pruned_audit_files = prune_rotated_files(
        audit_dir,
        AUDIT_ROTATED_PREFIX,
        AUDIT_ROTATED_SUFFIX,
        retention_days,
        now,
        "audit",
    );
    report.rotated_audit_file = rotated;
    report.audit_failed = failed;
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
    let Some(label) = rotation::date_label(modified) else {
        return (None, false);
    };
    if rotation::date_label(now).as_deref() == Some(label.as_str()) {
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
        .and_then(rotation::date_label)
    else {
        return 0;
    };
    let mut pruned = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(label) = name
            .to_str()
            .and_then(|name| rotation::rotated_label(name, rotated_prefix, rotated_suffix))
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
