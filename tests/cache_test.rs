use sha2::{Digest, Sha256};
use stacktory::{rustup::DistPath, storage::ArtifactCache};
use tempfile::tempdir;

#[test]
fn cache_only_publishes_artifact_when_sha256_matches() {
    let directory = tempdir().expect("temporary directory");
    let cache = ArtifactCache::new(directory.path());
    let path =
        DistPath::parse("2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz").expect("dist path");
    let contents = b"verified artifact";
    let checksum = format!("{:x}", Sha256::digest(contents));

    cache
        .publish_verified("test", &path, contents, &checksum)
        .expect("matching artifact");

    assert_eq!(
        cache
            .get_verified("test", &path, &checksum)
            .expect("cache lookup"),
        Some(contents.to_vec())
    );
}

#[test]
fn cache_rejects_mismatched_sha256_without_creating_a_hit() {
    let directory = tempdir().expect("temporary directory");
    let cache = ArtifactCache::new(directory.path());
    let path =
        DistPath::parse("2026-08-20/rustc-x86_64-pc-windows-msvc.tar.xz").expect("dist path");
    let wrong_checksum = "0000000000000000000000000000000000000000000000000000000000000000";

    assert!(
        cache
            .publish_verified("test", &path, b"untrusted artifact", wrong_checksum,)
            .is_err()
    );
    assert_eq!(
        cache
            .get_verified("test", &path, wrong_checksum)
            .expect("cache lookup"),
        None
    );
}
