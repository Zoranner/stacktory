use std::{fs, sync::Arc};

use stacktory::{
    Config, SourcePool,
    audit::AuditSink,
    cargo::CargoAdapter,
    docker::{DockerRegistrationOptions, register_adapter_with_options},
    http::{router_with_docker_entry, router_with_rustup_and_registry},
    logging,
    npm::NpmAdapter,
    nuget::NugetAdapter,
    retention,
    service::RustupService,
    sources::{ArtifactBudget, FetchTimeouts, registry::AdapterRegistry},
    upm::UpmAdapter,
    vscode::VscodeAdapter,
};
use tokio::net::TcpListener;

const PUBLIC_URL_MARKER: &str = ".public-url";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None => serve().await,
        Some("prune-legacy-layout") => prune_legacy_layout(arguments),
        Some("invalidate-nuget-metadata") => invalidate_nuget_metadata(arguments),
        Some(command) => {
            eprintln!("Stacktory: unknown command `{command}`");
            eprintln!(
                "usage: stacktory [prune-legacy-layout [--dry-run] | invalidate-nuget-metadata --entry <name> [--dry-run]]"
            );
            std::process::exit(2);
        }
    }
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let config = read_config()?;
    let (log_file, rotated_log) =
        logging::init(config.storage.log_dir.clone(), std::time::SystemTime::now())?;
    if record_public_url(&config.storage.data_dir, config.server.public_url.as_str())? {
        logging::info("recorded the configured public_url; cached metadata will render lazily");
    }
    let mut docker_listeners = Vec::new();
    let artifact_budget = ArtifactBudget::default();
    let registry =
        configured_registry_with_budget(&config, &mut docker_listeners, &artifact_budget)?;
    let listener = TcpListener::bind((config.server.address, config.server.port)).await?;
    retention::spawn(
        config.storage.data_dir.clone(),
        log_file.clone(),
        config.retention,
    );
    if let Some(rotated) = rotated_log {
        logging::info(format!("rotated stale log file to {rotated}"));
    }
    logging::info(format!(
        "listening on {}:{}; data_dir={}; log_dir={}; log_file={}",
        config.server.address,
        config.server.port,
        config.storage.data_dir.display(),
        config.storage.log_dir.display(),
        log_file.path().display()
    ));
    let service = Arc::new(RustupService::new_with_artifact_budget(
        config,
        artifact_budget,
    )?);
    let mut docker_tasks = Vec::new();
    for docker_listener in docker_listeners {
        let listener = TcpListener::bind((docker_listener.address, docker_listener.port)).await?;
        let router = router_with_docker_entry(registry.clone(), docker_listener.entry_name);
        docker_tasks.push(tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown_signal())
                .await
            {
                logging::error(format!("docker listener failed: {error}"));
            }
        }));
    }
    axum::serve(listener, router_with_rustup_and_registry(service, registry))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    for task in docker_tasks {
        if let Err(error) = task.await {
            logging::error(format!("docker listener task failed: {error}"));
        }
    }
    Ok(())
}

