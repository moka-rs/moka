use crate::{
    deque::{CacheRegion, DeqNode, Deque},
    thread_pool::{ThreadPool, ThreadPoolRegistry},
    ConcurrentCache,
};

use count_min_sketch::CountMinSketch8;
use crossbeam_channel::{Receiver, SendError, Sender};
use parking_lot::{Mutex, RwLock};
use scheduled_thread_pool::JobHandle;
use std::{
    cell::UnsafeCell,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    time::Duration,
};

const READ_LOG_SIZE: usize = 64;
const WRITE_LOG_SIZE: usize = 256;
const READ_LOG_HIGH_WATER_MARK: usize = 48; // 75% of READ_LOG_SIZE
const WRITE_LOG_HIGH_WATER_MARK: usize = 128; // 50% of WRITE_LOG_SIZE

pub struct LFUCache<K, V, S> {
    inner: Arc<Inner<K, V, S>>,
    read_op_ch: Sender<ReadOp<K, V>>,
    write_op_ch: Sender<WriteOp<K, V>>,
    housekeeper: Arc<Housekeeper<K, V, S>>,
}

unsafe impl<K, V, S> Send for LFUCache<K, V, S>
where
    K: Send + Sync,
    V: Send + Sync,
    S: Send,
{
}

unsafe impl<K, V, S> Sync for LFUCache<K, V, S>
where
    K: Send + Sync,
    V: Send + Sync,
    S: Sync,
{
}

impl<K, V, S> Clone for LFUCache<K, V, S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            read_op_ch: self.read_op_ch.clone(),
            write_op_ch: self.write_op_ch.clone(),
            housekeeper: Arc::clone(&self.housekeeper),
        }
    }
}

impl<K, V> LFUCache<K, V, std::collections::hash_map::RandomState>
where
    K: Clone + Eq + Hash,
{
    // TODO: Instead of taking capacity, take initial_capacity and max_capacity.
    pub fn new(capacity: usize) -> Self {
        let build_hasher = std::collections::hash_map::RandomState::default();
        Self::with_hasher(capacity, build_hasher)
    }
}

impl<K, V, S> LFUCache<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    // TODO: Instead of taking capacity, take initial_capacity and max_capacity.
    pub fn with_hasher(capacity: usize, build_hasher: S) -> Self {
        let (r_snd, r_rcv) = crossbeam_channel::bounded(READ_LOG_SIZE);
        let (w_snd, w_rcv) = crossbeam_channel::bounded(WRITE_LOG_SIZE);
        let inner = Arc::new(Inner::new(capacity, build_hasher, r_rcv, w_rcv));
        let housekeeper = Housekeeper::new(Arc::downgrade(&inner));

        Self {
            inner: Arc::clone(&inner),
            read_op_ch: r_snd,
            write_op_ch: w_snd,
            housekeeper: Arc::new(housekeeper),
        }
    }

    /// This is used by unit tests to get consistent result.
    #[allow(dead_code)]
    pub(crate) fn reconfigure_for_testing(&mut self) {
        // Stop the housekeeping job that may cause sync() method to return earlier.
        let mut job = self.housekeeper.job.lock();
        if let Some(job) = job.take() {
            job.cancel();
        }
    }
}

impl<K, V, S> ConcurrentCache<K, V> for LFUCache<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    fn get(&self, key: &K) -> Option<Arc<V>> {
        if let Some(entry) = self.inner.get(key) {
            let v = Arc::clone(&entry.value);
            self.record_read_op(key, Some(entry))
                .expect("Failed to record a get op");
            Some(v)
        } else {
            self.record_read_op(key, None)
                .expect("Failed to record a get op");
            None
        }
    }

    // fn get_or_insert(&self, _key: K, _default: V) -> Arc<V> {
    //     todo!()
    // }

    // fn get_or_insert_with<F>(&self, _key: K, _default: F) -> Arc<V>
    // where
    //     F: FnOnce() -> V,
    // {
    //     todo!()
    // }

    fn insert(&self, key: K, value: V) {
        let key = Arc::new(key);
        let value = Arc::new(value);
        let mut op1 = None;
        let mut op2 = None;

        self.inner.cache.insert_with_or_modify(
            Arc::clone(&key),
            // on_insert
            || {
                let entry = Arc::new(ValueEntry::new(Arc::clone(&value)));
                op1 = Some(WriteOp::Insert(key, entry.clone()));
                entry
            },
            // on_modify
            |_k, entry| {
                let entry = Arc::new(entry.replaced(Arc::clone(&value)));
                op2 = Some(WriteOp::Update(entry.clone()));
                entry
            },
        );

        match (op1, op2) {
            (Some(op), None) => self.schedule_insert_op(op).expect("Failed to insert"),
            (None, Some(op)) => self.schedule_insert_op(op).expect("Failed to insert"),
            _ => unreachable!(),
        }
    }

    fn remove(&self, key: &K) -> Option<Arc<V>> {
        self.inner.cache.remove(key).map(|entry| {
            let value = Arc::clone(&entry.value);
            self.schedule_remove_op(entry).expect("Failed to remove");
            value
        })
    }

    fn sync(&self) {
        self.inner.sync();
    }
}

