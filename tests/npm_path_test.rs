use stacktory::ecosystems::npm::path::{
    NpmArtifactPath, NpmPackagePath, NpmTarballPath, parse_artifact_path, parse_metadata_path,
    parse_tarball_path,
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
fn parses_scoped_metadata_package_with_a_lowercase_encoded_separator() {
    let path = parse_metadata_path("@scope%2fpackage").expect("valid scoped package");

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
    assert!(parse_metadata_path("@scope%2Fpackage%2fextra").is_err());
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
fn parses_standard_tarball_paths() {
    assert_eq!(
        parse_tarball_path("left-pad/-/left-pad-1.0.0.tgz").expect("unscoped tarball"),
        NpmTarballPath {
            package: "left-pad".to_owned(),
            file_name: "left-pad-1.0.0.tgz".to_owned(),
        }
    );
    assert_eq!(
        parse_tarball_path("@scope/package/-/package-1.0.0.tgz").expect("scoped tarball"),
        NpmTarballPath {
            package: "@scope/package".to_owned(),
            file_name: "package-1.0.0.tgz".to_owned(),
        }
    );
    assert_eq!(
        parse_tarball_path("@scope%2fpackage/-/package-1.0.0.tgz").expect("encoded tarball"),
        NpmTarballPath {
            package: "@scope/package".to_owned(),
            file_name: "package-1.0.0.tgz".to_owned(),
        }
    );
}

#[test]
fn rejects_unsafe_tarball_paths() {
    for path in [
        "left-pad/left-pad-1.0.0.tgz",
        "left-pad/-/../left-pad-1.0.0.tgz",
        "left-pad/-/left-pad-1.0.0.tar.gz",
        "left-pad/-/left-pad%2f1.0.0.tgz",
        "@scope%2Fpackage%2Fextra/-/extra-1.0.0.tgz",
    ] {
        assert!(
            parse_tarball_path(path).is_err(),
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
