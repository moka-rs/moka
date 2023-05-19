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
concurrency of retrievals and a high expected concurrency for updates.

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

Moka provides a rich and flexible feature set while maintaining high hit ratio and a
high level of concurrency for concurrent access.

- Thread-safe, highly concurrent in-memory cache implementations:
    - Synchronous caches that can be shared across OS threads.
    - An asynchronous (futures aware) cache that can be accessed inside and outside
      of asynchronous contexts.
- A cache can be bounded by one of the followings:
    - The maximum number of entries.
    - The total weighted size of entries. (Size aware eviction)
- Maintains near optimal hit ratio by using an entry replacement algorithms inspired
  by Caffeine:
    - Admission to a cache is controlled by the Least Frequently Used (LFU) policy.
    - Eviction from a cache is controlled by the Least Recently Used (LRU) policy.
    - [More details and some benchmark results are available here][tiny-lfu].
- Supports expiration policies:
    - Time to live.
    - Time to idle.
    - Per-entry variable expiration.
- Supports eviction listener, a callback function that will be called when an entry
  is removed from the cache.

### Choosing the right cache for your use case

No cache implementation is perfect for every use cases. Moka is a complex software
and can be overkill for your use case. Sometimes simpler caches like
[Mini Moka][mini-moka-crate] or [Quick Cache][quick-cache] might be a better fit.

The following table shows the trade-offs between the different cache implementations:

| Feature | Moka v0.11 | Mini Moka v0.10 | Quick Cache v0.3 |
|:------- |:---- |:--------- |:----------- |
| Thread-safe, sync cache | ✅ | ✅ | ✅ |
| Thread-safe, async cache | ✅ | ❌ | ✅ |
| Non-concurrent cache | ❌ | ✅ | ✅ |
| Bounded by the maximum number of entries | ✅ | ✅ | ✅ |
| Bounded by the total weighted size of entries | ✅ | ✅ | ✅ |
| Near optimal hit ratio | ✅ TinyLFU | ✅ TinyLFU | ✅ CLOCK-Pro |
| Per-key, atomic insertion. (e.g. `get_with` method) | ✅ | ❌ | ✅ |
| Cache-level expiration policies (time-to-live and time-to-idle) | ✅ | ✅ | ❌ |
| Per-entry variable expiration | ✅ | ❌ | ❌ |
| Eviction listener | ✅ | ❌ | ❌ |
| Lock-free, concurrent iterator | ✅ | ❌ | ❌ |
| Lock-per-shard, concurrent iterator | ❌ | ✅ | ❌ |

| Performance, etc. | Moka v0.11 | Mini Moka v0.10 | Quick Cache v0.3 |
|:------- |:---- |:--------- |:----------- |
| Small overhead compared to a concurrent hash table | ❌ | ❌ | ✅ |
| Does not use background threads | ❌ Will be removed from v0.12 or v0.13 | ✅ | ✅ |
| Small dependency tree | ❌ | ✅ | ✅ |

[tiny-lfu]: https://github.com/moka-rs/moka/wiki#admission-and-eviction-policies
[quick-cache]: https://crates.io/crates/quick_cache
[mini-moka-crate]: https://crates.io/crates/mini-moka

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


## Change Log

