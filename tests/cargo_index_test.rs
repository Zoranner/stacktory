use stacktory::cargo::{
    CargoIndexError, parse_index_lines, parse_registry_config, render_download_url,
    rewrite_crate_download_url, rewrite_index_lines,
};
use url::Url;

#[test]
fn parses_json_lines_index_and_keeps_checksums() {
    let source = concat!(
        r#"{"name":"serde","vers":"1.0.0","deps":[],"cksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","features":{},"yanked":false}"#,
        "\n",
        r#"{"name":"serde","vers":"1.0.1","deps":[],"cksum":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","features":{},"yanked":false}"#,
        "\n"
    );

    let records = parse_index_lines(source).expect("index records");

    assert_eq!(records.len(), 2);
    assert_eq!(records[0].name, "serde");
    assert_eq!(
        records[1].checksum,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    );
}

#[test]
fn rejects_index_record_without_sha256_checksum() {
    let error = parse_index_lines(
        r#"{"name":"serde","vers":"1.0.0","deps":[],"features":{},"yanked":false}"#,
    )
    .expect_err("checksum is required");

    assert!(matches!(error, CargoIndexError::MissingChecksum { .. }));
}

#[test]
fn preserves_case_in_a_valid_index_record_name() {
    let source = r#"{"name":"Serde","vers":"1.0.0","deps":[],"cksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","features":{},"yanked":false}"#;

    let records = parse_index_lines(source).expect("case-preserving record");

    assert_eq!(records[0].name, "Serde");
}

#[test]
fn rejects_non_semver_index_versions() {
    for version in ["abc", "1..0"] {
        let source = format!(
            r#"{{"name":"serde","vers":"{version}","deps":[],"cksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","features":{{}},"yanked":false}}"#
        );

        assert!(
            parse_index_lines(&source).is_err(),
            "version should be rejected: {version}"
        );
    }
}

#[test]
fn accepts_semver_prerelease_and_build_metadata() {
    let source = r#"{"name":"serde","vers":"1.0.0-alpha.1+build.5","deps":[],"cksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","features":{},"yanked":false}"#;

    let records = parse_index_lines(source).expect("semver record");

    assert_eq!(records[0].version, "1.0.0-alpha.1+build.5");
}

#[test]
fn expands_all_supported_download_markers() {
    let config = parse_registry_config(
        br#"{"dl":"https://download.example/{prefix}{crate}/{version}/{lowerprefix}{sha256-checksum}"}"#,
    )
    .expect("registry config");

    let url = render_download_url(
        &config.dl,
        "Serde",
        "1.0.0-alpha.1+build.5",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .expect("download URL");

    assert_eq!(
        url.as_str(),
        "https://download.example/Se/rd/Serde/1.0.0-alpha.1+build.5/se/rd/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
}

#[test]
fn rejects_registry_config_with_unsafe_download_url() {
    for dl in [
        "https://user:password@example/{crate}/{version}",
        "https://download.example/{crate}/{version}?token=secret",
        "https://download.example/../{crate}/{version}",
        "ftp://download.example/{crate}/{version}",
    ] {
        let source = format!(r#"{{"dl":"{dl}"}}"#);
        assert!(
            parse_registry_config(source.as_bytes())
                .and_then(|config| {
                    render_download_url(
                        &config.dl,
                        "serde",
                        "1.0.0",
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                })
                .is_err(),
            "unsafe dl should be rejected: {dl}"
        );
    }
}

#[test]
fn rewrites_crate_download_url_to_stacktory_entry() {
    let root = Url::parse("http://127.0.0.1:18761/base").expect("root URL");

    let rewritten =
        rewrite_crate_download_url(&root, "team", "serde", "1.0.0").expect("download URL");

    assert_eq!(
        rewritten.as_str(),
        "http://127.0.0.1:18761/base/mirror/cargo/team/crates/serde/1.0.0.crate"
    );
}

#[test]
fn rewrites_index_records_without_exposing_upstream_urls() {
    let source = concat!(
        r#"{"name":"serde","vers":"1.0.0","deps":[],"cksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","features":{},"yanked":false}"#,
        "\n"
    );
    let root = Url::parse("http://127.0.0.1:18761").expect("root URL");

    let rewritten = rewrite_index_lines(source, &root, "team").expect("rewritten index");

    assert_eq!(rewritten, source.as_bytes());
    assert!(
        !rewritten
            .windows(b"crates.io".len())
            .any(|window| window == b"crates.io")
    );
}
