#![allow(unused)]

use crate::common::{
    thread_pool::{PoolName, ThreadPool, ThreadPoolRegistry},
    unsafe_weak_pointer::UnsafeWeakPointer,
    AccessTime,
};

use super::{base_cache::Inner, ValueEntry};

use parking_lot::{Mutex, RwLock};
use quanta::Instant;
use std::{
    collections::HashMap,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Weak,
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

pub(crate) type PredicateFun<K, V> = Arc<dyn Fn(&K, &V) -> bool + Send + Sync + 'static>;

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

pub(crate) struct Invalidator<K, V, S> {
    predicates: RwLock<HashMap<u64, Predicate<K, V>>>,
    is_empty: AtomicBool,
    last_key: AtomicU64,
    scan_context: ScanContext<K, V>,
    task_result: Arc<Mutex<Option<InternalInvalidationResult<K, V>>>>,
    is_task_running: Arc<AtomicBool>,
    thread_pool: Arc<ThreadPool>,
    cache: Arc<Mutex<UnsafeWeakPointer>>,
    _marker: PhantomData<S>,
}

impl<K, V, S> Drop for Invalidator<K, V, S> {
    fn drop(&mut self) {
        ThreadPoolRegistry::release_pool(&self.thread_pool);
    }
}

//
// Crate public methods.
//
impl<K, V, S> Invalidator<K, V, S> {
    pub(crate) fn new(cache: Weak<Inner<K, V, S>>) -> Self {
        let thread_pool = ThreadPoolRegistry::acquire_pool(PoolName::Invalidator);
        let cache = Arc::new(Mutex::new(UnsafeWeakPointer::from_weak_arc(cache)));

        Self {
            predicates: RwLock::new(HashMap::new()),
            is_empty: AtomicBool::new(true),
            last_key: AtomicU64::new(std::u64::MAX),
            scan_context: ScanContext::default(),
            task_result: Arc::new(Mutex::new(None)),
            is_task_running: Arc::new(AtomicBool::new(false)),
            thread_pool,
            cache,
            _marker: PhantomData::default(),
        }
    }

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
            let pred = Predicate::new(id, predicate, registered_at);
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

    pub(crate) fn is_task_running(&self) -> bool {
        self.is_task_running.load(Ordering::Acquire)
    }

    pub(crate) fn submit_task(&self, candidates: Vec<KeyDateLite<K>>, is_truncated: bool)
    where
        K: Hash + Eq + Send + Sync + 'static,
        V: Send + Sync + 'static,
        S: BuildHasher + Send + 'static,
    {
        // Ensure there is no pending task and result.
        assert!(!self.is_task_running());
        assert!(self.task_result.lock().is_none());

        {
            let mut ctx_predicates = self.scan_context.predicates.lock();
            if ctx_predicates.is_empty() {
                let predicates = self.predicates.read().values().cloned().collect();
                *ctx_predicates = predicates;
            }
        }

        let task = InvalidationTask {
            cache: Arc::clone(&self.cache),
            scan_context: self.scan_context.clone(),
            candidates,
            is_truncated,
            task_result: Arc::clone(&self.task_result),
            is_running: Arc::clone(&self.is_task_running),
            _marker: PhantomData::<S>::default(),
        };

        self.is_task_running.store(true, Ordering::Release);

        self.thread_pool.pool.execute(move || {
            task.execute();
        });
    }

    pub(crate) fn task_result(&self) -> Option<InvalidationResult<K, V>> {
        assert!(!self.is_task_running());
        match self.task_result.lock().take() {
            Some(InternalInvalidationResult {
                invalidated,
                is_truncated,
                newest_timestamp,
            }) => {
                let mut is_done = false;
                let mut predicates = self.scan_context.predicates.lock();

                if is_truncated {
                    if let Some(ts) = newest_timestamp {
                        // Remove the predicates from the predicate registry.
                        for p in &*predicates {
                            if !p.is_applicable(ts) {
                                self.remove_predicate(p.id());
                            }
                        }

                        // Remove the predicates from the scan context.
                        let ps = predicates
                            .drain(..)
                            .filter(|p| p.is_applicable(ts))
                            .collect();
                        *predicates = ps;
                    }
                    is_done = predicates.is_empty();
                } else {
                    // Remove the predicates from the predicate registry.
                    for p in &*predicates {
                        self.remove_predicate(p.id());
                    }

                    // Clear the predicates in the scan context.
                    predicates.clear();
                    is_done = true;
                }

                Some(InvalidationResult {
                    invalidated,
                    is_done,
                })
            }
            None => None,
        }
    }
}

//
// Private methods.
//
impl<K, V, S> Invalidator<K, V, S> {
    #[inline]
    fn do_apply_predicates<'a, I>(predicates: I, key: &'a K, value: &'a V, ts: Instant) -> bool
    where
        I: Iterator<Item = &'a Predicate<K, V>>,
    {
        for predicate in predicates {
            if predicate.is_applicable(ts) && predicate.apply(key, value) {
                return true;
            }
        }
        false
    }
}

//
// for testing
//
#[cfg(test)]
impl<K, V, S> Invalidator<K, V, S> {
    pub(crate) fn predicate_count(&self) -> usize {
        self.predicates.read().len()
    }
}

struct ScanContext<K, V> {
    predicates: Arc<Mutex<Vec<Predicate<K, V>>>>,
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

struct Predicate<K, V> {
    id: u64,
    f: PredicateFun<K, V>,
    registered_at: Instant,
}

impl<K, V> Clone for Predicate<K, V> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            f: Arc::clone(&self.f),
            registered_at: self.registered_at,
        }
    }
}

