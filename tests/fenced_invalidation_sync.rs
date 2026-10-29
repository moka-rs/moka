#![cfg(feature = "sync")]

use moka::sync::{Cache, SegmentedCache};

#[test]
fn sync_cache_fenced_api() {
    let cache = Cache::<String, u32>::new(16);
    cache.insert("key".into(), 1);

    cache.invalidate_and_fence("key");
    assert_eq!(cache.get("key"), None);
    assert_eq!(cache.get_with("key".into(), || 2), 2);
    assert_eq!(cache.get("key"), Some(2));
}

#[test]
fn segmented_cache_fenced_api() {
    let cache = SegmentedCache::<String, u32>::new(16, 2);
    cache.insert("key".into(), 1);

    cache.invalidate_and_fence("key");
    assert_eq!(cache.get("key"), None);
    assert_eq!(cache.get_with("key".into(), || 2), 2);
    assert_eq!(cache.get("key"), Some(2));
}
