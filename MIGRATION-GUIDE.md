# Moka Cache &mdash; Migration Guide

## Migrating to v0.12 from a prior version

v0.12.0 has major breaking changes on the API and internal behavior. This section
describes the code changes required to migrate to v0.12.0.

### `future::Cache`

- The thread pool was removed from `future::Cache`. It no longer spawns background
  threads.
- The `notification::DeliveryMode` for eviction listener was changed from `Queued` to
  `Immediate`.

To support these changes, the following API changes were made:

1. `future::Cache::get` method is now `async fn`, so you must `await` for the result.
2. `future::Cache::blocking` method was removed.
   - Please use async runtime's blocking API instead.
   - See [Replacing the blocking API](#replacing-the-blocking-api) for more details.
3. Now `or_insert_with_if` method of the entry API requires `Send` bound for the
   `replace_if` closure.
4. `eviction_listener_with_queued_delivery_mode` method of `future::CacheBuilder` was
   removed.
   - Please use one of the new methods `eviction_listener` or
     `async_eviction_listener` instead.
   - See [Updating the eviction listener](#updating-the-eviction-listener) for more
     details.
5. `future::ConcurrentCacheExt::sync` method is renamed to
   `future::Cache::run_pending_tasks`. It is also changed to `async fn`.

The following internal behavior changes were made:

1. Maintenance tasks such as removing expired entries are not executed periodically
   anymore.
   - See [Maintenance tasks](#maintenance-tasks) for more details.
2. Now `future::Cache` only supports `Immediate` delivery mode for eviction listener.
   - In older versions, only `Queued` delivery mode was supported.
       - If you need `Queued` delivery mode back, please file an issue.

#### Replacing the blocking API

`future::Cache::blocking` method was removed. Please use async runtime's blocking API
instead.

**Tokio**

1. Call `tokio::runtime::Handle::current()` in async context to obtain a handle to
   the current Tokio runtime.
2. From outside async context, call cache's async function using `block_on` method of
   the runtime.

```rust
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // Create a future cache.
    let cache = Arc::new(moka::future::Cache::new(100));

    // In async context, you can obtain a handle to the current Tokio runtime.
    let rt = tokio::runtime::Handle::current();

    // Spawn an OS thread. Pass the handle and cache.
    let thread = {
        let cache = Arc::clone(&cache);

        std::thread::spawn(move || {
            // Call async function using block_on method of Tokio runtime.
            rt.block_on(cache.insert(0, 'a'));
        })
    };

    // Wait for the threads to complete.
    thread.join().unwrap();

    // Check the result.
    assert_eq!(cache.get(&0).await, Some('a'));
}
```

**async-std**

- From outside async context, call cache's async function using
  `async_std::task::block_on` method.

```rust
use std::sync::Arc;

#[async_std::main]
async fn main() {
    // Create a future cache.
    let cache = Arc::new(moka::future::Cache::new(100));

    // Spawn an OS thread. Pass the cache.
    let thread = {
        let cache = Arc::clone(&cache);

        std::thread::spawn(move || {
            use async_std::task::block_on;

            // Call async function using block_on method of async_std.
            block_on(cache.insert(0, 'a'));
        })
    };

    // Wait for the threads to complete.
    thread.join().unwrap();

    // Check the result.
    assert_eq!(cache.get(&0).await, Some('a'));
}
```

#### Updating the eviction listener

`eviction_listener_with_queued_delivery_mode` method of `future::CacheBuilder` was
removed. Please use one of the new methods `eviction_listener` or
`async_eviction_listener` instead.

##### `eviction_listener` method

`eviction_listener` takes the same closure as the old method. If you do not need to
`.await` anything in the eviction listener, use this method.

This code snippet is borrowed from [an example][listener-ex1] in the document of
`future::Cache`:

```rust
let eviction_listener = |key, _value, cause| {
    println!("Evicted key {key}. Cause: {cause:?}");
};

let cache = Cache::builder()
    .max_capacity(100)
    .expire_after(expiry)
    .eviction_listener(eviction_listener)
    .build();
```

[listener-ex1]: https://docs.rs/moka/latest/moka/future/struct.Cache.html#per-entry-expiration-policy

##### `async_eviction_listener` method

`async_eviction_listener` takes a closure that returns a `Future`. If you need to
`.await` something in the eviction listener, use this method. The actual return type
of the closure is `future::ListenerFuture`, which is a type alias of
`Pin<Box<dyn Future<Output = ()> + Send>>`. You can use the `boxed` method of
`future::FutureExt` trait to convert a regular `Future` into this type.

This code snippet is borrowed from [an example][listener-ex2] in the document of
`future::Cache`:

```rust
use moka::notification::ListenerFuture;
// FutureExt trait provides the boxed method.
use moka::future::FutureExt;

let listener = move |k, v: PathBuf, cause| -> ListenerFuture {
    println!(
        "\n== An entry has been evicted. k: {:?}, v: {:?}, cause: {:?}",
        k, v, cause
    );
    let file_mgr2 = Arc::clone(&file_mgr1);

    // Create a Future that removes the data file at the path `v`.
    async move {
        // Acquire the write lock of the DataFileManager.
        let mut mgr = file_mgr2.write().await;
        // Remove the data file. We must handle error cases here to
        // prevent the listener from panicking.
        if let Err(_e) = mgr.remove_data_file(v.as_path()).await {
            eprintln!("Failed to remove a data file at {:?}", v);
        }
    }
    // Convert the regular Future into ListenerFuture. This method is
    // provided by moka::future::FutureExt trait.
    .boxed()
};

// Create the cache. Set time to live for two seconds and set the
// eviction listener.
let cache = Cache::builder()
    .max_capacity(100)
    .time_to_live(Duration::from_secs(2))
    .async_eviction_listener(listener)
    .build();
```

[listener-ex2]: https://docs.rs/moka/latest/moka/future/struct.Cache.html#example-eviction-listener

#### Maintenance tasks

In older versions, the maintenance tasks needed by the cache were periodically
executed in background by a global thread pool managed by `moka`. Now `future::Cache`
does not use the thread pool anymore, so those maintenance tasks are executed
_sometimes_ in foreground when certain cache methods (`get`, `get_with`, `insert`,
etc.) are called by user code.

![The lifecycle of cached entries](https://github.com/moka-rs/moka/wiki/images/benchmarks/moka-tiny-lfu.png)

Figure 1. The lifecycle of cached entries

These maintenance tasks include:

1. Determine whether to admit a "temporary admitted" entry or not.
2. Apply the recording of cache reads and writes to the internal data structures,
   such as LFU filter, LRU queues, and timer wheels.
3. When cache's max capacity is exceeded, select existing entries to evict and remove
   them from cache.
4. Remove expired entries.
5. Remove entries that have been invalidated by `invalidate_all` or
   `invalidate_entries_if` methods.
6. Deliver removal notifications to the eviction listener. (Call the eviction
   listener closure with the information about evicted entry)

They will be executed in the following cache methods when necessary:

- All cache write methods: `insert`, `get_with`, `invalidate`, etc.
- Some of the cache read methods: `get`
- `run_pending_tasks` method, which executes the pending maintenance tasks
  explicitly.

Although expired entries will not be removed until the pending maintenance tasks are
executed, they will not be returned by cache read methods such as `get`, `get_with`
and `contains_key`. So unless you need to remove expired entries immediately (e.g. to
free some memory), you do not need to call `run_pending_tasks` method.

### `sync::Cache` and `sync::SegmentedCache`

1. (Not in v0.12.0-beta.1) `sync` caches will be no longer enabled by default. Use a
   crate feature `sync` to enable it.
2. (Not in v0.12.0-beta.1) The thread pool will be disabled by default.
   - In older versions, the thread pool was used to execute maintenance tasks in
     background.
   - When disabled:
      - those maintenance tasks are executed _sometimes_ in foreground when certain
        cache methods (`get`, `get_with`, `insert`, etc.) are called by user code
      -  See [Maintenance tasks](#maintenance-tasks) for more details.
   - To enable it, see [Enabling the thread pool](#enabling-the-thread-pool) for more
     details.


#### Enabling the thread pool

To enable the thread pool, do the followings:

- Specify a crate feature `thread-pool`.
- At the cache creation time, call the `thread_pool_enabled` method of
  `CacheBuilder`.
