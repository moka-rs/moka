#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

use count_min_sketch::CountMinSketch8;
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
    inner: RwLock<Inner<K, V>>,
}

impl<K, V> PoCLFUCache<K, V>
where
    K: std::fmt::Debug + Eq + std::hash::Hash,
{
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(Inner::new(capacity)),
        }
    }

    fn inner_mut(&self) -> std::sync::RwLockWriteGuard<'_, Inner<K, V>> {
        self.inner
            .write()
            .expect("Cannot get write lock on the map")
    }
}

impl<K, V> ConcurrentCache<K, V> for PoCLFUCache<K, V>
where
    K: std::fmt::Debug + Eq + std::hash::Hash,
{
    fn insert(&self, key: K, value: V) {
        self.inner_mut().insert(key, value);
    }

    fn get(&self, key: &K) -> Option<Arc<V>> {
        self.inner_mut().get(key)
    }

    fn get_or_insert(&self, key: K, default: V) -> Arc<V> {
        self.inner_mut().get_or_insert_with(key, || default)
    }

    fn get_or_insert_with<F>(&self, key: K, default: F) -> Arc<V>
    where
        F: FnOnce() -> V,
    {
        self.inner_mut().get_or_insert_with(key, default)
    }
}

unsafe impl<K, V> Send for PoCLFUCache<K, V> {}
unsafe impl<K, V> Sync for PoCLFUCache<K, V> {}

struct Inner<K, V> {
    cache: HashMap<K, Arc<V>>,
    frequency_sketch: CountMinSketch8<K>,
}

impl<K, V> Inner<K, V>
where
    K: std::fmt::Debug + Eq + std::hash::Hash,
{
    fn new(capacity: usize) -> Self {
        let cache = HashMap::with_capacity(capacity);
        let frequency_sketch = CountMinSketch8::new(capacity, 0.95, 10.0)
            .expect("Failed to create the frequency sketch");

        Self {
            cache,
            frequency_sketch,
        }
    }

    fn insert(&mut self, key: K, value: V) {
        println!(
            "insert() - estimated frequency of {:?}: {}",
            key,
            self.frequency_sketch.estimate(&key)
        );
        self.cache.insert(key, Arc::new(value));
    }

    fn get(&mut self, key: &K) -> Option<Arc<V>> {
        self.frequency_sketch.increment(key);
        println!(
            "get()    - estimated frequency of {:?}: {}",
            key,
            self.frequency_sketch.estimate(&key)
        );
        self.cache.get(key).map(|v| Arc::clone(v))
    }

    fn get_or_insert_with<F>(&mut self, key: K, default: F) -> Arc<V>
    where
        F: FnOnce() -> V,
    {
        self.frequency_sketch.increment(&key);
        let v = self.cache.entry(key).or_insert_with(|| Arc::new(default()));
        Arc::clone(v)
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
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
        assert_eq!(cache.get(&1), Some(Arc::new("a")));
    }
}
