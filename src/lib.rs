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
    K: Clone + std::fmt::Debug + Eq + std::hash::Hash,
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
    K: Clone + std::fmt::Debug + Eq + std::hash::Hash,
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
    capacity: usize,
    cache: HashMap<K, Arc<V>>,
    frequency_sketch: CountMinSketch8<K>,
}

impl<K, V> Inner<K, V>
where
    K: Clone + std::fmt::Debug + Eq + std::hash::Hash,
{
    fn new(capacity: usize) -> Self {
        let cache = HashMap::with_capacity(capacity);
        let skt_capacity = usize::max(capacity, 100);
        let frequency_sketch = CountMinSketch8::new(skt_capacity, 0.95, 10.0)
            .expect("Failed to create the frequency sketch");

        Self {
            capacity,
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
        if self.cache.len() < self.capacity {
            self.cache.insert(key, Arc::new(value));
        } else {
            let victim = self.find_cache_victim();
            if self.admit(&key, &victim) {
                self.cache.remove(&victim);
                self.cache.insert(key, Arc::new(value));
            }
        }
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

    // TODO: Run this in a background thread and push the victim to a queue.
    fn find_cache_victim(&self) -> K {
        let mut victim = None;
        for key in self.cache.keys() {
            let freq0 = self.frequency_sketch.estimate(key);
            match victim {
                None => victim = Some((freq0, key)),
                Some((freq1, _)) if freq0 < freq1 => victim = Some((freq0, key)),
                Some(_) => (),
            }
        }
        // TODO: Remove clone().
        // Maybe the cache map should have <Arc<K>, Arc<V>> instead of <K, Arc<V>>?
        victim.expect("No victim found").1.clone()
    }

    fn admit(&self, candidate: &K, victim: &K) -> bool {
        let skt = &self.frequency_sketch;
        skt.estimate(candidate) > skt.estimate(victim)
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::{ConcurrentCache, PoCLFUCache};
    use std::sync::Arc;

    #[test]
    fn simple() {
        let cache = PoCLFUCache::new(3);
        cache.insert("a", "alice");
        cache.insert("b", "bob");

        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        // counts: a -> 2, b -> 2

        cache.insert("c", "cindy");

        assert_eq!(cache.get(&"c"), Some(Arc::new("cindy")));
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        cache.insert("d", "david"); //        count: d -> 0
        assert_eq!(cache.get(&"d"), None); //        d -> 1

        cache.insert("d", "david");
        assert_eq!(cache.get(&"d"), None); //        d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher then c's.
        cache.insert("d", "dennis");
        assert_eq!(cache.get(&"d"), Some(Arc::new("dennis")));
        assert_eq!(cache.get(&"c"), None);
    }
}
