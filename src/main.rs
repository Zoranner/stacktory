use std::{fs, sync::Arc};

use stacktory::{
    Config, SourcePool,
    audit::AuditSink,
    cargo::CargoAdapter,
    docker::register_adapter_with_proxy,
    http::{router_with_docker_entry, router_with_rustup_and_registry},
    logging,
    npm::NpmAdapter,
    nuget::NugetAdapter,
    retention,
    service::RustupService,
    sources::registry::AdapterRegistry,
    upm::UpmAdapter,
    vscode::VscodeAdapter,
};
use tokio::net::TcpListener;

const ROOT_URL_MARKER: &str = ".root-url";

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
    let mut config = read_config()?;
    let (log_file, rotated_log) =
        logging::init(config.log_dir.clone(), std::time::SystemTime::now())?;
    if record_root_url(&config.data_dir, config.root_url.as_str())? {
        logging::info("recorded the configured root_url; cached metadata will render lazily");
    }
    if config.pypi.is_some() {
        logging::warn("pypi is configured but the PyPI adapter is not registered");
    }
    let mut docker_listeners = Vec::new();
    let registry = configured_registry(&mut config, &mut docker_listeners)?;
    let listener = TcpListener::bind((config.listen_address, config.listen_port)).await?;
    retention::spawn(config.data_dir.clone(), log_file.clone(), config.retention);
    if let Some(rotated) = rotated_log {
        logging::info(format!("rotated stale log file to {rotated}"));
    }
    logging::info(format!(
        "listening on {}:{}; data_dir={}; log_dir={}; log_file={}",
        config.listen_address,
        config.listen_port,
        config.data_dir.display(),
        config.log_dir.display(),
        log_file.path().display()
    ));
    let service = Arc::new(RustupService::new(config)?);
    for docker_listener in docker_listeners {
        let listener = TcpListener::bind((docker_listener.address, docker_listener.port)).await?;
        let router = router_with_docker_entry(registry.clone(), docker_listener.entry_name);
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                logging::error(format!("docker listener failed: {error}"));
            }
        });
    }
    axum::serve(listener, router_with_rustup_and_registry(service, registry))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn record_root_url(data_dir: &std::path::Path, root_url: &str) -> Result<bool, std::io::Error> {
    fs::create_dir_all(data_dir)?;
    let marker_path = data_dir.join(ROOT_URL_MARKER);
    match fs::read_to_string(&marker_path) {
        Ok(current) if current == root_url => return Ok(false),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    fs::write(marker_path, root_url)?;
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
        &config.data_dir,
        dry_run,
        &AuditSink::new(config.data_dir.clone()),
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
        stacktory::layout::metadata_root(&config.data_dir, "nuget").join(&entry_name);
    if dry_run {
        println!("Stacktory: would remove {}", metadata_path.display());
        return Ok(());
    }
    match fs::remove_dir_all(&metadata_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    AuditSink::new(config.data_dir).record("nuget_metadata_invalidated", &entry_name, "success")?;
    println!("Stacktory: removed NuGet metadata for `{entry_name}`");
    Ok(())
}

struct DockerListener {
    address: std::net::IpAddr,
    port: u16,
    entry_name: String,
}

fn configured_registry(
    config: &mut Config,
    docker_listeners: &mut Vec<DockerListener>,
) -> Result<AdapterRegistry, Box<dyn std::error::Error>> {
    let mut registry = AdapterRegistry::new();
    let proxy_url = config.proxy_url.clone();
    let proxy_no_proxy = config.proxy_no_proxy.clone();
    if let Some(cargo) = config.cargo.take() {
        registry.register(
            "cargo",
            Arc::new(SourcePool::new_with_proxy(
                "cargo",
                cargo.sources,
                proxy_url.as_ref(),
                proxy_no_proxy.as_deref(),
            )?),
            Arc::new(CargoAdapter::new(
                config.root_url.clone(),
                config.data_dir.clone(),
            )),
        )?;
    }
    if let Some(npm) = config.npm.take() {
        registry.register(
            "npm",
            Arc::new(SourcePool::new_with_proxy(
                "npm",
                npm.sources,
                proxy_url.as_ref(),
                proxy_no_proxy.as_deref(),
            )?),
            Arc::new(NpmAdapter::new(
                config.root_url.clone(),
                config.data_dir.clone(),
            )),
        )?;
    }
    if let Some(nuget) = config.nuget.take() {
        registry.register(
            "nuget",
            Arc::new(SourcePool::new_with_proxy(
                "nuget",
                nuget.sources,
                proxy_url.as_ref(),
                proxy_no_proxy.as_deref(),
            )?),
            Arc::new(NugetAdapter::new(
                config.root_url.clone(),
                config.data_dir.clone(),
            )),
        )?;
    }
    if let Some(upm) = config.upm.take() {
        registry.register(
            "upm",
            Arc::new(SourcePool::new_with_proxy(
                "upm",
                upm.sources,
                proxy_url.as_ref(),
                proxy_no_proxy.as_deref(),
            )?),
            Arc::new(UpmAdapter::new(
                config.root_url.clone(),
                config.data_dir.clone(),
            )),
        )?;
    }
    if let Some(vscode) = config.vscode.take() {
        registry.register(
            "vscode",
            Arc::new(SourcePool::new_with_proxy(
                "vscode",
                vscode.sources,
                proxy_url.as_ref(),
                proxy_no_proxy.as_deref(),
            )?),
            Arc::new(VscodeAdapter::new(
                config.root_url.clone(),
                config.data_dir.clone(),
            )),
        )?;
    }
    if let Some(docker) = config.docker.take() {
        docker_listeners.extend(docker.listen_ports.clone().into_iter().map(
            |(entry_name, port)| DockerListener {
                address: config.listen_address,
                port,
                entry_name,
            },
        ));
        register_adapter_with_proxy(
            &mut registry,
            docker,
            config.root_url.clone(),
            config.data_dir.clone(),
            proxy_url.as_ref(),
            proxy_no_proxy.as_deref(),
        )?;
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use std::{fs, net::IpAddr, path::PathBuf};

    use axum::{body::Body, http::Request};
    use stacktory::{
        Config, EcosystemConfig, NugetConfig, RetentionConfig, RustupConfig, SourceConfig,
        SourceGraph, SourceType, UpmConfig, http::router_with_registry,
    };
    use tower::ServiceExt;
    use url::Url;

    #[tokio::test]
    async fn configured_npm_and_upm_are_registered_separately() {
        let mut config = Config {
            listen_address: "127.0.0.1".parse::<IpAddr>().expect("address"),
            listen_port: 18761,
            data_dir: tempfile::tempdir().expect("data directory").keep(),
            log_dir: PathBuf::from("logs"),
            root_url: Url::parse("http://127.0.0.1:18761").expect("root URL"),
            proxy_url: None,
            proxy_no_proxy: None,
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
            pypi: None,
            upm: Some(UpmConfig {
                sources: SourceGraph::new(vec![proxy("unity", "http://127.0.0.1:9")])
                    .expect("upm sources"),
            }),
        };

        let mut docker_listeners = Vec::new();
        let registry = super::configured_registry(&mut config, &mut docker_listeners)
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
    fn records_root_url_change_without_removing_caches() {
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

        assert!(super::record_root_url(data_dir, "https://old.example").expect("initial root URL"));
        assert!(rewritten_metadata.exists());
        assert!(canonical_metadata.exists());
        assert!(cargo_metadata.exists());
        assert!(docker_metadata.exists());
        assert!(vscode_metadata.exists());
        assert!(snapshot.exists());
        assert!(artifact.exists());
        assert!(audit.exists());

        assert!(
            !super::record_root_url(data_dir, "https://old.example").expect("unchanged root URL")
        );
        assert!(rewritten_metadata.exists());

        assert!(super::record_root_url(data_dir, "https://new.example").expect("changed root URL"));
        assert!(rewritten_metadata.exists());
        assert!(canonical_metadata.exists());
        assert_eq!(
            fs::read_to_string(data_dir.join(super::ROOT_URL_MARKER)).expect("root URL marker"),
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
