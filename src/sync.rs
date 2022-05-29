//! Provides thread-safe, concurrent cache implementations.

mod builder;
mod cache;
mod segment;
mod value_initializer;

pub use crate::sync_base::{iter::Iter, PredicateId};
pub use {builder::CacheBuilder, cache::Cache, segment::SegmentedCache};

/// Provides extra methods that will be useful for testing.
pub trait ConcurrentCacheExt<K, V> {
    /// Performs any pending maintenance operations needed by the cache.
    fn sync(&self);
}
