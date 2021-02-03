mod builder;
pub(crate) mod cache;
mod segment;

pub use builder::Builder;
pub use cache::Cache;
pub use segment::SegmentedCache;

pub trait ConcurrentCacheExt<K, V> {
    fn sync(&self);
}