fn record_public_url(data_dir: &std::path::Path, public_url: &str) -> Result<bool, std::io::Error> {
    fs::create_dir_all(data_dir)?;
    let marker_path = data_dir.join(PUBLIC_URL_MARKER);
    match fs::read_to_string(&marker_path) {
        Ok(current) if current == public_url => return Ok(false),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    fs::write(marker_path, public_url)?;
    Ok(true)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            logging::error(format!("failed to listen for Ctrl+C: {error}"));
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                logging::error(format!("failed to listen for SIGTERM: {error}"));
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    logging::info("shutdown signal received");
}

fn read_config() -> Result<Config, Box<dyn std::error::Error>> {
    let source = fs::read_to_string("config.toml").map_err(|error| {
        format!(
            "cannot read config.toml from the current directory: {error}. \
             Copy deploy/app/config.example.toml to config.toml and adjust it."
        )
    })?;
    Ok(Config::from_toml(&source)?)
}

fn prune_legacy_layout(
    arguments: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut dry_run = false;
    for argument in arguments {
        match argument.as_str() {
            "--dry-run" => dry_run = true,
            other => {
                eprintln!("Stacktory: unknown option `{other}`");
                eprintln!("usage: stacktory [prune-legacy-layout [--dry-run]]");
                std::process::exit(2);
            }
        }
    }
    let config = read_config()?;
    let report = retention::prune_legacy_layout(
        &config.storage.data_dir,
        dry_run,
        &AuditSink::new(config.storage.data_dir.clone()),
    );
    println!(
        "Stacktory: {} {} legacy layout entries ({:.1} MiB)",
        if dry_run { "would remove" } else { "removed" },
        report.removed_entries,
        report.removed_bytes as f64 / (1024.0 * 1024.0)
    );
    if report.failed_entries > 0 {
        eprintln!(
            "Stacktory: {} legacy layout entries could not be removed",
            report.failed_entries
        );
        std::process::exit(1);
    }
    Ok(())
}

fn invalidate_nuget_metadata(
    mut arguments: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut entry_name = None;
    let mut dry_run = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--entry" => {
                let value = arguments.next().ok_or("--entry requires a source name")?;
                entry_name = Some(value);
            }
            "--dry-run" => dry_run = true,
            other => {
                return Err(format!("unknown option `{other}`").into());
            }
        }
    }
    let entry_name = entry_name.ok_or("--entry is required")?;
    let config = read_config()?;
    let Some(nuget) = config.nuget.as_ref() else {
        return Err("the configured file has no [nuget] section".into());
    };
    if nuget.sources.proxy_members(&entry_name).is_none() {
        return Err(format!("NuGet source entry `{entry_name}` was not found").into());
    }

    let metadata_path =
        stacktory::layout::metadata_root(&config.storage.data_dir, "nuget").join(&entry_name);
    if dry_run {
        println!("Stacktory: would remove {}", metadata_path.display());
        return Ok(());
    }
    match fs::remove_dir_all(&metadata_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    AuditSink::new(config.storage.data_dir).record(
        "nuget_metadata_invalidated",
        &entry_name,
        "success",
    )?;
    println!("Stacktory: removed NuGet metadata for `{entry_name}`");
    Ok(())
}

struct DockerListener {
    address: std::net::IpAddr,
    port: u16,
    entry_name: String,
}

#[cfg(test)]
fn configured_registry(
    config: &Config,
    docker_listeners: &mut Vec<DockerListener>,
) -> Result<AdapterRegistry, Box<dyn std::error::Error>> {
    let artifact_budget = ArtifactBudget::default();
    configured_registry_with_budget(config, docker_listeners, &artifact_budget)
}

