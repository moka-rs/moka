#![cfg(feature = "future")]

//! Regression tests for a TOCTOU race in `and_compute_with` (future).
//!
//! Before the fix, `try_compute` decided between [`compute::CompResult::Inserted`]
//! and [`compute::CompResult::ReplacedWith`] using the entry snapshot taken
//! _before_ the user closure ran. A concurrent `insert` or `invalidate` from
//! another task could land between that snapshot and the actual CAS into the
//! lock-free hash table, leaving the returned `CompResult` (and
//! `Entry::is_old_value_replaced`) stale.

use std::sync::Arc;

use moka::future::Cache;
use moka::ops::compute;
use tokio::sync::Barrier;

/// Closure observes `Some`, helper task invalidates concurrently, CAS sees no
/// entry — outcome must be `Inserted`, not `ReplacedWith`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observed_some_racing_invalidate_yields_inserted() {
    let cache: Cache<String, u64> = Cache::builder().max_capacity(100).build();
    let key = "k".to_string();

    cache.insert(key.clone(), 0).await;

    let before = Arc::new(Barrier::new(2));
    let after = Arc::new(Barrier::new(2));

    let cache_b = cache.clone();
    let key_b = key.clone();
    let before_b = Arc::clone(&before);
    let after_b = Arc::clone(&after);

    let helper = tokio::spawn(async move {
        before_b.wait().await;
        cache_b.invalidate(&key_b).await;
        after_b.wait().await;
    });

    let before_a = Arc::clone(&before);
    let after_a = Arc::clone(&after);

    let result = cache
        .entry_by_ref(&key)
        .and_compute_with(|entry| {
            let before_a = Arc::clone(&before_a);
            let after_a = Arc::clone(&after_a);
            async move {
                assert!(entry.is_some(), "closure should observe seeded entry");
                before_a.wait().await;
                after_a.wait().await;
                compute::Op::Put(2)
            }
        })
        .await;

    helper.await.unwrap();

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

/// Closure observes `None`, helper task inserts concurrently, CAS sees an
/// existing entry — outcome must be `ReplacedWith`, not `Inserted`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observed_none_racing_insert_yields_replaced_with() {
    let cache: Cache<String, u64> = Cache::builder().max_capacity(100).build();
    let key = "k".to_string();

    let before = Arc::new(Barrier::new(2));
    let after = Arc::new(Barrier::new(2));

    let cache_b = cache.clone();
    let key_b = key.clone();
    let before_b = Arc::clone(&before);
    let after_b = Arc::clone(&after);

    let helper = tokio::spawn(async move {
        before_b.wait().await;
        cache_b.insert(key_b, 1).await;
        after_b.wait().await;
    });

    let before_a = Arc::clone(&before);
    let after_a = Arc::clone(&after);

    let result = cache
        .entry_by_ref(&key)
        .and_compute_with(|entry| {
            let before_a = Arc::clone(&before_a);
            let after_a = Arc::clone(&after_a);
            async move {
                assert!(entry.is_none(), "closure should observe an empty cache");
                before_a.wait().await;
                after_a.wait().await;
                compute::Op::Put(2)
            }
        })
        .await;

    helper.await.unwrap();

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
