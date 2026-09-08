use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use axum::response::Response;

use super::SourcePool;

pub type AdapterFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

pub trait Adapter: Send + Sync + 'static {
    fn handle(&self, source_name: String, path: String, pool: Arc<SourcePool>) -> AdapterFuture;
}

#[derive(Clone, Default)]
pub struct AdapterRegistry {
    adapters: Arc<BTreeMap<String, RegisteredAdapter>>,
}

pub(crate) struct RegisteredAdapter {
    pub(crate) pool: Arc<SourcePool>,
    pub(crate) adapter: Arc<dyn Adapter>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_adapter(
        self,
        ecosystem: impl Into<String>,
        pool: Arc<SourcePool>,
        adapter: Arc<dyn Adapter>,
    ) -> Self {
        let mut adapters = (*self.adapters).clone();
        adapters.insert(ecosystem.into(), RegisteredAdapter { pool, adapter });
        Self {
            adapters: Arc::new(adapters),
        }
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
