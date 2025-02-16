use std::{
    fmt::Debug,
    sync::Arc,
    time::{Duration, Instant},
};

use super::concurrent::KeyHashDate;

/// A snapshot of a single entry in the cache.
///
/// `Entry` is constructed from the methods like `or_insert` on the struct returned
/// by cache's `entry` or `entry_by_ref` methods. `Entry` holds the cached key and
/// value at the time it was constructed. It also carries extra information about the
/// entry; [`is_fresh`](#method.is_fresh) method returns `true` if the value was not
/// cached and was freshly computed.
///
/// See the followings for more information about `entry` and `entry_by_ref` methods:
///
/// - `sync::Cache`:
///     - [`entry`](./sync/struct.Cache.html#method.entry)
///     - [`entry_by_ref`](./sync/struct.Cache.html#method.entry_by_ref)
/// - `future::Cache`:
///     - [`entry`](./future/struct.Cache.html#method.entry)
///     - [`entry_by_ref`](./future/struct.Cache.html#method.entry_by_ref)
///
pub struct Entry<K, V> {
    key: Option<Arc<K>>,
    value: V,
    is_fresh: bool,
    is_old_value_replaced: bool,
}

impl<K, V> Debug for Entry<K, V>
where
    K: Debug,
    V: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("key", self.key())
            .field("value", &self.value)
            .field("is_fresh", &self.is_fresh)
            .field("is_old_value_replaced", &self.is_old_value_replaced)
            .finish()
    }
}

impl<K, V> Entry<K, V> {
    pub(crate) fn new(
        key: Option<Arc<K>>,
        value: V,
        is_fresh: bool,
        is_old_value_replaced: bool,
    ) -> Self {
        Self {
            key,
            value,
            is_fresh,
            is_old_value_replaced,
        }
    }

    /// Returns a reference to the wrapped key.
    pub fn key(&self) -> &K {
        self.key.as_ref().expect("Bug: Key is None")
    }

    /// Returns a reference to the wrapped value.
    ///
    /// Note that the returned reference is _not_ pointing to the original value in
    /// the cache. Instead, it is pointing to the cloned value in this `Entry`.
    pub fn value(&self) -> &V {
        &self.value
    }

    /// Consumes this `Entry`, returning the wrapped value.
    ///
    /// Note that the returned value is a clone of the original value in the cache.
    /// It was cloned when this `Entry` was constructed.
    pub fn into_value(self) -> V {
        self.value
    }

    /// Returns `true` if the value in this `Entry` was not cached and was freshly
    /// computed.
    pub fn is_fresh(&self) -> bool {
        self.is_fresh
    }

    /// Returns `true` if an old value existed in the cache and was replaced by the
    /// value in this `Entry`.
    ///
    /// Note that the new value can be the same as the old value. This method still
    /// returns `true` in that case.
    pub fn is_old_value_replaced(&self) -> bool {
        self.is_old_value_replaced
    }
}

#[derive(Debug, Clone)]
pub struct EntryMetadata {
    region: EntryRegion,
    policy_weight: u32,
    last_modified: Instant,
    last_accessed: Instant,
    expiration_time: Option<Instant>,
    snapshot_at: Instant,
}

impl EntryMetadata {
    pub fn new(
        region: EntryRegion,
        policy_weight: u32,
        last_modified: Instant,
        last_accessed: Instant,
        expiration_time: Option<Instant>,
        snapshot_at: Instant,
    ) -> Self {
        Self {
            region,
            policy_weight,
            last_modified,
            last_accessed,
            expiration_time,
            snapshot_at,
        }
    }

    pub(crate) fn from_element<K>(
        region: EntryRegion,
        element: &KeyHashDate<K>,
        clock: &super::time::Clock,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
        snapshot_at: Instant,
    ) -> Self {
        // SAFETY: `last_accessed` and `last_modified` should be `Some` since we
        // assume the element is not dirty. But we use `unwrap_or_default` to avoid
        // panicking just in case they are `None`.
        let last_modified = clock.to_std_instant(element.last_modified().unwrap_or_default());
        let last_accessed = clock.to_std_instant(element.last_accessed().unwrap_or_default());

        // When per-entry expiration is used, the expiration time is set in the
        // element, otherwise, we calculate the expiration time based on the
        // `time_to_live` and `time_to_idle` settings.
        let expiration_time = if element.expiration_time().is_some() {
            element.expiration_time().map(|ts| clock.to_std_instant(ts))
        } else {
            match (time_to_live, time_to_idle) {
                (Some(ttl), Some(tti)) => {
                    let exp_by_ttl = last_modified + ttl;
                    let exp_by_tti = last_accessed + tti;
                    Some(exp_by_ttl.min(exp_by_tti))
                }
                (Some(ttl), None) => Some(last_modified + ttl),
                (None, Some(tti)) => Some(last_accessed + tti),
                (None, None) => None,
            }
        };

        Self {
            region,
            policy_weight: element.entry_info().policy_weight(),
            last_modified,
            last_accessed,
            expiration_time,
            snapshot_at,
        }
    }

    pub fn region(&self) -> EntryRegion {
        self.region
    }

    pub fn policy_weight(&self) -> u32 {
        self.policy_weight
    }

    pub fn last_modified(&self) -> Instant {
        self.last_modified
    }

    pub fn last_accessed(&self) -> Instant {
        self.last_accessed
    }

    pub fn expiration_time(&self) -> Option<Instant> {
        self.expiration_time
    }

    pub fn snapshot_at(&self) -> Instant {
        self.snapshot_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryRegion {
    Window,
    Main,
}
