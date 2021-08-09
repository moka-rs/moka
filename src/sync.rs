//! Provides thread-safe, blocking cache implementations.

use crate::common::{deque::DeqNode, u64_to_instant, AccessTime};

use parking_lot::Mutex;
use quanta::Instant;
use std::{
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    pub(crate) key: Arc<K>,
    pub(crate) timestamp: Arc<AtomicU64>,
}

impl<K> KeyDate<K> {
    pub(crate) fn new(key: Arc<K>, timestamp: Arc<AtomicU64>) -> Self {
        Self { key, timestamp }
    }

    pub(crate) fn timestamp(&self) -> Option<Instant> {
        u64_to_instant(self.timestamp.load(Ordering::Acquire))
    }
}

pub(crate) struct KeyHashDate<K> {
    pub(crate) key: Arc<K>,
    pub(crate) hash: u64,
    pub(crate) timestamp: Arc<AtomicU64>,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(kh: KeyHash<K>, timestamp: Arc<AtomicU64>) -> Self {
        Self {
            key: kh.key,
            hash: kh.hash,
            timestamp,
        }
    }
}

pub(crate) struct KeyValueEntry<K, V> {
    pub(crate) key: Arc<K>,
    pub(crate) entry: Arc<ValueEntry<K, V>>,
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
    is_admitted: Arc<AtomicBool>,
    last_accessed: Arc<AtomicU64>,
    last_modified: Arc<AtomicU64>,
    nodes: Mutex<DeqNodes<K>>,
}

impl<K, V> ValueEntry<K, V> {
    pub(crate) fn new(value: V) -> Self {
        Self {
            value,
            is_admitted: Arc::new(AtomicBool::new(false)),
            last_accessed: Arc::new(AtomicU64::new(std::u64::MAX)),
            last_modified: Arc::new(AtomicU64::new(std::u64::MAX)),
            nodes: Mutex::new(DeqNodes {
                access_order_q_node: None,
                write_order_q_node: None,
            }),
        }
    }

    pub(crate) fn new_with(value: V, other: &Self) -> Self {
        let nodes = {
            let other_nodes = other.nodes.lock();
            DeqNodes {
                access_order_q_node: other_nodes.access_order_q_node,
                write_order_q_node: other_nodes.write_order_q_node,
            }
        };
        let last_accessed = Arc::clone(&other.last_accessed);
        let last_modified = Arc::clone(&other.last_modified);
        // To prevent this updated ValueEntry from being evicted by a expiration policy,
        // set the max value to the timestamps. They will be replaced with the real
        // timestamps when applying writes.
        last_accessed.store(std::u64::MAX, Ordering::Release);
        last_modified.store(std::u64::MAX, Ordering::Release);
        Self {
            value,
            is_admitted: Arc::clone(&other.is_admitted),
            last_accessed,
            last_modified,
            nodes: Mutex::new(nodes),
        }
    }

    pub(crate) fn is_admitted(&self) -> bool {
        self.is_admitted.load(Ordering::Acquire)
    }

    pub(crate) fn set_is_admitted(&self, value: bool) {
        self.is_admitted.store(value, Ordering::Release);
    }

    pub(crate) fn raw_last_accessed(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.last_accessed)
    }

    pub(crate) fn raw_last_modified(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.last_modified)
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
        u64_to_instant(self.last_accessed.load(Ordering::Acquire))
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        self.last_accessed
            .store(timestamp.as_u64(), Ordering::Release);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        u64_to_instant(self.last_modified.load(Ordering::Acquire))
    }

    #[inline]
    fn set_last_modified(&mut self, timestamp: Instant) {
        self.last_modified
            .store(timestamp.as_u64(), Ordering::Release);
    }
}

impl<K> AccessTime for DeqNode<KeyDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_accessed(&mut self, _timestamp: Instant) {
        unreachable!();
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        u64_to_instant(self.element.timestamp.load(Ordering::Acquire))
    }

    #[inline]
    fn set_last_modified(&mut self, timestamp: Instant) {
        self.element
            .timestamp
            .store(timestamp.as_u64(), Ordering::Release);
    }
}

impl<K> AccessTime for DeqNode<KeyHashDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        u64_to_instant(self.element.timestamp.load(Ordering::Acquire))
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        self.element
            .timestamp
            .store(timestamp.as_u64(), Ordering::Release);
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        None
    }

    #[inline]
    fn set_last_modified(&mut self, _timestamp: Instant) {
        unreachable!();
    }
}

pub(crate) enum ReadOp<K, V> {
    // u64 is the hash of the key.
    Hit(u64, Arc<ValueEntry<K, V>>, Instant),
    Miss(u64),
}

pub(crate) enum WriteOp<K, V> {
    Upsert(KeyHash<K>, Arc<ValueEntry<K, V>>),
    Remove(Arc<K>, Arc<ValueEntry<K, V>>),
}
