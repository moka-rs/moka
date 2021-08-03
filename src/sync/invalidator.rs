#![allow(unused)]

use crate::{
    common::{
        thread_pool::{PoolName, ThreadPool, ThreadPoolRegistry},
        unsafe_weak_pointer::UnsafeWeakPointer,
        AccessTime,
    },
    PredicateError,
};

use super::{base_cache::Inner, PredicateId, PredicateIdStr, ValueEntry};

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
    time::Duration,
};
use uuid::Uuid;

pub(crate) type PredicateFun<K, V> = Arc<dyn Fn(&K, &V) -> bool + Send + Sync + 'static>;

pub(crate) trait GetOrRemoveEntry<K, V> {
    fn get_value_entry(&self, key: &Arc<K>) -> Option<Arc<ValueEntry<K, V>>>;

    fn remove_key_value_if<F>(&self, key: &Arc<K>, condition: F) -> Option<Arc<ValueEntry<K, V>>>
    where
        F: FnMut(&Arc<K>, &Arc<ValueEntry<K, V>>) -> bool;
}

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

pub(crate) struct InvalidationResult<K, V> {
    pub(crate) invalidated: Vec<Arc<ValueEntry<K, V>>>,
    pub(crate) is_done: bool,
}

impl<K, V> InvalidationResult<K, V> {
    fn new(invalidated: Vec<Arc<ValueEntry<K, V>>>, is_done: bool) -> Self {
        Self {
            invalidated,
            is_done,
        }
    }
}

pub(crate) struct Invalidator<K, V, S> {
    predicates: RwLock<HashMap<PredicateId, Predicate<K, V>>>,
    is_empty: AtomicBool,
    scan_context: Arc<ScanContext<K, V, S>>,
    thread_pool: Arc<ThreadPool>,
}

impl<K, V, S> Drop for Invalidator<K, V, S> {
    fn drop(&mut self) {
        let ctx = &self.scan_context;
        // Disallow to create and run a scanning task by now.
        ctx.is_shutting_down.store(true, Ordering::Release);

        // Wait for the scanning task to finish. (busy loop)
        while ctx.is_running.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }

        ThreadPoolRegistry::release_pool(&self.thread_pool);
    }
}

//
// Crate public methods.
//
impl<K, V, S> Invalidator<K, V, S> {
    pub(crate) fn new(cache: Weak<Inner<K, V, S>>) -> Self {
        let thread_pool = ThreadPoolRegistry::acquire_pool(PoolName::Invalidator);
        Self {
            predicates: RwLock::new(HashMap::new()),
            is_empty: AtomicBool::new(true),
            scan_context: Arc::new(ScanContext::new(cache)),
            thread_pool,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.is_empty.load(Ordering::Acquire)
    }

    pub(crate) fn remove_predicates_registered_before(&self, ts: Instant) {
        let mut pred_map = self.predicates.write();

        let removing_ids = pred_map
            .iter()
            .filter(|(_, pred)| pred.registered_at <= ts)
            .map(|(id, _)| id)
            .cloned()
            .collect::<Vec<_>>();

        for id in removing_ids {
            pred_map.remove(&id);
        }

        if pred_map.is_empty() {
            self.is_empty.store(true, Ordering::Release);
        }
    }

    pub(crate) fn register_predicate(
        &self,
        predicate: PredicateFun<K, V>,
        registered_at: Instant,
    ) -> Result<PredicateId, PredicateError> {
        const MAX_RETRY: usize = 1_000;
        let mut tries = 0;
        let mut preds = self.predicates.write();

        while tries < MAX_RETRY {
            let id = Uuid::new_v4().to_hyphenated().to_string();
            if preds.contains_key(&id) {
                tries += 1;

                continue; // Retry
            }
            let pred = Predicate::new(&id, predicate, registered_at);
            preds.insert(id.clone(), pred);
            self.is_empty.store(false, Ordering::Release);

            return Ok(id);
        }

        // Since we are using 128-bit UUID for the ID and we do retries for MAX_RETRY
        // times, this panic should extremely unlikely occur (unless there is a bug in
        // UUID generation).
        panic!("Cannot assign a new PredicateId to a predicate");
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
        self.scan_context.is_running.load(Ordering::Acquire)
    }

    pub(crate) fn submit_task(&self, candidates: Vec<KeyDateLite<K>>, is_truncated: bool)
    where
        K: Hash + Eq + Send + Sync + 'static,
        V: Send + Sync + 'static,
        S: BuildHasher + Send + Sync + 'static,
    {
        let ctx = &self.scan_context;

        // Do not submit a task if this invalidator is about to be dropped.
        if ctx.is_shutting_down.load(Ordering::Acquire) {
            return;
        }

        // Ensure there is no pending task and result.
        assert!(!self.is_task_running());
        assert!(ctx.result.lock().is_none());

        // Populate ctx.predicates if it is empty.
        {
            let mut ps = ctx.predicates.lock();
            if ps.is_empty() {
                *ps = self.predicates.read().values().cloned().collect();
            }
        }

        self.scan_context.is_running.store(true, Ordering::Release);

        let task = ScanTask::new(&self.scan_context, candidates, is_truncated);
        self.thread_pool.pool.execute(move || {
            task.execute();
        });
    }

    pub(crate) fn task_result(&self) -> Option<InvalidationResult<K, V>> {
        assert!(!self.is_task_running());
        let ctx = &self.scan_context;

        ctx.result.lock().take().map(|result| {
            self.remove_finished_predicates(ctx, &result);
            let is_done = ctx.predicates.lock().is_empty();
            InvalidationResult::new(result.invalidated, is_done)
        })
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

    fn remove_finished_predicates(&self, ctx: &ScanContext<K, V, S>, result: &ScanResult<K, V>) {
        let mut predicates = ctx.predicates.lock();

        if result.is_truncated {
            if let Some(ts) = result.newest_timestamp {
                let (active, finished): (Vec<_>, Vec<_>) =
                    predicates.drain(..).partition(|p| p.is_applicable(ts));

                // Remove finished predicates from the predicate registry.
                self.remove_predicates(&finished);
                // Set the active predicates to the scan context.
                *predicates = active;
            }
        } else {
            // Remove all the predicates from the predicate registry and scan context.
            self.remove_predicates(&predicates);
            predicates.clear();
        }
    }

    fn remove_predicates(&self, predicates: &[Predicate<K, V>]) {
        let mut pred_map = self.predicates.write();
        predicates.iter().for_each(|p| {
            pred_map.remove(p.id());
        });
        if pred_map.is_empty() {
            self.is_empty.store(true, Ordering::Release);
        }
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

struct ScanContext<K, V, S> {
    predicates: Mutex<Vec<Predicate<K, V>>>,
    cache: Mutex<UnsafeWeakPointer>,
    result: Mutex<Option<ScanResult<K, V>>>,
    is_running: AtomicBool,
    is_shutting_down: AtomicBool,
    _marker: PhantomData<S>,
}

impl<K, V, S> ScanContext<K, V, S> {
    fn new(cache: Weak<Inner<K, V, S>>) -> Self {
        Self {
            predicates: Mutex::new(Vec::default()),
            cache: Mutex::new(UnsafeWeakPointer::from_weak_arc(cache)),
            result: Mutex::new(None),
            is_running: AtomicBool::new(false),
            is_shutting_down: AtomicBool::new(false),
            _marker: PhantomData::default(),
        }
    }
}

struct Predicate<K, V> {
    id: PredicateId,
    f: PredicateFun<K, V>,
    registered_at: Instant,
}

impl<K, V> Clone for Predicate<K, V> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            f: Arc::clone(&self.f),
            registered_at: self.registered_at,
        }
    }
}

impl<K, V> Predicate<K, V> {
    fn new(id: PredicateIdStr<'_>, f: PredicateFun<K, V>, registered_at: Instant) -> Self {
        Self {
            id: id.to_string(),
            f,
            registered_at,
        }
    }

