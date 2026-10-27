use std::{path::PathBuf, sync::Arc};

use crate::{
    PublicUrls, SourcePool,
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
    pub fn new(public_urls: impl Into<PublicUrls>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            service: Arc::new(NpmService::new_with_flavor(
                public_urls,
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

    fn public_urls(&self) -> &PublicUrls {
        self.service.public_urls()
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
