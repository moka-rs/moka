use crate::{cache::Cache, ConcurrentCache, ConcurrentCacheExt};

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    sync::Arc,
    time::Duration,
};

pub struct SegmentedCache<K, V, S = RandomState> {
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

impl<K, V> SegmentedCache<K, V, RandomState>
where
    K: Eq + Hash,
{
    // TODO: Instead of taking the capacity as an argument, take the followings:
    // - initial_capacity of the cache (hashmap)
    // - max_capacity of the cache (hashmap)
    // - estimated_max_unique_keys (for the frequency sketch)

    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    pub fn new(capacity: usize, num_segments: usize) -> Self {
        let build_hasher = RandomState::default();
        Self::with_everything(capacity, num_segments, build_hasher, None, None)
    }
}

impl<K, V, S> SegmentedCache<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    pub fn with_hasher(capacity: usize, num_segments: usize, build_hasher: S) -> Self {
        Self::with_everything(capacity, num_segments, build_hasher, None, None)
    }

    // TODO: Instead of taking the capacity as an argument, take the followings:
    // - initial_capacity of the cache (hashmap)
    // - max_capacity of the cache (hashmap)
    // - estimated_max_unique_keys (for the frequency sketch)

    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    pub(crate) fn with_everything(
        capacity: usize,
        num_segments: usize,
        build_hasher: S,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner::new(
                capacity,
                num_segments,
                build_hasher,
                time_to_live,
                time_to_idle,
            )),
        }
    }

    // /// This is used by unit tests to get consistent result.
    // #[cfg(test)]
    // pub(crate) fn reconfigure_for_testing(&mut self) {
    //     // Stop the housekeeping job that may cause sync() method to return earlier.
    //     for segment in self.inner.segments.iter_mut() {
    //         segment.reconfigure_for_testing()
    //     }
    // }
}

impl<K, V, S> ConcurrentCache<K, V> for SegmentedCache<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    fn get(&self, key: &K) -> Option<Arc<V>> {
        let hash = self.inner.hash(key);
        self.inner.select(hash).get_with_hash(key, hash)
    }

    fn insert(&self, key: K, value: V) -> Arc<V> {
        let hash = self.inner.hash(&key);
        self.inner.select(hash).insert_with_hash(key, hash, value)
    }

    fn remove(&self, key: &K) -> Option<Arc<V>> {
        let hash = self.inner.hash(key);
        self.inner.select(hash).remove(key)
    }

    fn capacity(&self) -> usize {
        self.inner.desired_capacity
    }

    fn time_to_live(&self) -> Option<Duration> {
        self.inner.segments[0].time_to_live()
    }

    fn time_to_idle(&self) -> Option<Duration> {
        self.inner.segments[0].time_to_idle()
    }

    fn num_segments(&self) -> usize {
        self.inner.segments.len()
    }
}

impl<K, V, S> ConcurrentCacheExt<K, V> for SegmentedCache<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    fn sync(&self) {
        for segment in self.inner.segments.iter() {
            segment.sync();
        }
    }
}

struct Inner<K, V, S> {
    desired_capacity: usize,
    segments: Box<[Cache<K, V, S>]>,
    build_hasher: S,
    segment_shift: u32,
}

impl<K, V, S> Inner<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    fn new(
        capacity: usize,
        num_segments: usize,
        build_hasher: S,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        assert!(num_segments > 0);

        let actual_num_segments = num_segments.next_power_of_two();
        let segment_shift = 64 - actual_num_segments.trailing_zeros();
        // TODO: Round up.
        let seg_capacity = capacity / actual_num_segments;
        // NOTE: We cannot initialize the segments as `vec![cache; actual_num_segments]`
        // because Cache::clone() does not clone its inner but shares the same inner.
        let segments = (0..num_segments)
            .map(|_| {
                Cache::with_everything(
                    seg_capacity,
                    build_hasher.clone(),
                    time_to_live,
                    time_to_idle,
                )
            })
            .collect::<Vec<_>>();

        Self {
            desired_capacity: capacity,
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
    fn select(&self, hash: u64) -> &Cache<K, V, S> {
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
    use super::{ConcurrentCache, ConcurrentCacheExt, SegmentedCache};
    use std::sync::Arc;

    #[test]
    fn basic_single_thread() {
        let cache = SegmentedCache::new(3, 1);
        // cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        assert_eq!(cache.insert("a", "alice"), Arc::new("alice"));
        assert_eq!(cache.insert("b", "bob"), Arc::new("bob"));
        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        cache.sync();
        // counts: a -> 1, b -> 1

        assert_eq!(cache.insert("c", "cindy"), Arc::new("cindy"));
        assert_eq!(cache.get(&"c"), Some(Arc::new("cindy")));
        // counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        cache.sync();
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        assert_eq!(cache.insert("d", "david"), Arc::new("david")); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1

        assert_eq!(cache.insert("d", "david"), Arc::new("david"));
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher then c's.
        assert_eq!(cache.insert("d", "dennis"), Arc::new("dennis"));
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

        let cache = SegmentedCache::new(100, num_threads);
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