    fn id(&self) -> PredicateIdStr<'_> {
        &self.id
    }

    fn is_applicable(&self, last_modified: Instant) -> bool {
        last_modified <= self.registered_at
    }

    fn apply(&self, key: &K, value: &V) -> bool {
        (self.f)(key, value)
    }
}

struct ScanTask<K, V, S> {
    scan_context: Arc<ScanContext<K, V, S>>,
    candidates: Vec<KeyDateLite<K>>,
    is_truncated: bool,
}

impl<K, V, S> ScanTask<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher,
{
    fn new(
        scan_context: &Arc<ScanContext<K, V, S>>,
        candidates: Vec<KeyDateLite<K>>,
        is_truncated: bool,
    ) -> Self {
        Self {
            scan_context: Arc::clone(scan_context),
            candidates,
            is_truncated,
        }
    }

    fn execute(&self) {
        let cache_lock = self.scan_context.cache.lock();

        // Restore the Weak pointer to Inner<K, V, S>.
        let weak = unsafe { cache_lock.as_weak_arc::<Inner<K, V, S>>() };
        if let Some(inner_cache) = weak.upgrade() {
            // TODO: Protect this call with catch_unwind().
            *self.scan_context.result.lock() = Some(self.do_execute(&inner_cache));

            // Change this flag here (before downgrading the Arc to a Weak) to avoid a (soft)
            // deadlock. (forget_arc might trigger to drop the cache, which is in turn to drop
            // this invalidator. To do it, this flag must be false, otherwise dropping self
            // will be blocked forever)
            self.scan_context.is_running.store(false, Ordering::Release);
            // Avoid to drop the Arc<Inner<K, V, S>>.
            UnsafeWeakPointer::forget_arc(inner_cache);
        } else {
            *self.scan_context.result.lock() = Some(ScanResult::default());
            self.scan_context.is_running.store(false, Ordering::Release);
            // Avoid to drop the Weak<Inner<K, V, S>>.
            UnsafeWeakPointer::forget_weak_arc(weak);
        }
    }

    fn do_execute<C>(&self, cache: &Arc<C>) -> ScanResult<K, V>
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

        ScanResult {
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

struct ScanResult<K, V> {
    invalidated: Vec<Arc<ValueEntry<K, V>>>,
    is_truncated: bool,
    newest_timestamp: Option<Instant>,
}

impl<K, V> Default for ScanResult<K, V> {
    fn default() -> Self {
        Self {
            invalidated: Vec::default(),
            is_truncated: false,
            newest_timestamp: None,
        }
    }
}