impl<K, V> Predicate<K, V> {
    fn new(id: u64, f: PredicateFun<K, V>, registered_at: Instant) -> Self {
        Self {
            id,
            f,
            registered_at,
        }
    }

    fn id(&self) -> u64 {
        self.id
    }

    fn is_applicable(&self, last_modified: Instant) -> bool {
        last_modified <= self.registered_at
    }

    fn apply(&self, key: &K, value: &V) -> bool {
        (self.f)(key, value)
    }
}

pub(crate) trait GetOrRemoveEntry<K, V> {
    fn get_value_entry(&self, key: &Arc<K>) -> Option<Arc<ValueEntry<K, V>>>;

    fn remove_key_value_if<F>(&self, key: &Arc<K>, condition: F) -> Option<Arc<ValueEntry<K, V>>>
    where
        F: FnMut(&Arc<K>, &Arc<ValueEntry<K, V>>) -> bool;
}

struct InvalidationTask<K, V, S> {
    cache: Arc<Mutex<UnsafeWeakPointer>>,
    scan_context: ScanContext<K, V>,
    candidates: Vec<KeyDateLite<K>>,
    is_truncated: bool,
    task_result: Arc<Mutex<Option<InternalInvalidationResult<K, V>>>>,
    is_running: Arc<AtomicBool>,
    _marker: PhantomData<S>,
}

impl<K, V, S> InvalidationTask<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher,
{
    fn execute(&self) {
        let result;
        let lock = self.cache.lock();
        // Restore the Weak pointer to Inner<K, V, S>.
        let weak = unsafe { lock.as_weak_arc::<Inner<K, V, S>>() };
        if let Some(inner_cache) = weak.upgrade() {
            // TODO: Protect this call with catch_unwind().
            result = self.do_execute(&inner_cache);
            // Avoid to drop the Arc<Inner<K, V, S>>.
            UnsafeWeakPointer::forget_arc(inner_cache);
        } else {
            result = InternalInvalidationResult::default();
            // Avoid to drop the Weak<Inner<K, V, S>>.
            UnsafeWeakPointer::forget_weak_arc(weak);
        }

        *self.task_result.lock() = Some(result);
        self.is_running.store(false, Ordering::Release);
    }

    fn do_execute<C>(&self, cache: &Arc<C>) -> InternalInvalidationResult<K, V>
    where
        Arc<C>: GetOrRemoveEntry<K, V>,
    {
        let predicates = self.scan_context.predicates.lock();
        let mut invalidated = Vec::default();
        let mut newest_timestamp = None;

        for candidate in &self.candidates {
            let key = &candidate.key;
            let ts = candidate.timestamp;
            if Self::apply(&predicates, cache, key, ts) {
                if let Some(entry) = Self::invalidate(cache, key, ts) {
                    invalidated.push(entry)
                }
            }
            newest_timestamp = Some(ts);
        }

        InternalInvalidationResult {
            invalidated,
            is_truncated: self.is_truncated,
            newest_timestamp,
        }
    }

    fn apply<C>(predicates: &[Predicate<K, V>], cache: &Arc<C>, key: &Arc<K>, ts: Instant) -> bool
    where
        Arc<C>: GetOrRemoveEntry<K, V>,
    {
        if let Some(entry) = cache.get_value_entry(key) {
            if let Some(lm) = entry.last_modified() {
                if lm == ts {
                    return Invalidator::<_, _, S>::do_apply_predicates(
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

    fn invalidate<C>(cache: &Arc<C>, key: &Arc<K>, ts: Instant) -> Option<Arc<ValueEntry<K, V>>>
    where
        Arc<C>: GetOrRemoveEntry<K, V>,
    {
        cache.remove_key_value_if(key, |_, v| {
            if let Some(lm) = v.last_modified() {
                lm == ts
            } else {
                false
            }
        })
    }
}

pub(crate) struct InvalidationResult<K, V> {
    pub(crate) invalidated: Vec<Arc<ValueEntry<K, V>>>,
    pub(crate) is_done: bool,
}

struct InternalInvalidationResult<K, V> {
    invalidated: Vec<Arc<ValueEntry<K, V>>>,
    is_truncated: bool,
    newest_timestamp: Option<Instant>,
}

impl<K, V> Default for InternalInvalidationResult<K, V> {
    fn default() -> Self {
        Self {
            invalidated: Vec::default(),
            is_truncated: false,
            newest_timestamp: None,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum PredicateRegistrationError {
    #[error("The write-order queue is disabled. Please enable it using the builder at the cache creation time")]
    WriteOrderQueueDisabled,
    #[error("No space left in the predicate registry")]
    NoSpaceLeft,
}
