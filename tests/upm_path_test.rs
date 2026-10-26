use stacktory::ecosystems::upm::path::{UpmPackagePath, parse_metadata_path};

#[test]
fn parses_unity_reverse_domain_package_name() {
    assert_eq!(
        parse_metadata_path("com.company.package").expect("valid UPM package"),
        UpmPackagePath {
            package: "com.company.package".to_owned(),
        }
    );
}

#[test]
fn rejects_non_upm_package_names() {
    for value in [
        "",
        "company.package",
        "Com.company.package",
        "com..package",
        "com.company/package",
        "com.company.package%2Fextra",
        "../com.company.package",
        "com.company.package\\extra",
    ] {
        assert!(
            parse_metadata_path(value).is_err(),
            "UPM package should be rejected: {value}"
        );
    }
}
