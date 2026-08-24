use std::{env, fs, sync::Arc};

use stacktory::{AccessGate, Config, http::router_with_rustup, service::RustupService};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path =
        env::var("STACKTORY_CONFIG").unwrap_or_else(|_| "/app/config.toml".to_owned());
    let config = Config::from_toml(&fs::read_to_string(config_path)?)?;
    let access = Arc::new(AccessGate::from_config(&config.access)?);
    let listener = TcpListener::bind(config.listen).await?;
    let service = Arc::new(RustupService::new(config)?);
    axum::serve(listener, router_with_rustup(service, access)).await?;
    Ok(())
}
