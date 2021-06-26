//! Provides a thread-safe, asynchronous (futures aware) cache implementation.
//!
//! To use this module, enable a crate feature called "future".

mod builder;
mod cache;
mod value_initializer;

pub use builder::CacheBuilder;
pub use cache::Cache;

/// Provides extra methods that will be useful for testing.
pub trait ConcurrentCacheExt<K, V> {
    /// Performs any pending maintenance operations needed by the cache.
    fn sync(&self);
}
