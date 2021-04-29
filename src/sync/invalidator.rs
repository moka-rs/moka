#![allow(unused)]

use crate::common::{
    thread_pool::{PoolName, ThreadPool, ThreadPoolRegistry},
    unsafe_weak_pointer::UnsafeWeakPointer,
    AccessTime,
};

use super::ValueEntry;

// use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Mutex, RwLock};
use quanta::Instant;
use std::{collections::HashMap, hash::{BuildHasher, Hash}, sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    }};

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

type PredicateFun<K, V> = Arc<dyn Fn(&K, &V) -> bool + Send + Sync + 'static>;

pub(crate) struct KeyDateLite<K> {
    key: Arc<K>,
    timestamp: Instant,
}

impl<K> Clone for KeyDateLite<K> {
    fn clone(&self) -> Self {
        Self {
            key: Arc::clone(&self.key),
            timestamp: self.timestamp,
        }
    }
}

impl<K> KeyDateLite<K> {
    pub(crate) fn new(key: &Arc<K>, timestamp: Instant) -> Self {
        Self {
            key: Arc::clone(key),
            timestamp,
        }
    }
}

pub(crate) struct Invalidator<K, V> {
    predicates: RwLock<HashMap<u64, PredicateImpl<K, V>>>,
    is_empty: AtomicBool,
    last_key: AtomicU64,
    scan_context: ScanContext<K, V>,
    scan_result: Arc<Mutex<Option<InvalidationResult<K>>>>,
    thread_pool: Arc<ThreadPool>,
}

impl<K, V> Default for Invalidator<K, V> {
    fn default() -> Self {
        let thread_pool = ThreadPoolRegistry::acquire_pool(PoolName::Invalidator);

        Self {
            predicates: RwLock::new(HashMap::new()),
            is_empty: AtomicBool::new(true),
            last_key: AtomicU64::new(std::u64::MAX),
            scan_context: ScanContext::default(),
            scan_result: Arc::new(Mutex::new(None)),
            thread_pool,
        }
    }
}

impl<K, V> Drop for Invalidator<K, V> {
    fn drop(&mut self) {
        ThreadPoolRegistry::release_pool(&self.thread_pool);
    }
}

//
// Crate public methods.
//
impl<K, V> Invalidator<K, V> {
    pub(crate) fn is_empty(&self) -> bool {
        self.is_empty.load(Ordering::Acquire)
    }

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
    #[inline]
    pub(crate) fn apply_predicates(&self, key: &Arc<K>, entry: &Arc<ValueEntry<K, V>>) -> bool {
        if self.is_empty() {
            false
        } else if let Some(ts) = entry.last_modified() {
            Self::do_apply_predicates(self.predicates.read().values(), key, &entry.value, ts)
        } else {
            false
        }
    }

    pub(crate) fn submit_invalidation_task<C>(
        &self,
        cache: Arc<C>,
        candidates: Vec<KeyDateLite<K>>,
        is_truncated: bool,
    ) where
        K: Send + Sync + 'static,
        V: 'static,
        C: GetOrRemoveEntry<K, V> + Send + Sync + 'static,
    {
        // let _task = InvalidationTask {
        //     cache,
        //     scan_context: self.scan_context.clone(),
        //     candidates,
        //     is_truncated,
        // };
        // self.thread_pool.pool.execute(move || {_task.execute2(); });
        todo!()
    }
}

//
// Private methods.
//
impl<K, V> Invalidator<K, V> {
    #[inline]
    fn do_apply_predicates<'a, I, P>(predicates: I, key: &K, value: &V, ts: Instant) -> bool
    where
        I: Iterator<Item = &'a P>,
        P: Predicate<K, V> + 'a,
    {
        for predicate in predicates {
            if predicate.is_applicable(ts) && predicate.apply(key, value) {
                return true;
            }
        }
        false
    }
}

struct ScanContext<K, V> {
    predicates: Arc<Mutex<Vec<PredicateImplLite<K, V>>>>,
}

