use std::{env, fs, sync::Arc};

use stacktory::{
    Config, SourcePool, cargo::CargoAdapter, http::router_with_rustup_and_cargo,
    service::RustupService,
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path =
        env::var("STACKTORY_CONFIG").unwrap_or_else(|_| "/app/config.toml".to_owned());
    let mut config = Config::from_toml(&fs::read_to_string(config_path)?)?;
    let cargo = config
        .cargo
        .take()
        .map(|cargo| {
            SourcePool::new("cargo", cargo.sources).map(|pool| {
                (
                    Arc::new(CargoAdapter::new(
                        config.root_url.clone(),
                        config.data_dir.clone(),
                    )),
                    Arc::new(pool),
                )
            })
        })
        .transpose()?;
    let listener = TcpListener::bind((config.listen_address, config.listen_port)).await?;
    let service = Arc::new(RustupService::new(config)?);
    axum::serve(listener, router_with_rustup_and_cargo(service, cargo)).await?;
    Ok(())
}
