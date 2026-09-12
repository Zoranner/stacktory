use std::{fs, sync::Arc};

use stacktory::{
    Config, SourcePool, cargo::CargoAdapter, http::router_with_rustup_and_registry,
    service::RustupService, sources::registry::AdapterRegistry, vscode::VscodeAdapter,
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::from_toml(&fs::read_to_string("config.toml")?)?;
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
    let listener = TcpListener::bind((config.listen_address, config.listen_port)).await?;
    let service = Arc::new(RustupService::new(config)?);
    axum::serve(listener, router_with_rustup_and_registry(service, registry)).await?;
    Ok(())
}
