use super::{
    cache::Cache,
    debug_fmt::{CacheRef, DebugFmt},
    iter::{Iter, ScanningGet},
    CacheBuilder, ConcurrentCacheExt, Weigher,
};
use crate::{Policy, PredicateError};

use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    fmt,
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

// TODO: https://github.com/moka-rs/moka/issues/54
#[allow(clippy::non_send_fields_in_send_ty)]
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

impl<K, V, S> fmt::Debug for SegmentedCache<K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.debug_fmt().default_fmt().fmt(f)
    }
}

impl<K, V> SegmentedCache<K, V, RandomState>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Constructs a new `SegmentedCache<K, V>` that has multiple internal
    /// segments and will store up to the `max_capacity`.
    ///
    /// To adjust various configuration knobs such as `initial_capacity` or
    /// `time_to_live`, use the [`CacheBuilder`][builder-struct].
    ///
    /// [builder-struct]: ./struct.CacheBuilder.html
    ///
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    pub fn new(max_capacity: u64, num_segments: usize) -> Self {
        let build_hasher = RandomState::default();
        Self::with_everything(
            Some(max_capacity),
            None,
            num_segments,
            build_hasher,
            None,
            None,
            None,
            false,
        )
    }

    /// Returns a [`CacheBuilder`][builder-struct], which can builds a
    /// `SegmentedCache` with various configuration knobs.
    ///
    /// [builder-struct]: ./struct.CacheBuilder.html
    pub fn builder(num_segments: usize) -> CacheBuilder<K, V, SegmentedCache<K, V, RandomState>> {
        CacheBuilder::default().segments(num_segments)
    }
}

impl<K, V, S> SegmentedCache<K, V, S> {
    /// Returns a read-only cache policy of this cache.
    ///
    /// At this time, cache policy cannot be modified after cache creation.
    /// A future version may support to modify it.
    pub fn policy(&self) -> Policy {
        let mut policy = self.inner.segments[0].policy();
        policy.set_max_capacity(self.inner.desired_capacity);
        policy.set_num_segments(self.inner.segments.len());
        policy
    }

    pub fn debug_fmt(&self) -> DebugFmt<'_, K, V, S> {
        DebugFmt::new(CacheRef::SyncSeg(self))
    }

    pub fn entry_count(&self) -> u64 {
        self.inner
            .segments
            .iter()
            .map(|seg| seg.entry_count())
            .sum()
    }

    pub fn weighted_size(&self) -> u64 {
        self.inner
            .segments
            .iter()
            .map(|seg| seg.weighted_size())
            .sum()
    }
}

