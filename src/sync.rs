//! Provides thread-safe, blocking cache implementations.

use crate::common::{atomic_time::AtomicInstant, deque::DeqNode, time::Instant};

use parking_lot::Mutex;
use std::{
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
};

pub(crate) mod base_cache;
mod builder;
mod cache;
mod deques;
pub(crate) mod housekeeper;
mod invalidator;
mod segment;
mod value_initializer;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use segment::SegmentedCache;

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

pub(crate) trait EntryInfo: AccessTime {
    fn is_admitted(&self) -> bool;
    fn set_is_admitted(&self, value: bool);
    fn reset_timestamps(&self);
    fn weighted_size(&self) -> u32;
    fn set_weighted_size(&self, size: u32);
}

pub(crate) struct EntryInfoFull {
    is_admitted: AtomicBool,
    last_accessed: AtomicInstant,
    last_modified: AtomicInstant,
    weighted_size: AtomicU32,
}

impl EntryInfoFull {
    fn new(weighted_size: u32) -> Self {
        Self {
            is_admitted: Default::default(),
            last_accessed: Default::default(),
            last_modified: Default::default(),
            weighted_size: AtomicU32::new(weighted_size),
        }
    }
}

impl EntryInfo for EntryInfoFull {
    #[inline]
    fn is_admitted(&self) -> bool {
        self.is_admitted.load(Ordering::Acquire)
    }

    #[inline]
    fn set_is_admitted(&self, value: bool) {
        self.is_admitted.store(value, Ordering::Release);
    }

    #[inline]
    fn reset_timestamps(&self) {
        self.last_accessed.reset();
        self.last_modified.reset();
    }

    #[inline]
    fn weighted_size(&self) -> u32 {
        self.weighted_size.load(Ordering::Acquire)
    }

    #[inline]
    fn set_weighted_size(&self, size: u32) {
        self.weighted_size.store(size, Ordering::Release);
    }
}

impl AccessTime for EntryInfoFull {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.last_accessed.instant()
    }

    #[inline]
    fn set_last_accessed(&self, timestamp: Instant) {
        self.last_accessed.set_instant(timestamp);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.last_modified.instant()
    }

    #[inline]
    fn set_last_modified(&self, timestamp: Instant) {
        self.last_modified.set_instant(timestamp);
    }
}

pub(crate) type ArcedEntryInfo = Arc<dyn EntryInfo + Send + Sync + 'static>;

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
    entry_info: ArcedEntryInfo,
}

impl<K> KeyDate<K> {
    pub(crate) fn new(key: Arc<K>, entry_info: &ArcedEntryInfo) -> Self {
        Self {
            key,
            entry_info: Arc::clone(entry_info),
        }
    }

    pub(crate) fn key(&self) -> &Arc<K> {
        &self.key
    }

    pub(crate) fn entry_info(&self) -> &ArcedEntryInfo {
        &self.entry_info
    }

    pub(crate) fn last_modified(&self) -> Option<Instant> {
        self.entry_info.last_modified()
    }
}

pub(crate) struct KeyHashDate<K> {
    key: Arc<K>,
    hash: u64,
    entry_info: Arc<dyn EntryInfo + Send + Sync + 'static>,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(kh: KeyHash<K>, entry_info: &ArcedEntryInfo) -> Self {
        Self {
            key: kh.key,
            hash: kh.hash,
            entry_info: Arc::clone(entry_info),
        }
    }

    pub(crate) fn key(&self) -> &Arc<K> {
        &self.key
    }

    pub(crate) fn entry_info(&self) -> &ArcedEntryInfo {
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
    info: ArcedEntryInfo,
    nodes: Mutex<DeqNodes<K>>,
}

impl<K, V> ValueEntry<K, V> {
    pub(crate) fn new(value: V, weighted_size: u32) -> Self {
        Self {
            value,
            info: Arc::new(EntryInfoFull::new(weighted_size)),
            nodes: Mutex::new(DeqNodes {
                access_order_q_node: None,
                write_order_q_node: None,
            }),
        }
    }

    pub(crate) fn new_with(value: V, weighted_size: u32, other: &Self) -> Self {
        let nodes = {
            let other_nodes = other.nodes.lock();
            DeqNodes {
                access_order_q_node: other_nodes.access_order_q_node,
                write_order_q_node: other_nodes.write_order_q_node,
            }
        };
        let info = Arc::clone(&other.info);
        info.set_weighted_size(weighted_size);
        // To prevent this updated ValueEntry from being evicted by an expiration policy,
        // set the max value to the timestamps. They will be replaced with the real
        // timestamps when applying writes.
        info.reset_timestamps();
        Self {
            value,
            info,
            nodes: Mutex::new(nodes),
        }
    }

    pub(crate) fn entry_info(&self) -> &ArcedEntryInfo {
        &self.info
    }

    pub(crate) fn is_admitted(&self) -> bool {
        self.info.is_admitted()
    }

    pub(crate) fn set_is_admitted(&self, value: bool) {
        self.info.set_is_admitted(value);
    }

    #[inline]
    pub(crate) fn weighted_size(&self) -> u32 {
        self.info.weighted_size()
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

pub(crate) enum ReadOp<K, V> {
    // u64 is the hash of the key.
    Hit(u64, Arc<ValueEntry<K, V>>, Instant),
    Miss(u64),
}

pub(crate) enum WriteOp<K, V> {
    Upsert {
        key_hash: KeyHash<K>,
        value_entry: Arc<ValueEntry<K, V>>,
        old_weighted_size: u32,
        new_weighted_size: u32,
    },
    Remove(KvEntry<K, V>),
}
