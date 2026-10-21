use std::path::Path;

use sha2::{Digest, Sha256};
use stacktory::npm::metadata::NpmArtifactDescriptor;
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

#[test]
fn metadata_cache_is_canonical_and_removes_old_descriptors() {
    let directory = tempdir().expect("data directory");
    let storage = NpmStorage::new(directory.path());
    let key = metadata_key();
    let first = descriptor("artifact-a", "https://registry.example/left-pad-1.0.0.tgz");
    storage
        .publish_metadata(
            &key,
            b"canonical metadata",
            std::slice::from_ref(&first),
            "official|https://registry.example|false",
        )
        .expect("first metadata publication");

    let stored = storage
        .read_metadata("group", "left-pad")
        .expect("metadata read")
        .expect("metadata present");
    assert_eq!(stored.body, b"canonical metadata");
    assert_eq!(stored.descriptors, vec![first.clone()]);

    let second = descriptor("artifact-b", "https://registry.example/left-pad-1.0.1.tgz");
    storage
        .publish_metadata(
            &key,
            b"new canonical metadata",
            std::slice::from_ref(&second),
            "official|https://registry.example|false",
        )
        .expect("second metadata publication");

    assert!(!descriptor_path(directory.path(), "artifact-a").exists());
    assert!(descriptor_path(directory.path(), "artifact-b").exists());

    storage
        .remove_metadata("group", "left-pad")
        .expect("metadata removal");
    assert!(!metadata_path(directory.path()).exists());
    assert!(!descriptor_path(directory.path(), "artifact-b").exists());
    assert!(
        storage
            .read_metadata("group", "left-pad")
            .expect("metadata read after removal")
            .is_none()
    );
}

fn metadata_key() -> NpmCacheKey {
    NpmCacheKey {
        ecosystem: "npm".to_owned(),
        entry: "group".to_owned(),
        protocol_variant: "npm-compatible-registry".to_owned(),
        package: "left-pad".to_owned(),
        version: "catalog".to_owned(),
        representation: "metadata".to_owned(),
        selected_proxy: "official".to_owned(),
    }
}

fn descriptor(artifact_key: &str, upstream_tarball: &str) -> NpmArtifactDescriptor {
    NpmArtifactDescriptor {
        artifact_key: artifact_key.to_owned(),
        package: "left-pad".to_owned(),
        version: "1.0.0".to_owned(),
        selected_proxy: "official".to_owned(),
        source_fingerprint: "official|https://registry.example|false".to_owned(),
        upstream_tarball: upstream_tarball.to_owned(),
        integrity: None,
        shasum: None,
    }
}

fn metadata_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("stores/npm/metadata/group/left-pad/current.toml")
}

fn descriptor_path(data_dir: &Path, artifact_key: &str) -> std::path::PathBuf {
    data_dir
        .join("stores/npm/metadata/group/descriptors")
        .join(format!("{artifact_key}.json"))
}

fn cache_key(selected_proxy: &str) -> NpmCacheKey {
    NpmCacheKey {
        ecosystem: "npm".to_owned(),
        entry: "group".to_owned(),
        protocol_variant: "npm-compatible-registry".to_owned(),
        package: "left-pad".to_owned(),
        version: "1.0.0".to_owned(),
        representation: "tarball".to_owned(),
        selected_proxy: selected_proxy.to_owned(),
    }
}
