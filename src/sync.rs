use std::time::Duration;

mod builder;
pub(crate) mod cache;
mod segment;

pub use builder::Builder;
pub use cache::Cache;
pub use segment::SegmentedCache;

// Interior mutability (no need for `&mut self`)
pub trait ConcurrentCache<K, V> {
    fn get(&self, key: &K) -> Option<V>;

    fn insert(&self, key: K, value: V);

    fn remove(&self, key: &K) -> Option<V>;

    fn capacity(&self) -> usize;

    fn time_to_live(&self) -> Option<Duration>;

    fn time_to_idle(&self) -> Option<Duration>;

    fn num_segments(&self) -> usize;
}

pub trait ConcurrentCacheExt<K, V> {
    fn sync(&self);
}