// private methods
impl<K, V, S> LFUCache<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    #[inline]
    fn record_read_op(
        &self,
        key: &K,
        entry: Option<Arc<ValueEntry<K, V>>>,
    ) -> Result<(), SendError<K>> {
        use ReadOp::*;
        let ch = &self.read_op_ch;
        if let Some(entry) = entry {
            let _ = ch.try_send(Hit(key.clone(), entry)); // Ignore Result<_, _>.
        } else {
            let _ = ch.try_send(Miss(key.clone())); // Ignore Result<_, _>.
        }
        self.apply_reads_if_needed();
        Ok(())
    }

    #[inline]
    fn schedule_insert_op(&self, op: WriteOp<K, V>) -> Result<(), SendError<WriteOp<K, V>>> {
        let ch = &self.write_op_ch;
        // NOTE: This will be blocked if the channel is full.
        ch.send(op)?;
        self.apply_reads_writes_if_needed();
        Ok(())
    }

    #[inline]
    fn schedule_remove_op(
        &self,
        entry: Arc<ValueEntry<K, V>>,
    ) -> Result<(), SendError<WriteOp<K, V>>> {
        let ch = &self.write_op_ch;
        // NOTE: This will be blocked if the channel is full.
        ch.send(WriteOp::Remove(entry))?;
        self.apply_reads_writes_if_needed();
        Ok(())
    }

    #[inline]
    fn apply_reads_if_needed(&self) {
        let len = self.read_op_ch.len();

        if self.should_apply_reads(len) {
            // if let Some(ref mut deqs) = self.inner.deques.try_lock() {
            //     self.inner.apply_reads(deqs, len);
            // }
            self.housekeeper.try_schedule_to_sync();
        }
    }

    #[inline]
    fn apply_reads_writes_if_needed(&self) {
        let w_len = self.write_op_ch.len();

        if self.should_apply_writes(w_len) {
            // let r_len = self.read_op_ch.len();
            // if let Some(ref mut deqs) = self.inner.deques.try_lock() {
            //     self.inner.apply_reads(deqs, r_len);
            //     self.inner.apply_writes(deqs, w_len);
            // }
            self.housekeeper.try_schedule_to_sync();
        }
    }

    #[inline]
    fn should_apply_reads(&self, ch_len: usize) -> bool {
        ch_len >= READ_LOG_HIGH_WATER_MARK
    }

    #[inline]
    fn should_apply_writes(&self, ch_len: usize) -> bool {
        ch_len >= WRITE_LOG_HIGH_WATER_MARK
    }
}

enum ReadOp<K, V> {
    Hit(K, Arc<ValueEntry<K, V>>),
    Miss(K),
}

enum WriteOp<K, V> {
    Insert(Arc<K>, Arc<ValueEntry<K, V>>),
    Update(Arc<ValueEntry<K, V>>),
    Remove(Arc<ValueEntry<K, V>>),
}

struct ValueEntry<K, V> {
    pub(crate) value: Arc<V>,
    pub(crate) deq_node: UnsafeCell<Option<NonNull<DeqNode<K>>>>,
}

impl<K, V> ValueEntry<K, V> {
    fn new(value: Arc<V>) -> Self {
        Self {
            value,
            deq_node: UnsafeCell::new(None),
        }
    }

    fn replaced(&self, value: Arc<V>) -> Self {
        Self {
            value,
            deq_node: UnsafeCell::new(unsafe { *self.deq_node.get() }),
        }
    }
}

type Cache<K, V, S> = cht::HashMap<Arc<K>, Arc<ValueEntry<K, V>>, S>;

