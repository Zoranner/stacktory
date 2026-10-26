pub mod cache;
pub mod metadata;
pub mod path;
mod service;

pub(crate) use service::RegistryFlavor;
pub use service::{NpmAdapter, NpmError, NpmService};
