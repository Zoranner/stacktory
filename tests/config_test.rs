use stacktory::{Config, ConfigError, RetentionConfig};

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
fn applies_default_retention_configuration() {
    let config = Config::from_toml(LOCAL_DEVELOPMENT).expect("local development configuration");

    assert_eq!(config.retention, RetentionConfig::default());
    assert_eq!(config.retention.metadata_ttl_secs, 604_800);
    assert_eq!(config.retention.audit_retention_days, 30);
    assert_eq!(config.retention.cleanup_interval_secs, 3_600);
    assert_eq!(config.retention.rustup_snapshot_revisions, 3);
    assert_eq!(config.retention.artifact_idle_secs, 7_776_000);
}

#[test]
fn accepts_explicit_retention_configuration() {
    let configuration = LOCAL_DEVELOPMENT.to_owned()
        + "\n[retention]\nmetadata_ttl_secs = 86_400\naudit_retention_days = 7\ncleanup_interval_secs = 600\nrustup_snapshot_revisions = 4\nartifact_idle_secs = 0\n";

    let config = Config::from_toml(&configuration).expect("explicit retention configuration");

    assert_eq!(config.retention.metadata_ttl_secs, 86_400);
    assert_eq!(config.retention.audit_retention_days, 7);
    assert_eq!(config.retention.cleanup_interval_secs, 600);
    assert_eq!(config.retention.rustup_snapshot_revisions, 4);
    assert_eq!(config.retention.artifact_idle_secs, 0);
}

#[test]
fn rejects_zero_retention_values() {
    let cases = [
        (
            "[retention]\nmetadata_ttl_secs = 0\n",
            ConfigError::InvalidMetadataTtl,
        ),
        (
            "[retention]\naudit_retention_days = 0\n",
            ConfigError::InvalidAuditRetentionDays,
        ),
        (
            "[retention]\ncleanup_interval_secs = 0\n",
            ConfigError::InvalidCleanupInterval,
        ),
        (
            "[retention]\nrustup_snapshot_revisions = 0\n",
            ConfigError::InvalidRustupSnapshotRevisions,
        ),
    ];

    for (section, expected) in cases {
        let configuration = LOCAL_DEVELOPMENT.to_owned() + "\n" + section;
        let error = Config::from_toml(&configuration).expect_err("zero retention value");
        assert_eq!(error.to_string(), expected.to_string());
    }
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
            .get("official")
            .expect("cargo official source")
            .source_type,
        stacktory::SourceType::Proxy
    );
    assert_eq!(
        config
            .cargo
            .as_ref()
            .expect("cargo configuration")
            .sources
            .get("official")
            .expect("cargo official source")
            .base_url
            .as_ref()
            .expect("cargo official source URL")
            .as_str(),
        "https://index.crates.io/"
    );
    assert_source_names(
        &config.rustup.sources,
        &["aliyun", "group", "official", "tsinghua"],
    );
    assert_source_names(
        &config.cargo.as_ref().expect("cargo configuration").sources,
        &["group", "official", "ustc"],
    );
}

#[test]
fn deployment_config_retains_all_configured_ecosystem_source_graphs() {
    let config = Config::from_toml(DEPLOYMENT_CONFIG).expect("deployment config");

    assert_source_names(
        &config.docker.expect("docker configuration").sources,
        &[
            "aliyun",
            "ghcr",
            "group",
            "microsoft",
            "official",
            "quay",
            "unstructured",
            "xuanyuan",
        ],
    );
    assert_source_names(
        &config.npm.expect("npm configuration").sources,
        &["binary", "group", "huawei", "npmjs", "npmmirror"],
    );
    assert_source_names(
        &config.nuget.expect("nuget configuration").sources,
        &["group", "official"],
    );
    assert_source_names(
        &config.pypi.expect("pypi configuration").sources,
        &["group", "official", "tsinghua", "ustc"],
    );
    assert_source_names(
        &config.upm.expect("upm configuration").sources,
        &["cesium", "group", "openupm", "unity"],
    );
    assert_source_names(
        &config
            .vscode
            .as_ref()
            .expect("vscode configuration")
            .sources,
        &["group", "official"],
    );
    assert_eq!(
        config
            .vscode
            .as_ref()
            .expect("vscode configuration")
            .sources
            .get("official")
            .expect("vscode official source")
            .base_url
            .as_ref()
            .expect("vscode official source URL")
            .as_str(),
        "https://marketplace.visualstudio.com/_apis/public/gallery"
    );
}

#[test]
fn accepts_public_production_configuration() {
    let config = Config::from_toml(PUBLIC_PRODUCTION).expect("public production configuration");

    assert_eq!(config.listen_address.to_string(), "0.0.0.0");
}

