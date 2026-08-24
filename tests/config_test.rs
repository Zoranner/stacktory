use stacktory::Config;

const LOCAL_DEVELOPMENT: &str = r#"
listen = "127.0.0.1:18081"
data_dir = "./var/stacktory"
public_base_url = "http://127.0.0.1:18081"

[access]
allow_anonymous = true

[rustup]
refresh_interval_secs = 3600
channels = ["stable", "beta"]

[[rustup.upstreams]]
name = "company-nexus"
base_url = "https://pkg.example.internal/repository/rustup-group"
"#;

const ANONYMOUS_PRODUCTION: &str = r#"
listen = "0.0.0.0:18081"
data_dir = "./var/stacktory"
public_base_url = "https://rustup.stacktory.internal"

[access]
allow_anonymous = true

[rustup]
refresh_interval_secs = 3600
channels = ["stable", "beta"]

[[rustup.upstreams]]
name = "company-nexus"
base_url = "https://pkg.example.internal/repository/rustup-group"
"#;

#[test]
fn accepts_explicit_local_anonymous_development_configuration() {
    let config = Config::from_toml(LOCAL_DEVELOPMENT).expect("local development configuration");

    assert_eq!(config.listen.port(), 18081);
    assert_eq!(config.rustup.refresh_interval_secs, 3600);
    assert_eq!(config.rustup.channels, ["stable", "beta"]);
    assert_eq!(config.rustup.upstreams.len(), 1);
}

#[test]
fn accepts_explicit_http_upstream() {
    let configuration = LOCAL_DEVELOPMENT
        .replace(
            "https://pkg.example.internal/repository/rustup-group",
            "http://nexus.internal/repository/rustup-group",
        )
        .replace(
            "base_url = \"http://nexus.internal/repository/rustup-group\"",
            "base_url = \"http://nexus.internal/repository/rustup-group\"\nrequire_https = false",
        );

    let config = Config::from_toml(&configuration).expect("configured HTTP upstream");

    assert_eq!(config.rustup.upstreams[0].base_url.scheme(), "http");
}

#[test]
fn rejects_http_upstream_without_explicit_opt_in() {
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
fn rejects_anonymous_production_configuration() {
    let error =
        Config::from_toml(ANONYMOUS_PRODUCTION).expect_err("anonymous production must fail");

    assert!(error.to_string().contains("allow_anonymous"));
}
