use async_lock::RwLock;
use std::{
    error::Error,
    future::Future,
    hash::{BuildHasher, Hash},
    sync::Arc,
};

type Waiter<V> = Arc<RwLock<Option<Result<V, Arc<Box<dyn Error + Send + Sync + 'static>>>>>>;

#[allow(clippy::redundant_allocation)]
// https://rust-lang.github.io/rust-clippy/master/index.html#redundant_allocation
pub(crate) enum InitResult<V> {
    Initialized(V),
    ReadExisting(V),
    // This `Arc<Box<dyn ..>>` creates an extra heap allocation. This will be
    // addressed by Moka v0.6.0.
    InitErr(Arc<Box<dyn Error + Send + Sync + 'static>>),
}

pub(crate) struct ValueInitializer<K, V, S> {
    waiters: moka_cht::SegmentedHashMap<Arc<K>, Waiter<V>, S>,
}

impl<K, V, S> ValueInitializer<K, V, S>
where
    Arc<K>: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    pub(crate) fn with_hasher(hasher: S) -> Self {
        Self {
            waiters: moka_cht::SegmentedHashMap::with_num_segments_and_hasher(16, hasher),
        }
    }

    pub(crate) async fn init_or_read<F>(&self, key: Arc<K>, init: F) -> InitResult<V>
    where
        F: Future<Output = V>,
    {
        use InitResult::*;

        let waiter = Arc::new(RwLock::new(None));
        let mut lock = waiter.write().await;

        match self.try_insert_waiter(&key, &waiter) {
            None => {
                // Inserted. Resolve the init future.
                let value = init.await;
                *lock = Some(Ok(value.clone()));
                Initialized(value)
            }
            Some(res) => {
                // Value already exists. Drop our write lock and wait for a read lock
                // to become available.
                std::mem::drop(lock);
                match &*res.read().await {
                    Some(Ok(value)) => ReadExisting(value.clone()),
                    Some(Err(_)) | None => unreachable!(),
                }
            }
        }
    }

    pub(crate) async fn try_init_or_read<F>(&self, key: Arc<K>, init: F) -> InitResult<V>
    where
        F: Future<Output = Result<V, Box<dyn Error + Send + Sync + 'static>>>,
    {
        use InitResult::*;

        let waiter = Arc::new(RwLock::new(None));
        let mut lock = waiter.write().await;

        match self.try_insert_waiter(&key, &waiter) {
            None => {
                // Inserted. Resolve the init future.
                match init.await {
                    Ok(value) => {
                        *lock = Some(Ok(value.clone()));
                        Initialized(value)
                    }
                    Err(e) => {
                        let err = Arc::new(e);
                        *lock = Some(Err(Arc::clone(&err)));
                        self.remove_waiter(&key);
                        InitErr(err)
                    }
                }
            }
            Some(res) => {
                // Value already exists. Drop our write lock and wait for a read lock
                // to become available.
                std::mem::drop(lock);
                match &*res.read().await {
                    Some(Ok(value)) => ReadExisting(value.clone()),
                    Some(Err(e)) => InitErr(Arc::clone(e)),
                    None => unreachable!(),
                }
            }
        }
    }

    #[inline]
    pub(crate) fn remove_waiter(&self, key: &Arc<K>) {
        self.waiters.remove(key);
    }

    fn try_insert_waiter(&self, key: &Arc<K>, waiter: &Waiter<V>) -> Option<Waiter<V>> {
        let key = Arc::clone(key);
        let waiter = Arc::clone(waiter);

        self.waiters
            .insert_with_or_modify(key, || waiter, |_, w| Arc::clone(w))
    }
}
