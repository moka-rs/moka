//! This example demonstrates how to increment a cached `u64` counter. It uses the
//! `and_compute_with` method of `Cache`.

use moka::{
    ops::compute::{self, PerformedOp},
    sync::Cache,
    Entry,
};

fn main() {
    let cache: Cache<String, u64> = Cache::new(100);
    let key = "key".to_string();

    // This should insert a now counter value 1 to the cache, and return the value
    // with the kind of the operation performed.
    let (maybe_entry, performed_op) = inclement_or_remove_counter(&cache, &key);
    let entry = maybe_entry.expect("An entry should be returned");
    assert_eq!(entry.into_value(), 1);
    assert_eq!(performed_op, PerformedOp::Inserted);

    // This should increment the cached counter value by 1.
    let (maybe_entry, performed_op) = inclement_or_remove_counter(&cache, &key);
    let entry = maybe_entry.expect("An entry should be returned");
    assert_eq!(entry.into_value(), 2);
    assert_eq!(performed_op, PerformedOp::Updated);

    // This should remove the cached counter from the cache, and returns the
    // _removed_ value.
    let (maybe_entry, performed_op) = inclement_or_remove_counter(&cache, &key);
    let entry = maybe_entry.expect("An entry should be returned");
    assert_eq!(entry.into_value(), 2);
    assert_eq!(performed_op, PerformedOp::Removed);

    // The key should no longer exist.
    assert!(!cache.contains_key(&key));

    // This should start over; insert a new counter value 1 to the cache.
    let (maybe_entry, performed_op) = inclement_or_remove_counter(&cache, &key);
    let entry = maybe_entry.expect("An entry should be returned");
    assert_eq!(entry.into_value(), 1);
    assert_eq!(performed_op, PerformedOp::Inserted);
}

/// Increment a cached `u64` counter. If the counter is greater than or equal to 2,
/// remove it.
///
/// This method uses cache's `and_compute_with` method.
fn inclement_or_remove_counter(
    cache: &Cache<String, u64>,
    key: &str,
) -> (Option<Entry<String, u64>>, compute::PerformedOp) {
    // - If the counter does not exist, insert a new value of 1.
    // - If the counter is less than 2, increment it by 1.
    // - If the counter is greater than or equal to 2, remove it.
    cache.entry_by_ref(key).and_compute_with(|maybe_entry| {
        if let Some(entry) = maybe_entry {
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
        }
    })
}
