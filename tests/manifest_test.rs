use sha2::{Digest, Sha256};
use stacktory::rustup::{ArtifactSource, DistPath, ManifestPolicy, rewrite_manifest};
use url::Url;

const SOURCE_MANIFEST: &str = r#"
manifest-version = "2"

[pkg.rust]
version = "1.99.0"

[pkg.rust.target.x86_64-pc-windows-msvc]
url = "https://upstream.internal/repository/rustup/dist/2026-08-20/rust-std-x86_64-pc-windows-msvc.tar.gz"
hash = "6ea0350d546321e56d11806b9e0ffd2d6d48a4e96147620caecfb5e79b0e4f90"
xz_url = "https://upstream.internal/repository/rustup/dist/2026-08-20/rust-std-x86_64-pc-windows-msvc.tar.xz"
xz_hash = "2ea0350d546321e56d11806b9e0ffd2d6d48a4e96147620caecfb5e79b0e4f90"
"#;

#[test]
fn rewrites_configured_dist_urls_and_calculates_matching_checksum() {
    let policy = ManifestPolicy::new(vec![
        Url::parse("https://upstream.internal/repository/rustup").expect("upstream URL"),
    ]);
    let root_url =
        Url::parse("https://stacktory.internal/mirror/rustup/group").expect("entry root URL");

    let rewritten = rewrite_manifest(SOURCE_MANIFEST, &policy, &root_url, "stable")
        .expect("configured dist URL should be rewritten");

    assert!(rewritten.manifest.contains(
        "https://stacktory.internal/mirror/rustup/group/dist/stable/2026-08-20/rust-std-x86_64-pc-windows-msvc.tar.gz"
    ));
    assert!(rewritten.manifest.contains(
        "https://stacktory.internal/mirror/rustup/group/dist/stable/2026-08-20/rust-std-x86_64-pc-windows-msvc.tar.xz"
    ));
    assert_eq!(
        rewritten.checksum,
        format!("{:x}", Sha256::digest(rewritten.manifest.as_bytes()))
    );
    assert_eq!(rewritten.artifact_sources.len(), 2);
}

#[test]
fn rejects_manifest_url_outside_configured_upstreams() {
    let policy = ManifestPolicy::new(vec![
        Url::parse("https://upstream.internal/repository/rustup").expect("upstream URL"),
    ]);
    let root_url =
        Url::parse("https://stacktory.internal/mirror/rustup/group").expect("entry root URL");
    let untrusted_manifest = SOURCE_MANIFEST.replace("upstream.internal", "untrusted.example");

    let error = rewrite_manifest(&untrusted_manifest, &policy, &root_url, "stable")
        .expect_err("unconfigured upstream URL must fail");

    assert!(error.to_string().contains("configured upstream"));
}

#[test]
fn rewrites_installer_urls_with_hash_sha256() {
    let manifest = r#"
[artifacts.installer-msi.target.x86_64-pc-windows-msvc]
url = "https://static.rust-lang.org/dist/2026-09-03/rust-1.98.1-x86_64-pc-windows-msvc.msi"
hash-sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#;
    let policy = ManifestPolicy::new(vec![
        Url::parse("https://static.rust-lang.org").expect("upstream URL"),
    ]);
    let root_url =
        Url::parse("http://127.0.0.1:18761/mirror/rustup/group").expect("entry root URL");

    let rewritten =
        rewrite_manifest(manifest, &policy, &root_url, "stable").expect("installer URL rewrite");

    assert!(rewritten.manifest.contains(
        "http://127.0.0.1:18761/mirror/rustup/group/dist/stable/2026-09-03/rust-1.98.1-x86_64-pc-windows-msvc.msi"
    ));
    assert_eq!(
        rewritten.artifact_sources.get(
            &stacktory::rustup::DistPath::parse(
                "2026-09-03/rust-1.98.1-x86_64-pc-windows-msvc.msi"
            )
            .expect("artifact path")
        ),
        Some(&ArtifactSource {
            checksum: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            source_name: "https://static.rust-lang.org/".to_owned(),
        })
    );
}

#[test]
fn binds_each_rewritten_artifact_to_its_matching_source() {
    let manifest = r#"
[pkg.rust.target.x86_64-pc-windows-msvc]
url = "https://static.rust-lang.org/dist/2026-09-03/rustc.tar.gz"
hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
xz_url = "https://mirror.example/rustup/dist/2026-09-03/rustc.tar.xz"
xz_hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
"#;
    let policy = ManifestPolicy::with_sources(vec![
        (
            "official".to_owned(),
            Url::parse("https://static.rust-lang.org").expect("official URL"),
        ),
        (
            "mirror".to_owned(),
            Url::parse("https://mirror.example/rustup").expect("mirror URL"),
        ),
    ]);
    let root_url =
        Url::parse("http://127.0.0.1:18761/mirror/rustup/group").expect("entry root URL");

    let rewritten =
        rewrite_manifest(manifest, &policy, &root_url, "stable").expect("mixed manifest rewrite");

    assert_eq!(
        rewritten
            .artifact_sources
            .get(&DistPath::parse("2026-09-03/rustc.tar.gz").expect("gzip artifact path")),
        Some(&ArtifactSource {
            checksum: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            source_name: "official".to_owned(),
        })
    );
    assert_eq!(
        rewritten
            .artifact_sources
            .get(&DistPath::parse("2026-09-03/rustc.tar.xz").expect("xz artifact path")),
        Some(&ArtifactSource {
            checksum: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            source_name: "mirror".to_owned(),
        })
    );
}
