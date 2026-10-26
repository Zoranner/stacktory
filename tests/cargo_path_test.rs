use stacktory::ecosystems::cargo::{CargoPath, normalize_crate_name, sparse_index_path};

#[test]
fn computes_sparse_index_path_for_short_crate_name() {
    assert_eq!(sparse_index_path("a").expect("one-character path"), "1/a");
}

#[test]
fn computes_sparse_index_path_for_two_character_crate_name() {
    assert_eq!(sparse_index_path("ab").expect("two-character path"), "2/ab");
}

#[test]
fn computes_sparse_index_path_for_long_crate_name() {
    assert_eq!(
        sparse_index_path("Serde_JSON").expect("long path"),
        "se/rd/serde_json"
    );
}

#[test]
fn rejects_invalid_crate_path() {
    for path in [
        "",
        "/serde",
        "index/../serde",
        "index/serde//more",
        "index/serde%2Fmore",
    ] {
        assert!(
            CargoPath::parse(path).is_err(),
            "path should be rejected: {path:?}"
        );
    }
}

#[test]
fn normalizes_crate_name_to_lowercase() {
    assert_eq!(
        normalize_crate_name("Serde_JSON").expect("crate name"),
        "serde_json"
    );
}

#[test]
fn accepts_numeric_leading_crate_name() {
    assert_eq!(
        sparse_index_path("3d").expect("numeric-leading crate path"),
        "2/3d"
    );
}
