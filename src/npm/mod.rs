pub mod metadata;
pub mod path;
mod service;
pub mod storage;

pub(crate) use service::RegistryFlavor;
pub use service::{NpmAdapter, NpmError, NpmService};
