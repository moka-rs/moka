//! Provides thread-safe, blocking cache implementations.

use crate::common::{deque::DeqNode, time::Instant};

use parking_lot::Mutex;
use std::{marker::PhantomData, ptr::NonNull, sync::Arc};

pub(crate) mod base_cache;
mod builder;
mod cache;
mod deques;
mod entry_info;
pub(crate) mod housekeeper;
mod invalidator;
mod segment;
mod value_initializer;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use segment::SegmentedCache;

use self::entry_info::{ArcEntryInfo, EntryInfo, EntryInfoFull, EntryInfoWo};

/// The type of the unique ID to identify a predicate used by
/// [`Cache#invalidate_entries_if`][invalidate-if] method.
///
/// A `PredicateId` is a `String` of UUID (version 4).
///
/// [invalidate-if]: ./struct.Cache.html#method.invalidate_entries_if
pub type PredicateId = String;

pub(crate) type PredicateIdStr<'a> = &'a str;

/// Provides extra methods that will be useful for testing.
pub trait ConcurrentCacheExt<K, V> {
    /// Performs any pending maintenance operations needed by the cache.
    fn sync(&self);
}

pub(crate) type Weigher<K, V> = Box<dyn Fn(&K, &V) -> u32 + Send + Sync + 'static>;

pub(crate) trait AccessTime {
    fn last_accessed(&self) -> Option<Instant>;
    fn set_last_accessed(&self, timestamp: Instant);
    fn last_modified(&self) -> Option<Instant>;
    fn set_last_modified(&self, timestamp: Instant);
}

pub(crate) struct KeyHash<K> {
    pub(crate) key: Arc<K>,
    pub(crate) hash: u64,
}

impl<K> KeyHash<K> {
    pub(crate) fn new(key: Arc<K>, hash: u64) -> Self {
        Self { key, hash }
    }
}

impl<K> Clone for KeyHash<K> {
    fn clone(&self) -> Self {
        Self {
            key: Arc::clone(&self.key),
            hash: self.hash,
        }
    }
}

pub(crate) struct KeyDate<K> {
    key: Arc<K>,
    entry_info: ArcEntryInfo,
}

impl<K> KeyDate<K> {
    pub(crate) fn new(key: Arc<K>, entry_info: &ArcEntryInfo) -> Self {
        Self {
            key,
            entry_info: Arc::clone(entry_info),
        }
    }

    pub(crate) fn key(&self) -> &Arc<K> {
        &self.key
    }

    pub(crate) fn last_modified(&self) -> Option<Instant> {
        self.entry_info.last_modified()
    }
}

pub(crate) struct KeyHashDate<K> {
    key: Arc<K>,
    hash: u64,
    entry_info: ArcEntryInfo,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(kh: KeyHash<K>, entry_info: &ArcEntryInfo) -> Self {
        Self {
            key: kh.key,
            hash: kh.hash,
            entry_info: Arc::clone(entry_info),
        }
    }

    pub(crate) fn key(&self) -> &Arc<K> {
        &self.key
    }

    pub(crate) fn entry_info(&self) -> &ArcEntryInfo {
        &self.entry_info
    }
}

pub(crate) struct KvEntry<K, V> {
    pub(crate) key: Arc<K>,
    pub(crate) entry: Arc<ValueEntry<K, V>>,
}

impl<K, V> KvEntry<K, V> {
    pub(crate) fn new(key: Arc<K>, entry: Arc<ValueEntry<K, V>>) -> Self {
        Self { key, entry }
    }
}

impl<K> AccessTime for DeqNode<KeyDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_accessed(&self, _timestamp: Instant) {
        unreachable!();
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.element.entry_info.last_modified()
    }

    #[inline]
    fn set_last_modified(&self, timestamp: Instant) {
        self.element.entry_info.set_last_modified(timestamp);
    }
}

impl<K> AccessTime for DeqNode<KeyHashDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.element.entry_info.last_accessed()
    }

    #[inline]
    fn set_last_accessed(&self, timestamp: Instant) {
        self.element.entry_info.set_last_accessed(timestamp);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_modified(&self, _timestamp: Instant) {
        unreachable!();
    }
}

// DeqNode for an access order queue.
type KeyDeqNodeAo<K> = NonNull<DeqNode<KeyHashDate<K>>>;

// DeqNode for the write order queue.
type KeyDeqNodeWo<K> = NonNull<DeqNode<KeyDate<K>>>;

struct DeqNodes<K> {
    access_order_q_node: Option<KeyDeqNodeAo<K>>,
    write_order_q_node: Option<KeyDeqNodeWo<K>>,
}

// We need this `unsafe impl` as DeqNodes have NonNull pointers.
unsafe impl<K> Send for DeqNodes<K> {}

pub(crate) struct ValueEntry<K, V> {
    pub(crate) value: V,
    info: ArcEntryInfo,
    nodes: Mutex<DeqNodes<K>>,
}

impl<K, V> ValueEntry<K, V> {
    fn new(value: V, entry_info: ArcEntryInfo) -> Self {
        Self {
            value,
            info: entry_info,
            nodes: Mutex::new(DeqNodes {
                access_order_q_node: None,
                write_order_q_node: None,
            }),
        }
    }

