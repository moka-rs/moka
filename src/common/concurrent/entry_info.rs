use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::{AccessTime, KeyDeqNodeAo, KeyDeqNodeWo};
use crate::common::{concurrent::atomic_time::AtomicInstant, time::Instant};

use parking_lot::Mutex;

pub(crate) struct DeqNodes<K> {
    access_order_q_node: Option<KeyDeqNodeAo<K>>,
    write_order_q_node: Option<KeyDeqNodeWo<K>>,
}

// We need this `unsafe impl` as DeqNodes have NonNull pointers.
unsafe impl<K> Send for DeqNodes<K> {}

pub(crate) struct EntryInfo<K> {
    /// `is_admitted` indicates that the entry has been admitted to the
    /// cache. When `false`, it means the entry is _temporary_ admitted to
    /// the cache or evicted from the cache (so it should not have LRU nodes).
    is_admitted: AtomicBool,
    /// `is_dirty` indicates that the entry has been inserted (or updated)
    /// in the hash table, but the history of the insertion has not yet
    /// been applied to the LRU deques and LFU estimator.
    is_dirty: AtomicBool,
    last_accessed: AtomicInstant,
    last_modified: AtomicInstant,
    policy_weight: AtomicU32,
    nodes: Mutex<DeqNodes<K>>,
}

impl<K> EntryInfo<K> {
    #[inline]
    pub(crate) fn new(timestamp: Instant, policy_weight: u32) -> Self {
        #[cfg(feature = "unstable-debug-counters")]
        super::debug_counters::InternalGlobalDebugCounters::entry_info_created();

        Self {
            is_admitted: Default::default(),
            is_dirty: AtomicBool::new(true),
            last_accessed: AtomicInstant::new(timestamp),
            last_modified: AtomicInstant::new(timestamp),
            policy_weight: AtomicU32::new(policy_weight),
            nodes: Mutex::new(DeqNodes {
                access_order_q_node: None,
                write_order_q_node: None,
            }),
        }
    }

    #[inline]
    pub(crate) fn is_admitted(&self) -> bool {
        self.is_admitted.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn set_admitted(&self, value: bool) {
        self.is_admitted.store(value, Ordering::Release);
    }

    #[inline]
    pub(crate) fn is_dirty(&self) -> bool {
        self.is_dirty.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn set_dirty(&self, value: bool) {
        self.is_dirty.store(value, Ordering::Release);
    }

    #[inline]
    pub(crate) fn policy_weight(&self) -> u32 {
        self.policy_weight.load(Ordering::Acquire)
    }

    pub(crate) fn set_policy_weight(&self, size: u32) {
        self.policy_weight.store(size, Ordering::Release);
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

#[cfg(feature = "unstable-debug-counters")]
impl<K> Drop for EntryInfo<K> {
    fn drop(&mut self) {
        super::debug_counters::InternalGlobalDebugCounters::entry_info_dropped();
    }
}

impl<K> AccessTime for EntryInfo<K> {
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

#[cfg(test)]
mod test {
    use super::EntryInfo;

    // Run with:
    //   RUSTFLAGS='--cfg rustver' cargo test --lib --features sync -- common::concurrent::entry_info::test --nocapture
    //   RUSTFLAGS='--cfg rustver' cargo test --lib --no-default-features --features sync -- common::concurrent::entry_info::test --nocapture
    //
    // Note: the size of the struct may change in a future version of Rust.
    #[cfg_attr(
        not(all(rustver, any(target_os = "linux", target_os = "macos"))),
        ignore
    )]
    #[test]
    fn check_struct_size() {
        use std::mem::size_of;

        #[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
        enum TargetArch {
            Linux64,
            Linux32,
            MacOS64,
        }

        use TargetArch::*;

        // e.g. "1.64"
        let ver = option_env!("RUSTC_SEMVER").expect("RUSTC_SEMVER env var not set");
        let is_quanta_enabled = cfg!(feature = "quanta");
        let arch = if cfg!(target_os = "linux") {
            if cfg!(target_pointer_width = "64") {
                Linux64
            } else if cfg!(target_pointer_width = "32") {
                Linux32
            } else {
                panic!("Unsupported pointer width for Linux");
            }
        } else if cfg!(target_os = "macos") {
            MacOS64
        } else {
            panic!("Unsupported target architecture");
        };

        let expected_sizes = match (arch, is_quanta_enabled) {
            (Linux64, true) => vec![("1.51", 24)],
            (Linux32, true) => vec![("1.51", 24)],
            (MacOS64, true) => vec![("1.62", 24)],
            (Linux64, false) => vec![("1.66", 56), ("1.51", 72)],
            (Linux32, false) => vec![("1.66", 56), ("1.62", 72), ("1.51", 40)],
            (MacOS64, false) => vec![("1.62", 56)],
        };

        let mut expected = None;
        for (ver_str, size) in expected_sizes {
            expected = Some(size);
            if ver >= ver_str {
                break;
            }
        }

        if let Some(size) = expected {
            assert_eq!(size_of::<EntryInfo<()>>(), size);
        } else {
            panic!("No expected size for {:?} with Rust version {}", arch, ver);
        }
    }
}