impl<K, V, S> SegmentedCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_everything(
        max_capacity: Option<u64>,
        initial_capacity: Option<usize>,
        num_segments: usize,
        build_hasher: S,
        weigher: Option<Weigher<K, V>>,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
        invalidator_enabled: bool,
    ) -> Self {
        Self {
            inner: Arc::new(Inner::new(
                max_capacity,
                initial_capacity,
                num_segments,
                build_hasher,
                weigher,
                time_to_live,
                time_to_idle,
                invalidator_enabled,
            )),
        }
    }

    /// Returns `true` if the cache contains a value for the key.
    ///
    /// Unlike the `get` method, this method is not considered a cache read operation,
    /// so it does not update the historic popularity estimator or reset the idle
    /// timer for the key.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        Arc<K>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.inner.hash(key);
        self.inner.select(hash).contains_key_with_hash(key, hash)
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

    /// Deprecated, replaced with [`get_with`](#method.get_with)
    #[deprecated(since = "0.8.0", note = "Replaced with `get_with`")]
    pub fn get_or_insert_with(&self, key: K, init: impl FnOnce() -> V) -> V {
        self.get_with(key, init)
    }

    /// Deprecated, replaced with [`try_get_with`](#method.try_get_with)
    #[deprecated(since = "0.8.0", note = "Replaced with `try_get_with`")]
    pub fn get_or_try_insert_with<F, E>(&self, key: K, init: F) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
    {
        self.try_get_with(key, init)
    }

    /// Ensures the value of the key exists by inserting the output of the `init`
    /// closure if not exist, and returns a _clone_ of the value.
    ///
    /// This method prevents to evaluate the `init` closure multiple times on the
    /// same key even if the method is concurrently called by many threads; only one
    /// of the calls evaluates its closure, and other calls wait for that closure to
    /// complete.
    pub fn get_with(&self, key: K, init: impl FnOnce() -> V) -> V {
        let hash = self.inner.hash(&key);
        let key = Arc::new(key);
        let replace_if = None as Option<fn(&V) -> bool>;
        self.inner
            .select(hash)
            .get_or_insert_with_hash_and_fun(key, hash, init, replace_if)
    }

    /// Works like [`get_with`](#method.get_with), but takes an additional
    /// `replace_if` closure.
    ///
    /// This method will evaluate the `init` closure and insert the output to the
    /// cache when:
    ///
    /// - The key does not exist.
    /// - Or, `replace_if` closure returns `true`.
    pub fn get_with_if(
        &self,
        key: K,
        init: impl FnOnce() -> V,
        replace_if: impl FnMut(&V) -> bool,
    ) -> V {
        let hash = self.inner.hash(&key);
        let key = Arc::new(key);
        self.inner
            .select(hash)
            .get_or_insert_with_hash_and_fun(key, hash, init, Some(replace_if))
    }

    /// Try to ensure the value of the key exists by inserting an `Ok` result of the
    /// init closure if not exist, and returns a _clone_ of the value or the `Err`
    /// returned by the closure.
    ///
    /// This method prevents to evaluate the init closure multiple times on the same
    /// key even if the method is concurrently called by many threads; only one of
    /// the calls evaluates its closure (as long as these closures return the same
    /// error type), and other calls wait for that closure to complete.
    pub fn try_get_with<F, E>(&self, key: K, init: F) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
    {
        let hash = self.inner.hash(&key);
        let key = Arc::new(key);
        self.inner
            .select(hash)
            .get_or_try_insert_with_hash_and_fun(key, hash, init)
    }

    /// Inserts a key-value pair into the cache.
    ///
    /// If the cache has this key present, the value is updated.
    pub fn insert(&self, key: K, value: V) {
        let hash = self.inner.hash(&key);
        let key = Arc::new(key);
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
        self.inner.select(hash).invalidate_with_hash(key, hash);
    }

    /// Discards all cached values.
    ///
    /// This method returns immediately and a background thread will evict all the
    /// cached values inserted before the time when this method was called. It is
    /// guaranteed that the `get` method must not return these invalidated values
    /// even if they have not been evicted.
    ///
    /// Like the `invalidate` method, this method does not clear the historic
    /// popularity estimator of keys so that it retains the client activities of
    /// trying to retrieve an item.
    pub fn invalidate_all(&self) {
        for segment in self.inner.segments.iter() {
            segment.invalidate_all();
        }
    }

    /// Discards cached values that satisfy a predicate.
    ///
    /// `invalidate_entries_if` takes a closure that returns `true` or `false`. This
    /// method returns immediately and a background thread will apply the closure to
    /// each cached value inserted before the time when `invalidate_entries_if` was
    /// called. If the closure returns `true` on a value, that value will be evicted
    /// from the cache.
    ///
    /// Also the `get` method will apply the closure to a value to determine if it
    /// should have been invalidated. Therefore, it is guaranteed that the `get`
    /// method must not return invalidated values.
    ///
    /// Note that you must call
    /// [`CacheBuilder::support_invalidation_closures`][support-invalidation-closures]
    /// at the cache creation time as the cache needs to maintain additional internal
    /// data structures to support this method. Otherwise, calling this method will
    /// fail with a
    /// [`PredicateError::InvalidationClosuresDisabled`][invalidation-disabled-error].
    ///
    /// Like the `invalidate` method, this method does not clear the historic
    /// popularity estimator of keys so that it retains the client activities of
    /// trying to retrieve an item.
    ///
    /// [support-invalidation-closures]: ./struct.CacheBuilder.html#method.support_invalidation_closures
    /// [invalidation-disabled-error]: ../enum.PredicateError.html#variant.InvalidationClosuresDisabled
    pub fn invalidate_entries_if<F>(&self, predicate: F) -> Result<(), PredicateError>
    where
        F: Fn(&K, &V) -> bool + Send + Sync + 'static,
    {
        let pred = Arc::new(predicate);
        for segment in self.inner.segments.iter() {
            segment.invalidate_entries_with_arc_fun(Arc::clone(&pred))?;
        }
        Ok(())
    }

    /// Creates an iterator visiting all key-value pairs in arbitrary order. The
    /// iterator element type is `(Arc<K>, V)`, where `V` is a clone of a stored
    /// value.
    ///
    /// Iterators do not block concurrent reads and writes on the cache. An entry can
    /// be inserted to, invalidated or evicted from a cache while iterators are alive
    /// on the same cache.
    ///
    /// Unlike the `get` method, visiting entries via an iterator do not update the
    /// historic popularity estimator or reset idle timers for keys.
    ///
    /// # Guarantees
    ///
    /// In order to allow concurrent access to the cache, iterator's `next` method
    /// does _not_ guarantee the following:
    ///
    /// - It does not guarantee to return a key-value pair (an entry) if its key has
    ///   been inserted to the cache _after_ the iterator was created.
    ///   - Such an entry may or may not be returned depending on key's hash and
    ///     timing.
    ///
    /// and the `next` method guarantees the followings:
    ///
    /// - It guarantees not to return the same entry more than once.
    /// - It guarantees not to return an entry if it has been removed from the cache
    ///   after the iterator was created.
    ///     - Note: An entry can be removed by following reasons:
    ///         - Manually invalidated.
    ///         - Expired (e.g. time-to-live).
    ///         - Evicted as the cache capacity exceeded.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use moka::sync::SegmentedCache;
    ///
    /// let cache = SegmentedCache::new(100, 4);
    /// cache.insert("Julia", 14);
    ///
    /// let mut iter = cache.iter();
    /// let (k, v) = iter.next().unwrap(); // (Arc<K>, V)
    /// assert_eq!(*k, "Julia");
    /// assert_eq!(v, 14);
    ///
    /// assert!(iter.next().is_none());
    /// ```
    ///
    pub fn iter(&self) -> Iter<'_, K, V> {
        let num_cht_segments = self.inner.segments[0].num_cht_segments();
        let segments = self
            .inner
            .segments
            .iter()
            .map(|c| c as &dyn ScanningGet<_, _>)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Iter::with_multiple_cache_segments(segments, num_cht_segments)
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

