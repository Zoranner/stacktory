use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use axum::{http::Method, response::Response};
use thiserror::Error;

use super::SourcePool;

pub type AdapterFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

#[derive(Debug)]
pub struct AdapterRequest {
    pub method: Method,
    pub path: String,
    pub raw_path: String,
    pub body: Vec<u8>,
}

pub trait Adapter: Send + Sync + 'static {
    fn ecosystem(&self) -> &str;

    fn handle(
        &self,
        source_name: String,
        request: AdapterRequest,
        pool: Arc<SourcePool>,
    ) -> AdapterFuture;
}

#[derive(Clone, Default)]
pub struct AdapterRegistry {
    adapters: Arc<BTreeMap<String, RegisteredAdapter>>,
}

pub(crate) struct RegisteredAdapter {
    pub(crate) pool: Arc<SourcePool>,
    pub(crate) adapter: Arc<dyn Adapter>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RegistryError {
    #[error("ecosystem `{0}` is already registered")]
    DuplicateEcosystem(String),
    #[error(
        "ecosystem identity mismatch: registered `{ecosystem}`, pool `{pool}`, adapter `{adapter}`"
    )]
    EcosystemMismatch {
        ecosystem: String,
        pool: String,
        adapter: String,
    },
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_adapter(
        mut self,
        ecosystem: impl Into<String>,
        pool: Arc<SourcePool>,
        adapter: Arc<dyn Adapter>,
    ) -> Result<Self, RegistryError> {
        self.register(ecosystem, pool, adapter)?;
        Ok(self)
    }

    pub fn register(
        &mut self,
        ecosystem: impl Into<String>,
        pool: Arc<SourcePool>,
        adapter: Arc<dyn Adapter>,
    ) -> Result<(), RegistryError> {
        let ecosystem = ecosystem.into();
        if pool.ecosystem() != ecosystem || adapter.ecosystem() != ecosystem {
            return Err(RegistryError::EcosystemMismatch {
                ecosystem,
                pool: pool.ecosystem().to_owned(),
                adapter: adapter.ecosystem().to_owned(),
            });
        }
        if self.adapters.contains_key(&ecosystem) {
            return Err(RegistryError::DuplicateEcosystem(ecosystem));
        }
        let mut adapters = (*self.adapters).clone();
        adapters.insert(ecosystem, RegisteredAdapter { pool, adapter });
        self.adapters = Arc::new(adapters);
        Ok(())
    }

    pub(crate) fn get(&self, ecosystem: &str) -> Option<&RegisteredAdapter> {
        self.adapters.get(ecosystem)
    }
}

impl Clone for RegisteredAdapter {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            adapter: self.adapter.clone(),
        }
    }
}
