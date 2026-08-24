use stacktory::Config;

const LOCAL_DEVELOPMENT: &str = r#"
listen = "127.0.0.1:18081"
data_dir = "./var/stacktory"
public_base_url = "http://127.0.0.1:18081"

[access]
allow_anonymous = true

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

[[rustup.upstreams]]
name = "company-nexus"
base_url = "https://pkg.example.internal/repository/rustup-group"
"#;

#[test]
fn accepts_explicit_local_anonymous_development_configuration() {
    let config = Config::from_toml(LOCAL_DEVELOPMENT).expect("local development configuration");

    assert_eq!(config.listen.port(), 18081);
    assert_eq!(config.rustup.upstreams.len(), 1);
}

#[test]
fn rejects_anonymous_production_configuration() {
    let error =
        Config::from_toml(ANONYMOUS_PRODUCTION).expect_err("anonymous production must fail");

    assert!(error.to_string().contains("allow_anonymous"));
}
