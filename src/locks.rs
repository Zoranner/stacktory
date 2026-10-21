use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use tokio::sync::{Mutex, MutexGuard};

const DEFAULT_SHARDS: usize = 256;

#[derive(Debug)]
pub(crate) struct ShardedLocks {
    shards: Vec<Mutex<()>>,
}

impl Default for ShardedLocks {
    fn default() -> Self {
        Self::new(DEFAULT_SHARDS)
    }
}

impl ShardedLocks {
    fn new(shard_count: usize) -> Self {
        assert!(shard_count > 0, "sharded locks require at least one shard");
        Self {
            shards: (0..shard_count).map(|_| Mutex::new(())).collect(),
        }
    }

    pub(crate) async fn lock(&self, key: &str) -> MutexGuard<'_, ()> {
        self.shards[self.shard_index(key)].lock().await
    }

    fn shard_index(&self, key: &str) -> usize {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        (hasher.finish() as usize) % self.shards.len()
    }
}

#[cfg(test)]
mod tests {
    use super::ShardedLocks;

    #[tokio::test]
    async fn many_keys_reuse_the_fixed_shard_set() {
        let locks = ShardedLocks::new(4);
        for index in 0..100 {
            let _guard = locks.lock(&format!("object-{index}")).await;
        }
        assert_eq!(locks.shards.len(), 4);
    }
}