impl<K, V> Clone for ScanContext<K, V> {
    fn clone(&self) -> Self {
        Self {
            predicates: Arc::clone(&self.predicates),
        }
    }
}

impl<K, V> Default for ScanContext<K, V> {
    fn default() -> Self {
        Self {
            predicates: Arc::new(Mutex::new(Vec::default())),
        }
    }
}

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

// PredicateImplLite is optimized for batch invalidation. Unlike PredicateImpl, it has
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

pub(crate) trait GetOrRemoveEntry<K, V> {
    fn get_value_entry(&self, key: &Arc<K>) -> Option<Arc<ValueEntry<K, V>>>;

    fn remove_key_value_if<F>(&self, key: &Arc<K>, condition: F) -> bool
    where
        F: FnMut(&Arc<K>, &Arc<ValueEntry<K, V>>) -> bool;
}

// impl<K, V, S> GetOrRemoveEntry<K, V> for cht::SegmentedHashMap<Arc<K>, Arc<ValueEntry<K, V>>, S> 
// where K: Hash + Eq,
// S: BuildHasher,

// {
//     fn get_value_entry(&self, key: &Arc<K>) -> Option<Arc<ValueEntry<K, V>>> {
//         self.get(key)
//     }

//     fn remove_key_value_if<F>(&self, key: &Arc<K>, condition: F) -> bool
//     where
//         F: FnMut(&Arc<K>, &Arc<ValueEntry<K, V>>) -> bool {
//         self.remove_if(key, condition).is_some()
//     }
// }

struct InvalidationTask<C, K, V> {
    cache: Arc<C>,
    scan_context: ScanContext<K, V>,
    candidates: Vec<KeyDateLite<K>>,
    is_truncated: bool,
}

impl<'task, C, K, V> InvalidationTask<C, K, V>
where
    C: GetOrRemoveEntry<K, V>,
{
    // fn execute(unsafe_weak_ptr: UnsafeWeakPointer) {
    //     let task = unsafe { unsafe_weak_ptr.as_weak_arc::<Self>() };
    //     if let Some(task) = task.upgrade() {
    //         task.execute2();
    //     }
    // }

    fn execute(&self) -> InvalidationResult<K> {
        let predicates = self.scan_context.predicates.lock();
        let mut invalidated = Vec::default();

        for candidate in &self.candidates {
            let key = &candidate.key;
            let ts = candidate.timestamp;
            if Self::apply(&predicates, &*self.cache, key, ts)
                && Self::invalidate(&*self.cache, key, ts)
            {
                invalidated.push(candidate.clone())
            }
        }

        // TODO:
        let is_done = false;

        InvalidationResult {
            invalidated,
            is_done,
        }
    }

    fn apply(
        predicates: &[PredicateImplLite<K, V>],
        cache: &impl GetOrRemoveEntry<K, V>,
        key: &Arc<K>,
        ts: Instant,
    ) -> bool {
        if let Some(entry) = cache.get_value_entry(key) {
            if let Some(lm) = entry.last_modified() {
                if lm == ts {
                    return Invalidator::do_apply_predicates(
                        predicates.iter(),
                        key,
                        &entry.value,
                        lm,
                    );
                }
            }
        }

        false
    }

    fn invalidate(cache: &impl GetOrRemoveEntry<K, V>, key: &Arc<K>, ts: Instant) -> bool {
        cache.remove_key_value_if(key, |_, v| {
            if let Some(lm) = v.last_modified() {
                lm == ts
            } else {
                false
            }
        })
    }
}

struct InvalidationResult<K> {
    invalidated: Vec<KeyDateLite<K>>,
    is_done: bool,
}

#[derive(thiserror::Error, Debug)]
pub enum PredicateRegistrationError {
    #[error("The write-order queue is disabled. Please enable it using the builder at the cache creation time")]
    WriteOrderQueueDisabled,
    #[error("No space left in the predicate registry")]
    NoSpaceLeft,
}
