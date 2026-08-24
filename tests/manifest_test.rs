use sha2::{Digest, Sha256};
use stacktory::rustup::{ManifestPolicy, rewrite_manifest};
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
    let public_base = Url::parse("https://stacktory.internal").expect("public URL");

    let rewritten = rewrite_manifest(SOURCE_MANIFEST, &policy, &public_base)
        .expect("configured dist URL should be rewritten");

    assert!(rewritten.manifest.contains(
        "https://stacktory.internal/mirror/rustup/dist/2026-08-20/rust-std-x86_64-pc-windows-msvc.tar.gz"
    ));
    assert!(rewritten.manifest.contains(
        "https://stacktory.internal/mirror/rustup/dist/2026-08-20/rust-std-x86_64-pc-windows-msvc.tar.xz"
    ));
    assert_eq!(
        rewritten.checksum,
        format!("{:x}", Sha256::digest(rewritten.manifest.as_bytes()))
    );
    assert_eq!(rewritten.artifact_hashes.len(), 2);
}

#[test]
fn rejects_manifest_url_outside_configured_upstreams() {
    let policy = ManifestPolicy::new(vec![
        Url::parse("https://upstream.internal/repository/rustup").expect("upstream URL"),
    ]);
    let public_base = Url::parse("https://stacktory.internal").expect("public URL");
    let untrusted_manifest = SOURCE_MANIFEST.replace("upstream.internal", "untrusted.example");

    let error = rewrite_manifest(&untrusted_manifest, &policy, &public_base)
        .expect_err("unconfigured upstream URL must fail");

    assert!(error.to_string().contains("configured upstream"));
}
