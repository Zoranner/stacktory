//! 按配置组装适配器注册表，并为 Docker 的标准 Registry 入口生成监听项。

use std::sync::Arc;

use crate::{
    Config,
    ecosystems::{
        cargo::CargoAdapter,
        docker::{DockerRegistrationOptions, register_adapter_with_options},
        npm::NpmAdapter,
        nuget::NugetAdapter,
        upm::UpmAdapter,
        vscode::VscodeAdapter,
    },
    upstream::{ArtifactBudget, FetchTimeouts, SourcePool, registry::AdapterRegistry},
};

pub(super) struct DockerListener {
    pub(super) address: std::net::IpAddr,
    pub(super) port: u16,
    pub(super) entry_name: String,
}

struct RegistrationContext<'a> {
    proxy_url: Option<&'a url::Url>,
    proxy_no_proxy: Option<&'a str>,
    fetch_timeouts: FetchTimeouts,
    artifact_budget: &'a ArtifactBudget,
}

#[cfg(test)]
fn configured_registry(
    config: &Config,
    docker_listeners: &mut Vec<DockerListener>,
) -> Result<AdapterRegistry, Box<dyn std::error::Error>> {
    let artifact_budget = ArtifactBudget::default();
    configured_registry_with_budget(config, docker_listeners, &artifact_budget)
}

pub(super) fn configured_registry_with_budget(
    config: &Config,
    docker_listeners: &mut Vec<DockerListener>,
    artifact_budget: &ArtifactBudget,
) -> Result<AdapterRegistry, Box<dyn std::error::Error>> {
    let mut registry = AdapterRegistry::new();
    let proxy_url = config.proxy.url.clone();
    let proxy_no_proxy = config.proxy.no_proxy.clone();
    let fetch_timeouts = FetchTimeouts {
        metadata: config.upstream.metadata_timeout(),
        artifact: config.upstream.artifact_timeout(),
    };
    let registration = RegistrationContext {
        proxy_url: proxy_url.as_ref(),
        proxy_no_proxy: proxy_no_proxy.as_deref(),
        fetch_timeouts,
        artifact_budget,
    };
    if let Some(cargo) = config.cargo.clone() {
        register_ecosystem(
            &mut registry,
            "cargo",
            cargo.sources,
            CargoAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            ),
            &registration,
        )?;
    }
    if let Some(npm) = config.npm.clone() {
        register_ecosystem(
            &mut registry,
            "npm",
            npm.sources,
            NpmAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            ),
            &registration,
        )?;
    }
    if let Some(nuget) = config.nuget.clone() {
        register_ecosystem(
            &mut registry,
            "nuget",
            nuget.sources,
            NugetAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            ),
            &registration,
        )?;
    }
    if let Some(upm) = config.upm.clone() {
        register_ecosystem(
            &mut registry,
            "upm",
            upm.sources,
            UpmAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            ),
            &registration,
        )?;
    }
    if let Some(vscode) = config.vscode.clone() {
        register_ecosystem(
            &mut registry,
            "vscode",
            vscode.sources,
            VscodeAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            ),
            &registration,
        )?;
    }
    if let Some(docker) = config.docker.clone() {
        docker_listeners.extend(docker.registry_endpoints.clone().into_iter().map(
            |(entry_name, endpoint)| DockerListener {
                address: config.server.address,
                port: endpoint.listen_port,
                entry_name,
            },
        ));
        register_adapter_with_options(
            &mut registry,
            docker,
            config.server.public_url.clone(),
            config.storage.data_dir.clone(),
            DockerRegistrationOptions {
                proxy_url: registration.proxy_url,
                proxy_no_proxy: registration.proxy_no_proxy,
                fetch_timeouts: registration.fetch_timeouts,
                artifact_budget: registration.artifact_budget,
            },
        )?;
    }
    Ok(registry)
}

fn register_ecosystem<A: crate::upstream::registry::Adapter>(
    registry: &mut AdapterRegistry,
    ecosystem: &str,
    sources: crate::SourceGraph,
    adapter: A,
    context: &RegistrationContext<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    registry.register(
        ecosystem,
        Arc::new(
            SourcePool::new_with_proxy(
                ecosystem,
                sources,
                context.proxy_url,
                context.proxy_no_proxy,
            )?
            .with_artifact_budget(context.artifact_budget.clone())
            .with_fetch_timeouts(context.fetch_timeouts),
        ),
        Arc::new(adapter),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, path::PathBuf};

    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    use url::Url;

    use crate::{
        Config, EcosystemConfig, NugetConfig, ProxyConfig, RetentionConfig, RustupConfig,
        ServerConfig, SourceConfig, SourceGraph, SourceType, StorageConfig, UpmConfig,
        UpstreamConfig, http::router_with_registry,
    };

    #[tokio::test]
    async fn configured_npm_and_upm_are_registered_separately() {
        let config = Config {
            server: ServerConfig {
                address: "127.0.0.1".parse::<IpAddr>().expect("address"),
                port: 18761,
                public_url: Url::parse("http://127.0.0.1:18761")
                    .expect("public URL")
                    .into(),
            },
            storage: StorageConfig {
                data_dir: tempfile::tempdir().expect("data directory").keep(),
                log_dir: PathBuf::from("logs"),
            },
            proxy: ProxyConfig::default(),
            upstream: UpstreamConfig::default(),
            retention: RetentionConfig::default(),
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
            nuget: Some(NugetConfig {
                sources: SourceGraph::new(vec![proxy(
                    "official",
                    "http://127.0.0.1:9/v3/index.json",
                )])
                .expect("NuGet sources"),
            }),
            upm: Some(UpmConfig {
                sources: SourceGraph::new(vec![proxy("unity", "http://127.0.0.1:9")])
                    .expect("upm sources"),
            }),
        };

        let mut docker_listeners = Vec::new();
        let registry = super::configured_registry(&config, &mut docker_listeners)
            .expect("configured registry");
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
            .clone()
            .oneshot(
                Request::get("/mirror/upm/unity/com.company.package")
                    .body(Body::empty())
                    .expect("upm request"),
            )
            .await
            .expect("upm response");
        let nuget_response = router
            .oneshot(
                Request::get("/mirror/nuget/official/v3/index.json")
                    .body(Body::empty())
                    .expect("NuGet request"),
            )
            .await
            .expect("NuGet response");

        assert_ne!(npm_response.status(), axum::http::StatusCode::NOT_FOUND);
        assert_ne!(upm_response.status(), axum::http::StatusCode::NOT_FOUND);
        assert_ne!(nuget_response.status(), axum::http::StatusCode::NOT_FOUND);
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
