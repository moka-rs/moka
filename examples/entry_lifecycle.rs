use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use moka::{
    notification::RemovalCause,
    sync::{Cache, ConcurrentCacheExt},
};

// Currently Rust does not allow to create a static value using Default::default.
// So we manually construct a Counters struct here.
static COUNTERS: Counters = Counters {
    inserted: AtomicU64::new(0),
    evicted: AtomicU64::new(0),
    invalidated: AtomicU64::new(0),
    value_created: AtomicU64::new(0),
    value_dropped: AtomicU64::new(0),
};

#[derive(Debug)]
struct Counters {
    pub inserted: AtomicU64,
    pub evicted: AtomicU64,
    pub invalidated: AtomicU64,
    pub value_created: AtomicU64,
    pub value_dropped: AtomicU64,
}

impl Counters {
    pub fn incl_inserted(&self) {
        self.inserted.fetch_add(1, Ordering::AcqRel);
    }

    pub fn incl_evicted(&self) {
        self.evicted.fetch_add(1, Ordering::AcqRel);
    }

    pub fn incl_invalidated(&self) {
        self.invalidated.fetch_add(1, Ordering::AcqRel);
    }

    pub fn incl_value_created(&self) {
        COUNTERS.value_created.fetch_add(1, Ordering::AcqRel);
    }

    pub fn incl_value_dropped(&self) {
        COUNTERS.value_dropped.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct Value {
    blob: Vec<u8>,
}

impl Value {
    fn new(blob: Vec<u8>) -> Self {
        COUNTERS.incl_value_created();
        Self { blob }
    }
}

impl Clone for Value {
    fn clone(&self) -> Self {
        COUNTERS.incl_value_created();
        Self {
            blob: self.blob.clone(),
        }
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        COUNTERS.incl_value_dropped();
    }
}

fn main() {
    const MAX_CAPACITY: u64 = 500;
    const KEYS: u64 = ((MAX_CAPACITY as f64) * 1.2) as u64;

    let listener = |_k, _v, cause| match cause {
        RemovalCause::Size => COUNTERS.incl_evicted(),
        RemovalCause::Explicit => COUNTERS.incl_invalidated(),
        _ => (),
    };

    let cache = Cache::builder()
        .max_capacity(MAX_CAPACITY)
        .eviction_listener(listener)
        .build();
    println!("{:?}", COUNTERS);

    for key in 0..KEYS {
        let value = Arc::new(Value::new(vec![0u8; 1024]));
        cache.insert(key, value);
        COUNTERS.incl_inserted();
        cache.sync();
        println!("{:?}", COUNTERS);
    }

    for key in 0..KEYS {
        cache.invalidate(&key);
        cache.sync();
        println!("{:?}", COUNTERS);
    }

    std::mem::drop(cache);

    println!("{:?}", COUNTERS);
}
