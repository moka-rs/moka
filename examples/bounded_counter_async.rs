use moka::{
    future::Cache,
    ops::compute::{self, PerformedOp},
    Entry,
};

/// This example demonstrates how to increment a cached `u64` counter. It uses the
/// `and_upsert_with` method of `Cache`.
#[tokio::main]
async fn main() {
    let cache: Cache<String, u64> = Cache::new(100);
    let key = "key".to_string();

    let (maybe_entry, performed_op) = inclement_or_remove_counter(&cache, &key).await;
    assert_eq!(performed_op, PerformedOp::Inserted);

    let entry = maybe_entry.expect("An entry should be returned");
    assert!(entry.is_fresh());
    assert!(!entry.is_updated());
    assert_eq!(entry.into_value(), 1);

    let (maybe_entry, performed_op) = inclement_or_remove_counter(&cache, &key).await;
    assert_eq!(performed_op, PerformedOp::Updated);

    let entry = maybe_entry.expect("An entry should be returned");
    assert!(entry.is_fresh());
    assert!(entry.is_updated());
    assert_eq!(entry.into_value(), 2);

    let (maybe_entry, performed_op) = inclement_or_remove_counter(&cache, &key).await;
    assert_eq!(performed_op, PerformedOp::Removed);

    let entry = maybe_entry.expect("An entry should be returned");
    assert!(!entry.is_fresh());
    assert!(!entry.is_updated());
    assert_eq!(entry.into_value(), 2);

    assert!(!cache.contains_key(&key));

    let (maybe_entry, performed_op) = inclement_or_remove_counter(&cache, &key).await;
    assert_eq!(performed_op, PerformedOp::Inserted);

    let entry = maybe_entry.expect("An entry should be returned");
    assert!(entry.is_fresh());
    assert!(!entry.is_updated());
    assert_eq!(entry.into_value(), 1);
}

/// Increment a cached `u64` counter. If the counter is greater than or equal to 2,
/// remove it.
async fn inclement_or_remove_counter(
    cache: &Cache<String, u64>,
    key: &str,
) -> (Option<Entry<String, u64>>, compute::PerformedOp) {
    // - If the counter does not exist, insert a new value of 1.
    // - If the counter is less than 2, increment it by 1.
    // - If the counter is greater than or equal to 2, remove it.
    cache
        .entry_by_ref(key)
        .and_compute_with(|maybe_entry| {
            let op = if let Some(entry) = maybe_entry {
                // The entry exists.
                let counter = entry.into_value();
                if counter < 2 {
                    // Increment the counter by 1.
                    compute::Op::Put(counter.saturating_add(1))
                } else {
                    // Remove the entry.
                    compute::Op::Remove
                }
            } else {
                // The entry does not exist, insert a new value of 1.
                compute::Op::Put(1)
            };
            // Return a Future that is resolved to `op` immediately.
            std::future::ready(op)
        })
        .await
}
