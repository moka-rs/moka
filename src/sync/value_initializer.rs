use parking_lot::RwLock;
use std::{
    error::Error,
    hash::{BuildHasher, Hash},
    sync::Arc,
};

type Waiter<V> = Arc<RwLock<Option<Result<V, Arc<Box<dyn Error>>>>>>;

pub(crate) enum InitResult<V> {
    Initialized(V),
    ReadExisting(V),
    InitErr(Arc<Box<dyn Error>>),
}

pub(crate) struct ValueInitializer<K, V, S> {
    waiters: cht::HashMap<Arc<K>, Waiter<V>, S>,
}

impl<K, V, S> ValueInitializer<K, V, S>
where
    Arc<K>: Eq + Hash,
    V: Clone,
    S: BuildHasher,
{
    pub(crate) fn with_hasher(hasher: S) -> Self {
        Self {
            waiters: cht::HashMap::with_hasher(hasher),
        }
    }

    pub(crate) fn insert_with(&self, key: Arc<K>, mut init: impl FnMut() -> V) -> InitResult<V> {
        use InitResult::*;

        let waiter = Arc::new(RwLock::new(None));
        let mut lock = waiter.write();

        match self.insert_or_modify(&key, &waiter) {
            None => {
                // Inserted. Resolve the init future.
                let value = init();
                *lock = Some(Ok(value.clone()));
                self.waiters.remove(&key);
                Initialized(value)
            }
            Some(res) => {
                // Value already exists. Drop our write lock and wait for a read lock
                // to become available.
                std::mem::drop(lock);
                match &*res.read() {
                    Some(Ok(value)) => ReadExisting(value.clone()),
                    Some(Err(_)) | None => unreachable!(),
                }
            }
        }
    }

    pub(crate) fn try_insert_with<F>(&self, key: Arc<K>, mut init: F) -> InitResult<V>
    where
        F: FnMut() -> Result<V, Box<dyn Error>>,
    {
        use InitResult::*;

        let waiter = Arc::new(RwLock::new(None));
        let mut lock = waiter.write();

        match self.insert_or_modify(&key, &waiter) {
            None => {
                // Inserted. Resolve the init future.
                match init() {
                    Ok(value) => {
                        *lock = Some(Ok(value.clone()));
                        self.waiters.remove(&key);
                        Initialized(value)
                    }
                    Err(e) => {
                        let err = Arc::new(e);
                        *lock = Some(Err(Arc::clone(&err)));
                        self.waiters.remove(&key);
                        InitErr(err)
                    }
                }
            }
            Some(res) => {
                // Value already exists. Drop our write lock and wait for a read lock
                // to become available.
                std::mem::drop(lock);
                match &*res.read() {
                    Some(Ok(value)) => ReadExisting(value.clone()),
                    Some(Err(e)) => InitErr(Arc::clone(e)),
                    None => unreachable!(),
                }
            }
        }
    }

    fn insert_or_modify(&self, key: &Arc<K>, waiter: &Waiter<V>) -> Option<Waiter<V>> {
        let key = Arc::clone(key);
        let waiter = Arc::clone(waiter);

        self.waiters
            .insert_with_or_modify(key, || waiter, |_, w| Arc::clone(w))
    }
}
