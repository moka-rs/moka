#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

// Interior mutability (no need for `&mut self`)
trait ConcurrentCache<K, V> {
    fn insert(&self, key: K, value: V);

    fn get(&self, key: &K) -> Option<Arc<V>>;

    fn get_or_insert(&self, key: K, default: V) -> Arc<V>;

    fn get_or_insert_with<F>(&self, key: K, default: F) -> Arc<V>
    where
        F: FnOnce() -> V;
}

pub struct PoCLFUCache<K, V> {
    main_storage: RwLock<HashMap<K, Arc<V>>>,
}

impl<K, V> PoCLFUCache<K, V>
where
    K: Eq + std::hash::Hash,
{
    pub fn new(capacity: usize) -> Self {
        Self {
            main_storage: RwLock::new(HashMap::with_capacity(capacity)),
        }
    }
}

impl<K, V> ConcurrentCache<K, V> for PoCLFUCache<K, V>
where
    K: Eq + std::hash::Hash,
{
    fn insert(&self, key: K, value: V) {
        let mut m = self
            .main_storage
            .write()
            .expect("Cannot get write lock on the map");
        m.insert(key, Arc::new(value));
    }

    fn get(&self, key: &K) -> Option<Arc<V>> {
        let m = self
            .main_storage
            .read()
            .expect("Cannot get read lock on the map");
        m.get(key).map(|v| Arc::clone(v))
    }

    fn get_or_insert(&self, key: K, default: V) -> Arc<V> {
        let mut m = self
            .main_storage
            .write()
            .expect("Cannot get write lock on the map");
        let v = m.entry(key).or_insert_with(|| Arc::new(default));
        Arc::clone(v)
    }

    fn get_or_insert_with<F>(&self, key: K, default: F) -> Arc<V>
    where
        F: FnOnce() -> V,
    {
        let mut m = self
            .main_storage
            .write()
            .expect("Cannot get write lock on the map");
        let v = m.entry(key).or_insert_with(|| Arc::new(default()));
        Arc::clone(v)
    }
}

unsafe impl<K, V> Send for PoCLFUCache<K, V> {}
unsafe impl<K, V> Sync for PoCLFUCache<K, V> {}

#[cfg(test)]
mod tests {
    use super::{ConcurrentCache, PoCLFUCache};
    use std::sync::Arc;

    #[test]
    fn simple() {
        let cache = PoCLFUCache::new(10);
        cache.insert(1, "a");
        cache.insert(2, "b");

        assert_eq!(cache.get(&1), Some(Arc::new("a")));
        assert_eq!(cache.get(&2), Some(Arc::new("b")));
    }
}