#[test]
fn docker_sources_accept_optional_cli_listen_ports() {
    let configuration = format!(
        "{LOCAL_DEVELOPMENT}\n\
\n\
[docker]\n\
\n\
[[docker.sources]]\n\
name = \"official\"\n\
type = \"proxy\"\n\
base_url = \"https://registry.example\"\n\
listen_port = 19001\n\
\n\
[[docker.sources]]\n\
name = \"group\"\n\
type = \"group\"\n\
members = [\"official\"]\n\
listen_port = 19000\n"
    );

    let config = Config::from_toml(&configuration).expect("Docker CLI port configuration");
    let docker = config.docker.expect("Docker configuration");

    assert_eq!(docker.listen_ports["official"], 19001);
    assert_eq!(docker.listen_ports["group"], 19000);
}

#[test]
fn docker_cli_listen_ports_must_be_unique() {
    let configuration = format!(
        "{LOCAL_DEVELOPMENT}\n\
\n\
[docker]\n\
\n\
[[docker.sources]]\n\
name = \"first\"\n\
type = \"proxy\"\n\
base_url = \"https://first.example\"\n\
listen_port = 19000\n\
\n\
[[docker.sources]]\n\
name = \"second\"\n\
type = \"proxy\"\n\
base_url = \"https://second.example\"\n\
listen_port = 19000\n"
    );

    let error = Config::from_toml(&configuration).expect_err("duplicate Docker port");
    assert!(error.to_string().contains("19000"));
}

#[test]
fn docker_cli_listen_port_cannot_use_the_main_port() {
    let configuration = format!(
        "{LOCAL_DEVELOPMENT}\n\
\n\
[docker]\n\
\n\
[[docker.sources]]\n\
name = \"official\"\n\
type = \"proxy\"\n\
base_url = \"https://official.example\"\n\
listen_port = 18761\n"
    );

    let error = Config::from_toml(&configuration).expect_err("main port conflict");
    assert!(error.to_string().contains("conflicts"));
}

#[test]
fn accepts_cargo_source_graph() {
    let configuration = format!(
        "{LOCAL_DEVELOPMENT}\n\
\n\
[cargo]\n\
\n\
[[cargo.sources]]\n\
name = \"cargo-group\"\n\
type = \"group\"\n\
members = [\"cargo-primary\", \"cargo-fallback\"]\n\
\n\
[[cargo.sources]]\n\
name = \"cargo-primary\"\n\
type = \"proxy\"\n\
base_url = \"https://primary.example.internal\"\n\
\n\
[[cargo.sources]]\n\
name = \"cargo-fallback\"\n\
type = \"proxy\"\n\
base_url = \"https://fallback.example.internal\"\n"
    );

    let config = Config::from_toml(&configuration).expect("cargo configuration");
    let cargo = config.cargo.expect("cargo configuration");

    assert_eq!(
        cargo
            .sources
            .proxy_members("cargo-group")
            .expect("cargo group")
            .into_iter()
            .map(|source| source.name.as_str())
            .collect::<Vec<_>>(),
        ["cargo-primary", "cargo-fallback"]
    );
}

#[test]
fn nuget_proxy_requires_a_v3_service_index_url() {
    let configuration = format!(
        "{LOCAL_DEVELOPMENT}\n\
\n\
[nuget]\n\
\n\
[[nuget.sources]]\n\
name = \"official\"\n\
type = \"proxy\"\n\
base_url = \"https://api.nuget.org/v3/index.json\"\n"
    );

    let config = Config::from_toml(&configuration).expect("NuGet service index configuration");
    assert_eq!(
        config
            .nuget
            .expect("NuGet configuration")
            .sources
            .get("official")
            .expect("NuGet source")
            .base_url
            .as_ref()
            .expect("NuGet service index URL")
            .path(),
        "/v3/index.json"
    );
}

#[test]
fn rejects_nuget_proxy_without_a_v3_service_index_url() {
    let configuration = format!(
        "{LOCAL_DEVELOPMENT}\n\
\n\
[nuget]\n\
\n\
[[nuget.sources]]\n\
name = \"official\"\n\
type = \"proxy\"\n\
base_url = \"https://api.nuget.org/v3\"\n"
    );

    let error = Config::from_toml(&configuration).expect_err("NuGet registry root must fail");
    assert!(error.to_string().contains("v3/index.json"));
}

fn assert_source_names(graph: &stacktory::SourceGraph, expected: &[&str]) {
    for name in expected {
        assert!(
            graph.get(name).is_some(),
            "source should be configured: {name}"
        );
    }
}
