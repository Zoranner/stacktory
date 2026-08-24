use std::{env, fs};

use stacktory::{Config, http::router};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path =
        env::var("STACKTORY_CONFIG").unwrap_or_else(|_| "/app/config.toml".to_owned());
    let config = Config::from_toml(&fs::read_to_string(config_path)?)?;
    let listener = TcpListener::bind(config.listen).await?;
    axum::serve(listener, router()).await?;
    Ok(())
}
