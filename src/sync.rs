//! Provides thread-safe, concurrent cache implementations.

mod builder;
mod cache;
mod entry_selector;
mod segment;
mod value_initializer;

pub use crate::sync_base::{iter::Iter, PredicateId};
pub use {
    builder::CacheBuilder,
    cache::Cache,
    entry_selector::{OwnedKeyEntrySelector, RefKeyEntrySelector},
    segment::SegmentedCache,
};

/// Provides extra methods that will be useful for testing.
pub trait ConcurrentCacheExt<K, V> {
    /// Performs any pending maintenance operations needed by the cache.
    fn sync(&self);
}

// Empty internal struct to be used in optionally_get_with to represent the None
// results.
pub(crate) struct OptionallyNone;
