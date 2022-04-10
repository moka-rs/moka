//! Provides a *not* thread-safe cache implementation built upon
//! [`std::collections::HashMap`][std-hashmap].
//!
//! [std-hashmap]: https://doc.rust-lang.org/std/collections/struct.HashMap.html

mod builder;
mod cache;
mod deques;
mod iter;

use std::{ptr::NonNull, rc::Rc};
use tagptr::TagNonNull;

pub use builder::CacheBuilder;
pub use cache::Cache;
pub use iter::Iter;

use crate::common::{deque::DeqNode, time::Instant};

pub(crate) type Weigher<K, V> = Box<dyn FnMut(&K, &V) -> u32>;

pub(crate) trait AccessTime {
    fn last_accessed(&self) -> Option<Instant>;
    fn set_last_accessed(&mut self, timestamp: Instant);
    fn last_modified(&self) -> Option<Instant>;
    fn set_last_modified(&mut self, timestamp: Instant);
}

pub(crate) struct KeyDate<K> {
    pub(crate) key: Rc<K>,
    pub(crate) timestamp: Option<Instant>,
}

impl<K> KeyDate<K> {
    pub(crate) fn new(key: Rc<K>, timestamp: Option<Instant>) -> Self {
        Self { key, timestamp }
    }
}

pub(crate) struct KeyHashDate<K> {
    pub(crate) key: Rc<K>,
    pub(crate) hash: u64,
    pub(crate) timestamp: Option<Instant>,
}

impl<K> KeyHashDate<K> {
    pub(crate) fn new(key: Rc<K>, hash: u64, timestamp: Option<Instant>) -> Self {
        Self {
            key,
            hash,
            timestamp,
        }
    }
}

// DeqNode for an access order queue.
type KeyDeqNodeAo<K> = TagNonNull<DeqNode<KeyHashDate<K>>, 2>;

// DeqNode for the write order queue.
type KeyDeqNodeWo<K> = NonNull<DeqNode<KeyDate<K>>>;

struct EntryInfo<K> {
    access_order_q_node: Option<KeyDeqNodeAo<K>>,
    write_order_q_node: Option<KeyDeqNodeWo<K>>,
    policy_weight: u32,
}

pub(crate) struct ValueEntry<K, V> {
    pub(crate) value: V,
    info: EntryInfo<K>,
}

impl<K, V> ValueEntry<K, V> {
    pub(crate) fn new(value: V, policy_weight: u32) -> Self {
        Self {
            value,
            info: EntryInfo {
                access_order_q_node: None,
                write_order_q_node: None,
                policy_weight,
            },
        }
    }

    #[inline]
    pub(crate) fn replace_deq_nodes_with(&mut self, mut other: Self) {
        self.info.access_order_q_node = other.info.access_order_q_node.take();
        self.info.write_order_q_node = other.info.write_order_q_node.take();
    }

    #[inline]
    pub(crate) fn access_order_q_node(&self) -> Option<KeyDeqNodeAo<K>> {
        self.info.access_order_q_node
    }

    #[inline]
    pub(crate) fn set_access_order_q_node(&mut self, node: Option<KeyDeqNodeAo<K>>) {
        self.info.access_order_q_node = node;
    }

    #[inline]
    pub(crate) fn take_access_order_q_node(&mut self) -> Option<KeyDeqNodeAo<K>> {
        self.info.access_order_q_node.take()
    }

    #[inline]
    pub(crate) fn write_order_q_node(&self) -> Option<KeyDeqNodeWo<K>> {
        self.info.write_order_q_node
    }

    #[inline]
    pub(crate) fn set_write_order_q_node(&mut self, node: Option<KeyDeqNodeWo<K>>) {
        self.info.write_order_q_node = node;
    }

    #[inline]
    pub(crate) fn take_write_order_q_node(&mut self) -> Option<KeyDeqNodeWo<K>> {
        self.info.write_order_q_node.take()
    }

    #[inline]
    pub(crate) fn policy_weight(&self) -> u32 {
        self.info.policy_weight
    }

    #[inline]
    pub(crate) fn set_policy_weight(&mut self, policy_weight: u32) {
        self.info.policy_weight = policy_weight;
    }
}

impl<K, V> AccessTime for ValueEntry<K, V> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.access_order_q_node()
            .and_then(|node| unsafe { node.as_ref() }.element.timestamp)
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        if let Some(mut node) = self.info.access_order_q_node {
            unsafe { node.as_mut() }.set_last_accessed(timestamp);
        }
    }

    #[inline]
    fn last_modified(&self) -> Option<Instant> {
        self.write_order_q_node()
            .and_then(|node| unsafe { node.as_ref() }.element.timestamp)
    }

    #[inline]
    fn set_last_modified(&mut self, timestamp: Instant) {
        if let Some(mut node) = self.info.write_order_q_node {
            unsafe { node.as_mut() }.set_last_modified(timestamp);
        }
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
        self.element.timestamp
    }

    #[inline]
    fn set_last_modified(&mut self, timestamp: Instant) {
        self.element.timestamp = Some(timestamp);
    }
}

impl<K> AccessTime for DeqNode<KeyHashDate<K>> {
    #[inline]
    fn last_accessed(&self) -> Option<Instant> {
        self.element.timestamp
    }

    #[inline]
    fn set_last_accessed(&mut self, timestamp: Instant) {
        self.element.timestamp = Some(timestamp);
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
