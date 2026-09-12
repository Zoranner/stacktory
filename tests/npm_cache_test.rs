use sha2::{Digest, Sha256};
use stacktory::npm::storage::{IntegrityExpectation, NpmCacheKey, NpmStorage, StorageError};
use tempfile::tempdir;

#[test]
fn publishes_and_reads_artifact_with_sha512_integrity() {
    let directory = tempdir().expect("data directory");
    let storage = NpmStorage::new(directory.path());
    let key = cache_key("official");
    let contents = b"verified npm tarball";
    let expectation = IntegrityExpectation {
        integrity: Some(
            "sha512-/N1CKDV6a6aguWS6zXumFLKdPEgUlyF3JMX0Wg6T/fsM5eQmI+NR77Kh7kOM8IQwcgdJi42buBpa19HvIO7JVw=="
                .to_owned(),
        ),
        shasum: None,
    };

    let result = storage
        .publish_verified_artifact(&key, "artifact-key", &expectation, contents)
        .expect("verified artifact");

    assert!(result.upstream_verified);
    assert_eq!(
        result.local_sha256,
        format!("{:x}", Sha256::digest(contents))
    );
    assert_eq!(
        storage
            .read_verified_artifact(&key, "artifact-key", &expectation)
            .expect("cache read"),
        Some(contents.to_vec())
    );
}

#[test]
fn publishes_artifact_with_sha1_shasum_when_integrity_is_absent() {
    let directory = tempdir().expect("data directory");
    let storage = NpmStorage::new(directory.path());
    let key = cache_key("official");
    let expectation = IntegrityExpectation {
        integrity: None,
        shasum: Some("32052d9fddd5b3bc862e66b74020ec46091c63dd".to_owned()),
    };

    let result = storage
        .publish_verified_artifact(&key, "artifact-key", &expectation, b"verified npm tarball")
        .expect("verified artifact");

    assert!(result.upstream_verified);
}

#[test]
fn rejects_mismatched_digest_without_publishing_cache() {
    let directory = tempdir().expect("data directory");
    let storage = NpmStorage::new(directory.path());
    let key = cache_key("official");
    let expectation = IntegrityExpectation {
        integrity: Some("sha512-YmFk".to_owned()),
        shasum: None,
    };

    let error = storage
        .publish_verified_artifact(&key, "artifact-key", &expectation, b"actual")
        .expect_err("digest mismatch");

    assert!(matches!(error, StorageError::IntegrityMismatch { .. }));
    assert!(!storage.artifact_path(&key, "artifact-key").exists());
}

#[test]
fn missing_upstream_digest_only_records_local_sha256() {
    let directory = tempdir().expect("data directory");
    let storage = NpmStorage::new(directory.path());
    let key = cache_key("official");
    let expectation = IntegrityExpectation {
        integrity: None,
        shasum: None,
    };

    let result = storage
        .publish_verified_artifact(&key, "artifact-key", &expectation, b"undigested")
        .expect("artifact without upstream digest");

    assert!(!result.upstream_verified);
    assert_eq!(
        result.local_sha256,
        format!("{:x}", Sha256::digest(b"undigested"))
    );
}

#[test]
fn cache_path_contains_all_npm_identity_dimensions() {
    let directory = tempdir().expect("data directory");
    let storage = NpmStorage::new(directory.path());
    let key = cache_key("official");
    let path = storage.artifact_path(&key, "artifact-key");

    let path = path.to_string_lossy();
    assert!(path.contains("npm"));
    assert!(path.contains("group"));
    assert!(path.contains("left-pad"));
    assert!(path.contains("1.0.0"));
    assert!(path.contains("tarball"));
    assert!(path.contains("official"));
}

fn cache_key(selected_proxy: &str) -> NpmCacheKey {
    NpmCacheKey {
        entry: "group".to_owned(),
        package: "left-pad".to_owned(),
        version: "1.0.0".to_owned(),
        representation: "tarball".to_owned(),
        selected_proxy: selected_proxy.to_owned(),
    }
}
