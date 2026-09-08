use stacktory::Config;

const LOCAL_DEVELOPMENT: &str = r#"
listen_address = "127.0.0.1"
listen_port = 18761
data_dir = "./var/stacktory"
root_url = "http://127.0.0.1:18761"

[rustup]
refresh_interval_secs = 3600
channels = ["stable", "beta"]

[[rustup.sources]]
name = "company-nexus"
type = "proxy"
base_url = "https://pkg.example.internal/repository/rustup-group"
"#;

const PUBLIC_PRODUCTION: &str = r#"
listen_address = "0.0.0.0"
listen_port = 18761
data_dir = "./var/stacktory"
root_url = "https://rustup.stacktory.internal"

[rustup]
refresh_interval_secs = 3600
channels = ["stable", "beta"]

[[rustup.sources]]
name = "company-nexus"
type = "proxy"
base_url = "https://pkg.example.internal/repository/rustup-group"
"#;

const DEPLOYMENT_CONFIG: &str = include_str!("../deploy/app/config.example.toml");

#[test]
fn accepts_local_development_configuration() {
    let config = Config::from_toml(LOCAL_DEVELOPMENT).expect("local development configuration");

    assert_eq!(config.listen_address.to_string(), "127.0.0.1");
    assert_eq!(config.listen_port, 18761);
    assert_eq!(config.rustup.refresh_interval_secs, 3600);
    assert_eq!(config.rustup.channels, ["stable", "beta"]);
    assert_eq!(
        config
            .rustup
            .sources
            .get("company-nexus")
            .expect("company source")
            .base_url
            .as_ref()
            .expect("company source URL")
            .scheme(),
        "https"
    );
    assert!(config.cargo.is_none());
}

#[test]
fn accepts_explicit_http_source() {
    let configuration = LOCAL_DEVELOPMENT
        .replace(
            "https://pkg.example.internal/repository/rustup-group",
            "http://nexus.internal/repository/rustup-group",
        )
        .replace(
            "base_url = \"http://nexus.internal/repository/rustup-group\"",
            "base_url = \"http://nexus.internal/repository/rustup-group\"\nrequire_https = false",
        );

    let config = Config::from_toml(&configuration).expect("configured HTTP source");

    assert_eq!(
        config
            .rustup
            .sources
            .get("company-nexus")
            .expect("company source")
            .base_url
            .as_ref()
            .expect("company source URL")
            .scheme(),
        "http"
    );
}

#[test]
fn rejects_http_source_without_explicit_opt_in() {
    let configuration = LOCAL_DEVELOPMENT.replace(
        "https://pkg.example.internal/repository/rustup-group",
        "http://nexus.internal/repository/rustup-group",
    );

    let error = Config::from_toml(&configuration).expect_err("HTTP must require opt-in");

    assert!(error.to_string().contains("require_https"));
}

#[test]
fn rejects_zero_rustup_refresh_interval() {
    let configuration =
        LOCAL_DEVELOPMENT.replace("refresh_interval_secs = 3600", "refresh_interval_secs = 0");

    let error = Config::from_toml(&configuration).expect_err("zero refresh interval must fail");

    assert!(error.to_string().contains("refresh_interval_secs"));
}

#[test]
fn rejects_empty_rustup_channel_set() {
    let configuration =
        LOCAL_DEVELOPMENT.replace("channels = [\"stable\", \"beta\"]", "channels = []");

    let error = Config::from_toml(&configuration).expect_err("empty channel set must fail");

    assert!(error.to_string().contains("channels"));
}

#[test]
fn deployment_config_matches_the_runtime_schema() {
    let config = Config::from_toml(DEPLOYMENT_CONFIG).expect("deployment config");

    assert_eq!(config.data_dir, std::path::PathBuf::from("/app/data"));
    assert_eq!(config.listen_port, 18761);
    assert_eq!(
        config
            .cargo
            .as_ref()
            .expect("cargo configuration")
            .sources
            .get("crates-io")
            .expect("crates.io source")
            .source_type,
        stacktory::SourceType::Proxy
    );
}

#[test]
fn accepts_public_production_configuration() {
    let config = Config::from_toml(PUBLIC_PRODUCTION).expect("public production configuration");

    assert_eq!(config.listen_address.to_string(), "0.0.0.0");
}
