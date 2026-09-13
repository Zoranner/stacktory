use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

use stacktory::{RetentionConfig, audit::AuditSink, retention::sweep};
use tempfile::tempdir;

const DAY: u64 = 86_400;

fn retention_config() -> RetentionConfig {
    RetentionConfig {
        metadata_ttl_secs: 7 * DAY,
        audit_retention_days: 30,
        cleanup_interval_secs: 3_600,
    }
}

fn write_file_with_mtime(path: &Path, contents: &str, modified: SystemTime) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory");
    }
    fs::write(path, contents).expect("file contents");
    fs::File::options()
        .write(true)
        .open(path)
        .expect("open file")
        .set_times(fs::FileTimes::new().set_modified(modified))
        .expect("file modification time");
}

#[test]
fn removes_expired_metadata_and_keeps_fresh_files() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let expired_index = data_dir.join("cargo/metadata/group/index/left-pad");
    let expired_query = data_dir.join("vscode/metadata/group/marketplace/abc.json");
    let fresh_index = data_dir.join("cargo/metadata/group/index/serde");
    write_file_with_mtime(&expired_index, "old", now - Duration::from_secs(8 * DAY));
    write_file_with_mtime(&expired_query, "old", now - Duration::from_secs(8 * DAY));
    write_file_with_mtime(&fresh_index, "new", now - Duration::from_secs(60));

    let report = sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    assert_eq!(report.removed_metadata_files, 2);
    assert_eq!(report.failed_metadata_files, 0);
    assert!(!expired_index.exists());
    assert!(!expired_query.exists());
    assert!(fresh_index.exists());
}

#[test]
fn keeps_artifacts_and_rustup_snapshots() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let old = now - Duration::from_secs(60 * DAY);
    let artifact = data_dir.join("rustup/artifacts/group/objects/2026-08-20/rust-std.tar.xz");
    let crate_file = data_dir.join("cargo/artifacts/group/objects/crates/serde/1.0.0.crate");
    let snapshot = data_dir.join("rustup/snapshots/group/stable/revision/manifest.toml");
    write_file_with_mtime(&artifact, "artifact", old);
    write_file_with_mtime(&crate_file, "crate", old);
    write_file_with_mtime(&snapshot, "manifest", old);

    let report = sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    assert_eq!(report.removed_metadata_files, 0);
    assert!(artifact.exists());
    assert!(crate_file.exists());
    assert!(snapshot.exists());
}

#[test]
fn rotates_the_current_audit_file_and_prunes_retired_files() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let audit_dir = data_dir.join("audit");
    let current = audit_dir.join("events.jsonl");
    let retired = audit_dir.join("events.19700101.jsonl");
    let recent = audit_dir.join("events.29991231.jsonl");
    write_file_with_mtime(&current, "{}\n", now - Duration::from_secs(DAY));
    write_file_with_mtime(&retired, "{}\n", now - Duration::from_secs(DAY));
    write_file_with_mtime(&recent, "{}\n", now);

    let report = sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    let rotated = report.rotated_audit_file.expect("rotated audit file");
    assert!(rotated.starts_with("events.") && rotated.ends_with(".jsonl"));
    // 轮转保留原内容；本次清理事件会重新创建当前的 events.jsonl。
    assert_eq!(
        fs::read_to_string(audit_dir.join(rotated)).expect("rotated audit contents"),
        "{}\n"
    );
    assert!(
        !fs::read_to_string(&current)
            .unwrap_or_default()
            .contains("{}\n")
    );
    assert!(!retired.exists());
    assert!(recent.exists());
    assert_eq!(report.pruned_audit_files, 1);
}

#[test]
fn keeps_the_audit_file_written_today() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let current = data_dir.join("audit/events.jsonl");
    write_file_with_mtime(&current, "{}\n", now);

    let report = sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    assert_eq!(report.rotated_audit_file, None);
    assert!(current.exists());
}

#[test]
fn records_metadata_cleanup_in_the_audit_log() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let expired = data_dir.join("npm/metadata/group/left-pad/1.0.0-metadata.json");
    write_file_with_mtime(&expired, "old", now - Duration::from_secs(8 * DAY));

    sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    let events = fs::read_to_string(data_dir.join("audit/events.jsonl")).expect("audit events");
    assert!(events.contains("\"metadata_cleanup\""));
    assert!(events.contains("\"count\":1"));
}

#[test]
fn tolerates_a_missing_data_directory() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("missing");

    let report = sweep(
        &data_dir,
        &retention_config(),
        SystemTime::now(),
        &AuditSink::new(&data_dir),
    );

    assert_eq!(report, stacktory::retention::SweepReport::default());
}
