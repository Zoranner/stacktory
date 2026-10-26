//! 进程装配：读取配置、组装适配器注册表并启动监听。

mod registry;
mod serve;

pub use serve::serve;

use crate::Config;

/// 从当前工作目录读取 `config.toml`。
pub(crate) fn read_config() -> Result<Config, Box<dyn std::error::Error>> {
    let source = std::fs::read_to_string("config.toml").map_err(|error| {
        format!(
            "cannot read config.toml from the current directory: {error}. \
             Copy deploy/app/config.example.toml to config.toml and adjust it."
        )
    })?;
    Ok(Config::from_toml(&source)?)
}
