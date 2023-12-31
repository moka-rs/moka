# Moka Examples

This directory contains examples of how to use Moka cache. Each example is a
standalone binary that can be run with `cargo run --example <example_name>`.

Each example has a suffix `_async` or `_sync`:

- `_async` indicates that the example uses the `moka::future::Cache`, which is a
  `Future`-aware, concurrent cache.
- `_sync` indicates that the example uses the `moka::sync::Cache`, which is a
  multi-thread safe, concurrent cache.

## Basics of the Cache API

- [basics_async](./basics_async.rs) and [basics_sync](./basics_sync.rs)
    - Sharing a cache between async tasks or OS threads.
        - Do not wrap a `Cache` with `Arc<Mutex<_>>`! Just clone the `Cache` and you
          are all set.
    - `insert`, `get` and `invalidate` methods.

- [size_aware_eviction_sync](./size_aware_eviction_sync.rs)
    - Configuring the max capacity of the cache based on the total size of the cached
      entries.

## Expiration and Eviction Listener

- [eviction_listener_sync](./eviction_listener_sync.rs)
    - Setting the `time_to_live` expiration policy.
    - Registering a listener (closure) to be notified when an entry is evicted from
      the cache.
    - `insert`, `invalidate` and `invalidate_all` methods.
    - Demonstrating when the expired entries will be actually evicted from the cache,
      and why the `run_pending_tasks` method could be important in some cases.

- [cascading_drop_async](./cascading_drop_async.rs)
    - Controlling the lifetime of the objects in a separate `BTreeMap` collection
      from the cache using an eviction listener.
    - `BTreeMap`, `Arc` and mpsc channel (multi-producer, single consumer channel).

## Check out the API Documentation too!

The examples are not meant to be exhaustive. Please check the
[API documentation][api-doc] for more examples and details.

[api-doc]: https://docs.rs/moka