impl<'a, K, V, S> IntoIterator for &'a SegmentedCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    type Item = (Arc<K>, V);

    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<K, V, S> ConcurrentCacheExt<K, V> for SegmentedCache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
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
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn invalidation_predicate_count(&self) -> usize {
        self.inner
            .segments
            .iter()
            .map(|seg| seg.invalidation_predicate_count())
            .sum()
    }

    fn reconfigure_for_testing(&mut self) {
        let inner = Arc::get_mut(&mut self.inner)
            .expect("There are other strong reference to self.inner Arc");

        for segment in inner.segments.iter_mut() {
            segment.reconfigure_for_testing();
        }
    }

    fn create_mock_expiration_clock(&self) -> MockExpirationClock {
        let mut exp_clock = MockExpirationClock::default();

        for segment in self.inner.segments.iter() {
            let (clock, mock) = crate::common::time::Clock::mock();
            segment.set_expiration_clock(Some(clock));
            exp_clock.mocks.push(mock);
        }

        exp_clock
    }
}

// For unit tests.
#[cfg(test)]
#[derive(Default)]
struct MockExpirationClock {
    mocks: Vec<Arc<crate::common::time::Mock>>,
}

#[cfg(test)]
impl MockExpirationClock {
    fn increment(&mut self, duration: Duration) {
        for mock in &mut self.mocks {
            mock.increment(duration);
        }
    }
}

struct Inner<K, V, S> {
    desired_capacity: Option<u64>,
    segments: Box<[Cache<K, V, S>]>,
    build_hasher: S,
    segment_shift: u32,
}

