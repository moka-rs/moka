use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
/// The policy of a cache.
pub struct Policy {
    max_capacity: Option<u64>,
    num_segments: usize,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
}

impl Policy {
    pub(crate) fn new(
        max_capacity: Option<u64>,
        num_segments: usize,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
    ) -> Self {
        Self {
            max_capacity,
            num_segments,
            time_to_live,
            time_to_idle,
        }
    }

    /// Returns the `max_capacity` of the cache.
    pub fn max_capacity(&self) -> Option<u64> {
        self.max_capacity
    }

    #[cfg(feature = "sync")]
    pub(crate) fn set_max_capacity(&mut self, capacity: Option<u64>) {
        self.max_capacity = capacity;
    }

    /// Returns the number of internal segments of the cache.
    pub fn num_segments(&self) -> usize {
        self.num_segments
    }

    #[cfg(feature = "sync")]
    pub(crate) fn set_num_segments(&mut self, num: usize) {
        self.num_segments = num;
    }

    /// Returns the `time_to_live` of the cache.
    pub fn time_to_live(&self) -> Option<Duration> {
        self.time_to_live
    }

    /// Returns the `time_to_idle` of the cache.
    pub fn time_to_idle(&self) -> Option<Duration> {
        self.time_to_idle
    }
}

/// Calculates when cache entries expire. A single expiration time is retained on
/// each entry so that the lifetime of an entry may be extended or reduced by
/// subsequent evaluations.
///
/// `Expiry` trait provides three methods. They specify the expiration time of an
/// entry by returning a `Some(duration)` until the entry expires:
///
/// - [`expire_after_create`](#method.expire_after_create) &mdash; Returns the
///   duration (or none) after the entry's creation.
/// - [`expire_after_read`](#method.expire_after_read) &mdash; Returns the duration
///   (or none)  after its last read.
/// - [`expire_after_update`](#method.expire_after_update) &mdash; Returns the
///   duration (or none)  after its last update.
///
/// The default implementations are provided that return `None` (no expiration) or
/// `current_duration: Option<Instant>` (not modify the current expiration time).
/// Override some of them as you need.
///
pub trait Expiry<K, V> {
    /// Specifies that the entry should be automatically removed from the cache once
    /// the duration has elapsed after the entry's creation. This method is called
    /// for cache write methods such as `insert` and `get_with` but only when the key
    /// was not present in the cache.
    ///
    /// # Parameters
    ///
    /// - `key` &mdash; A reference to the key of the entry.
    /// - `value` &mdash; A reference to the value of the entry.
    /// - `created_at` &mdash; The time when this entry was inserted.
    ///
    /// # Return value
    ///
    /// The returned `Option<Duration>` is used to set the expiration time of the
    /// entry.
    ///
    /// - Returning `Some(duration)` &mdash; The expiration time is set to
    ///   `created_at + duration`.
    /// - Returning `None` &mdash; The expiration time is cleared (no expiration).
    ///   - This is the value that the default implementation returns.
    ///
    /// # Notes on `time_to_live` and `time_to_idle` policies
    ///
    /// When the cache is configured with `time_to_live` and/or `time_to_idle`
    /// policies, the entry will be evicted after the earliest of the expiration time
    /// returned by this expiry, the `time_to_live` and `time_to_idle` policies.
    #[allow(unused_variables)]
    fn expire_after_create(&self, key: &K, value: &V, created_at: Instant) -> Option<Duration> {
        None
    }

    /// Specifies that the entry should be automatically removed from the cache once
    /// the duration has elapsed after its last read. This method is called for cache
    /// read methods such as `get` and `get_with` but only when the key is present in
    /// the cache.
    ///
    /// # Parameters
    ///
    /// - `key` &mdash; A reference to the key of the entry.
    /// - `value` &mdash; A reference to the value of the entry.
    /// - `read_at` &mdash; The time when this entry was read.
    /// - `duration_until_expiry` &mdash; The remaining duration until the entry
    ///   expires. (Calculated by `expiration_time - read_at`)
    /// - `last_modified_at` &mdash; The time when this entry was created or updated.
    ///
    /// # Return value
    ///
    /// The returned `Option<Duration>` is used to set the expiration time of the
    /// entry.
    ///
    /// - Returning `Some(duration)` &mdash; The expiration time is set to
    ///   `read_at + duration`.
    /// - Returning `None` &mdash; The expiration time is cleared (no expiration).
    /// - Returning `duration_until_expiry` will not modify the expiration time.
    ///   - This is the value that the default implementation returns.
    ///
    /// # Notes on `time_to_live` and `time_to_idle` policies
    ///
    /// When the cache is configured with `time_to_live` and/or `time_to_idle`
    /// policies, then:
    ///
    /// - The entry will be evicted after the earliest of the expiration time
    ///   returned by this expiry, the `time_to_live` and `time_to_idle` policies.
    /// - The `duration_until_expiry` takes in account the `time_to_live` and
    ///   `time_to_idle` policies.
    #[allow(unused_variables)]
    fn expire_after_read(
        &self,
        key: &K,
        value: &V,
        read_at: Instant,
        duration_until_expiry: Option<Duration>,
        last_modified_at: Instant,
    ) -> Option<Duration> {
        duration_until_expiry
    }

