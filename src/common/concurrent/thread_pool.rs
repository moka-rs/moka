use once_cell::sync::Lazy;
use parking_lot::RwLock;
use scheduled_thread_pool::ScheduledThreadPool;
use std::{collections::HashMap, sync::Arc};

static REGISTRY: Lazy<ThreadPoolRegistry> = Lazy::new(ThreadPoolRegistry::default);

#[derive(Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(any(feature = "sync", feature = "future"), derive(Debug))]
pub(crate) enum PoolName {
    Housekeeper,
    #[cfg(any(feature = "sync", feature = "future"))]
    Invalidator,
    #[cfg(any(feature = "sync", feature = "future"))]
    RemovalNotifier,
}

impl PoolName {
    fn thread_name_template(&self) -> &'static str {
        match self {
            PoolName::Housekeeper => "moka-housekeeper-{}",
            #[cfg(any(feature = "sync", feature = "future"))]
            PoolName::Invalidator => "moka-invalidator-{}",
            #[cfg(any(feature = "sync", feature = "future"))]
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
        let pool = ScheduledThreadPool::with_name(name.thread_name_template(), num_threads);
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
                    // TODO: When we upgrade the MSRV to 1.59 (2022-02-24) or newer,
                    // replace num_cpus crate with `thread::available_parallelism` in
                    // std.
                    //
                    // NOTE: On some platforms, `num_cpus::get` may return 0. In that
                    // case, use 1.
                    // https://github.com/moka-rs/moka/pull/39#issuecomment-916888859
                    // https://github.com/seanmonstar/num_cpus/issues/69
                    let num_threads = num_cpus::get().max(1);
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
