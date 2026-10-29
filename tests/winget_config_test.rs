use stacktory::{Config, ConfigError};

const BASE: &str = r#"
[server]
address = "127.0.0.1"
port = 18761
public_url = "http://127.0.0.1:18761"

[storage]
data_dir = "./var/stacktory"

[rustup]
refresh_interval_secs = 3600
channels = ["stable"]

[[rustup.sources]]
name = "official"
type = "proxy"
base_url = "https://static.rust-lang.org"
"#;

const WINGET: &str = r#"
[winget]

[[winget.sources]]
name = "internal"
type = "proxy"
base_url = "https://winget.example/api"
installer_hosts = ["github.com", "objects.githubusercontent.com", "127.0.0.1:18080"]

[[winget.sources]]
name = "group"
type = "group"
members = ["internal"]
"#;

#[test]
fn parses_winget_sources_with_a_proxy_installer_allowlist() {
    let config = Config::from_toml(&(BASE.to_owned() + WINGET)).expect("Winget configuration");
    let winget = config.winget.expect("Winget config");

    assert_eq!(
        winget
            .installer_hosts
            .get("internal")
            .expect("installer hosts")
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "127.0.0.1:18080",
            "github.com",
            "objects.githubusercontent.com"
        ]
    );
    assert!(!winget.installer_hosts.contains_key("group"));
    assert_eq!(
        winget
            .sources
            .proxy_members("group")
            .expect("group members")
            .iter()
            .map(|source| source.name.as_str())
            .collect::<Vec<_>>(),
        ["internal"]
    );
}

#[test]
fn winget_proxy_requires_an_installer_allowlist() {
    let configuration = (BASE.to_owned() + WINGET).replace(
        "installer_hosts = [\"github.com\", \"objects.githubusercontent.com\", \"127.0.0.1:18080\"]",
        "",
    );
    let error = Config::from_toml(&configuration).expect_err("missing allowlist");
    assert!(
        matches!(error, ConfigError::WingetInstallerHostsMissing(ref name) if name == "internal")
    );
}

#[test]
fn winget_group_rejects_installer_hosts() {
    let configuration = (BASE.to_owned() + WINGET).replace(
        "type = \"group\"\nmembers = [\"internal\"]",
        "type = \"group\"\ninstaller_hosts = [\"github.com\"]\nmembers = [\"internal\"]",
    );
    let error = Config::from_toml(&configuration).expect_err("group allowlist");
    assert!(
        matches!(error, ConfigError::WingetGroupWithInstallerHosts(ref name) if name == "group")
    );
}

#[test]
fn winget_installer_host_accepts_only_host_or_host_port() {
    for entry in [
        "https://github.com",
        "github.com/path",
        "*.github.com",
        "github.com?x=1",
        "github.com:0",
    ] {
        let configuration = (BASE.to_owned() + WINGET)
            .replace("installer_hosts = [\"github.com\", \"objects.githubusercontent.com\", \"127.0.0.1:18080\"]", &format!("installer_hosts = [\"{entry}\"]"));
        let error = Config::from_toml(&configuration).expect_err("invalid installer host");
        assert!(
            matches!(error, ConfigError::InvalidWingetInstallerHost(ref name, _) if name == "internal"),
            "{entry}"
        );
    }
}

#[test]
fn winget_source_rejects_unknown_fields() {
    let configuration = (BASE.to_owned() + WINGET).replace(
        "base_url = \"https://winget.example/api\"",
        "base_url = \"https://winget.example/api\"\ninstaller_origin = [\"github.com\"]",
    );
    assert!(Config::from_toml(&configuration).is_err());
}
