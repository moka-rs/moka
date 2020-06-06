use lazy_static::lazy_static;
use parking_lot::RwLock;
use scheduled_thread_pool::ScheduledThreadPool;
use std::{collections::HashMap, sync::Arc};

// TODO: Use enum. e.g. Pool::{Default, Name(String)}.
const DEFAULT_POOL_NAME: &str = "$$default$$";

lazy_static! {
    static ref REGISTRY: ThreadPoolRegistry = ThreadPoolRegistry::default();
}

pub(crate) struct ThreadPool {
    pub(crate) name: String,
    pub(crate) pool: ScheduledThreadPool,
    // pub(crate) num_threads: usize,
}

pub(crate) struct ThreadPoolRegistry {
    pools: RwLock<HashMap<String, Arc<ThreadPool>>>,
}

impl Default for ThreadPoolRegistry {
    fn default() -> Self {
        Self {
            pools: RwLock::new(HashMap::default()),
        }
    }
}

impl ThreadPoolRegistry {
    pub(crate) fn acquire_default_pool() -> Arc<ThreadPool> {
        // Maybe it is enough to use Mutex and the entry API `get_or_else()`.
        // Or just use cht crate.
        loop {
            {
                let pools = REGISTRY.pools.read();
                if let Some(pool) = pools.get(DEFAULT_POOL_NAME) {
                    return Arc::clone(pool);
                }
            }
            {
                let mut pools = REGISTRY.pools.write();
                if !pools.contains_key(DEFAULT_POOL_NAME) {
                    let num_threads = num_cpus::get();
                    let pool = ScheduledThreadPool::with_name("moka-default-{}", num_threads);
                    let t_pool = ThreadPool {
                        name: DEFAULT_POOL_NAME.to_string(),
                        pool,
                        // num_threads,
                    };
                    pools.insert(DEFAULT_POOL_NAME.to_string(), Arc::new(t_pool));
                }
            }
        }
    }

    pub(crate) fn release_pool(pool: &Arc<ThreadPool>) {
        if Arc::strong_count(&pool) <= 2 {
            // No other client exists; only this Arc and the registry are
            // the owners. Let's remove and drop the one in the registry.
            let name = pool.name.clone();
            let mut pools = REGISTRY.pools.write();
            if let Some(pool) = pools.get(&name) {
                if Arc::strong_count(pool) <= 2 {
                    pools.remove(&name);
                }
            }
        }
    }
}
