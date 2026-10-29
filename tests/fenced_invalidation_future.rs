#![cfg(feature = "future")]

use moka::future::Cache;

#[tokio::test]
async fn future_cache_fenced_api() {
    let cache = Cache::<String, u32>::new(16);
    cache.insert("key".into(), 1).await;

    cache.invalidate_and_fence("key").await;
    assert_eq!(cache.get("key").await, None);
    assert_eq!(cache.get_with("key".into(), async { 2 }).await, 2);
    assert_eq!(cache.get("key").await, Some(2));
}
