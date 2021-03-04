#![allow(unused)]

use crate::common::AccessTime;

use super::ValueEntry;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Mutex, RwLock};
use quanta::Instant;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

// TODO: Do a research on concurrent/persistent data structures that could
// replace RwLock<HashMap<_, _>> to store predicates.
//
// Requirements:
// - A write will not block reads or other writes.
// - Provides a way to iterate through the predicates.
//
// Candidates (concurrent data structure):
// - cht crate's HashMap, once iterator is implemented.
//     - https://github.com/Gregory-Meyer/cht/issues/20
//
// Candidates (persistent data structure):
// - im crate's Vector or HashMap.
// - rpds crate's Vector or RedBlackTreeMap.

pub(crate) struct Invalidator<K, V> {
    predicates: RwLock<HashMap<u64, PredicateImpl<K, V>>>,
    is_empty: AtomicBool,
    last_key: AtomicU64,
    scan_context: ScanContext<K, V>,
}

impl<K, V> Default for Invalidator<K, V> {
    fn default() -> Self {
        Self {
            predicates: RwLock::new(HashMap::new()),
            is_empty: AtomicBool::new(true),
            last_key: AtomicU64::new(std::u64::MAX),
            scan_context: ScanContext::default(),
        }
    }
}

impl<K, V> Invalidator<K, V>
where
    K: 'static,
    V: 'static,
{
    pub(crate) fn register_predicate(
        &self,
        predicate: PredicateFun<K, V>,
        registered_at: Instant,
    ) -> Result<u64, PredicateRegistrationError> {
        const MAX_RETRY: usize = 10_000;
        let mut tries = 0;
        let mut preds = self.predicates.write();

        while tries < MAX_RETRY {
            // NOTE: fetch_add operation wraps around on overflow.
            let id = self.last_key.fetch_add(1, Ordering::SeqCst);
            if preds.contains_key(&id) {
                tries += 1;

                continue; // Retry
            }
            let pred = PredicateImpl::new(predicate, registered_at);
            preds.insert(id, pred);
            self.is_empty.store(false, Ordering::Release);

            return Ok(id);
        }

        Err(PredicateRegistrationError::NoSpaceLeft)
    }

    pub(crate) fn remove_predicate(&self, id: u64) {
        let mut preds = self.predicates.write();
        preds.remove(&id);
        if preds.is_empty() {
            self.is_empty.store(true, Ordering::Release);
        }
    }

    // This method will be called by the get method of Cache.
    pub(crate) fn apply_predicates(&self, key: &Arc<K>, entry: &Arc<ValueEntry<K, V>>) -> bool {
        if self.is_empty.load(Ordering::Acquire) {
            false
        } else {
            Self::do_apply_predicates(self.predicates.read().values(), key, entry)
        }
    }

    // This method will be called by eviction scan.
    pub(crate) fn process_candidates(&self, remover: impl Fn(K, &PredicateFun<K, V>) -> Option<V>) {
        todo!()
    }

    fn do_apply_predicates<'a, I, P>(
        predicates: I,
        key: &Arc<K>,
        entry: &Arc<ValueEntry<K, V>>,
    ) -> bool
    where
        I: Iterator<Item = &'a P>,
        P: Predicate<K, V> + 'static,
    {
        if let Some(ts) = entry.last_modified() {
            predicates
                .filter(|pred| pred.is_applicable(ts))
                .any(|pred| pred.apply(key, &entry.value))
        } else {
            false
        }
    }
}

struct ScanContext<K, V> {
    predicates: Mutex<Vec<PredicateImplLite<K, V>>>,
    newest_timestamp: AtomicU64,
    snd: Sender<CacheEntry<K>>,
    rcv: Receiver<CacheEntry<K>>,
}

impl<K, V> Default for ScanContext<K, V> {
    fn default() -> Self {
        let (snd, rcv) = crossbeam_channel::unbounded();
        Self {
            predicates: Mutex::new(Vec::default()),
            newest_timestamp: AtomicU64::new(0),
            snd,
            rcv,
        }
    }
}

type PredicateFun<K, V> = Arc<dyn Fn(&K, &V) -> bool + Send + Sync + 'static>;

trait Predicate<K, V> {
    fn is_applicable(&self, last_modified: Instant) -> bool;
    fn apply(&self, key: &K, value: &V) -> bool;
}

struct PredicateImpl<K, V> {
    f: PredicateFun<K, V>,
    registered_at: Instant,
    // The oldest timestamp of the entries checked by this predicate.
    oldest: AtomicU64,
    // The newest timestamp of the entries checked by this predicate.
    newest: AtomicU64,
}

impl<K, V> PredicateImpl<K, V> {
    fn new(f: PredicateFun<K, V>, registered_at: Instant) -> Self {
        Self {
            f,
            registered_at,
            oldest: AtomicU64::new(std::u64::MAX),
            newest: AtomicU64::new(std::u64::MIN),
        }
    }
}

impl<K, V> Predicate<K, V> for PredicateImpl<K, V> {
    fn is_applicable(&self, last_modified: Instant) -> bool {
        last_modified <= self.registered_at
            && (last_modified.as_u64() < self.oldest.load(Ordering::Acquire)
                || last_modified.as_u64() > self.newest.load(Ordering::Acquire))
    }

    fn apply(&self, key: &K, value: &V) -> bool {
        (self.f)(key, value)
    }
}

// PredicateImplLite is optimized for batch eviction process. Unlike PredicateImpl, it has
// no synchronization primitives such as AtomicU64.
struct PredicateImplLite<K, V> {
    f: PredicateFun<K, V>,
    registered_at: Instant,
    // The oldest timestamp of the entries checked by this predicate.
    oldest: Instant,
    // The newest timestamp of the entries checked by this predicate.
    newest: Instant,
}

impl<K, V> PredicateImplLite<K, V> {
    fn new(pred: &PredicateImpl<K, V>) -> Self {
        Self {
            f: Arc::clone(&pred.f),
            registered_at: pred.registered_at,
            oldest: unsafe { std::mem::transmute(pred.oldest.load(Ordering::Acquire)) },
            newest: unsafe { std::mem::transmute(pred.newest.load(Ordering::Acquire)) },
        }
    }
}

impl<K, V> Predicate<K, V> for PredicateImplLite<K, V> {
    fn is_applicable(&self, last_modified: Instant) -> bool {
        last_modified <= self.registered_at
            && (last_modified < self.oldest || last_modified > self.newest)
    }

    fn apply(&self, key: &K, value: &V) -> bool {
        (self.f)(key, value)
    }
}

enum CacheEntry<K> {
    // The timestamp of the first entry of a scan.
    Begin(u64),
    // Entry's key
    Item(Arc<K>),
    // The timestamp of the last entry of a scan.
    End(u64),
}

#[derive(thiserror::Error, Debug)]
pub enum PredicateRegistrationError {
    #[error("The write-order queue is disabled. Please enable it using the builder at the cache creation time")]
    WriteOrderQueueDisabled,
    #[error("No space left in the predicate registry")]
    NoSpaceLeft,
}