    /// Specifies that the entry should be automatically removed from the cache once
    /// the duration has elapsed after the replacement of its value. This method is
    /// called for cache write methods such as `insert` but only when the key is
    /// already present in the cache.
    ///
    /// # Parameters
    ///
    /// - `key` &mdash; A reference to the key of the entry.
    /// - `value` &mdash; A reference to the value of the entry.
    /// - `updated_at` &mdash; The time when this entry was updated.
    /// - `duration_until_expiry` &mdash; The remaining duration until the entry
    ///   expires. (Calculated by `expiration_time - updated_at`)
    ///
    /// # Return value
    ///
    /// The returned `Option<Duration>` is used to set the expiration time of the
    /// entry.
    ///
    /// - Returning `Some(duration)` &mdash; The expiration time is set to
    ///   `updated_at + duration`.
    /// - Returning `None` &mdash; The expiration time is cleared (no expiration).
    /// - Returning `duration_until_expiry` will not modify the expiration time.
    ///   - This is the value that the default implementation returns.
    ///
    /// # Notes on `time_to_live` and `time_to_idle` policies
    ///
    /// When the cache is configured with `time_to_live` and/or `time_to_idle`
    /// policies, then:
    ///
    /// - The entry will be evicted after the earliest of the expiration time
    ///   returned by this expiry, the `time_to_live` and `time_to_idle` policies.
    /// - The `duration_until_expiry` takes in account the `time_to_live` and
    ///   `time_to_idle` policies.
    #[allow(unused_variables)]
    fn expire_after_update(
        &self,
        key: &K,
        value: &V,
        updated_at: Instant,
        duration_until_expiry: Option<Duration>,
    ) -> Option<Duration> {
        duration_until_expiry
    }
}

pub(crate) struct ExpirationPolicy<K, V> {
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
    expiry: Option<Arc<dyn Expiry<K, V> + Send + Sync + 'static>>,
}

impl<K, V> Default for ExpirationPolicy<K, V> {
    fn default() -> Self {
        Self {
            time_to_live: None,
            time_to_idle: None,
            expiry: None,
        }
    }
}

impl<K, V> Clone for ExpirationPolicy<K, V> {
    fn clone(&self) -> Self {
        Self {
            time_to_live: self.time_to_live,
            time_to_idle: self.time_to_idle,
            expiry: self.expiry.as_ref().map(Arc::clone),
        }
    }
}

impl<K, V> ExpirationPolicy<K, V> {
    #[cfg(test)]
    pub(crate) fn new(
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
        expiry: Option<Arc<dyn Expiry<K, V> + Send + Sync + 'static>>,
    ) -> Self {
        Self {
            time_to_live,
            time_to_idle,
            expiry,
        }
    }

    /// Returns the `time_to_live` of the cache.
    pub(crate) fn time_to_live(&self) -> Option<Duration> {
        self.time_to_live
    }

    pub(crate) fn set_time_to_live(&mut self, duration: Duration) {
        self.time_to_live = Some(duration);
    }

    /// Returns the `time_to_idle` of the cache.
    pub(crate) fn time_to_idle(&self) -> Option<Duration> {
        self.time_to_idle
    }

    pub(crate) fn set_time_to_idle(&mut self, duration: Duration) {
        self.time_to_idle = Some(duration);
    }

    pub(crate) fn expiry(&self) -> Option<Arc<dyn Expiry<K, V> + Send + Sync + 'static>> {
        self.expiry.as_ref().map(Arc::clone)
    }

    pub(crate) fn set_expiry(&mut self, expiry: Arc<dyn Expiry<K, V> + Send + Sync + 'static>) {
        self.expiry = Some(expiry);
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    use std::sync::atomic::{AtomicU8, Ordering};

    #[derive(Default)]
    pub(crate) struct ExpiryCallCounters {
        expected_creations: AtomicU8,
        expected_reads: AtomicU8,
        expected_updates: AtomicU8,
        actual_creations: AtomicU8,
        actual_reads: AtomicU8,
        actual_updates: AtomicU8,
    }

    impl ExpiryCallCounters {
        pub(crate) fn incl_expected_creations(&self) {
            self.expected_creations.fetch_add(1, Ordering::Relaxed);
        }

        pub(crate) fn incl_expected_reads(&self) {
            self.expected_reads.fetch_add(1, Ordering::Relaxed);
        }

        pub(crate) fn incl_expected_updates(&self) {
            self.expected_updates.fetch_add(1, Ordering::Relaxed);
        }

        pub(crate) fn incl_actual_creations(&self) {
            self.actual_creations.fetch_add(1, Ordering::Relaxed);
        }

        pub(crate) fn incl_actual_reads(&self) {
            self.actual_reads.fetch_add(1, Ordering::Relaxed);
        }

        pub(crate) fn incl_actual_updates(&self) {
            self.actual_updates.fetch_add(1, Ordering::Relaxed);
        }

        pub(crate) fn verify(&self) {
            assert_eq!(
                self.expected_creations.load(Ordering::Relaxed),
                self.actual_creations.load(Ordering::Relaxed),
                "expected_creations != actual_creations"
            );
            assert_eq!(
                self.expected_reads.load(Ordering::Relaxed),
                self.actual_reads.load(Ordering::Relaxed),
                "expected_reads != actual_reads"
            );
            assert_eq!(
                self.expected_updates.load(Ordering::Relaxed),
                self.actual_updates.load(Ordering::Relaxed),
                "expected_updates != actual_updates"
            );
        }
    }
}
