# Moka Cache &mdash; Migration Guide

## Migrating to v0.12 from a prior version

v0.12.0 has major breaking changes on the API and internal behavior. This section
walks you through ...

### `future::Cache`

- The thread pool was removed from `future::Cache`. It no longer spawns background
  threads.
- The `notification::DeliveryMode` for eviction listener was changed from `Queued` to
  `Immediate`.

To support these changes, the following API changes were made:

1. `future::Cache::get` method is now `async fn`, so you must `await` for the result.
2. `future::Cache::blocking` method was removed.
   - You need to use async runtime's blocking API instead.
   - See [Replacing the blocking API](#replacing-the-blocking-api) for more details.
3. Now `or_insert_with_if` method of the entry API requires `Send` bound for the
   `replace_if` closure.
4. `future::CacheBuilder::eviction_listener_with_queued_delivery_mode` method was
   removed.
   - You need to use a new method `future::CacheBuilder::eviction_listener` instead.
5. The signature of the eviction listener closure was changed. Now it should return
   `future::ListenerFuture` instead of `()`.
   - `ListenerFuture` is a type alias of `Pin<Box<dyn Future<Output = ()> + Send>>`.
   - See [Updating the eviction listener](#updating-the-eviction-listener) for more
     details.
6. `future::ConcurrentCacheExt::sync` method is renamed to
   `future::Cache::run_pending_tasks`. It also changed to `async fn`.

The following internal behavior changes were made:

1. Maintenance tasks such as removing expired entries are not executed periodically
   anymore.
   - See [Maintenance tasks](#maintenance-tasks) for more details.
2. Now `future::Cache` only supports `Immediate` delivery mode for eviction listener.
   - In older versions, only `Queued` delivery mode was supported.
       - If you need `Queued` delivery mode back, please file an issue.
   - See [Updating the eviction listener](#updating-the-eviction-listener) for more
     details.


#### Replacing the blocking API

`future::Cache::blocking` method was removed. You need to use async runtime's
blocking API instead.

**Tokio**

```rust

```

**async-std**

```rust

```

**actix-rt**

```rust

```

#### Updating the eviction listener

The signature of the eviction listener closure was changed. Now it should return
`future::ListenerFuture` instead of `()`. It is a type alias of
`Pin<Box<dyn Future<Output = ()> + Send>>`.

```rust

```

#### Maintenance tasks

In older versions, the maintenance tasks needed by the cache were periodically
executed in background by a global thread pool managed by `moka`. Now `future::Cache`
does not use the thread pool anymore. Instead, those maintenance tasks are executed
_sometimes_ in foreground when certain cache methods (`get`, `get_with`, `insert`,
etc.) are called by user code.

These maintenance tasks include:

![The lifecycle of cached entries](https://github.com/moka-rs/moka/wiki/images/benchmarks/moka-tiny-lfu.png)

1. Determine whether to admit a "temporary admitted" entry or not.
2. Apply the recording of cache reads and writes to the internal data structures,
   such as LFU filter, LRU queues, and timer wheels.
3. Remove expired entries.
4. Remove entries that have been invalidated by `invalidate_all` or
   `invalidate_entries_if` methods.
5. Deliver removal notifications to the eviction listener.

They will be executed in the following cache methods when necessary:

- All cache write methods: `insert`, `get_with`, `invalidate`, etc.
- Some of the cache read methods: `get`
- `run_pending_tasks` method, which executes the pending maintenance tasks
  explicitly.


### `sync::Cache` and `sync::SegmentedCache`

1. `sync` module is no longer enabled by default. Use a crate feature `sync` to
   enable it.
2. The thread pool is disabled by default.
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
- Call ... at the cache build time.

