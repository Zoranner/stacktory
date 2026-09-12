use std::collections::BTreeMap;

use stacktory::{
    rustup::{ArtifactSource, DistPath, RewrittenManifest},
    storage::{SnapshotStore, StorageError},
};
use tempfile::tempdir;

#[test]
fn current_snapshot_keeps_manifest_and_checksum_from_the_same_publication() {
    let directory = tempdir().expect("temporary directory");
    let store = SnapshotStore::new(directory.path());

    store
        .publish(
            "test",
            "test",
            "stable",
            snapshot(
                "first manifest",
                "1111111111111111111111111111111111111111111111111111111111111111",
            ),
        )
        .expect("first publication");
    store
        .publish(
            "test",
            "test",
            "stable",
            snapshot(
                "second manifest",
                "2222222222222222222222222222222222222222222222222222222222222222",
            ),
        )
        .expect("second publication");

    let current = store.current("test", "stable").expect("current snapshot");

    assert_eq!(current.manifest, "second manifest");
    assert_eq!(
        current.checksum,
        "2222222222222222222222222222222222222222222222222222222222222222"
    );
    assert!(current.published_at_unix_secs > 0);
    assert_eq!(
        current
            .artifact_sources
            .get(&DistPath::parse("2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz").expect("path"))
            .expect("artifact source")
            .source_name,
        "test"
    );
}

#[test]
fn rejects_snapshots_without_per_artifact_sources() {
    let directory = tempdir().expect("temporary directory");
    let store = SnapshotStore::new(directory.path());
    let checksum = "1111111111111111111111111111111111111111111111111111111111111111";
    let snapshot_dir = directory
        .path()
        .join("rustup/snapshots/test/stable")
        .join(checksum);
    std::fs::create_dir_all(&snapshot_dir).expect("snapshot directory");
    std::fs::write(snapshot_dir.join("manifest.toml"), "manifest").expect("manifest");
    std::fs::write(snapshot_dir.join("checksum"), checksum).expect("checksum");
    std::fs::write(
        snapshot_dir.join("artifacts.toml"),
        "artifact_hashes = { \"2026-08-20/rustc.tar.xz\" = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }",
    )
    .expect("old artifact index");
    std::fs::write(
        directory
            .path()
            .join("rustup/snapshots/test/stable/current.toml"),
        format!("revision = \"{checksum}\"\nsource_name = \"test\"\npublished_at_unix_secs = 1\n"),
    )
    .expect("current pointer");

    assert!(matches!(
        store.current("test", "stable"),
        Err(StorageError::IncompatibleSnapshot(channel)) if channel == "stable"
    ));
}

fn snapshot(manifest: &str, checksum: &str) -> RewrittenManifest {
    RewrittenManifest {
        manifest: manifest.to_owned(),
        checksum: checksum.to_owned(),
        artifact_sources: BTreeMap::from([(
            DistPath::parse("2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz").expect("dist path"),
            ArtifactSource {
                checksum: "6ea0350d546321e56d11806b9e0ffd2d6d48a4e96147620caecfb5e79b0e4f90"
                    .to_owned(),
                source_name: "test".to_owned(),
            },
        )]),
    }
}