fn configured_registry_with_budget(
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
    if let Some(cargo) = config.cargo.clone() {
        registry.register(
            "cargo",
            Arc::new(
                SourcePool::new_with_proxy(
                    "cargo",
                    cargo.sources,
                    proxy_url.as_ref(),
                    proxy_no_proxy.as_deref(),
                )?
                .with_artifact_budget(artifact_budget.clone())
                .with_fetch_timeouts(fetch_timeouts),
            ),
            Arc::new(CargoAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            )),
        )?;
    }
    if let Some(npm) = config.npm.clone() {
        registry.register(
            "npm",
            Arc::new(
                SourcePool::new_with_proxy(
                    "npm",
                    npm.sources,
                    proxy_url.as_ref(),
                    proxy_no_proxy.as_deref(),
                )?
                .with_artifact_budget(artifact_budget.clone())
                .with_fetch_timeouts(fetch_timeouts),
            ),
            Arc::new(NpmAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            )),
        )?;
    }
    if let Some(nuget) = config.nuget.clone() {
        registry.register(
            "nuget",
            Arc::new(
                SourcePool::new_with_proxy(
                    "nuget",
                    nuget.sources,
                    proxy_url.as_ref(),
                    proxy_no_proxy.as_deref(),
                )?
                .with_artifact_budget(artifact_budget.clone())
                .with_fetch_timeouts(fetch_timeouts),
            ),
            Arc::new(NugetAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            )),
        )?;
    }
    if let Some(upm) = config.upm.clone() {
        registry.register(
            "upm",
            Arc::new(
                SourcePool::new_with_proxy(
                    "upm",
                    upm.sources,
                    proxy_url.as_ref(),
                    proxy_no_proxy.as_deref(),
                )?
                .with_artifact_budget(artifact_budget.clone())
                .with_fetch_timeouts(fetch_timeouts),
            ),
            Arc::new(UpmAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            )),
        )?;
    }
    if let Some(vscode) = config.vscode.clone() {
        registry.register(
            "vscode",
            Arc::new(
                SourcePool::new_with_proxy(
                    "vscode",
                    vscode.sources,
                    proxy_url.as_ref(),
                    proxy_no_proxy.as_deref(),
                )?
                .with_artifact_budget(artifact_budget.clone())
                .with_fetch_timeouts(fetch_timeouts),
            ),
            Arc::new(VscodeAdapter::new(
                config.server.public_url.clone(),
                config.storage.data_dir.clone(),
            )),
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
                proxy_url: proxy_url.as_ref(),
                proxy_no_proxy: proxy_no_proxy.as_deref(),
                fetch_timeouts,
                artifact_budget,
            },
        )?;
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use std::{fs, net::IpAddr, path::PathBuf};

    use axum::{body::Body, http::Request};
    use stacktory::{
        Config, EcosystemConfig, NugetConfig, ProxyConfig, RetentionConfig, RustupConfig,
        ServerConfig, SourceConfig, SourceGraph, SourceType, StorageConfig, UpmConfig,
        UpstreamConfig, http::router_with_registry,
    };
    use tower::ServiceExt;
    use url::Url;

    #[tokio::test]
    async fn configured_npm_and_upm_are_registered_separately() {
        let config = Config {
            server: ServerConfig {
                address: "127.0.0.1".parse::<IpAddr>().expect("address"),
                port: 18761,
                public_url: Url::parse("http://127.0.0.1:18761").expect("public URL"),
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

    #[test]
    fn records_public_url_change_without_removing_caches() {
        let directory = tempfile::tempdir().expect("data directory");
        let data_dir = directory.path();
        let rewritten_metadata = data_dir.join("stores/nuget/metadata/group/v3/index.json");
        let canonical_metadata = data_dir.join("stores/npm/metadata/group/left-pad/current.toml");
        let cargo_metadata = data_dir.join("stores/cargo/metadata/group/index/se/rd/serde");
        let docker_metadata =
            data_dir.join("stores/docker/metadata/group/library/demo/latest.json");
        let vscode_metadata = data_dir.join("stores/vscode/metadata/group/marketplace/query.json");
        let snapshot = data_dir.join("stores/rustup/snapshots/group/stable/current.toml");
        let artifact = data_dir.join("stores/npm/artifacts/npmjs/objects/artifact");
        let audit = data_dir.join("audit/events.jsonl");
        for (path, contents) in [
            (&rewritten_metadata, "old rewritten metadata"),
            (&canonical_metadata, "canonical metadata"),
            (&cargo_metadata, "cargo metadata"),
            (&docker_metadata, "docker metadata"),
            (&vscode_metadata, "vscode metadata"),
            (&snapshot, "old snapshot"),
            (&artifact, "artifact"),
            (&audit, "audit"),
        ] {
            fs::create_dir_all(path.parent().expect("parent")).expect("cache directory");
            fs::write(path, contents).expect("cache file");
        }

        assert!(
            super::record_public_url(data_dir, "https://old.example").expect("initial public URL")
        );
        assert!(rewritten_metadata.exists());
        assert!(canonical_metadata.exists());
        assert!(cargo_metadata.exists());
        assert!(docker_metadata.exists());
        assert!(vscode_metadata.exists());
        assert!(snapshot.exists());
        assert!(artifact.exists());
        assert!(audit.exists());

        assert!(
            !super::record_public_url(data_dir, "https://old.example")
                .expect("unchanged public URL")
        );
        assert!(rewritten_metadata.exists());

        assert!(
            super::record_public_url(data_dir, "https://new.example").expect("changed public URL")
        );
        assert!(rewritten_metadata.exists());
        assert!(canonical_metadata.exists());
        assert_eq!(
            fs::read_to_string(data_dir.join(super::PUBLIC_URL_MARKER)).expect("public URL marker"),
            "https://new.example"
        );
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
