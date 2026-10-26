//! 启动服务：读取配置、注册适配器、监听端口并等待退出信号。

use std::{fs, sync::Arc};

use tokio::net::TcpListener;

use super::{read_config, registry::configured_registry_with_budget};
use crate::{
    ecosystems::rustup::RustupService,
    http::{router_with_docker_entry, router_with_rustup_and_registry},
    logging, retention,
    upstream::ArtifactBudget,
};

const PUBLIC_URL_MARKER: &str = ".public-url";

pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
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
        retention::Maintainer::new(|data_dir, keep_revisions| {
            crate::ecosystems::rustup::maintain::prune_snapshots(data_dir, keep_revisions)
                .map_err(|error| error.to_string())
        }),
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

/// 记录本次启动使用的公开地址，供运行期判断缓存中的地址是否仍旧有效。
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

#[cfg(test)]
mod tests {
    use std::fs;

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
}