struct Inner<K, V, S> {
    capacity: usize,
    cache: Cache<K, V, S>,
    deques: Mutex<Deques<K>>,
    frequency_sketch: RwLock<CountMinSketch8<K>>,
    read_op_ch: Receiver<ReadOp<K, V>>,
    write_op_ch: Receiver<WriteOp<K, V>>,
}

// functions/methods used by LFUCache
impl<K, V, S> Inner<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    fn new(
        capacity: usize,
        build_hasher: S,
        read_op_ch: Receiver<ReadOp<K, V>>,
        write_op_ch: Receiver<WriteOp<K, V>>,
    ) -> Self {
        let cache = cht::HashMap::with_capacity_and_hasher(capacity, build_hasher);
        let skt_capacity = usize::max(capacity, 100);
        let frequency_sketch = CountMinSketch8::new(skt_capacity, 0.95, 10.0)
            .expect("Failed to create the frequency sketch");

        Self {
            capacity,
            cache,
            deques: Mutex::new(Deques::default()),
            frequency_sketch: RwLock::new(frequency_sketch),
            read_op_ch,
            write_op_ch,
        }
    }

    #[inline]
    fn get(&self, key: &K) -> Option<Arc<ValueEntry<K, V>>> {
        self.cache.get(key)
    }

    fn apply_reads(&self, deqs: &mut Deques<K>, count: usize) {
        use ReadOp::*;
        let mut freq = self.frequency_sketch.write();
        let ch = &self.read_op_ch;
        for _ in 0..count {
            match ch.try_recv() {
                Ok(Hit(key, entry)) => {
                    freq.increment(&key);
                    deqs.move_to_back(entry)
                }
                Ok(Miss(key)) => freq.increment(&key),
                Err(_) => break,
            }
        }
    }

    fn apply_writes(&self, deqs: &mut Deques<K>, count: usize) {
        use WriteOp::*;
        let freq = self.frequency_sketch.read();
        let ch = &self.write_op_ch;
        for _ in 0..count {
            match ch.try_recv() {
                Ok(Insert(key, entry)) => self.handle_insert(key, entry, deqs, &freq),
                Ok(Update(entry)) => deqs.move_to_back(entry),
                Ok(Remove(entry)) => deqs.unlink(entry),
                Err(_) => break,
            };
        }
    }

    fn sync(&self) {
        let r_len = self.read_op_ch.len();
        if r_len > 0 {
            let mut deqs = self.deques.lock();
            self.apply_reads(&mut deqs, r_len);
        }

        let l_len = self.write_op_ch.len();
        if l_len > 0 {
            let mut deqs = self.deques.lock();
            self.apply_writes(&mut deqs, l_len);
        }
    }
}

// private methods
impl<K, V, S> Inner<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    #[inline]
    fn admit(&self, candidate: &K, victim: &DeqNode<K>, freq: &CountMinSketch8<K>) -> bool {
        // TODO: Implement some randomness to mitigate hash DoS attack.
        // See Caffeine's implementation.
        freq.estimate(candidate) > freq.estimate(&*victim.element)
    }

    #[inline]
    fn find_cache_victim<'a>(
        &self,
        deqs: &'a mut Deques<K>,
        _freq: &CountMinSketch8<K>,
    ) -> &'a DeqNode<K> {
        // TODO: Check its frequency. If it is not very low, maybe we should
        // check frequencies of next few others and pick from them.
        deqs.probation.peek_front().expect("No victim found")
    }

    #[inline]
    fn handle_insert(
        &self,
        key: Arc<K>,
        entry: Arc<ValueEntry<K, V>>,
        deqs: &mut Deques<K>,
        freq: &CountMinSketch8<K>,
    ) {
        if self.cache.len() <= self.capacity {
            // Add the candidate to the deque.
            deqs.push_back(CacheRegion::MainProbation, key, &entry);
        } else {
            let victim = self.find_cache_victim(deqs, freq);
            if self.admit(&key, victim, freq) {
                // Remove the victim from the cache and deque.
                //
                // TODO: Check if the selected victim was actually removed. If not,
                // maybe we should find another victim. This can happen because it
                // could have been already removed from the cache but the removal
                // from the deque is still on the write operations queue and is not
                // yet executed.
                self.cache.remove(&victim.element);
                let victim = NonNull::from(victim);
                deqs.unlink_node(victim);
                // Add the candidate to the deque.
                deqs.push_back(CacheRegion::MainProbation, key, &entry);
            } else {
                // Remove the candidate from the cache.
                self.cache.remove(&key);
            }
        }
    }
}