impl<K, V, S> Inner<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    #[allow(clippy::too_many_arguments)]
    fn new(
        max_capacity: Option<u64>,
        initial_capacity: Option<usize>,
        num_segments: usize,
        build_hasher: S,
        weigher: Option<Weigher<K, V>>,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
        invalidator_enabled: bool,
    ) -> Self {
        assert!(num_segments > 0);

        let actual_num_segments = num_segments.next_power_of_two();
        let segment_shift = 64 - actual_num_segments.trailing_zeros();
        // TODO: Round up.
        let seg_max_capacity = max_capacity.map(|n| n / actual_num_segments as u64);
        let seg_init_capacity = initial_capacity.map(|cap| cap / actual_num_segments);
        // NOTE: We cannot initialize the segments as `vec![cache; actual_num_segments]`
        // because Cache::clone() does not clone its inner but shares the same inner.
        let segments = (0..actual_num_segments)
            .map(|_| {
                Cache::with_everything(
                    seg_max_capacity,
                    seg_init_capacity,
                    build_hasher.clone(),
                    weigher.as_ref().map(Arc::clone),
                    time_to_live,
                    time_to_idle,
                    invalidator_enabled,
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
    use std::{sync::Arc, time::Duration};

    #[test]
    fn basic_single_thread() {
        let mut cache = SegmentedCache::new(3, 1);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        cache.sync();
        // counts: a -> 1, b -> 1

        cache.insert("c", "cindy");
        assert_eq!(cache.get(&"c"), Some("cindy"));
        assert!(cache.contains_key(&"c"));
        // counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert!(cache.contains_key(&"a"));
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert!(cache.contains_key(&"b"));
        cache.sync();
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        cache.insert("d", "david"); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1
        assert!(!cache.contains_key(&"d"));

        cache.insert("d", "david");
        cache.sync();
        assert!(!cache.contains_key(&"d"));
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher than c's.
        cache.insert("d", "dennis");
        cache.sync();
        assert_eq!(cache.get(&"a"), Some("alice"));
        assert_eq!(cache.get(&"b"), Some("bob"));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some("dennis"));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"c"));
        assert!(cache.contains_key(&"d"));

        cache.invalidate(&"b");
        assert_eq!(cache.get(&"b"), None);
        assert!(!cache.contains_key(&"b"));
    }

    #[test]
    fn non_power_of_two_segments() {
        let mut cache = SegmentedCache::new(100, 5);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        assert_eq!(cache.iter().count(), 0);

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        cache.insert("c", "cindy");

        assert_eq!(cache.iter().count(), 3);
        cache.sync();
        assert_eq!(cache.iter().count(), 3);
    }

    #[test]
    fn size_aware_eviction() {
        let weigher = |_k: &&str, v: &(&str, u32)| v.1;

        let alice = ("alice", 10);
        let bob = ("bob", 15);
        let bill = ("bill", 20);
        let cindy = ("cindy", 5);
        let david = ("david", 15);
        let dennis = ("dennis", 15);

        let mut cache = SegmentedCache::builder(1)
            .max_capacity(31)
            .weigher(weigher)
            .build();
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", alice);
        cache.insert("b", bob);
        assert_eq!(cache.get(&"a"), Some(alice));
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert_eq!(cache.get(&"b"), Some(bob));
        cache.sync();
        // order (LRU -> MRU) and counts: a -> 1, b -> 1

        cache.insert("c", cindy);
        assert_eq!(cache.get(&"c"), Some(cindy));
        assert!(cache.contains_key(&"c"));
        // order and counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert!(cache.contains_key(&"a"));
        assert_eq!(cache.get(&"a"), Some(alice));
        assert_eq!(cache.get(&"b"), Some(bob));
        assert!(cache.contains_key(&"b"));
        cache.sync();
        // order and counts: c -> 1, a -> 2, b -> 2

        // To enter "d" (weight: 15), it needs to evict "c" (w: 5) and "a" (w: 10).
        // "d" must have higher count than 3, which is the aggregated count
        // of "a" and "c".
        cache.insert("d", david); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 1
        assert!(!cache.contains_key(&"d"));

        cache.insert("d", david);
        cache.sync();
        assert!(!cache.contains_key(&"d"));
        assert_eq!(cache.get(&"d"), None); //   d -> 2

        cache.insert("d", david);
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //   d -> 3
        assert!(!cache.contains_key(&"d"));

        cache.insert("d", david);
        cache.sync();
        assert!(!cache.contains_key(&"d"));
        assert_eq!(cache.get(&"d"), None); //   d -> 4

        // Finally "d" should be admitted by evicting "c" and "a".
        cache.insert("d", dennis);
        cache.sync();
        assert_eq!(cache.get(&"a"), None);
        assert_eq!(cache.get(&"b"), Some(bob));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some(dennis));
        assert!(!cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"c"));
        assert!(cache.contains_key(&"d"));

        // Update "b" with "bill" (w: 15 -> 20). This should evict "d" (w: 15).
        cache.insert("b", bill);
        cache.sync();
        assert_eq!(cache.get(&"b"), Some(bill));
        assert_eq!(cache.get(&"d"), None);
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"d"));

        // Re-add "a" (w: 10) and update "b" with "bob" (w: 20 -> 15).
        cache.insert("a", alice);
        cache.insert("b", bob);
        cache.sync();
        assert_eq!(cache.get(&"a"), Some(alice));
        assert_eq!(cache.get(&"b"), Some(bob));
        assert_eq!(cache.get(&"d"), None);
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"d"));

        // Verify the sizes.
        assert_eq!(cache.entry_count(), 2);
        assert_eq!(cache.weighted_size(), 25);
    }

    #[test]
    fn basic_multi_threads() {
        let num_threads = 4;

        let mut cache = SegmentedCache::new(100, num_threads);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        // https://rust-lang.github.io/rust-clippy/master/index.html#needless_collect
        #[allow(clippy::needless_collect)]
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
        assert!(!cache.contains_key(&10));
        assert!(cache.contains_key(&20));
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
        assert!(cache.contains_key(&"a"));
        assert!(cache.contains_key(&"b"));
        assert!(cache.contains_key(&"c"));
        cache.sync();

        cache.invalidate_all();
        cache.sync();

        cache.insert("d", "david");
        cache.sync();

        assert!(cache.get(&"a").is_none());
        assert!(cache.get(&"b").is_none());
        assert!(cache.get(&"c").is_none());
        assert_eq!(cache.get(&"d"), Some("david"));
        assert!(!cache.contains_key(&"a"));
        assert!(!cache.contains_key(&"b"));
        assert!(!cache.contains_key(&"c"));
        assert!(cache.contains_key(&"d"));
    }

    #[test]
    fn invalidate_entries_if() -> Result<(), Box<dyn std::error::Error>> {
        use std::collections::HashSet;

        const SEGMENTS: usize = 4;

        let mut cache = SegmentedCache::builder(SEGMENTS)
            .max_capacity(100)
            .support_invalidation_closures()
            .build();
        cache.reconfigure_for_testing();

        let mut mock = cache.create_mock_expiration_clock();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert(0, "alice");
        cache.insert(1, "bob");
        cache.insert(2, "alex");
        cache.sync();
        mock.increment(Duration::from_secs(5)); // 5 secs from the start.
        cache.sync();

        assert_eq!(cache.get(&0), Some("alice"));
        assert_eq!(cache.get(&1), Some("bob"));
        assert_eq!(cache.get(&2), Some("alex"));
        assert!(cache.contains_key(&0));
        assert!(cache.contains_key(&1));
        assert!(cache.contains_key(&2));

        let names = ["alice", "alex"].iter().cloned().collect::<HashSet<_>>();
        cache.invalidate_entries_if(move |_k, &v| names.contains(v))?;
        assert_eq!(cache.invalidation_predicate_count(), SEGMENTS);

        mock.increment(Duration::from_secs(5)); // 10 secs from the start.

        cache.insert(3, "alice");

        // Run the invalidation task and wait for it to finish. (TODO: Need a better way than sleeping)
        cache.sync(); // To submit the invalidation task.
        std::thread::sleep(Duration::from_millis(200));
        cache.sync(); // To process the task result.
        std::thread::sleep(Duration::from_millis(200));

        assert!(cache.get(&0).is_none());
        assert!(cache.get(&2).is_none());
        assert_eq!(cache.get(&1), Some("bob"));
        // This should survive as it was inserted after calling invalidate_entries_if.
        assert_eq!(cache.get(&3), Some("alice"));

        assert!(!cache.contains_key(&0));
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));

        assert_eq!(cache.entry_count(), 2);
        assert_eq!(cache.invalidation_predicate_count(), 0);

        mock.increment(Duration::from_secs(5)); // 15 secs from the start.

        cache.invalidate_entries_if(|_k, &v| v == "alice")?;
        cache.invalidate_entries_if(|_k, &v| v == "bob")?;
        assert_eq!(cache.invalidation_predicate_count(), SEGMENTS * 2);

        // Run the invalidation task and wait for it to finish. (TODO: Need a better way than sleeping)
        cache.sync(); // To submit the invalidation task.
        std::thread::sleep(Duration::from_millis(200));
        cache.sync(); // To process the task result.
        std::thread::sleep(Duration::from_millis(200));

        assert!(cache.get(&1).is_none());
        assert!(cache.get(&3).is_none());

        assert!(!cache.contains_key(&1));
        assert!(!cache.contains_key(&3));

        assert_eq!(cache.entry_count(), 0);
        assert_eq!(cache.invalidation_predicate_count(), 0);

        Ok(())
    }

    #[test]
    fn test_iter() {
        const NUM_KEYS: usize = 50;

        fn make_value(key: usize) -> String {
            format!("val: {}", key)
        }

        // let cache = SegmentedCache::builder(5)
        let cache = SegmentedCache::builder(4)
            .max_capacity(100)
            .time_to_idle(Duration::from_secs(10))
            .build();

        for key in 0..NUM_KEYS {
            cache.insert(key, make_value(key));
        }

        let mut key_set = std::collections::HashSet::new();

        for (key, value) in &cache {
            assert_eq!(value, make_value(*key));

            key_set.insert(*key);
        }

        // Ensure there are no missing or duplicate keys in the iteration.
        assert_eq!(key_set.len(), NUM_KEYS);
    }

    /// Runs 16 threads at the same time and ensures no deadlock occurs.
    ///
    /// - Eight of the threads will update key-values in the cache.
    /// - Eight others will iterate the cache.
    ///
    #[test]
    fn test_iter_multi_threads() {
        use std::collections::HashSet;

        const NUM_KEYS: usize = 1024;
        const NUM_THREADS: usize = 16;

        fn make_value(key: usize) -> String {
            format!("val: {}", key)
        }

        let cache = SegmentedCache::builder(4)
            .max_capacity(2048)
            .time_to_idle(Duration::from_secs(10))
            .build();

        // Initialize the cache.
        for key in 0..NUM_KEYS {
            cache.insert(key, make_value(key));
        }

        let rw_lock = Arc::new(std::sync::RwLock::<()>::default());
        let write_lock = rw_lock.write().unwrap();

        // https://rust-lang.github.io/rust-clippy/master/index.html#needless_collect
        #[allow(clippy::needless_collect)]
        let handles = (0..NUM_THREADS)
            .map(|n| {
                let cache = cache.clone();
                let rw_lock = Arc::clone(&rw_lock);

                if n % 2 == 0 {
                    // This thread will update the cache.
                    std::thread::spawn(move || {
                        let read_lock = rw_lock.read().unwrap();
                        for key in 0..NUM_KEYS {
                            // TODO: Update keys in a random order?
                            cache.insert(key, make_value(key));
                        }
                        std::mem::drop(read_lock);
                    })
                } else {
                    // This thread will iterate the cache.
                    std::thread::spawn(move || {
                        let read_lock = rw_lock.read().unwrap();
                        let mut key_set = HashSet::new();
                        for (key, value) in &cache {
                            assert_eq!(value, make_value(*key));
                            key_set.insert(*key);
                        }
                        // Ensure there are no missing or duplicate keys in the iteration.
                        assert_eq!(key_set.len(), NUM_KEYS);
                        std::mem::drop(read_lock);
                    })
                }
            })
            .collect::<Vec<_>>();

        // Let these threads to run by releasing the write lock.
        std::mem::drop(write_lock);

        handles.into_iter().for_each(|h| h.join().expect("Failed"));

        // Ensure there are no missing or duplicate keys in the iteration.
        let key_set = cache.iter().map(|(k, _v)| *k).collect::<HashSet<_>>();
        assert_eq!(key_set.len(), NUM_KEYS);
    }

    #[test]
    fn get_with() {
        use std::thread::{sleep, spawn};

        let cache = SegmentedCache::new(100, 4);
        const KEY: u32 = 0;

        // This test will run five threads:
        //
        // Thread1 will be the first thread to call `get_with` for a key, so its init
        // closure will be evaluated and then a &str value "thread1" will be inserted
        // to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `get_with` immediately.
                let v = cache1.get_with(KEY, || {
                    // Wait for 300 ms and return a &str value.
                    sleep(Duration::from_millis(300));
                    "thread1"
                });
                assert_eq!(v, "thread1");
            })
        };

        // Thread2 will be the second thread to call `get_with` for the same key, so
        // its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the value inserted by thread1's init closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `get_with`.
                sleep(Duration::from_millis(100));
                let v = cache2.get_with(KEY, || unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread3 will be the third thread to call `get_with` for the same key. By
        // the time it calls, thread1's init closure should have finished already and
        // the value should be already inserted to the cache. So its init closure
        // will not be evaluated and will get the value insert by thread1's init
        // closure immediately.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get_with`.
                sleep(Duration::from_millis(400));
                let v = cache3.get_with(KEY, || unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread4 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache4.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread5 will call `get` for the same key. It will call after thread1's init
        // closure finished, so it will get the value insert by thread1's init closure.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache5.get(&KEY);
                assert_eq!(maybe_v, Some("thread1"));
            })
        };

        for t in vec![thread1, thread2, thread3, thread4, thread5] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn get_with_if() {
        use std::thread::{sleep, spawn};

        let cache = SegmentedCache::new(100, 4);
        const KEY: u32 = 0;

        // This test will run seven threads:
        //
        // Thread1 will be the first thread to call `get_with_if` for a key, so its
        // init closure will be evaluated and then a &str value "thread1" will be
        // inserted to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `get_with` immediately.
                let v = cache1.get_with_if(
                    KEY,
                    || {
                        // Wait for 300 ms and return a &str value.
                        sleep(Duration::from_millis(300));
                        "thread1"
                    },
                    |_v| unreachable!(),
                );
                assert_eq!(v, "thread1");
            })
        };

        // Thread2 will be the second thread to call `get_with_if` for the same key,
        // so its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the value inserted by thread1's init closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `get_with`.
                sleep(Duration::from_millis(100));
                let v = cache2.get_with_if(KEY, || unreachable!(), |_v| unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread3 will be the third thread to call `get_with_if` for the same
        // key. By the time it calls, thread1's init closure should have finished
        // already and the value should be already inserted to the cache. Also
        // thread3's `replace_if` closure returns `false`. So its init closure will
        // not be evaluated and will get the value inserted by thread1's init closure
        // immediately.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 350 ms before calling `get_with_if`.
                sleep(Duration::from_millis(350));
                let v = cache3.get_with_if(
                    KEY,
                    || unreachable!(),
                    |v| {
                        assert_eq!(v, &"thread1");
                        false
                    },
                );
                assert_eq!(v, "thread1");
            })
        };

        // Thread4 will be the fourth thread to call `get_with_if` for the same
        // key. The value should have been already inserted to the cache by
        // thread1. However thread4's `replace_if` closure returns `true`. So its
        // init closure will be evaluated to replace the current value.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get_with_if`.
                sleep(Duration::from_millis(400));
                let v = cache4.get_with_if(
                    KEY,
                    || "thread4",
                    |v| {
                        assert_eq!(v, &"thread1");
                        true
                    },
                );
                assert_eq!(v, "thread4");
            })
        };

        // Thread5 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache5.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread6 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread6 = {
            let cache6 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(350));
                let maybe_v = cache6.get(&KEY);
                assert_eq!(maybe_v, Some("thread1"));
            })
        };

        // Thread7 will call `get` for the same key. It will call after thread1's init
        // closure finished, so it will get the value insert by thread1's init closure.
        let thread7 = {
            let cache7 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(450));
                let maybe_v = cache7.get(&KEY);
                assert_eq!(maybe_v, Some("thread4"));
            })
        };

        for t in vec![
            thread1, thread2, thread3, thread4, thread5, thread6, thread7,
        ] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn try_get_with() {
        use std::{
            sync::Arc,
            thread::{sleep, spawn},
        };

        #[derive(thiserror::Error, Debug)]
        #[error("{}", _0)]
        pub struct MyError(String);

        type MyResult<T> = Result<T, Arc<MyError>>;

        let cache = SegmentedCache::new(100, 4);
        const KEY: u32 = 0;

        // This test will run eight threads:
        //
        // Thread1 will be the first thread to call `try_get_with` for a key, so its
        // init closure will be evaluated and then an error will be returned. Nothing
        // will be inserted to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `try_get_with` immediately.
                let v = cache1.try_get_with(KEY, || {
                    // Wait for 300 ms and return an error.
                    sleep(Duration::from_millis(300));
                    Err(MyError("thread1 error".into()))
                });
                assert!(v.is_err());
            })
        };

        // Thread2 will be the second thread to call `try_get_with` for the same key,
        // so its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the same error value returned by thread1's init
        // closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `try_get_with`.
                sleep(Duration::from_millis(100));
                let v: MyResult<_> = cache2.try_get_with(KEY, || unreachable!());
                assert!(v.is_err());
            })
        };

        // Thread3 will be the third thread to call `get_with` for the same key. By
        // the time it calls, thread1's init closure should have finished already,
        // but the key still does not exist in the cache. So its init closure will be
        // evaluated and then an okay &str value will be returned. That value will be
        // inserted to the cache.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `try_get_with`.
                sleep(Duration::from_millis(400));
                let v: MyResult<_> = cache3.try_get_with(KEY, || {
                    // Wait for 300 ms and return an Ok(&str) value.
                    sleep(Duration::from_millis(300));
                    Ok("thread3")
                });
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // thread4 will be the fourth thread to call `try_get_with` for the same
        // key. So its init closure will not be evaluated. Once thread3's init
        // closure finishes, it will get the same okay &str value.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 500 ms before calling `try_get_with`.
                sleep(Duration::from_millis(500));
                let v: MyResult<_> = cache4.try_get_with(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread5 will be the fifth thread to call `try_get_with` for the same
        // key. So its init closure will not be evaluated. By the time it calls,
        // thread3's init closure should have finished already, so its init closure
        // will not be evaluated and will get the value insert by thread3's init
        // closure immediately.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `try_get_with`.
                sleep(Duration::from_millis(800));
                let v: MyResult<_> = cache5.try_get_with(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread6 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread6 = {
            let cache6 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache6.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread7 will call `get` for the same key. It will call after thread1's init
        // closure finished with an error. So it will get none for the key.
        let thread7 = {
            let cache7 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache7.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread8 will call `get` for the same key. It will call after thread3's init
        // closure finished, so it will get the value insert by thread3's init closure.
        let thread8 = {
            let cache8 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `get`.
                sleep(Duration::from_millis(800));
                let maybe_v = cache8.get(&KEY);
                assert_eq!(maybe_v, Some("thread3"));
            })
        };

        for t in vec![
            thread1, thread2, thread3, thread4, thread5, thread6, thread7, thread8,
        ] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn debug_formats() {
        // TODO: Change the num_segments to 4 after fixing the admission/eviction
        // policies to manage capacity using the sum of the capacities of every segment.
        let mut cache = SegmentedCache::builder(1).max_capacity(10).build();
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert('a', "alice");
        cache.insert('b', "bob");
        cache.insert('c', "cindy");
        cache.sync();

        assert_eq!(
            format!("{:?}", cache),
            "SegmentedCache { max_capacity: Some(10), entry_count: 3, weighted_size: 3 }"
        );

        let debug_str = format!("{:?}", cache.debug_fmt().entries());
        assert!(debug_str.starts_with('{'));
        assert!(debug_str.contains(r#"'a': "alice""#));
        assert!(debug_str.contains(r#"'b': "bob""#));
        assert!(debug_str.contains(r#"'c': "cindy""#));
        assert!(debug_str.ends_with('}'));

        let weigher = |_k: &char, v: &(&str, u32)| v.1;

        // TODO: Change the num_segments to 4 after fixing the admission/eviction
        // policies to manage capacity using the sum of the capacities of every segment.
        let mut cache = SegmentedCache::builder(1)
            .max_capacity(50)
            .weigher(weigher)
            .build();
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert('a', ("alice", 10));
        cache.insert('b', ("bob", 15));
        cache.insert('c', ("cindy", 5));
        cache.sync();

        assert_eq!(
            format!("{:?}", cache),
            "SegmentedCache { max_capacity: Some(50), entry_count: 3, weighted_size: 30 }"
        );

        let debug_str = format!("{:?}", cache.debug_fmt().entries());
        assert!(debug_str.starts_with('{'));
        assert!(debug_str.contains(r#"'a': ("alice", 10)"#));
        assert!(debug_str.contains(r#"'b': ("bob", 15)"#));
        assert!(debug_str.contains(r#"'c': ("cindy", 5)"#));
        assert!(debug_str.ends_with('}'));
    }
}
