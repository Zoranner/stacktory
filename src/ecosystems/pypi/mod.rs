//! PyPI Simple API 只读适配器。

mod accept;
mod cache;
mod index;
mod path;
mod service;

pub use service::PyPiAdapter;
