use stacktory::{Config, SourceType};

const CONFIGURATION: &str = r#"
[server]
address = "127.0.0.1"
port = 18761
public_url = "http://127.0.0.1:18761"

[storage]
data_dir = "./var/stacktory"

[rustup]
refresh_interval_secs = 3600
channels = ["stable", "beta"]

[[rustup.sources]]
name = "company-nexus"
type = "proxy"
base_url = "https://pkg.example.internal/repository/rustup-group"

[[rustup.sources]]
name = "rust-lang"
type = "proxy"
base_url = "https://static.rust-lang.org"

[[rustup.sources]]
name = "group"
type = "group"
members = ["company-nexus", "rust-lang"]
"#;

#[test]
fn accepts_proxy_and_group_sources() {
    let config = Config::from_toml(CONFIGURATION).expect("source configuration");
    let group = config
        .rustup
        .sources
        .get("group")
        .expect("configured group");

    assert_eq!(group.source_type, SourceType::Group);
    assert_eq!(
        config
            .rustup
            .sources
            .proxy_members("group")
            .expect("group proxy members")
            .iter()
            .map(|source| source.name.as_str())
            .collect::<Vec<_>>(),
        ["company-nexus", "rust-lang"]
    );
}

#[test]
fn rejects_group_with_base_url() {
    let configuration = CONFIGURATION.replace(
        "type = \"group\"\nmembers = [\"company-nexus\", \"rust-lang\"]",
        "type = \"group\"\nbase_url = \"https://unexpected.example.internal\"\nmembers = [\"company-nexus\", \"rust-lang\"]",
    );

    let error = Config::from_toml(&configuration).expect_err("group base URL must fail");

    assert!(error.to_string().contains("group"));
    assert!(error.to_string().contains("base_url"));
}

#[test]
fn rejects_proxy_without_base_url() {
    let configuration = CONFIGURATION.replace(
        "type = \"proxy\"\nbase_url = \"https://pkg.example.internal/repository/rustup-group\"",
        "type = \"proxy\"",
    );

    let error = Config::from_toml(&configuration).expect_err("proxy base URL is required");

    assert!(error.to_string().contains("proxy"));
    assert!(error.to_string().contains("base_url"));
}

#[test]
fn rejects_unknown_group_member() {
    let configuration = CONFIGURATION.replace(
        "members = [\"company-nexus\", \"rust-lang\"]",
        "members = [\"company-nexus\", \"missing\"]",
    );

    let error = Config::from_toml(&configuration).expect_err("unknown group member must fail");

    assert!(error.to_string().contains("missing"));
}

#[test]
fn rejects_duplicate_group_member() {
    let configuration = CONFIGURATION.replace(
        "members = [\"company-nexus\", \"rust-lang\"]",
        "members = [\"company-nexus\", \"company-nexus\"]",
    );

    let error = Config::from_toml(&configuration).expect_err("duplicate group member must fail");

    assert!(error.to_string().contains("duplicate"));
}

#[test]
fn rejects_duplicate_source_name() {
    let configuration = CONFIGURATION.replace("name = \"rust-lang\"", "name = \"company-nexus\"");

    let error = Config::from_toml(&configuration).expect_err("duplicate source name must fail");

    assert!(error.to_string().contains("duplicated"));
}

#[test]
fn rejects_group_with_allow_http() {
    let configuration = CONFIGURATION.replace(
        "type = \"group\"\nmembers = [\"company-nexus\", \"rust-lang\"]",
        "type = \"group\"\nallow_http = true\nmembers = [\"company-nexus\", \"rust-lang\"]",
    );

    let error = Config::from_toml(&configuration).expect_err("group allow_http must fail");

    assert!(error.to_string().contains("allow_http"));
}

#[test]
fn rejects_nested_group_and_self_reference() {
    let nested_group = CONFIGURATION
        .replace(
            "name = \"rust-lang\"\ntype = \"proxy\"\nbase_url = \"https://static.rust-lang.org\"",
            "name = \"fallback\"\ntype = \"group\"\nmembers = [\"company-nexus\"]",
        )
        .replace(
            "members = [\"company-nexus\", \"rust-lang\"]",
            "members = [\"fallback\"]",
        );
    let self_reference = CONFIGURATION.replace(
        "members = [\"company-nexus\", \"rust-lang\"]",
        "members = [\"group\"]",
    );

    let nested_error = Config::from_toml(&nested_group).expect_err("nested group member must fail");
    let self_error =
        Config::from_toml(&self_reference).expect_err("self-referencing group must fail");

    assert!(nested_error.to_string().contains("group"));
    assert!(self_error.to_string().contains("group"));
}

#[test]
fn rejects_invalid_source_name() {
    let configuration = CONFIGURATION.replace("name = \"company-nexus\"", "name = \"bad/name\"");

    let error = Config::from_toml(&configuration).expect_err("invalid source name must fail");

    assert!(error.to_string().contains("source name"));
}

#[test]
fn rejects_dot_source_names() {
    for name in [".", ".."] {
        let configuration =
            CONFIGURATION.replace("name = \"company-nexus\"", &format!("name = \"{name}\""));
        let error = Config::from_toml(&configuration).expect_err("dot source name must fail");
        assert!(error.to_string().contains("source name"));
    }
}
