use crate::ConcurrentCache;

use count_min_sketch::CountMinSketch8;
use crossbeam_channel::{Receiver, SendError, Sender};
use parking_lot::{Mutex, MutexGuard, RwLock};
use std::{
    hash::{BuildHasher, Hash},
    sync::Arc,
};

const READ_LOG_SIZE: usize = 64;
const WRITE_LOG_SIZE: usize = 256;
const READ_LOG_HIGH_WATER_MARK: usize = 48; // 75% of READ_LOG_SIZE
const WRITE_LOG_HIGH_WATER_MARK: usize = 128; // 50% of WRITE_LOG_SIZE

pub struct LFUCache<K, V, S> {
    inner: Arc<Inner<K, V, S>>,
    read_op_ch: Sender<K>,
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
            let r_lock = self.inner.reads_apply_lock.lock();
            self.inner.apply_reads(r_lock, r_len);
        }

        let l_len = self.write_op_ch.len();
        if l_len > 0 {
            let w_lock = self.inner.writes_apply_lock.lock();
            self.inner.apply_writes(w_lock, l_len);
        }
    }
}

impl<K, V, S> ConcurrentCache<K, V> for LFUCache<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    fn get(&self, key: &K) -> Option<Arc<V>> {
        let v = self.inner.get(key);
        self.record_read_op(key).expect("Failed to record a get op");
        v
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
        self.schedule_insert_op(key, value)
            .expect("Failed to insert");
    }

    fn remove(&self, key: &K) -> Option<Arc<V>> {
        self.schedule_remove_op(key).expect("Failed to remove");
        self.inner.get(key)
    }
}