struct Deques<K> {
    window: Deque<K>, //    Not yet used.
    probation: Deque<K>,
    protected: Deque<K>, // Not yet used.
}

impl<K> Default for Deques<K> {
    fn default() -> Self {
        Self {
            window: Deque::new(CacheRegion::Window),
            probation: Deque::new(CacheRegion::MainProbation),
            protected: Deque::new(CacheRegion::MainProtected),
        }
    }
}

impl<K> Deques<K> {
    fn push_back<V>(&mut self, region: CacheRegion, key: Arc<K>, entry: &Arc<ValueEntry<K, V>>) {
        use CacheRegion::*;
        let node = Box::new(DeqNode::new(region, key));
        let node = match node.as_ref().region {
            Window => self.window.push_back(node),
            MainProbation => self.probation.push_back(node),
            MainProtected => self.protected.push_back(node),
        };
        unsafe { *(entry.deq_node.get()) = Some(node) };
    }

    fn move_to_back<V>(&mut self, entry: Arc<ValueEntry<K, V>>) {
        use CacheRegion::*;
        unsafe {
            if let Some(node) = *entry.deq_node.get() {
                match node.as_ref().region {
                    Window => self.window.move_to_back(node),
                    MainProbation => self.probation.move_to_back(node),
                    MainProtected => self.protected.move_to_back(node),
                }
            }
        }
    }

    fn unlink<V>(&mut self, entry: Arc<ValueEntry<K, V>>) {
        unsafe {
            if let Some(node) = (*entry.deq_node.get()).take() {
                self.unlink_node(node);
            }
        }
    }

    fn unlink_node(&mut self, node: NonNull<DeqNode<K>>) {
        use CacheRegion::*;
        unsafe {
            match node.as_ref().region {
                Window => self.window.unlink(node),
                MainProbation => self.probation.unlink(node),
                MainProtected => self.protected.unlink(node),
            }
        }
    }
}

struct Housekeeper<K, V, S> {
    inner: Arc<Mutex<UnsafeWeakPointer>>,
    thread_pool: Arc<ThreadPool>,
    job: Mutex<Option<JobHandle>>,
    sync_scheduled: Arc<AtomicBool>,
    _marker: PhantomData<(K, V, S)>,
}

impl<K, V, S> Drop for Housekeeper<K, V, S> {
    fn drop(&mut self) {
        if let Some(j) = self.job.lock().as_ref() {
            j.cancel()
        }
        ThreadPoolRegistry::release_pool(&self.thread_pool);
        std::mem::drop(unsafe { self.inner.lock().as_weak_arc::<K, V, S>() });
    }
}

// functions/methods used by LFUCache
impl<K, V, S> Housekeeper<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    fn new(inner: Weak<Inner<K, V, S>>) -> Self {
        let thread_pool = ThreadPoolRegistry::acquire_default_pool();

        let inner_ptr = Arc::new(Mutex::new(UnsafeWeakPointer::from_weak_arc(inner)));
        let initial_delay = Duration::from_secs(1);
        let rate = Duration::from_millis(3017);

        // This clone will be moved into the housekeeper closure.
        let unsafe_weak_ptr = Arc::clone(&inner_ptr);

        let housekeeper_closure = move || {
            Self::call_sync(&unsafe_weak_ptr);
        };

        // Execute a task in a worker thread.
        let job = thread_pool
            .pool
            .execute_at_fixed_rate(initial_delay, rate, housekeeper_closure);

        Self {
            inner: inner_ptr,
            thread_pool,
            job: Mutex::new(Some(job)),
            sync_scheduled: Arc::new(AtomicBool::new(false)),
            _marker: PhantomData::default(),
        }
    }

    fn try_schedule_to_sync(&self) -> bool {
        // TODO: Check if the `Orderings` are correct.
        //
        // Try to flip the value of sync_scheduled from false to true.
        // compare_and_swap() (CAS) returns the previous value:
        // - if true  => this CAS operation has failed.    (true  -> unchanged)
        // - if false => this CAS operation has succeeded. (false -> true)
        let prev = self
            .sync_scheduled
            .compare_and_swap(false, true, Ordering::Acquire);

        if prev {
            false
        } else {
            let unsafe_weak_ptr = Arc::clone(&self.inner);
            let sync_scheduled = Arc::clone(&self.sync_scheduled);
            // Execute a task in a worker thread.
            self.thread_pool.pool.execute(move || {
                Self::call_sync(&unsafe_weak_ptr);
                sync_scheduled.store(false, Ordering::Release);
            });
            true
        }
    }
}

