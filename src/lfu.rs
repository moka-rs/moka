use crate::{
    deque::{CacheArea, DeqNode, Deque, ValueEntry},
    ConcurrentCache,
};

use count_min_sketch::CountMinSketch8;
use crossbeam_channel::{Receiver, SendError, Sender};
use parking_lot::{Mutex, RwLock};
use std::{
    hash::{BuildHasher, Hash},
    ptr::NonNull,
    sync::Arc,
};

const READ_LOG_SIZE: usize = 64;
const WRITE_LOG_SIZE: usize = 256;
const READ_LOG_HIGH_WATER_MARK: usize = 48; // 75% of READ_LOG_SIZE
const WRITE_LOG_HIGH_WATER_MARK: usize = 128; // 50% of WRITE_LOG_SIZE

pub struct LFUCache<K, V, S> {
    inner: Arc<Inner<K, V, S>>,
    read_op_ch: Sender<ReadOp<K, V>>,
    write_op_ch: Sender<WriteOp<K, V>>,
}

unsafe impl<K, V, S> Send for LFUCache<K, V, S> {}
unsafe impl<K, V, S> Sync for LFUCache<K, V, S> {}

impl<K, V, S> Clone for LFUCache<K, V, S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            read_op_ch: self.read_op_ch.clone(),
            write_op_ch: self.write_op_ch.clone(),
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
        let (r_snd, r_rcv) = crossbeam_channel::bounded(READ_LOG_SIZE);
        let (w_snd, w_rcv) = crossbeam_channel::bounded(WRITE_LOG_SIZE);
        Self {
            inner: Arc::new(Inner::new(capacity, build_hasher, r_rcv, w_rcv)),
            read_op_ch: r_snd,
            write_op_ch: w_snd,
        }
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
        Self {
            inner: Arc::new(Inner::new(capacity, build_hasher, r_rcv, w_rcv)),
            read_op_ch: r_snd,
            write_op_ch: w_snd,
        }
    }

    // TODO: Call this periodically (e.g. every 100 micro seconds)
    pub fn sync(&self) {
        let r_len = self.read_op_ch.len();
        if r_len > 0 {
            let mut deqs = self.inner.deques.lock();
            self.inner.apply_reads(&mut deqs, r_len);
        }

        let l_len = self.write_op_ch.len();
        if l_len > 0 {
            let mut deqs = self.inner.deques.lock();
            self.inner.apply_writes(&mut deqs, l_len);
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

    fn get_or_insert(&self, _key: K, _default: V) -> Arc<V> {
        todo!()
    }

    fn get_or_insert_with<F>(&self, _key: K, _default: F) -> Arc<V>
    where
        F: FnOnce() -> V,
    {
        todo!()
    }

    fn insert(&self, key: K, value: V) {
        let key = Arc::new(key);
        let value = Arc::new(value);
        self.inner.cache.insert_with_or_modify(
            key.clone(),
            || {
                let entry = Arc::new(ValueEntry::new(Arc::clone(&value)));
                let op = WriteOp::Insert(key, entry.clone());
                self.schedule_insert_op(op).expect("Failed to insert");
                entry
            },
            |_k, entry| {
                let entry = Arc::clone(entry);
                let entry = Arc::new(entry.replace(Arc::clone(&value)));
                let op = WriteOp::Update(entry.clone());
                self.schedule_insert_op(op).expect("Failed to insert");
                entry
            },
        );
    }

    fn remove(&self, key: &K) -> Option<Arc<V>> {
        self.inner.cache.remove(key).map(|entry| {
            let value = Arc::clone(&entry.value);
            self.schedule_remove_op(entry).expect("Failed to remove");
            value
        })
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
            let _ = ch.try_send(ReadExisting(key.clone(), entry)); // Ignore Result<_, _>.
        } else {
            let _ = ch.try_send(ReadNonExisting(key.clone())); // Ignore Result<_, _>.
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
            if let Some(ref mut deqs) = self.inner.deques.try_lock() {
                self.inner.apply_reads(deqs, len);
            }
        }
    }

    #[inline]
    fn apply_reads_writes_if_needed(&self) {
        let w_len = self.write_op_ch.len();

        if self.should_apply_writes(w_len) {
            let r_len = self.read_op_ch.len();
            if let Some(ref mut deqs) = self.inner.deques.try_lock() {
                self.inner.apply_reads(deqs, r_len);
                self.inner.apply_writes(deqs, w_len);
            }
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
    ReadExisting(K, Arc<ValueEntry<K, V>>),
    ReadNonExisting(K),
}

enum WriteOp<K, V> {
    Insert(Arc<K>, Arc<ValueEntry<K, V>>),
    Update(Arc<ValueEntry<K, V>>),
    Remove(Arc<ValueEntry<K, V>>),
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
                Ok(ReadExisting(key, entry)) => {
                    freq.increment(&key);
                    deqs.move_to_back(entry)
                }
                Ok(ReadNonExisting(key)) => freq.increment(&key),
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
}

// private methods
impl<K, V, S> Inner<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    #[inline]
    fn admit(&self, candidate: &K, victim: &DeqNode<K>, freq: &CountMinSketch8<K>) -> bool {
        // TODO: Implement some randomness to mitigate hash DoS.
        freq.estimate(candidate) > freq.estimate(&*victim.element)
    }

    // TODO: Maybe run this periodically in background?
    #[inline]
    fn find_cache_victim<'a>(
        &self,
        deqs: &'a mut Deques<K>,
        _freq: &CountMinSketch8<K>,
    ) -> &'a DeqNode<K> {
        deqs.probation.peek_front().expect("No victim found")

        // let mut victim = None;

        // Find a key with minimum access frequency in the given set of keys.
        // TODO: Do this on a set of randomly sampled keys rather than doing on
        // the whole set of keys in the cache.
        // for key in deqs.probation.iter() {
        //     let freq = freq.estimate(key);
        //     match victim {
        //         None => victim = Some((freq, key)),
        //         Some((freq0, _)) if freq < freq0 => victim = Some((freq, key)),
        //         Some(_) => (),
        //     }
        // }

        // let (_, key) = victim.expect("No victim found");
        // Arc::clone(key)
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
            let node = Box::new(DeqNode::new(CacheArea::MainProbation, key));
            let node = deqs.probation.push_back(node);
            unsafe { *(entry.deq_node.get()) = Some(node) };
        } else {
            let victim = self.find_cache_victim(deqs, freq);
            if self.admit(&key, victim, freq) {
                // Remove the victim from the cache.
                self.cache.remove(&victim.element);
                // Remove the victim from the deque.
                let node = NonNull::from(victim);
                unsafe { deqs.probation.unlink(node) };
                // Add the candidate to the deque.
                let node = Box::new(DeqNode::new(CacheArea::MainProbation, key));
                let node = deqs.probation.push_back(node);
                unsafe { *(entry.deq_node.get()) = Some(node) };
            } else {
                // Remove the candidate from the cache.
                self.cache.remove(&key);
            }
        }
    }
}

