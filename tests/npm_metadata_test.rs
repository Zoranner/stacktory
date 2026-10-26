use stacktory::ecosystems::npm::metadata::{NpmMetadata, parse_metadata, rewrite_metadata};
use url::Url;

#[test]
fn preserves_npm_metadata_and_rewrites_version_tarball_urls() {
    let source = br#"{
        "name": "@scope/package",
        "description": "kept",
        "dist-tags": {"latest": "1.0.0"},
        "versions": {
            "1.0.0": {
                "name": "@scope/package",
                "version": "1.0.0",
                "dependencies": {"dep": "^2.0.0"},
                "custom": {"preserve": true},
                "dist": {
                    "tarball": "http://registry.example/@scope/package/-/package-1.0.0.tgz",
                    "integrity": "sha512-YWJj",
                    "shasum": "00112233445566778899aabbccddeeff00112233"
                }
            }
        }
    }"#;
    let metadata = parse_metadata(source, "@scope/package").expect("metadata");
    let rewritten = rewrite_metadata(
        &metadata,
        &Url::parse("http://127.0.0.1:18761").expect("root URL"),
        "group",
        "official",
    )
    .expect("rewritten metadata");

    assert_eq!(rewritten.body["name"], "@scope/package");
    assert_eq!(rewritten.body["description"], "kept");
    assert_eq!(
        rewritten.body["versions"]["1.0.0"]["dependencies"]["dep"],
        "^2.0.0"
    );
    assert_eq!(
        rewritten.body["versions"]["1.0.0"]["custom"]["preserve"],
        true
    );
    let tarball = rewritten.body["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .expect("rewritten tarball");
    assert_eq!(
        tarball,
        "http://127.0.0.1:18761/mirror/npm/group/@scope/package/-/package-1.0.0.tgz"
    );
    assert_eq!(
        rewritten.body["versions"]["1.0.0"]["dist"]["integrity"],
        "sha512-YWJj"
    );
    assert_eq!(
        rewritten.body["versions"]["1.0.0"]["dist"]["shasum"],
        "00112233445566778899aabbccddeeff00112233"
    );
    assert_eq!(rewritten.descriptors.len(), 1);
    assert_eq!(rewritten.descriptors[0].package, "@scope/package");
    assert_eq!(rewritten.descriptors[0].version, "1.0.0");
    assert_eq!(rewritten.descriptors[0].selected_proxy, "official");
    assert_eq!(
        rewritten.descriptors[0].upstream_tarball,
        "http://registry.example/@scope/package/-/package-1.0.0.tgz"
    );
}

#[test]
fn parses_metadata_as_a_lossless_json_value() {
    let metadata = parse_metadata(
        br#"{"name":"left-pad","versions":{},"unknown":{"value":42}}"#,
        "left-pad",
    )
    .expect("metadata");

    assert!(matches!(metadata, NpmMetadata { .. }));
    assert_eq!(metadata.body["unknown"]["value"], 42);
}

#[test]
fn rejects_mismatched_or_malformed_metadata() {
    assert!(parse_metadata(br#"{"name":"other","versions":{}}"#, "left-pad").is_err());
    assert!(parse_metadata(br#"{"name":"left-pad"}"#, "left-pad").is_err());
    assert!(parse_metadata(
        br#"{"name":"left-pad","versions":{"bad":{"dist":{"tarball":"https://example.test/a.tgz"}}}}"#,
        "left-pad",
    )
    .is_err());
}
