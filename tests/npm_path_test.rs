use stacktory::npm::path::{
    NpmArtifactPath, NpmPackagePath, parse_artifact_path, parse_metadata_path,
};

#[test]
fn parses_unscoped_metadata_package() {
    let path = parse_metadata_path("lodash").expect("valid package");

    assert_eq!(
        path,
        NpmPackagePath {
            package: "lodash".to_owned(),
        }
    );
}

#[test]
fn parses_scoped_metadata_package_with_one_encoded_separator() {
    let path = parse_metadata_path("@scope%2Fpackage").expect("valid scoped package");

    assert_eq!(
        path,
        NpmPackagePath {
            package: "@scope/package".to_owned(),
        }
    );
}

#[test]
fn rejects_decoded_scoped_separator_and_other_encoded_slashes() {
    assert!(parse_metadata_path("@scope/package").is_err());
    assert!(parse_metadata_path("@scope%2fpackage").is_err());
    assert!(parse_metadata_path("@scope%252Fpackage").is_err());
    assert!(parse_metadata_path("package%2Fextra").is_err());
}

#[test]
fn rejects_invalid_package_paths() {
    for path in [
        "",
        "/lodash",
        "../lodash",
        "lodash/../other",
        "lodash\\other",
        "lodash//other",
        ".",
        "..",
        "@scope%",
        "@scope%2F",
        "%40scope%2Fpackage",
        "@scope%2Fpackage%2Fextra",
    ] {
        assert!(
            parse_metadata_path(path).is_err(),
            "path should be rejected: {path}"
        );
    }
}

#[test]
fn parses_hex_artifact_key_only() {
    let value = "a".repeat(64);
    let path = parse_artifact_path(&value).expect("valid artifact key");

    assert_eq!(
        path,
        NpmArtifactPath {
            artifact_key: value,
        }
    );
    assert!(parse_artifact_path(&"g".repeat(64)).is_err());
    assert!(parse_artifact_path("https://registry.example/package.tgz").is_err());
    assert!(parse_artifact_path(&"a".repeat(63)).is_err());
    assert!(parse_artifact_path(&format!("{}/extra", "a".repeat(64))).is_err());
}
