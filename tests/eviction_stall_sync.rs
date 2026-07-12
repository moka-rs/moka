#![cfg(all(test, feature = "sync"))]

use moka::{policy::EvictionPolicy, sync::Cache};
use std::sync::{Arc, Barrier};
use std::thread;

/// Regression test for the unbounded-`weighted_size` bug in moka's LRU
/// admission path, sync variant
/// (<https://github.com/moka-rs/moka/issues/590>). See
/// `tests/eviction_stall_future.rs` for the future variant.
///
/// ## What the pre-fix bug looked like
///
/// Under LRU admission with concurrent writers that mix repeatedly-reinserted
/// keys with unique keys, a stale `WriteOp::Upsert` could be drained for an
/// entry whose backing CHT slot had already been evicted. The stale op pushed
/// a deque node that was not reachable from the CHT — an orphan at the LRU
/// front. `evict_lru_entries` would then make no further progress and the
/// cache's `weighted_size` would grow without bound.
///
/// With the `is_retired` lifecycle fix on `EntryInfo`, every drained
/// `WriteOp` whose entry has been retired is short-circuited before
/// `handle_admit` runs, so the post-drain `weighted_size` always converges
/// to `<= max_capacity`.
///
/// ## Why this shape of workload
///
/// - **Repeated-key writers** drive the re-insertion pattern. When a reused
///   key is evicted while a stale `WriteOp` for its prior generation is still
///   sitting in the channel, the stale op will try to re-admit on drain —
///   that's the window the bug exploited.
/// - **Unique-key writers** keep the cache above capacity so eviction runs
///   frequently and the race window is repeatedly entered.
/// - **10 MiB capacity, 64 KiB values** keeps the working set at ~160 entries:
///   small enough that the LRU eviction fires often, large enough to exercise
///   multi-segment bookkeeping.
///
/// ## Determinism
///
/// The test uses a fixed insertion count per writer (not a wall-clock
/// duration), so its input is deterministic. The writer thread interleaving
/// is still non-deterministic, which is what the test depends on to exercise
/// the race. The pass/fail property — convergence after the post-hoc drain —
/// is invariant of the interleaving. Related flakiness notes:
/// <https://github.com/moka-rs/moka/issues/591>.
#[test]
fn eviction_converges_under_concurrent_key_reuse() {
    const MAX_CAPACITY: u64 = 10 * 1024 * 1024;
    const VALUE_SIZE: usize = 64 * 1024;
    const WRITERS_PER_GROUP: usize = 10;
    const INSERTS_PER_WRITER: u64 = 1_000;
    const REUSED_KEY_POOL: u64 = 500;
    const DRAIN_ROUNDS: usize = 64;

    let cache: Cache<u64, Vec<u8>> = Cache::builder()
        .max_capacity(MAX_CAPACITY)
        .weigher(|_k, v: &Vec<u8>| v.len() as u32)
        .eviction_policy(EvictionPolicy::lru())
        .build();

    let barrier = Arc::new(Barrier::new(WRITERS_PER_GROUP * 2));
    let mut handles = Vec::with_capacity(WRITERS_PER_GROUP * 2);

    // Group A: cycle through a small pool of reused keys. Each cycle an
    // entry is (re-)inserted, potentially while a prior-generation
    // `WriteOp` is still in the channel — this is the race.
    for _ in 0..WRITERS_PER_GROUP {
        let cache = cache.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..INSERTS_PER_WRITER {
                let key = i % REUSED_KEY_POOL;
                cache.insert(key, vec![0u8; VALUE_SIZE]);
            }
        }));
    }

    // Group B: fresh, never-repeated keys. These supply eviction pressure
    // so the reused keys are forced out of the cache between reinsertions.
    for writer_id in 0..WRITERS_PER_GROUP {
        let cache = cache.clone();
        let barrier = Arc::clone(&barrier);
        let base = (writer_id as u64 + 1) * 1_000_000_000;
        handles.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..INSERTS_PER_WRITER {
                cache.insert(base + i, vec![0u8; VALUE_SIZE]);
            }
        }));
    }

    for h in handles {
        h.join().expect("writer thread panicked");
    }

    // Drain: let housekeeping catch up. Pre-fix this loop made no progress
    // once a zombie reached the LRU front; post-fix convergence is immediate.
    for _ in 0..DRAIN_ROUNDS {
        cache.run_pending_tasks();
    }

    let ws = cache.weighted_size();
    let ec = cache.entry_count();
    let max_entries = MAX_CAPACITY / VALUE_SIZE as u64;

    assert!(
        ws <= MAX_CAPACITY,
        "eviction stalled: weighted_size {:.1} MiB > max_capacity {:.1} MiB \
         after {} rounds of run_pending_tasks()",
        ws as f64 / (1024.0 * 1024.0),
        MAX_CAPACITY as f64 / (1024.0 * 1024.0),
        DRAIN_ROUNDS,
    );
    assert!(
        ec <= max_entries,
        "eviction stalled: entry_count {ec} > {max_entries} (the number of \
         {VALUE_SIZE}-byte entries that fit in {MAX_CAPACITY} bytes) after \
         {DRAIN_ROUNDS} rounds of run_pending_tasks()",
    );
}
