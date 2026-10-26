use std::{path::PathBuf, sync::Arc};

use url::Url;

use crate::{
    SourcePool,
    ecosystems::npm::{NpmError, NpmService, RegistryFlavor},
    upstream::registry::{Adapter, AdapterFuture, AdapterRequest},
};

#[derive(Debug)]
pub struct UpmAdapter {
    service: Arc<NpmService>,
}

pub type UpmError = NpmError;
pub type UpmService = NpmService;

impl UpmAdapter {
    pub fn new(root_url: Url, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(NpmService::new_with_flavor(
                root_url,
                data_dir,
                RegistryFlavor::upm(),
            )),
        }
    }

    pub fn service(&self) -> &UpmService {
        &self.service
    }
}

impl Adapter for UpmAdapter {
    fn ecosystem(&self) -> &str {
        "upm"
    }

    fn handle(
        &self,
        source_name: String,
        request: AdapterRequest,
        pool: Arc<SourcePool>,
    ) -> AdapterFuture {
        let service = self.service.clone();
        Box::pin(async move { service.handle_request(&source_name, request, &pool).await })
    }
}