- [CHANGELOG.md](https://github.com/moka-rs/moka/blob/master/CHANGELOG.md)

The `unsync::Cache` and `dash::Cache` have been moved to a separate crate called
[Mini Moka][mini-moka-crate]:

- `moka::unsync::Cache` → [`mini_moka::unsync::Cache`][unsync-cache-struct]
- `moka::dash::Cache` → [`mini_moka::sync::Cache`][dash-cache-struct]

[unsync-cache-struct]: https://docs.rs/mini-moka/latest/mini_moka/unsync/struct.Cache.html
[dash-cache-struct]: https://docs.rs/mini-moka/latest/mini_moka/sync/struct.Cache.html


## Table of Contents

- [Features](#features)
    - [Choosing the right cache for your use case](#choosing-the-right-cache-for-your-use-case)
- [Moka in Production](#moka-in-production)
- [Change Log](#change-log)
- [Usage](#usage)
- Examples (Part 1)
    - [Synchronous Cache](#example-synchronous-cache)
    - [Asynchronous Cache](#example-asynchronous-cache)
- [Avoiding to clone the value at `get`](#avoiding-to-clone-the-value-at-get)
- Example (Part 2)
    - [Size Aware Eviction](#example-size-aware-eviction)
- [Expiration Policies](#expiration-policies)
- [Minimum Supported Rust Versions](#minimum-supported-rust-versions)
- Troubleshooting
    - [Compile Errors on Some 32-bit Platforms](#compile-errors-on-some-32-bit-platforms)
- [Developing Moka](#developing-moka)
- [Road Map](#road-map)
- [About the Name](#about-the-name)
- [Credits](#credits)
- [License](#license)


## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
moka = "0.11"
```

To use the asynchronous cache, enable a crate feature called "future".

```toml
[dependencies]
moka = { version = "0.11", features = ["future"] }
```


## Example: Synchronous Cache

The thread-safe, synchronous caches are defined in the `sync` module.

Cache entries are manually added using `insert` or `get_with` method, and
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

You can try the synchronous example by cloning the repository and running the following
cargo instruction:

```
cargo run --example sync_example
```

If you want to atomically initialize and insert a value when the key is not present,
you might want to check [the document][doc-sync-cache] for other insertion methods
`get_with` and `try_get_with`.

[doc-sync-cache]: https://docs.rs/moka/*/moka/sync/struct.Cache.html#method.get_with


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
- Outside any async context, use `blocking` method to access blocking version of
  `insert` or `invalidate` methods.

Here is a similar program to the previous example, but using asynchronous cache with
[Tokio][tokio-crate] runtime:

```rust,ignore
// Cargo.toml
//
// [dependencies]
// moka = { version = "0.11", features = ["future"] }
// tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
// futures-util = "0.3"

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
    futures_util::future::join_all(tasks).await;

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

You can try the asynchronous example by cloning the repository and running the following
cargo instruction:
```
cargo run --features="future" --example async_example
```

If you want to atomically initialize and insert a value when the key is not present,
you might want to check [the document][doc-future-cache] for other insertion methods
`get_with` and `try_get_with`.

[doc-future-cache]: https://docs.rs/moka/*/moka/future/struct.Cache.html#method.get_with


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


## Example: Size Aware Eviction

If different cache entries have different "weights" &mdash; e.g. each entry has
different memory footprints &mdash; you can specify a `weigher` closure at the cache
creation time. The closure should return a weighted size (relative size) of an entry
in `u32`, and the cache will evict entries when the total weighted size exceeds its
`max_capacity`.

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

You can try the size aware eviction example by cloning the repository and running the following
cargo instruction:
```
cargo run  --example size_aware_eviction
```


## Expiration Policies

Moka supports the following expiration policies:

- **Cache-level expiration policies:**
    - Cache-level policies are applied to all entries in the cache.
    - **Time to live (TTL)**: A cached entry will be expired after the specified
      duration past from `insert`.
    - **Time to idle (TTI)**: A cached entry will be expired after the specified
      duration past from `get` or `insert`.
- **Per-entry expiration policy:**
    - The per-entry expiration lets you sets a different expiration time for each
      entry.

For details and examples of above policies, see the "Example: Time-based Expiration"
section ([`sync::Cache`][doc-sync-cache-expiration],
[`future::Cache`][doc-future-cache-expiration]) of the document.

[doc-sync-cache-expiration]: https://docs.rs/moka/latest/moka/sync/struct.Cache.html#example-time-based-expirations
[doc-future-cache-expiration]: https://docs.rs/moka/latest/moka/future/struct.Cache.html#example-time-based-expirations


## Minimum Supported Rust Versions

Moka's minimum supported Rust versions (MSRV) are the followings:

| Feature          | MSRV                     |
|:-----------------|:------------------------:|
| default features | Rust 1.60.0 (2022-04-07) |
| `future`         | Rust 1.60.0 (2022-04-07) |

It will keep a rolling MSRV policy of at least 6 months. If only the default features
are enabled, MSRV will be updated conservatively. When using other features, like
`future`, MSRV might be updated more frequently, up to the latest stable. In both
cases, increasing MSRV is _not_ considered a semver-breaking change.

<!--
- quanta v0.11.0 requires 1.60.
-->


## Troubleshooting

### Compile Errors on Some 32-bit Platforms

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
moka = { version = "0.11", default-features = false, features = ["sync"] }
# Or
moka = { version = "0.11", default-features = false, features = ["future"] }
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

**Generating the Doc**

```console
$ cargo +nightly -Z unstable-options --config 'build.rustdocflags="--cfg docsrs"' \
    doc --no-deps --features future
```

## Road Map

- [x] Size-aware eviction. (`v0.7.0` via [#24][gh-pull-024])
- [x] API stabilization. (Smaller core cache API, shorter names for frequently
      used methods) (`v0.8.0` via [#105][gh-pull-105])
    - e.g.
    - `get_or_insert_with(K, F)` → `get_with(K, F)`
    - `get_or_try_insert_with(K, F)` → `try_get_with(K, F)`
    - `blocking_insert(K, V)` → `blocking().insert(K, V)`
    - `time_to_live()` → `policy().time_to_live()`
- [x] Notifications on eviction. (`v0.9.0` via [#145][gh-pull-145])
- [x] Variable (per-entry) expiration, using a hierarchical timer wheel.
  (`v0.11.0` via [#248][gh-pull-248])
- [ ] Cache statistics. (Hit rate, etc.)
- [ ] Upgrade TinyLFU to Window-TinyLFU. ([details][tiny-lfu])

[gh-pull-024]: https://github.com/moka-rs/moka/pull/24
[gh-pull-105]: https://github.com/moka-rs/moka/pull/105
[gh-pull-145]: https://github.com/moka-rs/moka/pull/145
[gh-pull-248]: https://github.com/moka-rs/moka/pull/248


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


## Credits

### Caffeine

Moka's architecture is heavily inspired by the [Caffeine][caffeine-git] library for
Java. Thanks go to Ben Manes and all contributors of Caffeine.


### cht

The source files of the concurrent hash table under `moka::cht` module were copied
from the [cht crate v0.4.1][cht-v041] and modified by us. We did so for better
integration. cht v0.4.1 and earlier are licensed under the MIT license.

Thanks go to Gregory Meyer.

[cht-v041]: https://github.com/Gregory-Meyer/cht/tree/v0.4.1


## License

Moka is distributed under either of

- The MIT license
- The Apache License (Version 2.0)

at your option.

See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) for details.

[![FOSSA Status](https://app.fossa.com/api/projects/git%2Bgithub.com%2Fmoka-rs%2Fmoka.svg?type=large)](https://app.fossa.com/projects/git%2Bgithub.com%2Fmoka-rs%2Fmoka?ref=badge_large)
