use std::collections::BTreeMap;

use stacktory::{
    rustup::{DistPath, RewrittenManifest},
    storage::SnapshotStore,
};
use tempfile::tempdir;

#[test]
fn current_snapshot_keeps_manifest_and_checksum_from_the_same_publication() {
    let directory = tempdir().expect("temporary directory");
    let store = SnapshotStore::new(directory.path());

    store
        .publish(
            "stable",
            snapshot(
                "first manifest",
                "1111111111111111111111111111111111111111111111111111111111111111",
            ),
        )
        .expect("first publication");
    store
        .publish(
            "stable",
            snapshot(
                "second manifest",
                "2222222222222222222222222222222222222222222222222222222222222222",
            ),
        )
        .expect("second publication");

    let current = store.current("stable").expect("current snapshot");

    assert_eq!(current.manifest, "second manifest");
    assert_eq!(
        current.checksum,
        "2222222222222222222222222222222222222222222222222222222222222222"
    );
}

fn snapshot(manifest: &str, checksum: &str) -> RewrittenManifest {
    RewrittenManifest {
        manifest: manifest.to_owned(),
        checksum: checksum.to_owned(),
        artifact_hashes: BTreeMap::from([(
            DistPath::parse("2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz").expect("dist path"),
            "6ea0350d546321e56d11806b9e0ffd2d6d48a4e96147620caecfb5e79b0e4f90".to_owned(),
        )]),
    }
}
