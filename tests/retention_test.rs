use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

use stacktory::{
    RetentionConfig,
    audit::AuditSink,
    retention::{note_access, prune_legacy_layout, sweep},
};
use tempfile::tempdir;

const DAY: u64 = 86_400;

fn retention_config() -> RetentionConfig {
    RetentionConfig {
        metadata_ttl_secs: 7 * DAY,
        audit_retention_days: 30,
        cleanup_interval_secs: 3_600,
        rustup_snapshot_revisions: 2,
        artifact_idle_secs: 30 * DAY,
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
    let expired_index = data_dir.join("stores/cargo/metadata/group/index/left-pad");
    let expired_query = data_dir.join("stores/vscode/metadata/group/marketplace/abc.json");
    let fresh_index = data_dir.join("stores/cargo/metadata/group/index/serde");
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
fn evicts_idle_artifacts_and_keeps_recent_ones() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let old = now - Duration::from_secs(60 * DAY);
    let idle_rustup =
        data_dir.join("stores/rustup/artifacts/group/objects/2026-08-20/rust-std.tar.xz");
    let idle_crate = data_dir.join("stores/cargo/artifacts/group/objects/crates/serde/1.0.0.crate");
    let fresh_blob = data_dir.join("stores/docker/artifacts/official/library/alpine/blob");
    let snapshot = data_dir.join("stores/rustup/snapshots/group/stable/revision/manifest.toml");
    write_file_with_mtime(&idle_rustup, "artifact", old);
    write_file_with_mtime(&idle_crate, "crate", old);
    write_file_with_mtime(&fresh_blob, "blob", now - Duration::from_secs(60));
    write_file_with_mtime(&snapshot, "manifest", old);

    let report = sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    assert_eq!(report.removed_artifact_files, 2);
    assert!(!idle_rustup.exists());
    assert!(!idle_crate.exists());
    assert!(fresh_blob.exists());
    assert!(snapshot.exists());
    let events = fs::read_to_string(data_dir.join("audit/events.jsonl")).expect("audit events");
    assert!(events.contains("\"artifact_cleanup\""));
}

#[test]
fn keeps_an_idle_artifact_that_was_accessed() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let artifact =
        data_dir.join("stores/npm/artifacts/npmjs/objects/group/is-odd/3.0.1/tarball/sha256");
    write_file_with_mtime(&artifact, "tarball", now - Duration::from_secs(60 * DAY));

    note_access(&artifact).expect("access time update");
    let report = sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    assert_eq!(report.removed_artifact_files, 0);
    assert!(artifact.exists());
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
    let expired = data_dir.join("stores/npm/metadata/group/left-pad/1.0.0-metadata.json");
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

fn write_snapshot_revision(channel_dir: &Path, revision: &str, modified: SystemTime) {
    write_file_with_mtime(
        &channel_dir.join(revision).join("manifest.toml"),
        "manifest",
        modified,
    );
}

fn write_current_pointer(channel_dir: &Path, revision: &str) {
    fs::create_dir_all(channel_dir).expect("channel directory");
    fs::write(
        channel_dir.join("current.toml"),
        format!(
            "revision = \"{revision}\"\nsource_name = \"official\"\npublished_at_unix_secs = 1\n"
        ),
    )
    .expect("current pointer");
}

#[test]
fn keeps_recent_rustup_snapshot_revisions() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let channel_dir = data_dir.join("stores/rustup/snapshots/group/stable");
    let revisions: Vec<String> = ["a", "b", "c", "d"]
        .iter()
        .map(|label| label.repeat(64))
        .collect();
    for (index, revision) in revisions.iter().enumerate() {
        write_snapshot_revision(
            &channel_dir,
            revision,
            now - Duration::from_secs((4 - index as u64) * 1_200),
        );
    }
    write_current_pointer(&channel_dir, &revisions[3]);

    let report = sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    assert_eq!(report.removed_snapshot_revisions, 2);
    assert!(!channel_dir.join(&revisions[0]).exists());
    assert!(!channel_dir.join(&revisions[1]).exists());
    assert!(channel_dir.join(&revisions[2]).exists());
    assert!(channel_dir.join(&revisions[3]).exists());
    assert!(channel_dir.join("current.toml").exists());
}

