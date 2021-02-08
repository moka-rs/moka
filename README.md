# Moka

[![GitHub Actions][gh-actions-badge]][gh-actions]
[![crates.io release][release-badge]][crate]
[![docs][docs-badge]][docs]
[![dependency status][deps-rs-badge]][deps-rs]
[![license][license-badge]](#license)

Moka is a fast, concurrent cache library for Rust. Moka is inspired by
[Caffeine][caffeine-git] (Java) and [Ristretto][ristretto-git] (Go).

Moka provides a cache that supports full concurrency of retrievals and a high
expected concurrency for updates. It also provides a segmented cache for increased
concurrent update performance. These caches perform a best-effort bounding of a map
using an entry replacement algorithm to determine which entries to evict when the
capacity is exceeded.

[gh-actions-badge]: https://github.com/moka-rs/moka/workflows/CI/badge.svg
[release-badge]: https://img.shields.io/crates/v/moka.svg
[docs-badge]: https://docs.rs/moka/badge.svg
[deps-rs-badge]: https://deps.rs/repo/github/moka-rs/moka/status.svg
[license-badge]: https://img.shields.io/crates/l/moka.svg

[gh-actions]: https://github.com/moka-rs/moka/actions?query=workflow%3ACI
[crate]: https://crates.io/crates/moka
[docs]: https://docs.rs/moka
[deps-rs]: https://deps.rs/repo/github/moka-rs/moka

[caffeine-git]: https://github.com/ben-manes/caffeine
[ristretto-git]: https://github.com/dgraph-io/ristretto


## Features

- Thread-safe, highly concurrent in-memory cache implementations.
    - Synchronous (blocking) caches for OS threads.
    - Asynchronous (future aware) caches for async runtimes: Tokio, async-std
      and actix-rt.
- Caches are bounded by the maximum number of entries.
- Maintains good hit rate by using entry replacement algorithms inspired by
  [Caffeine][caffeine-git]:
    - Admission to a cache is controlled by the Least Frequently Used (LFU) policy.
    - Eviction from a cache is controlled by the Least Recently Used (LRU) policy.
- Supports expiration policies:
    - Time to live
    - Time to idle


## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
moka = "0.1"
```

## Example: Synchronous (Blocking) Cache

Cache entries are manually added using `insert` method, and are stored in the cache
until either evicted or manually invalidated.

Here's an example that reads and updates a cache by using multiple threads:

```rust
use moka::sync::Cache;

use std::thread;

fn value(n: usize) -> String {
    format!("value {}", n)
}

fn main() {
    const NUM_THREADS: usize = 16;
    const NUM_KEYS_PER_THREAD: usize = 64;

    // Create a cache that can store up to 10,000 entries.
    let cache = Cache::new(10_000);

    // Spawn threads and read and update the cache simultaneously.
    let threads: Vec<_> = (0..NUM_THREADS)
        .map(|i| {
            // To share the same cache across the threads, clone it.
            // This is a cheap operation.
            let my_cache = cache.clone();
            let start = i * NUM_KEYS_PER_THREAD;
            let end = (i + 1) * NUM_KEYS_PER_THREAD;

            thread::spawn(move || {
                // Insert 64 entries. (NUM_KEYS_PER_THREAD = 64)
                for key in start..end {
                    my_cache.insert(key, value(key));
                    // get() returns Option<String>, a clone of the stored value.
                    assert_eq!(my_cache.get(&key), Some(value(key)));
                }

                // Invalidate every 4 element of the inserted entries.
                for key in (start..end).step_by(4) {
                    my_cache.invalidate(&key);
                }
            })
        })
        .collect();

    // Wait for all threads to complete.
    threads.into_iter().for_each(|t| t.join().expect("Failed"));

    // Verify the result.
    for key in 0..(NUM_THREADS * NUM_KEYS_PER_THREAD) {
        if key % 4 == 0 {
            assert_eq!(cache.get(&key), None);
        } else {
            assert_eq!(cache.get(&key), Some(value(key)));
        }
    }
}
```

## Example: Asynchronous (Future Aware) Cache

- **runtime-actix**
- **runtime-async-std**
- **runtime-tokio**

Update operations `insert` and `invalidate` method are provided as `async fn` as they
can be blocked for a short time under heavy updates.

Here is a similar program to the previous example, but using [Tokio][tokio-crate]
runtime:

```rust
// Cargo.toml
//
// [dependencies]
// tokio = { version = "1.1", features = ["rt-multi-thread", "macros" ] }
// futures = "0.3"

use moka::future::Cache;

use tokio::task;

#[tokio::main]
async fn main() {
    const NUM_TASKS: usize = 16;
    const NUM_KEYS_PER_TASK: usize = 64;

    fn value(n: usize) -> String {
        format!("value {}", n)
    }

    // Create a cache that can store up to 10,000 entries.
    let cache = Cache::new(10_000);

    // Spawn async tasks and write to and read from the cache.
    let tasks: Vec<_> = (0..NUM_TASKS)
        .map(|i| {
            // To share the same cache across the async tasks, clone it.
            // This is a cheap operation.
            let my_cache = cache.clone();
            let start = i * NUM_KEYS_PER_TASK;
            let end = (i + 1) * NUM_KEYS_PER_TASK;

            tokio::spawn(async move {
                // Insert 64 entries. (NUM_KEYS_PER_TASK = 64)
                for key in start..end {
                    // insert() is an async method as it may block for
                    // a short time under heavy updates.
                    my_cache.insert(key, value(key)).await;
                    // get() is returns Option<String>, a clone of the stored value.
                    assert_eq!(my_cache.get(&key), Some(value(key)));
                }

                // Invalidate every 4 element of the inserted entries.
                for key in (start..end).step_by(4) {
                    // invalidate() is an async method as it may block for
                    // a short time under heavy updates.
                    my_cache.invalidate(&key).await;
                }
            })
        })
        .collect();

    // Wait for all tasks to complete.
    futures::future::join_all(tasks).await;

    // Verify the result.
    for key in 0..(NUM_TASKS * NUM_KEYS_PER_TASK) {
        if key % 4 == 0 {
            assert_eq!(cache.get(&key), None);
        } else {
            assert_eq!(cache.get(&key), Some(value(key)));
        }
    }
}
```

[std-future]: https://doc.rust-lang.org/stable/std/future/trait.Future.html
[tokio-crate]: https://crates.io/crates/tokio

## Usage: Expiration Policies

Moka supports the following expiration policies:

- **Time to live**: A cached entry will be expired after the specified duration past
  from `insert`.
- **Time to idle**: A cached entry will be expired after the specified duration past
  from `get` or `insert`.

To set them, use the `CacheBuilder`.

```rust
use moka::sync::CacheBuilder;

use std::time::Duration;

fn main() {
    let cache = CacheBuilder::new(10_000) // Max 10,000 elements
        // Time to live (TTL): 30 minutes
        .time_to_live(Duration::from_secs(30 * 60))
        // Time to idle (TTI):  5 minutes
        .time_to_idle(Duration::from_secs( 5 * 60))
        // Create the cache.
        .build();

    // This entry will expire after 5 minutes (TTI) if there is no get().
    cache.insert(0, "zero");

    // This get() will extend the entry life for another 5 minutes.
    cache.get(&0);

    // Even though we keep calling get(), the entry will expire
    // after 30 minutes (TTL) from the insert().
}
```

## Avoiding to clone the value at `get`

The return type of `get` method is `Option<V>` instead of `Option<&V>`, where `V` is
the value type. Every time `get` is called for an existing key, it creates a clone of
the stored value `V` and returns it. This is because the `Cache` allows concurrent
updates from threads so a value stored in the cache can be dropped or replaced at any
time by any other thread. `get` cannot return a reference `&V` as it is impossible to
guarantee the value outlives the reference.

If you want to store values that will be expensive to clone, wrap them by
`std::sync::Arc` before storing in a cache. [`Arc`][rustdoc-std-arc] is a thread-safe
reference-counted pointer and its `clone()` method is cheap.

[rustdoc-std-arc]: https://doc.rust-lang.org/stable/std/sync/struct.Arc.html

```rust,ignore
use std::sync::Arc;

let key = ...
let large_value = vec![0u8; 2 * 1024 * 1024]; // 2 MiB

// When insert, wrap the large_value by Arc.
cache.insert(key.clone(), Arc::new(large_value));

// get() will call Arc::clone() on the stored value, which is cheap.
cache.get(&key);
```

## Segmented Cache

Moka caches maintain internal data structures for entry replacement algorithms. These
structures are guarded by a lock and operations are applied in batches using a
dedicated worker thread to avoid lock contention. `sync::Cache` has only one worker
thread, so under heavy updates, the worker thread may not be able to catch up to the
updates. When this happens, `insert` or `invalidate` call will be paused (blocked)
for a short time.

If this pause happens very often, you may want to switch to `sync::SegmentedCache`.
A segmented cache has multiple internal cache segments and each segment has its own
worker thread. This will reduce the chances of the pausing.

Use `segments` method of the `CacheBuilder` to create a segmented cache.


## Hashing Algorithm

By default, a cache uses a hashing algorithm selected to provide resistance against
HashDoS attacks.

The default hashing algorithm is the one used by `std::collections::HashMap`, which
is currently SipHash 1-3, though this is subject to change at any point in the
future.

While its performance is very competitive for medium sized keys, other hashing
algorithms will outperform it for small keys such as integers as well as large keys
such as long strings. However those algorithms will typically not protect against
attacks such as HashDoS.

The hashing algorithm can be replaced on a per-`Cache` basis using the
`build_with_hasher` method of the `CacheBuilder`. Many alternative algorithms are
available on crates.io, such as the [aHash][ahash-crate] crate.

[ahash-crate]: https://crates.io/crates/ahash


## Minimum Supported Rust Version

This crate's minimum supported Rust version (MSRV) is 1.45.2.

<!--
- quanta requires 1.45.
- aHash 0.5 requires 1.43.
- cht requires 1.41.
-->

If no feature is enabled, MSRV will be updated conservatively. When using other
features, like `async` (which is not available yet), MSRV might be updated more
frequently, up to the latest stable. In both cases, increasing MSRV is _not_
considered a semver-breaking change.


## Road Map

- [x] `async` optimized caches. **DONE** `v0.2.0`
- [ ] Cache statistics. (Hit rate, etc.)
- [ ] Upgrade TinyLFU to Window TinyLFU.
- [ ] The variable (per-entry) expiration, using a hierarchical timer wheel.


## About the Name

Moka is named after the [moka pot][moka-pot-wikipedia], a stove-top coffee maker that
brews espresso-like coffee using boiling water pressurized by steam.

[moka-pot-wikipedia]: https://en.wikipedia.org/wiki/Moka_pot


## License

Moka is distributed under the terms of both the MIT license and the Apache License
(Version 2.0).

See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) for details.


<!--

MEMO:
- Column width is 85. (Emacs: C-x f)

-->