    fn new_from(value: V, entry_info: ArcEntryInfo, other: &Self) -> Self {
        let nodes = {
            let other_nodes = other.nodes.lock();
            DeqNodes {
                access_order_q_node: other_nodes.access_order_q_node,
                write_order_q_node: other_nodes.write_order_q_node,
            }
        };
        // To prevent this updated ValueEntry from being evicted by an expiration policy,
        // set the max value to the timestamps. They will be replaced with the real
        // timestamps when applying writes.
        entry_info.reset_timestamps();
        Self {
            value,
            info: entry_info,
            nodes: Mutex::new(nodes),
        }
    }

    pub(crate) fn entry_info(&self) -> &ArcEntryInfo {
        &self.info
    }

    pub(crate) fn is_admitted(&self) -> bool {
        self.info.is_admitted()
    }

    pub(crate) fn set_is_admitted(&self, value: bool) {
        self.info.set_is_admitted(value);
    }

    #[inline]
    pub(crate) fn policy_weight(&self) -> u32 {
        self.info.policy_weight()
    }

    pub(crate) fn access_order_q_node(&self) -> Option<KeyDeqNodeAo<K>> {
        self.nodes.lock().access_order_q_node
    }

    pub(crate) fn set_access_order_q_node(&self, node: Option<KeyDeqNodeAo<K>>) {
        self.nodes.lock().access_order_q_node = node;
    }

    pub(crate) fn take_access_order_q_node(&self) -> Option<KeyDeqNodeAo<K>> {
        self.nodes.lock().access_order_q_node.take()
    }

    pub(crate) fn write_order_q_node(&self) -> Option<KeyDeqNodeWo<K>> {
        self.nodes.lock().write_order_q_node
    }

    pub(crate) fn set_write_order_q_node(&self, node: Option<KeyDeqNodeWo<K>>) {
        self.nodes.lock().write_order_q_node = node;
    }

    pub(crate) fn take_write_order_q_node(&self) -> Option<KeyDeqNodeWo<K>> {
        self.nodes.lock().write_order_q_node.take()
    }

    pub(crate) fn unset_q_nodes(&self) {
        let mut nodes = self.nodes.lock();
        nodes.access_order_q_node = None;
        nodes.write_order_q_node = None;
    }
}

impl<K, V> AccessTime for Arc<ValueEntry<K, V>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.info.last_accessed()
    }

    #[inline]
    fn set_last_accessed(&self, timestamp: Instant) {
        self.info.set_last_accessed(timestamp);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.info.last_modified()
    }

    #[inline]
    fn set_last_modified(&self, timestamp: Instant) {
        self.info.set_last_modified(timestamp);
    }
}

pub(crate) trait ValueEntryBuilder<K, V> {
    fn build(&self, value: V, policy_weight: u32) -> ValueEntry<K, V>;

    fn build_from(
        &self,
        value: V,
        policy_weight: u32,
        other: &ValueEntry<K, V>,
    ) -> ValueEntry<K, V>;
}

pub(crate) struct ValueEntryBuilderImpl<K, V, EI>(PhantomData<(K, V, EI)>);

impl<K, V, EI: EntryInfo> ValueEntryBuilderImpl<K, V, EI> {
    pub(crate) fn new() -> Self {
        Self(PhantomData::default())
    }
}

impl<K, V> ValueEntryBuilder<K, V> for ValueEntryBuilderImpl<K, V, EntryInfoFull> {
    fn build(&self, value: V, policy_weight: u32) -> ValueEntry<K, V> {
        let info = Arc::new(EntryInfoFull::new(policy_weight));
        ValueEntry::new(value, info)
    }

    fn build_from(
        &self,
        value: V,
        policy_weight: u32,
        other: &ValueEntry<K, V>,
    ) -> ValueEntry<K, V> {
        let info = Arc::clone(&other.info);
        info.set_policy_weight(policy_weight);
        ValueEntry::new_from(value, info, other)
    }
}

impl<K, V> ValueEntryBuilder<K, V> for ValueEntryBuilderImpl<K, V, EntryInfoWo> {
    fn build(&self, value: V, policy_weight: u32) -> ValueEntry<K, V> {
        let info = Arc::new(EntryInfoWo::new(policy_weight));
        ValueEntry::new(value, info)
    }

    fn build_from(
        &self,
        value: V,
        policy_weight: u32,
        other: &ValueEntry<K, V>,
    ) -> ValueEntry<K, V> {
        let info = Arc::clone(&other.info);
        info.set_policy_weight(policy_weight);
        ValueEntry::new_from(value, info, other)
    }
}

pub(crate) enum ReadOp<K, V> {
    // u64 is the hash of the key.
    Hit(u64, Arc<ValueEntry<K, V>>, Instant),
    Miss(u64),
}

pub(crate) enum WriteOp<K, V> {
    Upsert {
        key_hash: KeyHash<K>,
        value_entry: Arc<ValueEntry<K, V>>,
        old_weight: u32,
        new_weight: u32,
    },
    Remove(KvEntry<K, V>),
}
