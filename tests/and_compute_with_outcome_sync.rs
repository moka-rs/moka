#![cfg(feature = "sync")]

//! Regression tests for a TOCTOU race in `and_compute_with` (sync).
//!
//! Before the fix, `try_compute` decided between [`compute::CompResult::Inserted`]
//! and [`compute::CompResult::ReplacedWith`] using the entry snapshot taken
//! _before_ the user closure ran. A concurrent `insert` or `invalidate` from
//! another thread could land between that snapshot and the actual CAS into the
//! lock-free hash table, leaving the returned `CompResult` (and
//! `Entry::is_old_value_replaced`) stale.
//!
//! These tests force that race by gating the closure on a pair of barriers:
//! the closure wakes a helper thread, waits for it to mutate the cache directly,
//! then resumes and lets `try_compute` perform the CAS against the post-mutation
//! state.

use std::sync::{Arc, Barrier};
use std::thread;

use moka::ops::compute;
use moka::sync::Cache;

/// Closure observes `Some`, helper thread invalidates concurrently, CAS sees no
/// entry — outcome must be `Inserted`, not `ReplacedWith`.
#[test]
fn observed_some_racing_invalidate_yields_inserted() {
    let cache: Cache<String, u64> = Cache::builder().max_capacity(100).build();
    let key = "k".to_string();

    // Seed so the closure observes `Some`.
    cache.insert(key.clone(), 0);

    let before = Arc::new(Barrier::new(2));
    let after = Arc::new(Barrier::new(2));

    let cache_b = cache.clone();
    let key_b = key.clone();
    let before_b = Arc::clone(&before);
    let after_b = Arc::clone(&after);

    let helper = thread::spawn(move || {
        before_b.wait();
        // Synchronously remove the entry from the lock-free hash table.
        cache_b.invalidate(&key_b);
        after_b.wait();
    });

    let before_a = Arc::clone(&before);
    let after_a = Arc::clone(&after);

    let result = cache.entry_by_ref(&key).and_compute_with(|entry| {
        // The closure must observe the seeded entry.
        assert!(entry.is_some(), "closure should observe seeded entry");
        before_a.wait();
        after_a.wait();
        compute::Op::Put(2)
    });

    helper.join().unwrap();

    match result {
        compute::CompResult::Inserted(entry) => {
            assert!(
                !entry.is_old_value_replaced(),
                "Inserted entry must not report old-value-replaced"
            );
            assert_eq!(*entry.value(), 2);
        }
        other => panic!(
            "expected CompResult::Inserted after racing invalidate, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

/// Closure observes `None`, helper thread inserts concurrently, CAS sees an
/// existing entry — outcome must be `ReplacedWith`, not `Inserted`.
#[test]
fn observed_none_racing_insert_yields_replaced_with() {
    let cache: Cache<String, u64> = Cache::builder().max_capacity(100).build();
    let key = "k".to_string();

    let before = Arc::new(Barrier::new(2));
    let after = Arc::new(Barrier::new(2));

    let cache_b = cache.clone();
    let key_b = key.clone();
    let before_b = Arc::clone(&before);
    let after_b = Arc::clone(&after);

    let helper = thread::spawn(move || {
        before_b.wait();
        // Synchronously install an entry into the lock-free hash table.
        cache_b.insert(key_b, 1);
        after_b.wait();
    });

    let before_a = Arc::clone(&before);
    let after_a = Arc::clone(&after);

    let result = cache.entry_by_ref(&key).and_compute_with(|entry| {
        assert!(entry.is_none(), "closure should observe an empty cache");
        before_a.wait();
        after_a.wait();
        compute::Op::Put(2)
    });

    helper.join().unwrap();

    match result {
        compute::CompResult::ReplacedWith(entry) => {
            assert!(
                entry.is_old_value_replaced(),
                "ReplacedWith entry must report old-value-replaced"
            );
            assert_eq!(*entry.value(), 2);
        }
        other => panic!(
            "expected CompResult::ReplacedWith after racing insert, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}