// private methods
impl<K, V, S> LFUCache<K, V, S>
where
    K: Clone + Eq + Hash,
    S: BuildHasher,
{
    #[inline]
    fn record_read_op(&self, key: &K) -> Result<(), SendError<K>> {
        let ch = &self.read_op_ch;
        let _ = ch.try_send(key.clone()); // Ignore Result<_, _>.
        self.apply_reads_if_needed();
        Ok(())
    }

    #[inline]
    fn schedule_insert_op(&self, key: K, value: V) -> Result<(), SendError<WriteOp<K, V>>> {
        let ch = &self.write_op_ch;
        // NOTE: This will be blocked if the channel is full.
        ch.send(WriteOp::Insert(key, value))?;
        self.apply_reads_writes_if_needed();
        Ok(())
    }

    #[inline]
    fn schedule_remove_op(&self, key: &K) -> Result<(), SendError<WriteOp<K, V>>> {
        let ch = &self.write_op_ch;
        // NOTE: This will be blocked if the channel is full.
        ch.send(WriteOp::Remove(key.clone()))?;
        self.apply_reads_writes_if_needed();
        Ok(())
    }

    #[inline]
    fn apply_reads_if_needed(&self) {
        let len = self.read_op_ch.len();

        if self.should_apply_reads(len) {
            if let Some(lock) = self.inner.reads_apply_lock.try_lock() {
                self.inner.apply_reads(lock, len);
            }
        }
    }

    #[inline]
    fn apply_reads_writes_if_needed(&self) {
        let w_len = self.write_op_ch.len();

        if self.should_apply_writes(w_len) {
            let r_len = self.read_op_ch.len();
            if let Some(r_lock) = self.inner.reads_apply_lock.try_lock() {
                self.inner.apply_reads(r_lock, r_len);
            }

            if let Some(w_lock) = self.inner.writes_apply_lock.try_lock() {
                self.inner.apply_writes(w_lock, w_len);
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

enum WriteOp<K, V> {
    Insert(K, V),
    Remove(K),
}

type Cache<K, V, S> = cht::HashMap<Arc<K>, Arc<V>, S>;
type KeySet<K> = std::collections::HashSet<Arc<K>>;

struct Inner<K, V, S> {
    capacity: usize,
    cache: Cache<K, V, S>,
    keys: Mutex<KeySet<K>>,
    frequency_sketch: RwLock<CountMinSketch8<K>>,
    reads_apply_lock: Mutex<()>,
    writes_apply_lock: Mutex<()>,
    read_op_ch: Receiver<K>,
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
        read_op_ch: Receiver<K>,
        write_op_ch: Receiver<WriteOp<K, V>>,
    ) -> Self {
        let cache = cht::HashMap::with_capacity_and_hasher(capacity, build_hasher);
        let skt_capacity = usize::max(capacity, 100);
        let frequency_sketch = CountMinSketch8::new(skt_capacity, 0.95, 10.0)
            .expect("Failed to create the frequency sketch");

        Self {
            capacity,
            cache,
            keys: Mutex::new(std::collections::HashSet::default()),
            frequency_sketch: RwLock::new(frequency_sketch),
            reads_apply_lock: Mutex::new(()),
            writes_apply_lock: Mutex::new(()),
            read_op_ch,
            write_op_ch,
        }
    }

    #[inline]
    fn get(&self, key: &K) -> Option<Arc<V>> {
        self.cache.get(key)
    }

    fn apply_reads(&self, _lock: MutexGuard<'_, ()>, count: usize) {
        let mut freq = self.frequency_sketch.write();
        let ch = &self.read_op_ch;
        for _ in 0..count {
            match ch.try_recv() {
                Ok(key) => freq.increment(&key),
                Err(_) => break,
            }
        }
    }

    fn apply_writes(&self, _lock: MutexGuard<'_, ()>, count: usize) {
        use WriteOp::*;

        let freq = self.frequency_sketch.read();
        let mut keys = self.keys.lock();

        let ch = &self.write_op_ch;
        for _ in 0..count {
            match ch.try_recv() {
                Ok(Insert(key, value)) => {
                    self.handle_insert(key, Arc::new(value), &mut keys, &freq)
                }
                Ok(Remove(key)) => self.remove(&Arc::new(key), &mut keys),
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
    fn admit(&self, candidate: &K, victim: &K, freq: &CountMinSketch8<K>) -> bool {
        // TODO: Implement some randomness to mitigate hash DoS.
        freq.estimate(candidate) > freq.estimate(victim)
    }

    // TODO: Maybe run this periodically in background?
    #[inline]
    fn find_cache_victim(&self, keys: &KeySet<K>, freq: &CountMinSketch8<K>) -> Arc<K> {
        let mut victim = None;

        // Find a key with minimum access frequency in the given set of keys.
        // TODO: Do this on a set of randomly sampled keys rather than doing on
        // the whole set of keys in the cache.
        for key in keys.iter() {
            let freq = freq.estimate(key);
            match victim {
                None => victim = Some((freq, key)),
                Some((freq0, _)) if freq < freq0 => victim = Some((freq, key)),
                Some(_) => (),
            }
        }

        let (_, key) = victim.expect("No victim found");
        Arc::clone(key)
    }

    #[inline]
    fn handle_insert(
        &self,
        key: K,
        value: Arc<V>,
        keys: &mut KeySet<K>,
        freq: &CountMinSketch8<K>,
    ) {
        let cache = &self.cache;

        if cache.len() < self.capacity {
            self.insert(Arc::new(key), value, keys);
            return;
        }

        let key = Arc::new(key);

        if keys.contains(&key) {
            self.insert(key, value, keys);
        } else {
            let victim = self.find_cache_victim(&keys, freq);
            if self.admit(&key, &victim, freq) {
                self.remove(&victim, keys);
                self.insert(key, value, keys);
            }
        }
    }

    #[inline]
    fn insert(&self, key: Arc<K>, value: Arc<V>, keys: &mut KeySet<K>) {
        keys.insert(Arc::clone(&key));
        self.cache.insert(key, value);
    }

    #[inline]
    fn remove(&self, key: &Arc<K>, keys: &mut KeySet<K>) {
        keys.remove(key);
        self.cache.remove(key);
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
        cache.sync();

        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        assert_eq!(cache.get(&"a"), Some(Arc::new("alice")));
        assert_eq!(cache.get(&"b"), Some(Arc::new("bob")));
        // counts: a -> 2, b -> 2

        cache.insert("c", "cindy");
        cache.sync();

        assert_eq!(cache.get(&"c"), Some(Arc::new("cindy")));
        // counts: a -> 2, b -> 2, c -> 1

        // "d" should not be admitted because its frequency is too low.
        cache.insert("d", "david"); //        count: d -> 0
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //        d -> 1

        cache.insert("d", "david");
        cache.sync();
        assert_eq!(cache.get(&"d"), None); //        d -> 2

        // "d" should be admitted and "c" should be evicted
        // because d's frequency is higher then c's.
        cache.insert("d", "dennis");
        cache.sync();
        assert_eq!(cache.get(&"d"), Some(Arc::new("dennis")));
        assert_eq!(cache.get(&"c"), None);

        assert_eq!(cache.remove(&"b"), Some(Arc::new("bob")));
    }
}
