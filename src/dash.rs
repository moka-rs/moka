//! **Experimental**: Provides a thread-safe, concurrent cache implementation
//! built upon [`dashmap::DashMap`][dashmap].
//!
//! To use this module, enable a crate feature called "dash".
//!
//! [dashmap]: https://docs.rs/dashmap/*/dashmap/struct.DashMap.html

mod base_cache;
mod builder;
mod cache;
mod iter;
mod mapref;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use iter::Iter;
pub use mapref::EntryRef;

/// Provides extra methods that will be useful for testing.
pub trait ConcurrentCacheExt<K, V> {
    /// Performs any pending maintenance operations needed by the cache.
    fn sync(&self);
}
