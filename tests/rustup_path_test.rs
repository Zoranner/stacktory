use stacktory::ecosystems::rustup::DistPath;

#[test]
fn accepts_a_single_relative_dist_artifact_path() {
    let path = DistPath::parse("2026-08-20/rust-std-x86_64-pc-windows-msvc.tar.xz")
        .expect("relative dist artifact path");

    assert_eq!(
        path.as_str(),
        "2026-08-20/rust-std-x86_64-pc-windows-msvc.tar.xz"
    );
}

#[test]
fn rejects_parent_segments_and_encoded_paths() {
    for path in [
        "../channel-rust-stable.toml",
        "%2e%2e/channel-rust-stable.toml",
        "2026-08-20/%2fescape.tar.xz",
        "2026-08-20\\escape.tar.xz",
        "/absolute.tar.xz",
    ] {
        assert!(
            DistPath::parse(path).is_err(),
            "path `{path}` must be rejected"
        );
    }
}
