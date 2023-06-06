use once_cell::sync::Lazy;
use parking_lot::RwLock;
use scheduled_thread_pool::ScheduledThreadPool;
use std::{collections::HashMap, sync::Arc};

static REGISTRY: Lazy<ThreadPoolRegistry> = Lazy::new(ThreadPoolRegistry::default);

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PoolName {
    Housekeeper,
    Invalidator,
    RemovalNotifier,
}

impl PoolName {
    fn thread_name_template(&self) -> &'static str {
        match self {
            PoolName::Housekeeper => "moka-housekeeper-{}",
            PoolName::Invalidator => "moka-invalidator-{}",
            PoolName::RemovalNotifier => "moka-notifier-{}",
        }
    }
}

pub(crate) struct ThreadPool {
    pub(crate) name: PoolName,
    pub(crate) pool: ScheduledThreadPool,
    // pub(crate) num_threads: usize,
}

impl ThreadPool {
    fn new(name: PoolName, num_threads: usize) -> Self {
        let pool = ScheduledThreadPool::builder()
            .num_threads(num_threads)
            .thread_name_pattern(name.thread_name_template())
            .build();
        Self {
            name,
            pool,
            // num_threads,
        }
    }
}

pub(crate) struct ThreadPoolRegistry {
    pools: RwLock<HashMap<PoolName, Arc<ThreadPool>>>,
}

impl Default for ThreadPoolRegistry {
    fn default() -> Self {
        Self {
            pools: RwLock::new(HashMap::default()),
        }
    }
}

impl ThreadPoolRegistry {
    pub(crate) fn acquire_pool(name: PoolName) -> Arc<ThreadPool> {
        loop {
            {
                // Acquire a read lock and get the pool.
                let pools = REGISTRY.pools.read();
                if let Some(pool) = pools.get(&name) {
                    return Arc::clone(pool);
                }
            }
            {
                // Acquire the write lock, double check the pool still does not exist,
                // and insert a new pool.
                let mut pools = REGISTRY.pools.write();
                pools.entry(name).or_insert_with(|| {
                    let num_threads = crate::common::available_parallelism();
                    let pool = ThreadPool::new(name, num_threads);
                    Arc::new(pool)
                });
            }
        }
    }

    pub(crate) fn release_pool(pool: &Arc<ThreadPool>) {
        if Arc::strong_count(pool) <= 2 {
            // No other client exists; only this Arc and the registry are
            // the owners. Let's remove and drop the one in the registry.
            let name = pool.name;
            let mut pools = REGISTRY.pools.write();
            if let Some(pool) = pools.get(&name) {
                if Arc::strong_count(pool) <= 2 {
                    pools.remove(&name);
                }
            }
        }
    }

    #[cfg(all(test, feature = "sync"))]
    pub(crate) fn enabled_pools() -> Vec<PoolName> {
        let mut names: Vec<_> = REGISTRY.pools.read().keys().cloned().collect();
        names.sort_unstable();
        names
    }
}
