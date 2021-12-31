# Moka

[![GitHub Actions][gh-actions-badge]][gh-actions]
[![crates.io release][release-badge]][crate]
[![docs][docs-badge]][docs]
[![dependency status][deps-rs-badge]][deps-rs]
[![coverage status][coveralls-badge]][coveralls]
[![license][license-badge]](#license)
[![FOSSA Status](https://app.fossa.com/api/projects/git%2Bgithub.com%2Fmoka-rs%2Fmoka.svg?type=shield)](https://app.fossa.com/projects/git%2Bgithub.com%2Fmoka-rs%2Fmoka?ref=badge_shield)

Moka is a fast, concurrent cache library for Rust. Moka is inspired by the
[Caffeine][caffeine-git] library for Java.

Moka provides cache implementations on top of hash maps. They support full
concurrency of retrievals and a high expected concurrency for updates. Moka also
provides a non-thread-safe cache implementation for single thread applications.

All caches perform a best-effort bounding of a hash map using an entry replacement
algorithm to determine which entries to evict when the capacity is exceeded.

[gh-actions-badge]: https://github.com/moka-rs/moka/workflows/CI/badge.svg
[release-badge]: https://img.shields.io/crates/v/moka.svg
[docs-badge]: https://docs.rs/moka/badge.svg
[deps-rs-badge]: https://deps.rs/repo/github/moka-rs/moka/status.svg
[coveralls-badge]: https://coveralls.io/repos/github/moka-rs/moka/badge.svg?branch=master
[license-badge]: https://img.shields.io/crates/l/moka.svg
[fossa-badge]: https://app.fossa.com/api/projects/git%2Bgithub.com%2Fmoka-rs%2Fmoka.svg?type=shield

[gh-actions]: https://github.com/moka-rs/moka/actions?query=workflow%3ACI
[crate]: https://crates.io/crates/moka
[docs]: https://docs.rs/moka
[deps-rs]: https://deps.rs/repo/github/moka-rs/moka
[coveralls]: https://coveralls.io/github/moka-rs/moka?branch=master
[fossa]: https://app.fossa.com/projects/git%2Bgithub.com%2Fmoka-rs%2Fmoka?ref=badge_shield

[caffeine-git]: https://github.com/ben-manes/caffeine


## Features

- Thread-safe, highly concurrent in-memory cache implementations:
    - Synchronous caches that can be shared across OS threads.
    - An asynchronous (futures aware) cache that can be accessed inside and outside
      of asynchronous contexts.
- A cache can be bounded by one of the followings:
    - The maximum number of entries.
    - The total weighted size of entries.
- Maintains good hit rate by using an entry replacement algorithms inspired by
  [Caffeine][caffeine-git]:
    - Admission to a cache is controlled by the Least Frequently Used (LFU) policy.
    - Eviction from a cache is controlled by the Least Recently Used (LRU) policy.
- Supports expiration policies:
    - Time to live
    - Time to idle


## Moka in Production

Moka is powering production services as well as embedded Linux devices like home
routers. Here are some highlights:

- [crates.io](https://crates.io/): The official crate registry has been using Moka in
  its API service to reduce the loads on PostgreSQL. Moka is maintaining
  [cache hit rates of ~85%][gh-discussions-51] for the high-traffic download endpoint.
  (Moka used: Nov 2021 &mdash; present)
- [aliyundrive-webdav][aliyundrive-webdav-git]: This WebDAV gateway for a cloud drive
  may have been deployed in hundreds of home Wi-Fi routers, including inexpensive
  models with 32-bit MIPS or ARMv5TE-based SoCs. Moka is used to cache the metadata
  of remote files. (Moka used: Aug 2021 &mdash; present)

[gh-discussions-51]: https://github.com/moka-rs/moka/discussions/51
[aliyundrive-webdav-git]: https://github.com/messense/aliyundrive-webdav


## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
moka = "0.7"
```

To use the asynchronous cache, enable a crate feature called "future".

```toml
[dependencies]
moka = { version = "0.7", features = ["future"] }
```


## Example: Synchronous Cache

The thread-safe, synchronous caches are defined in the `sync` module.

Cache entries are manually added using `insert` or `get_or_insert_with` method, and
are stored in the cache until either evicted or manually invalidated.

Here's an example of reading and updating a cache by using multiple threads:

```rust
// Use the synchronous cache.
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

If you want to atomically initialize and insert a value when the key is not present,
you might want to check [the document][doc-sync-cache] for other insertion methods
`get_or_insert_with` and `get_or_try_insert_with`.

[doc-sync-cache]: https://docs.rs/moka/*/moka/sync/struct.Cache.html#method.get_or_insert_with


## Example: Asynchronous Cache

The asynchronous (futures aware) cache is defined in the `future` module.
It works with asynchronous runtime such as [Tokio][tokio-crate],
[async-std][async-std-crate] or [actix-rt][actix-rt-crate].
To use the asynchronous cache, [enable a crate feature called "future"](#usage).

[tokio-crate]: https://crates.io/crates/tokio
[async-std-crate]: https://crates.io/crates/asinc-std
[actix-rt-crate]: https://crates.io/crates/actix-rt

Cache entries are manually added using an insert method, and are stored in the cache
until either evicted or manually invalidated:

- Inside an async context (`async fn` or `async` block), use `insert` or `invalidate`
  method for updating the cache and `await` them.
- Outside any async context, use `blocking_insert` or `blocking_invalidate`
  methods. They will block for a short time under heavy updates.

Here is a similar program to the previous example, but using asynchronous cache with
[Tokio][tokio-crate] runtime:

```rust,ignore
// Cargo.toml
//
// [dependencies]
// moka = { version = "0.7", features = ["future"] }
// tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
// futures = "0.3"

// Use the asynchronous cache.
use moka::future::Cache;

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
                    // insert() is an async method, so await it.
                    my_cache.insert(key, value(key)).await;
                    // get() returns Option<String>, a clone of the stored value.
                    assert_eq!(my_cache.get(&key), Some(value(key)));
                }

                // Invalidate every 4 element of the inserted entries.
                for key in (start..end).step_by(4) {
                    // invalidate() is an async method, so await it.
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

If you want to atomically initialize and insert a value when the key is not present,
you might want to check [the document][doc-future-cache] for other insertion methods
`get_or_insert_with` and `get_or_try_insert_with`.

[doc-future-cache]: https://docs.rs/moka/*/moka/future/struct.Cache.html#method.get_or_insert_with


## Avoiding to clone the value at `get`

For the concurrent caches (`sync` and `future` caches), the return type of `get`
method is `Option<V>` instead of `Option<&V>`, where `V` is the value type. Every
time `get` is called for an existing key, it creates a clone of the stored value `V`
and returns it. This is because the `Cache` allows concurrent updates from threads so
a value stored in the cache can be dropped or replaced at any time by any other
thread. `get` cannot return a reference `&V` as it is impossible to guarantee the
value outlives the reference.

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


## Example: Bounding a Cache with Weighted Size of Entry

A `weigher` closure can be set at the cache creation time. It will calculate and
return a weighted size (relative size) of an entry. When it is set, a cache tries to
evict entries when the total weighted size exceeds its `max_capacity`.

```rust
use std::convert::TryInto;
use moka::sync::Cache;

fn main() {
    let cache = Cache::builder()
        // A weigher closure takes &K and &V and returns a u32 representing the
        // relative size of the entry. Here, we use the byte length of the value
        // String as the size.
        .weigher(|_key, value: &String| -> u32 {
            value.len().try_into().unwrap_or(u32::MAX)
        })
        // This cache will hold up to 32MiB of values.
        .max_capacity(32 * 1024 * 1024)
        .build();
    cache.insert(0, "zero".to_string());
}
```

Note that weighted sizes are not used when making eviction selections.


## Example: Expiration Policies

Moka supports the following expiration policies:

- **Time to live**: A cached entry will be expired after the specified duration past
  from `insert`.
- **Time to idle**: A cached entry will be expired after the specified duration past
  from `get` or `insert`.

To set them, use the `CacheBuilder`.

```rust
use moka::sync::Cache;
use std::time::Duration;

fn main() {
    let cache = Cache::builder()
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

### A note on expiration policies

The cache builders will panic if configured with either `time_to_live` or `time to idle`
longer than 1000 years. This is done to protect against overflow when computing key
expiration.


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



## Minimum Supported Rust Versions

This crate's minimum supported Rust versions (MSRV) are the followings:

| Feature    | Enabled by default? | MSRV        |
|:-----------|:-------------------:|:-----------:|
| no feature |                     | Rust 1.45.2 |
| `atomic64` |       yes           | Rust 1.45.2 |
| `future`   |                     | Rust 1.46.0 |

If only the default features are enabled, MSRV will be updated conservatively. When
using other features, like `future`, MSRV might be updated more frequently, up to the
latest stable. In both cases, increasing MSRV is _not_ considered a semver-breaking
change.

<!--
- socket2 0.4.0 requires 1.46.
- quanta requires 1.45.
- moka-cht requires 1.41.
-->


## Resolving Compile Errors on Some 32-bit Platforms

On some 32-bit target platforms including the followings, you may encounter compile
errors:

- `armv5te-unknown-linux-musleabi`
- `mips-unknown-linux-musl`
- `mipsel-unknown-linux-musl`

```console
error[E0432]: unresolved import `std::sync::atomic::AtomicU64`
  --> ... /moka-0.5.3/src/sync.rs:10:30
   |
10 |         atomic::{AtomicBool, AtomicU64, Ordering},
   |                              ^^^^^^^^^
   |                              |
   |                              no `AtomicU64` in `sync::atomic`
```

Such errors can occur because `std::sync::atomic::AtomicU64` is not provided on these
platforms but Moka uses it.

You can resolve the errors by disabling `atomic64` feature, which is one of the
default features of Moka. Edit your Cargo.toml to add `default-features = false`
to the dependency declaration.

```toml:Cargo.toml
[dependencies]
moka = { version = "0.7", default-feautures = false }
# Or
moka = { version = "0.7", default-feautures = false, features = ["future"] }
```

This will make Moka to switch to a fall-back implementation, so it will compile.


## Developing Moka

**Running All Tests**

To run all tests including `future` feature and doc tests on the README, use the
following command:

```console
$ RUSTFLAGS='--cfg skeptic --cfg trybuild' cargo test --all-features
```

**Running All Tests without Default Features**

```console
$ RUSTFLAGS='--cfg skeptic --cfg trybuild' cargo test \
    --no-default-features --features future
```


## Road Map

- [x] `async` optimized caches. (`v0.2.0`)
- [x] Bounding a cache with weighted size of entry.
      (`v0.7.0` via [#24](https://github.com/moka-rs/moka/pull/24))
- [ ] API stabilization. (Smaller core API, shorter names for frequently used
      methods)
    - e.g.
    - `get(&Q)` → `get_if_present(&Q)`
    - `get_or_insert_with(K, F)` → `get(K, F)`
    - `get_or_try_insert_with(K, F)` → `try_get(K, F)`
    - `blocking_insert(K, V)` → `blocking().insert(K, V)`.
    - `time_to_live()` → `config().time_to_live()`
- [ ] Cache statistics. (Hit rate, etc.)
- [ ] Notifications on eviction, etc.
- [ ] Upgrade TinyLFU to Window TinyLFU.
- [ ] The variable (per-entry) expiration, using a hierarchical timer wheel.


## About the Name

Moka is named after the [moka pot][moka-pot-wikipedia], a stove-top coffee maker that
brews espresso-like coffee using boiling water pressurized by steam.

This name would imply the following facts and hopes:

- Moka is a part of the Java Caffeine cache family.
- It is written in Rust. (Many moka pots are made of aluminum alloy or stainless
  steel. We know they don't rust though)
- It should be fast. ("Espresso" in Italian means express)
- It should be easy to use, like a moka pot.

[moka-pot-wikipedia]: https://en.wikipedia.org/wiki/Moka_pot


## License

Moka is distributed under either of

- The MIT license
- The Apache License (Version 2.0)

at your option.

See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) for details.

[![FOSSA Status](https://app.fossa.com/api/projects/git%2Bgithub.com%2Fmoka-rs%2Fmoka.svg?type=large)](https://app.fossa.com/projects/git%2Bgithub.com%2Fmoka-rs%2Fmoka?ref=badge_large)

<!--

MEMO:
- Column width is 85. (Emacs: C-x f)

-->