// private functions/methods
impl<K, V, S> Housekeeper<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    fn call_sync(unsafe_weak_ptr: &Arc<Mutex<UnsafeWeakPointer>>) {
        let lock = unsafe_weak_ptr.lock();
        // Restore the Weak pointer to Inner<K, V, S>.
        let weak = unsafe { lock.as_weak_arc::<K, V, S>() };
        if let Some(inner) = weak.upgrade() {
            // TODO: Protect this call with catch_unwind().
            inner.sync();
            // Avoid to drop the Arc<Inner<K, V, S>.
            UnsafeWeakPointer::forget_arc(inner);
        } else {
            // Avoid to drop the Weak<Inner<K, V, S>.
            UnsafeWeakPointer::forget_weak_arc(weak);
        }
    }
}

/// WARNING: Do not use this struct unless you are absolutely sure
/// what you are doing. Using this struct is unsafe and may cause
/// memory related crashes and/or security vulnerabilities.
///
/// This struct exists with the sole purpose of avoiding compile
/// errors relevant to the thread pool usages. The thread pool
/// requires that the generic parameters on the `LFUCache` and `Inner`
/// structs to have trait bounds `Send`, `Sync` and `'static`. This
/// will be unacceptable for many cache usages.
///
/// This struct avoids the trait bounds by transmuting a pointer
/// between `std::sync::Weak<Inner<K, V, S>>` and `usize`.
///
/// If you know a better solution than this, we would love te hear it.
struct UnsafeWeakPointer {
    // This is a std::sync::Weak pointer to Inner<K, V, S>.
    raw_ptr: usize,
}

impl UnsafeWeakPointer {
    fn from_weak_arc<K, V, S>(p: Weak<Inner<K, V, S>>) -> Self {
        Self {
            raw_ptr: unsafe { std::mem::transmute(p) },
        }
    }

    unsafe fn as_weak_arc<K, V, S>(&self) -> Weak<Inner<K, V, S>> {
        std::mem::transmute(self.raw_ptr)
    }

    fn forget_arc<K, V, S>(p: Arc<Inner<K, V, S>>) {
        // Downgrade the Arc to Weak, then forget.
        let weak = Arc::downgrade(&p);
        std::mem::forget(weak);
    }

    fn forget_weak_arc<K, V, S>(p: Weak<Inner<K, V, S>>) {
        std::mem::forget(p);
    }
}

/// `clone()` simply creates a copy of the `raw_ptr`, effectively
/// creating many copies of the same `Weak` pointer. We are doing this
/// for a good reason for our use case.
///
/// When you want to drop the Weak pointer, ensure that you drop it
/// only once for the same `raw_ptr` across clones.
impl Clone for UnsafeWeakPointer {
    fn clone(&self) -> Self {
        Self {
            raw_ptr: self.raw_ptr,
        }
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::{ConcurrentCache, LFUCache};
    use std::sync::Arc;

    #[test]
    fn basic_single_thread() {
        let mut cache = LFUCache::new(3);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("a", "alice");
        cache.insert("b", "bob");
        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        cache.sync();
        // counts: a -> 1, b -> 1

        cache.insert("c", "cindy");
        assert_eq!(cache.get(&"c"), Some(Arc::new("cindy")));
        // counts: a -> 1, b -> 1, c -> 1
        cache.sync();

        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        cache.sync();
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        cache.insert("d", "david"); //   count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //        d -> 1

        cache.insert("d", "david");
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //        d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher then c's.
        cache.insert("d", "dennis");
        cache.sync();
        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"d"), Some(Arc::new("dennis")));

        assert_eq!(cache.remove(&"b"), Some(Arc::new("bob")));
    }

    #[test]
    fn basic_multi_threads() {
        let mut cache = LFUCache::new(100);
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        let handles = (0..4)
            .map(|id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    cache.insert(10, format!("{}-100", id));
                    cache.get(&10);
                    cache.sync();
                    cache.insert(20, format!("{}-200", id));
                    cache.remove(&10);
                })
            })
            .collect::<Vec<_>>();

        handles.into_iter().for_each(|h| h.join().expect("Failed"));

        cache.sync();

        assert!(cache.get(&10).is_none());
        assert!(cache.get(&20).is_some());
    }
}
