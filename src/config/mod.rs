//! 配置契约、TOML 解析与校验。

mod catalog;
mod model;
mod parse;
mod validate;

pub(crate) use catalog::{SourceCatalog, SourceEntry};
pub use model::*;
