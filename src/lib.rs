#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

mod lfu;
mod naive_lfu;

pub use lfu::LFUCache;
pub use naive_lfu::NaiveLFUCache;

use std::sync::Arc;

// Interior mutability (no need for `&mut self`)
pub trait ConcurrentCache<K, V> {
    fn get(&self, key: &K) -> Option<Arc<V>>;

    fn get_or_insert(&self, key: K, default: V) -> Arc<V>;

    fn get_or_insert_with<F>(&self, key: K, default: F) -> Arc<V>
    where
        F: FnOnce() -> V;

    fn insert(&self, key: K, value: V);

    fn remove(&self, key: &K) -> Option<Arc<V>>;
}
