use super::{cache::Cache, ConcurrentCacheExt};

use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash, Hasher},
    sync::Arc,
    time::Duration,
};

/// A thread-safe concurrent in-memory cache, with multiple internal segments.
///
/// `SegmentedCache` has multiple internal [`Cache`][cache-struct] instances for
/// increased concurrent update performance. However, it has little overheads on
/// retrievals and updates for managing these segments.
///
/// For usage examples, see the document of the [`Cache`][cache-struct].
///
/// [cache-struct]: ./struct.Cache.html
///
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
    /// Makes a clone of this shared cache.
    ///
    /// This operation is cheap as it only creates thread-safe reference counted
    /// pointers to the shared internal data structures.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V> SegmentedCache<K, V, RandomState>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + 'static,
{
    /// Constructs a new `SegmentedCache<K, V>` that has multiple internal
    /// segments and will store up to the `max_capacity` entries.
    ///
    /// To adjust various configuration knobs such as `initial_capacity` or
    /// `time_to_live`, use the [`CacheBuilder`][builder-struct].
    ///
    /// [builder-struct]: ./struct.CacheBuilder.html
    ///
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    pub fn new(max_capacity: usize, num_segments: usize) -> Self {
        let build_hasher = RandomState::default();
        Self::with_everything(max_capacity, None, num_segments, build_hasher, None, None)
    }
}

impl<K, V, S> SegmentedCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + 'static,
    S: BuildHasher + Clone + Send + 'static,
{
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    pub(crate) fn with_everything(
        max_capacity: usize,
        initial_capacity: Option<usize>,
        num_segments: usize,
        build_hasher: S,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner::new(
                max_capacity,
                initial_capacity,
                num_segments,
                build_hasher,
                time_to_live,
                time_to_idle,
            )),
        }
    }

    /// Returns a _clone_ of the value corresponding to the key.
    ///
    /// If you want to store values that will be expensive to clone, wrap them by
    /// `std::sync::Arc` before storing in a cache. [`Arc`][rustdoc-std-arc] is a
    /// thread-safe reference-counted pointer and its `clone()` method is cheap.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    ///
    /// [rustdoc-std-arc]: https://doc.rust-lang.org/stable/std/sync/struct.Arc.html
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.inner.hash(key);
        self.inner.select(hash).get_with_hash(key, hash)
    }

    /// Inserts a key-value pair into the cache.
    ///
    /// If the cache has this key present, the value is updated.
    pub fn insert(&self, key: K, value: V) {
        let hash = self.inner.hash(&key);
        self.inner.select(hash).insert_with_hash(key, hash, value);
    }

    /// Discards any cached value for the key.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    pub fn invalidate<Q>(&self, key: &Q)
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.inner.hash(key);
        self.inner.select(hash).invalidate(key);
    }

    pub fn invalidate_all(&self) {
        for segment in self.inner.segments.iter() {
            segment.invalidate_all();
        }
    }

    /// Returns the `max_capacity` of this cache.
    pub fn max_capacity(&self) -> usize {
        self.inner.desired_capacity
    }

    /// Returns the `time_to_live` of this cache.
    pub fn time_to_live(&self) -> Option<Duration> {
        self.inner.segments[0].time_to_live()
    }

    /// Returns the `time_to_idle` of this cache.
    pub fn time_to_idle(&self) -> Option<Duration> {
        self.inner.segments[0].time_to_idle()
    }

    /// Returns the number of internal segments of this cache.
    pub fn num_segments(&self) -> usize {
        self.inner.segments.len()
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

impl<K, V, S> ConcurrentCacheExt<K, V> for SegmentedCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: 'static,
    S: BuildHasher + Clone + Send + 'static,
{
    fn sync(&self) {
        for segment in self.inner.segments.iter() {
            segment.sync();
        }
    }
}

// For unit tests.
#[cfg(test)]
impl<K, V, S> SegmentedCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: 'static,
    S: BuildHasher + Clone + Send + 'static,
{
    fn reconfigure_for_testing(&mut self) {
        let inner = Arc::get_mut(&mut self.inner)
            .expect("There are other strong reference to self.inner Arc");

        for segment in inner.segments.iter_mut() {
            segment.reconfigure_for_testing();
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
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + 'static,
    S: BuildHasher + Clone + Send + 'static,
{
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    fn new(
        max_capacity: usize,
        initial_capacity: Option<usize>,
        num_segments: usize,
        build_hasher: S,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        assert!(num_segments > 0);

        let actual_num_segments = num_segments.next_power_of_two();
        let segment_shift = 64 - actual_num_segments.trailing_zeros();
        // TODO: Round up.
        let seg_capacity = max_capacity / actual_num_segments;
        let seg_init_capacity = initial_capacity.map(|cap| cap / actual_num_segments);
        // NOTE: We cannot initialize the segments as `vec![cache; actual_num_segments]`
        // because Cache::clone() does not clone its inner but shares the same inner.
        let segments = (0..num_segments)
            .map(|_| {
                Cache::with_everything(
                    seg_capacity,
                    seg_init_capacity,
                    build_hasher.clone(),
                    time_to_live,
                    time_to_idle,
                )
            })
            .collect::<Vec<_>>();

        Self {
            desired_capacity: max_capacity,
            segments: segments.into_boxed_slice(),
            build_hasher,
            segment_shift,
        }
    }

    #[inline]
    fn hash<Q>(&self, key: &Q) -> u64
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
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
    use super::{ConcurrentCacheExt, SegmentedCache};

    #[test]
    fn basic_single_thread() {
        let mut cache = SegmentedCache::new(3, 1);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        cache.sync();
        // counts: a -> 1, b -> 1

        cache.insert("c", "cindy");
        assert_eq!(cache.get(&"c"), Some("cindy"));
        // counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
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
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some("dennis"));

        cache.invalidate(&"b");
    }

    #[test]
    fn basic_multi_threads() {
        let num_threads = 4;

        let mut cache = SegmentedCache::new(100, num_threads);
        cache.reconfigure_for_testing();

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
                    cache.invalidate(&10);
                })
            })
            .collect::<Vec<_>>();

        handles.into_iter().for_each(|h| h.join().expect("Failed"));

        cache.sync();

        assert!(cache.get(&10).is_none());
        assert!(cache.get(&20).is_some());
    }

    #[test]
    fn invalidate_all() {
        let mut cache = SegmentedCache::new(100, 4);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        cache.insert("c", "cindy");
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.get(&"c"), Some("cindy"));
        cache.sync();

        cache.invalidate_all();
        cache.sync();

        cache.insert("d", "david");
        cache.sync();

        assert!(cache.get(&"a").is_none());
        assert!(cache.get(&"b").is_none());
        assert!(cache.get(&"c").is_none());
        assert_eq!(cache.get(&"d"), Some("david"));
    }
}