struct Deques<K> {
    window: Deque<K>,
    probation: Deque<K>,
    protected: Deque<K>,
}

impl<K> Default for Deques<K> {
    fn default() -> Self {
        Self {
            window: Deque::new(CacheArea::Window),
            probation: Deque::new(CacheArea::MainProbation),
            protected: Deque::new(CacheArea::MainProtected),
        }
    }
}

impl<K> Deques<K> {
    fn move_to_back<V>(&mut self, entry: Arc<ValueEntry<K, V>>) {
        use CacheArea::*;
        unsafe {
            if let Some(node) = *entry.deq_node.get() {
                match node.as_ref().area {
                    Window => self.window.move_to_back(node),
                    MainProbation => self.probation.move_to_back(node),
                    MainProtected => self.protected.move_to_back(node),
                }
            }
        }
    }

    fn unlink<V>(&mut self, entry: Arc<ValueEntry<K, V>>) {
        use CacheArea::*;
        unsafe {
            if let Some(node) = *entry.deq_node.get() {
                match node.as_ref().area {
                    Window => self.window.unlink(node),
                    MainProbation => self.probation.unlink(node),
                    MainProtected => self.protected.unlink(node),
                }
            }
        }
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::{ConcurrentCache, LFUCache};
    use std::sync::Arc;

    #[test]
    fn naive_basics() {
        let cache = LFUCache::new(3);
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
}