#[test]
fn keeps_the_current_snapshot_revision_beyond_the_keep_window() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let channel_dir = data_dir.join("stores/rustup/snapshots/group/stable");
    let revisions: Vec<String> = ["a", "b", "c", "d"]
        .iter()
        .map(|label| label.repeat(64))
        .collect();
    for (index, revision) in revisions.iter().enumerate() {
        write_snapshot_revision(
            &channel_dir,
            revision,
            now - Duration::from_secs((4 - index as u64) * 1_200),
        );
    }
    write_current_pointer(&channel_dir, &revisions[0]);

    let report = sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    assert_eq!(report.removed_snapshot_revisions, 1);
    assert!(channel_dir.join(&revisions[0]).exists());
    assert!(!channel_dir.join(&revisions[1]).exists());
    assert!(channel_dir.join(&revisions[2]).exists());
    assert!(channel_dir.join(&revisions[3]).exists());
}

#[test]
fn skips_snapshot_cleanup_when_the_current_pointer_is_unreadable() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let channel_dir = data_dir.join("stores/rustup/snapshots/group/stable");
    let revisions: Vec<String> = ["a", "b", "c", "d"]
        .iter()
        .map(|label| label.repeat(64))
        .collect();
    for (index, revision) in revisions.iter().enumerate() {
        write_snapshot_revision(
            &channel_dir,
            revision,
            now - Duration::from_secs((4 - index as u64) * 1_200),
        );
    }
    fs::write(channel_dir.join("current.toml"), "revision =").expect("broken pointer");

    let report = sweep(
        data_dir,
        &retention_config(),
        now,
        &AuditSink::new(data_dir),
    );

    assert_eq!(report.removed_snapshot_revisions, 0);
    for revision in &revisions {
        assert!(channel_dir.join(revision).exists());
    }
}

#[test]
fn reports_legacy_layout_entries_without_removing_them_on_dry_run() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let legacy_crate = data_dir.join("cargo/artifacts/group/crates/serde/1.0.0.crate");
    write_file_with_mtime(&legacy_crate, "legacy", SystemTime::now());

    let report = prune_legacy_layout(data_dir, true, &AuditSink::new(data_dir));

    assert_eq!(report.removed_entries, 1);
    assert_eq!(report.removed_bytes, 6);
    assert!(legacy_crate.exists());
    assert!(!data_dir.join("audit/events.jsonl").exists());
}

#[test]
fn removes_legacy_layout_trees_and_keeps_stores_and_audit() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let now = SystemTime::now();
    let legacy_flat = data_dir.join("cargo/artifacts/group/crates/serde/1.0.0.crate");
    let legacy_snapshots = data_dir.join("rustup/snapshots/group/stable/revision/manifest.toml");
    let legacy_docker = data_dir.join("stores/docker/artifacts/official/library/alpine/blob.json");
    let current_crate =
        data_dir.join("stores/cargo/artifacts/official/objects/crates/serde/1.0.0.crate");
    let current_metadata = data_dir.join("stores/vscode/metadata/group/marketplace/abc.json");
    let audit = data_dir.join("audit/events.jsonl");
    write_file_with_mtime(&legacy_flat, "legacy", now);
    write_file_with_mtime(&legacy_snapshots, "legacy", now);
    write_file_with_mtime(&legacy_docker, "docker", now);
    write_file_with_mtime(&current_crate, "current", now);
    write_file_with_mtime(&current_metadata, "metadata", now);
    write_file_with_mtime(&audit, "{}\n", now);

    let report = prune_legacy_layout(data_dir, false, &AuditSink::new(data_dir));

    assert_eq!(report.removed_entries, 3);
    assert_eq!(report.failed_entries, 0);
    assert!(!data_dir.join("cargo").exists());
    assert!(!data_dir.join("rustup").exists());
    assert!(
        !data_dir
            .join("stores/docker/artifacts/official/library")
            .exists()
    );
    assert!(current_crate.exists());
    assert!(current_metadata.exists());
    assert!(audit.exists());
    let events = fs::read_to_string(data_dir.join("audit/events.jsonl")).expect("audit events");
    assert!(events.contains("legacy_layout_cleanup"));
    assert!(events.contains("\"count\":3"));
}
