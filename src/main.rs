use std::{fs, sync::Arc};

use stacktory::{
    Config, SourcePool, cargo::CargoAdapter, http::router_with_rustup_and_registry,
    npm::NpmAdapter, service::RustupService, sources::registry::AdapterRegistry,
    vscode::VscodeAdapter,
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::from_toml(&fs::read_to_string("config.toml")?)?;
    let registry = configured_registry(&mut config)?;
    let listener = TcpListener::bind((config.listen_address, config.listen_port)).await?;
    let service = Arc::new(RustupService::new(config)?);
    axum::serve(listener, router_with_rustup_and_registry(service, registry)).await?;
    Ok(())
}

fn configured_registry(config: &mut Config) -> Result<AdapterRegistry, Box<dyn std::error::Error>> {
    let mut registry = AdapterRegistry::new();
    if let Some(cargo) = config.cargo.take() {
        registry.register(
            "cargo",
            Arc::new(SourcePool::new("cargo", cargo.sources)?),
            Arc::new(CargoAdapter::new(
                config.root_url.clone(),
                config.data_dir.clone(),
            )),
        )?;
    }
    if let Some(npm) = config.npm.take() {
        registry.register(
            "npm",
            Arc::new(SourcePool::new("npm", npm.sources)?),
            Arc::new(NpmAdapter::new(
                config.root_url.clone(),
                config.data_dir.clone(),
            )),
        )?;
    }
    if let Some(vscode) = config.vscode.take() {
        registry.register(
            "vscode",
            Arc::new(SourcePool::new("vscode", vscode.sources)?),
            Arc::new(VscodeAdapter::new(
                config.root_url.clone(),
                config.data_dir.clone(),
            )),
        )?;
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use axum::{body::Body, http::Request};
    use stacktory::{
        Config, EcosystemConfig, RustupConfig, SourceConfig, SourceGraph, SourceType, UpmConfig,
        http::router_with_registry,
    };
    use tower::ServiceExt;
    use url::Url;

    #[tokio::test]
    async fn configured_npm_is_registered_without_registering_upm() {
        let mut config = Config {
            listen_address: "127.0.0.1".parse::<IpAddr>().expect("address"),
            listen_port: 18761,
            data_dir: tempfile::tempdir().expect("data directory").keep(),
            root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
            rustup: RustupConfig {
                refresh_interval_secs: 60,
                channels: vec!["stable".to_owned()],
                sources: SourceGraph::new(vec![proxy("rustup", "http://127.0.0.1:9")])
                    .expect("rustup sources"),
            },
            cargo: None,
            vscode: None,
            docker: None,
            npm: Some(EcosystemConfig {
                sources: SourceGraph::new(vec![proxy("official", "http://127.0.0.1:9")])
                    .expect("npm sources"),
            }),
            nuget: None,
            pypi: None,
            upm: Some(UpmConfig {
                sources: SourceGraph::new(vec![proxy("unity", "http://127.0.0.1:9")])
                    .expect("upm sources"),
            }),
        };

        let registry = super::configured_registry(&mut config).expect("configured registry");
        let router = router_with_registry(registry);
        let npm_response = router
            .clone()
            .oneshot(
                Request::get("/mirror/npm/official/left-pad")
                    .body(Body::empty())
                    .expect("npm request"),
            )
            .await
            .expect("npm response");
        let upm_response = router
            .oneshot(
                Request::get("/mirror/upm/unity/com.example.package")
                    .body(Body::empty())
                    .expect("upm request"),
            )
            .await
            .expect("upm response");

        assert_ne!(npm_response.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(upm_response.status(), axum::http::StatusCode::NOT_FOUND);
    }

    fn proxy(name: &str, base_url: &str) -> SourceConfig {
        SourceConfig {
            name: name.to_owned(),
            source_type: SourceType::Proxy,
            base_url: Some(Url::parse(base_url).expect("base URL")),
            require_https: false,
            members: vec![],
        }
    }
}
