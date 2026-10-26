//! 上游请求的并发与超时预算。

use std::{sync::Arc, time::Duration};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_CONCURRENT_ARTIFACTS: usize = 4;

#[derive(Clone, Debug)]
pub struct ArtifactBudget {
    semaphore: Arc<Semaphore>,
}

impl Default for ArtifactBudget {
    fn default() -> Self {
        Self::new(MAX_CONCURRENT_ARTIFACTS)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FetchTimeouts {
    pub metadata: Duration,
    pub artifact: Duration,
}

impl Default for FetchTimeouts {
    fn default() -> Self {
        Self {
            metadata: Duration::from_secs(crate::config::DEFAULT_METADATA_TIMEOUT_SECS),
            artifact: Duration::from_secs(crate::config::DEFAULT_ARTIFACT_TIMEOUT_SECS),
        }
    }
}

impl ArtifactBudget {
    pub fn new(limit: usize) -> Self {
        assert!(limit > 0, "artifact budget requires at least one slot");
        Self {
            semaphore: Arc::new(Semaphore::new(limit)),
        }
    }

    pub(crate) async fn acquire(&self) -> OwnedSemaphorePermit {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("artifact budget must remain open")
    }

    #[cfg(test)]
    pub(crate) fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}
