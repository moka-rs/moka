use crate::{cache::Cache, ConcurrentCache};

use std::{
    hash::{BuildHasher, Hash, Hasher},
    sync::Arc,
};

pub struct SegmentedCache<K, V, S> {
    inner: Arc<Inner<K, V, S>>,
}

unsafe impl<K, V, S> Send for SegmentedCache<K, V, S>
where
    K: Send + Sync,
    V: Send + Sync,
    S: Send,
{
}

unsafe impl<K, V, S> Sync for SegmentedCache<K, V, S>
where
    K: Send + Sync,
    V: Send + Sync,
    S: Sync,
{
}

impl<K, V, S> Clone for SegmentedCache<K, V, S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V> SegmentedCache<K, V, std::collections::hash_map::RandomState>
where
    K: Clone + Eq + Hash,
{
    // TODO: Instead of taking the capacity as an argument, take the followings:
    // - initial_capacity of the cache (hashmap)
    // - max_capacity of the cache (hashmap)
    // - estimated_max_unique_keys (for the frequency sketch)

    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    pub fn new(capacity: usize, num_segments: usize) -> Self {
        let build_hasher = std::collections::hash_map::RandomState::default();
        Self::with_hasher(capacity, num_segments, build_hasher)
    }
}

impl<K, V, S> SegmentedCache<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher + Clone,
{
    // TODO: Instead of taking the capacity as an argument, take the followings:
    // - initial_capacity of the cache (hashmap)
    // - max_capacity of the cache (hashmap)
    // - estimated_max_unique_keys (for the frequency sketch)

    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    pub fn with_hasher(capacity: usize, num_segments: usize, build_hasher: S) -> Self {
        Self {
            inner: Arc::new(Inner::new(capacity, num_segments, build_hasher)),
        }
    }

    /// This is used by unit tests to get consistent result.
    #[cfg(test)]
    pub(crate) fn reconfigure_for_testing(&mut self) {
        // Stop the housekeeping job that may cause sync() method to return earlier.
        // for segment in self.inner.segments.iter_mut() {
        //     segment.reconfigure_for_testing()
        // }
    }
}

impl<K, V, S> ConcurrentCache<K, V> for SegmentedCache<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher + Clone,
{
    fn get(&self, key: &K) -> Option<Arc<V>> {
        self.inner.select(key).get(key)
    }

    fn insert(&self, key: K, value: V) {
        self.inner.select(&key).insert(key, value)
    }

    fn remove(&self, key: &K) -> Option<Arc<V>> {
        self.inner.select(key).remove(key)
    }

    fn sync(&self) {
        for segment in self.inner.segments.iter() {
            segment.sync()
        }
    }
}

struct Inner<K, V, S> {
    segments: Box<[Cache<K, V, S>]>,
    build_hasher: S,
    segment_shift: u32,
}

impl<K, V, S> Inner<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher + Clone,
{
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    fn new(capacity: usize, num_segments: usize, build_hasher: S) -> Self {
        assert!(num_segments > 0);

        let actual_num_segments = num_segments.next_power_of_two();
        let segment_shift = 64 - actual_num_segments.trailing_zeros();
        // TODO: Round up.
        let seg_capacity = capacity / actual_num_segments;
        // NOTE: We cannot initialize the segments as `vec![cache; actual_num_segments]`
        // because Cache::clone() does not clone its inner but shares the same inner.
        let segments = (0..num_segments)
            .map(|_| Cache::with_hasher(seg_capacity, build_hasher.clone()))
            .collect::<Vec<_>>();

        Self {
            segments: segments.into_boxed_slice(),
            build_hasher,
            segment_shift,
        }
    }

    #[inline]
    fn hash(&self, key: &K) -> u64 {
        // TODO: Ensure that build_hasher() is thread safe.
        let mut hasher = self.build_hasher.build_hasher();
        key.hash(&mut hasher);
        hasher.finish()
    }

    #[inline]
    fn select(&self, key: &K) -> &Cache<K, V, S> {
        let hash = self.hash(key);
        let index = self.segment_index_from_hash(hash);
        &self.segments[index]
    }

    #[inline]
    fn segment_index_from_hash(&self, hash: u64) -> usize {
        if self.segment_shift == 64 {
            0
        } else {
            (hash >> self.segment_shift) as usize
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConcurrentCache, SegmentedCache};
    use std::sync::Arc;

    #[test]
    fn basic_single_thread() {
        let mut cache = SegmentedCache::new(3, 1);
        // cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        cache.sync();
        // counts: a -> 1, b -> 1

        cache.insert("c", "cindy");
        assert_eq!(cache.get(&"c"), Some(Arc::new("cindy")));
        // counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        cache.sync();
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        cache.insert("d", "david"); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1

        cache.insert("d", "david");
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher then c's.
        cache.insert("d", "dennis");
        cache.sync();
        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some(Arc::new("dennis")));

        assert_eq!(cache.remove(&"b"), Some(Arc::new("bob")));
    }

    #[test]
    fn basic_multi_threads() {
        let num_threads = 4;

        let mut cache = SegmentedCache::new(100, num_threads);
        // cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        let handles = (0..num_threads)
            .map(|id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    cache.insert(10, format!("{}-100", id));
                    cache.get(&10);
                    cache.sync();
                    cache.insert(20, format!("{}-200", id));
                    cache.remove(&10);
                })
            })
            .collect::<Vec<_>>();

        handles.into_iter().for_each(|h| h.join().expect("Failed"));

        cache.sync();

        assert!(cache.get(&10).is_none());
        assert!(cache.get(&20).is_some());
    }
}
