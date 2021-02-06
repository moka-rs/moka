//! Provides thread-safe, synchronous (blocking) cache implementations.

mod builder;
pub(crate) mod cache;
mod segment;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use segment::SegmentedCache;

/// Provides extra methods that will be useful for testing.
pub trait ConcurrentCacheExt<K, V> {
    /// Performs any pending maintenance operations needed by the cache.
    fn sync(&self);
}
