#![cfg(feature = "future")]

/// Regression test for TOCTOU race in `and_compute_with`.
///
/// In `value_initializer.rs::try_compute`, the waiter was removed from the waiter
/// map (via `set_waiter_value(ReadyNone)`) before the actual cache mutation
/// (`cache.insert_with_hash`). This allowed a concurrent `and_compute_with` caller
/// to insert its own waiter, read stale cache state, and both callers to execute
/// `Op::Put` based on the same old value — losing one update.
use moka::future::Cache;
use moka::ops::compute;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_and_compute_with_concurrent_atomicity() {
    let cache: Cache<String, u64> = Cache::builder().max_capacity(100).build();
    let key = "counter".to_string();

    // Seed the cache with an initial value.
    cache.insert(key.clone(), 0).await;

    let n_writers: u64 = 8;
    let writes_per_writer: u64 = 100;

    let mut handles = Vec::new();
    for _ in 0..n_writers {
        let cache = cache.clone();
        let key = key.clone();

        handles.push(tokio::spawn(async move {
            for _ in 0..writes_per_writer {
                loop {
                    let result = cache
                        .entry_by_ref(&key)
                        .and_compute_with(async |entry| match entry {
                            Some(entry) => {
                                let val: u64 = entry.into_value();
                                compute::Op::Put(val + 1)
                            }
                            None => compute::Op::Nop,
                        })
                        .await;

                    if matches!(result, compute::CompResult::ReplacedWith(_)) {
                        break;
                    }
                }
            }
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let expected: u64 = n_writers * writes_per_writer;
    let actual: u64 = cache.get(&key).await.unwrap();

    assert_eq!(
        actual, expected,
        "Lost {} increments out of {expected}. \
         and_compute_with did not properly serialize concurrent calls on the same key.",
        expected.saturating_sub(actual)
    );
}
