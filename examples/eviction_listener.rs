use moka::sync::Cache;
use moka::sync::ConcurrentCacheExt;
use std::thread::sleep;
use std::time::Duration;

fn main() {
    // Make an artifically small cache and 1-second ttl to observe eviction listener.
    {
        let cache = Cache::builder()
            .max_capacity(2)
            .time_to_live(Duration::from_secs(1))
            .eviction_listener(|key, value, cause| {
                println!("Evicted ({:?},{:?}) because {:?}", key, value, cause)
            })
            .build();
        // Overload capacity of the cache.
        cache.insert(&0, "zero".to_string());
        cache.insert(&1, "one".to_string());
        cache.insert(&2, "twice".to_string());
        // Due to race condition spilled over maybe evicted twice by cause
        // Replaced and Size.
        cache.insert(&2, "two".to_string());
        // With 1-second ttl, keys 0 and 1 will be evicted if we wait long enough.
        sleep(Duration::from_secs(2));
        println!("Wake up!");
        cache.insert(&3, "three".to_string());
        cache.insert(&4, "four".to_string());
        cache.insert(&5, "five".to_string());
        let _ = cache.remove(&3);
        cache.invalidate(&4);
        cache.invalidate_all();
        loop {
            // Synchronization is limited to at most 500 entries for each call.
            cache.sync();
            // Check if all is done. Calling entry_count() requires calling sync() first!
            if cache.entry_count() == 0 {
                break;
            }
        }
        cache.insert(&6, "six".to_string());
        // When cache is dropped eviction listener is not called. Eiher
        // call invalidate_all() or wait longer than ttl.
        sleep(Duration::from_secs(2));
        println!("Cache structure removed.");
    }
    sleep(Duration::from_secs(1));
    println!("Exit program.");
}
