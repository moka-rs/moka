# Moka Cache &mdash; Change Log

## Version 0.9.3

### Fixed

- Ensure that the following caches will drop the value of evicted entries immediately
  after eviction ([#169][gh-pull-0169]):
    - `sync::Cache`
    - `sync::SegmentedCache`
    - `future::Cache`


## Version 0.9.2

### Fixed

- Fix segmentation faults in `sync` and `future` caches under heavy loads on
  many-core machine ([#34][gh-issue-0034]):
    - NOTE: Although this issue was found in our testing environment ten months ago
      (v0.5.1), no user reported that they had the same issue.
    - NOTE: In [v0.8.4](#version-084), we added a mitigation to reduce the chance of
      the segfaults occurring.

### Changed

- Upgrade crossbeam-epoch from v0.8.2 to v0.9.9 ([#157][gh-pull-0157]):
    - This will make GitHub Dependabot to stop alerting about a security advisory
      [CVE-2022-23639][ghsa-qc84-gqf4-9926] for crossbeam-utils versions < 0.8.7.
    - Moka v0.9.1 or older was _not_ vulnerable to the CVE:
        - Although the older crossbeam-epoch v0.8.2 depends on an affected version of
          crossbeam-utils, epoch v0.8.2 does not use the affected _functions_ of
          utils. ([#162][gh-issue-0162])


## Version 0.9.1

### Fixed

- Relax a too restrictive requirement `Arc<K>: Borrow<Q>` for the key `&Q` of the
  `contains_key`, `get` and `invalidate` methods in the following caches (with `K` as
  the key type) ([#167][gh-pull-0167]). The requirement is now `K: Borrow<Q>` so these
  methods will accept `&[u8]` for the key `&Q` when the stored key `K` is `Vec<u8>`.
    - `sync::Cache`
    - `sync::SegmentedCache`
    - `future::Cache`


## Version 0.9.0

### Added

- Add support for eviction listener to the following caches ([#145][gh-pull-0145]).
  Eviction listener is a callback function that will be called when an entry is
  removed from the cache:
    - `sync::Cache`
    - `sync::SegmentedCache`
    - `future::Cache`
- Add a crate feature `sync` for enabling and disabling `sync` caches.
  ([#143][gh-pull-0143])
    - This feature is enabled by default.
    - When using experimental `dash` cache, opting out of `sync` will reduce the
      number of dependencies.
- Add a crate feature `logging` to enable optional log crate dependency.
  ([#159][gh-pull-0159])
    - Currently log will be emitted only when an eviction listener has panicked.


## Version 0.8.6

### Fixed

- Fix a bug caused `invalidate_all` and `invalidate_entries_if` of the following
  caches will not invalidate entries inserted just before calling them
  ([#155][gh-issue-0155]):
    - `sync::Cache`
    - `sync::SegmentedCache`
    - `future::Cache`
    - Experimental `dash::Cache`


## Version 0.8.5

### Added

- Add basic stats (`entry_count` and `weighted_size`) methods to all caches.
  ([#137][gh-pull-0137])
- Add `Debug` impl to the following caches ([#138][gh-pull-0138]):
    - `sync::Cache`
    - `sync::SegmentedCache`
    - `future::Cache`
    - `unsync::Cache`

### Fixed

- Remove unnecessary `K: Clone` bound from the following caches when they are `Clone`
  ([#133][gh-pull-0133]):
    - `sync::Cache`
    - `future::Cache`
    - Experimental `dash::Cache`


## Version 0.8.4

### Fixed

- Fix the following issue by upgrading Quanta crate to v0.10.0 ([#126][gh-pull-0126]):
    - Quanta v0.9.3 or older may not work correctly on some x86_64 machines where
      the Time Stamp Counter (TSC) is not synched across the processor cores.
      ([#119][gh-issue-0119])
    - For more details about the issue, see [the relevant section][panic_in_quanta]
      of the README.

### Added

- Add `get_with_if` method to the following caches ([#123][gh-issue-0123]):
    - `sync::Cache`
    - `sync::SegmentedCache`
    - `future::Cache`

### Changed

The followings are internal changes to improve memory safety in unsafe Rust usages
in Moka:

- Remove pointer-to-integer transmute by converting `UnsafeWeakPointer` from `usize`
  to `*mut T`. ([#127][gh-pull-0127])
- Increase the num segments of the waiters hash table from 16 to 64
  ([#129][gh-pull-0129]) to reduce the chance of the following issue occurring:
    - Segfaults under heavy workloads on a many-core machine. ([#34][gh-issue-0034])


## Version 0.8.3

### Changed

- Make [Quanta crate][quanta-crate] optional (but enabled by default)
  ([#121][gh-pull-0121])
    - Quanta v0.9.3 or older may not work correctly on some x86_64 machines where
      the Time Stamp Counter (TSC) is not synched across the processor cores.
      ([#119][gh-issue-0119])
    - This issue was fixed by Quanta v0.10.0. You can prevent the issue by upgrading
      Moka to v0.8.4 or newer.
    - For more details about the issue, see [the relevant section][panic_in_quanta]
      of the README.


## Version 0.8.2

### Added

- Add iterator to the following caches: ([#114][gh-pull-0114])
    - `sync::Cache`
    - `sync::SegmentedCache`
    - `future::Cache`
    - `unsync::Cache`
- Implement `IntoIterator` to the all caches (including experimental `dash::Cache`)
  ([#114][gh-pull-0114])

### Fixed

- Fix the `dash::Cache` iterator not to return expired entries.
  ([#116][gh-pull-0116])
- Prevent "index out of bounds" error when `sync::SegmentedCache` was created with
  a non-power-of-two segments. ([#117][gh-pull-0117])


## Version 0.8.1

### Added

- Add `contains_key` method to check if a key is present without resetting the idle
  timer or updating the historic popularity estimator. ([#107][gh-issue-0107])


## Version 0.8.0

As a part of stabilizing the cache API, the following cache methods have been renamed:

- `get_or_insert_with(K, F)` → `get_with(K, F)`
- `get_or_try_insert_with(K, F)` → `try_get_with(K, F)`

Old methods are still available but marked as deprecated. They will be removed in a
future version.

Also `policy` method was added to all caches and `blocking` method was added to
`future::Cache`. They return a `Policy` struct or `BlockingOp` struct
respectively. Some uncommon cache methods were moved to these structs, and old
methods were removed without deprecating.

Please see [#105][gh-pull-0105] for the complete list of the affected methods.

### Changed

- API stabilization. (Smaller core cache API, shorter names for common methods)
  ([#105][gh-pull-0105])
- Performance related:
    - Improve performance of `get_with` and `try_get_with`. ([#88][gh-pull-0088])
    - Avoid to calculate the same hash twice in `get`, `get_with`, `insert`,
      `invalidate`, etc. ([#90][gh-pull-0090])
- Update the minimum versions of dependencies:
    - crossbeam-channel to v0.5.4. ([#100][gh-pull-0100])
    - scheduled-thread-pool to v0.2.5. ([#103][gh-pull-0103])
    - (dev-dependency) skeptic to v0.13.5. ([#104][gh-pull-0104])

### Added

#### Experimental Additions

- Add a synchronous cache `moka::dash::Cache`, which uses `dashmap::DashMap` as the
  internal storage. ([#99][gh-pull-0099])
- Add iterator to `moka::dash::Cache`. ([#101][gh-pull-0101])

Please note that the above additions are highly experimental and their APIs will
be frequently changed in next few releases.


## Version 0.7.2

The minimum supported Rust version (MSRV) is now 1.51.0 (2021-03-25).

### Fixed

- Addressed a memory utilization issue that will get worse when keys have hight
  cardinality ([#72][gh-issue-0072]):
    - Reduce memory overhead in the internal concurrent hash table (cht).
      ([#79][gh-pull-0079])
    - Fix a bug that can create oversized frequency sketch when weigher is set.
      ([#75][gh-pull-0075])
    - Change `EntryInfo` from `enum` to `struct` to reduce memory utilization.
      ([#76][gh-pull-0076])
    - Replace some `std::sync::Arc` usages with `triomphe::Arc` to reduce memory
      utilization. ([#80][gh-pull-0080])
    - Embed `CacheRegion` value into a 2-bit tag space of `TagNonNull` pointer.
      ([#84][gh-pull-0084])
- Fix a bug that will use wrong (oversized) initial capacity for the internal cht.
  ([#83][gh-pull-0083])

### Added

- Add `unstable-debug-counters` feature for testing purpose. ([#82][gh-pull-0082])

### Changed

- Import (include) cht source files for better integration. ([#77][gh-pull-0077],
  [#86](gh-pull-0086))


## Version 0.7.1

- **Important Fix**: A memory leak issue (#65 below) was found in all previous
  versions (since v0.1.0) and fixed in this version. All users are encouraged to
  upgrade to this or newer version.

### Fixed

- Fix a memory leak that will happen when evicting/expiring an entry or manually
  invalidating an entry. ([#65][gh-pull-0065])

### Changed

- Update the minimum depending version of crossbeam-channel from v0.5.0 to v0.5.2.
  ([#67][gh-pull-0067])


## Version 0.7.0

- **Breaking change**: The type of the `max_capacity` has been changed from `usize`
  to `u64`. This was necessary to have the weight-based cache management consistent
  across different CPU architectures.

### Added

- Add support for weight-based (size aware) cache management.
  ([#24][gh-pull-0024])
- Add support for unbound cache. ([#24][gh-pull-0024])


## Version 0.6.3

### Fixed

- Fix a bug in `get_or_insert_with` and `get_or_try_insert_with` methods of
  `future::Cache`, which caused a panic if previously inserting task aborted.
  ([#59][gh-issue-0059])


## Version 0.6.2

### Removed

- Remove `Send` and `'static` bounds from `get_or_insert_with` and
  `get_or_try_insert_with` methods of `future::Cache`. ([#53][gh-pull-0053])

### Fixed

- Protect overflow when computing expiration. ([#56][gh-pull-0056])


## Version 0.6.1

### Changed

- Replace futures with futures-util. ([#47][gh-pull-0047])


## Version 0.6.0

### Fixed

- Fix a bug in `get_or_insert_with` and `get_or_try_insert_with` methods of
  `future::Cache` and `sync::Cache`; a panic in the `init` future/closure
  causes subsequent calls on the same key to get "unreachable code" panics.
  ([#43][gh-issue-0043])

### Changed

- Change `get_or_try_insert_with` to return a concrete error type rather
  than a trait object. ([#23][gh-pull-0023], [#37][gh-pull-0037])


## Version 0.5.4

### Changed

-  Restore quanta dependency on some 32-bit platforms such as
   `armv5te-unknown-linux-musleabi` or `mips-unknown-linux-musl`.
   ([#42][gh-pull-0042])


## Version 0.5.3

### Added

- Add support for some 32-bit platforms where `std::sync::atomic::AtomicU64` is not
  provided. (e.g. `armv5te-unknown-linux-musleabi` or `mips-unknown-linux-musl`)
  ([#38][gh-issue-0038])
    - On these platforms, you will need to disable the default features of Moka.
      See [the relevant section][resolving-error-on-32bit] of the README.


## Version 0.5.2

### Fixed

- Fix a bug in `get_or_insert_with` and `get_or_try_insert_with` methods of
  `future::Cache` by adding missing bounds `Send` and `'static` to the `init`
  future. Without this fix, these methods will accept non-`Send` or
  non-`'static` future and may cause undefined behavior.
  ([#31][gh-issue-0031])
- Fix `usize` overflow on big cache capacity. ([#28][gh-pull-0028])

### Added

- Add examples for `get_or_insert_with` and `get_or_try_insert_with`
  methods to the docs. ([#30][gh-pull-0030])

### Changed

- Downgrade crossbeam-epoch used in moka-cht from v0.9.x to v0.8.x as a possible
  workaround for segmentation faults on many-core CPU machines.
  ([#33][gh-pull-0033])


## Version 0.5.1

### Changed

- Replace a dependency cht v0.4 with moka-cht v0.5. ([#22][gh-pull-0022])


## Version 0.5.0

### Added

- Add `get_or_insert_with` and `get_or_try_insert_with` methods to `sync` and
  `future` caches. ([#20][gh-pull-0020])


## Version 0.4.0

### Fixed

- **Breaking change**: Now `sync::{Cache, SegmentedCache}` and `future::Cache`
  require `Send`, `Sync` and `'static` for the generic parameters `K` (key),
  `V` (value) and `S` (hasher state). This is necessary to prevent potential
  undefined behaviors in applications using single-threaded async runtime such as
  Actix-rt. ([#19][gh-pull-0019])

### Added

- Add `invalidate_entries_if` method to `sync`, `future` and `unsync` caches.
  ([#12][gh-pull-0012])


## Version 0.3.1

### Changed

- Stop skeptic from having to be compiled by all downstream users. ([#16][gh-pull-0016])


## Version 0.3.0

### Added

- Add an unsync cache (`moka::unsync::Cache`) and its builder for single-thread
  applications. ([#9][gh-pull-0009])
- Add `invalidate_all` method to `sync`, `future` and `unsync` caches.
  ([#11][gh-pull-0011])

### Fixed

- Fix problems including segfault caused by race conditions between the sync/eviction
  thread and client writes. (Addressed as a part of [#11][gh-pull-0011]).


## Version 0.2.0

### Added

- Add an asynchronous, futures aware cache (`moka::future::Cache`) and its builder.
  ([#7][gh-pull-0007])


## Version 0.1.0

### Added

- Add thread-safe, highly concurrent in-memory cache implementations
  (`moka::sync::{Cache, SegmentedCache}`) with the following features:
    - Bounded by the maximum number of elements.
    - Maintains good hit rate by using entry replacement algorithms inspired by
      [Caffeine][caffeine-git]:
        - Admission to a cache is controlled by the Least Frequently Used (LFU) policy.
        - Eviction from a cache is controlled by the Least Recently Used (LRU) policy.
    - Expiration policies:
        - Time to live
        - Time to idle


<!-- Links -->

[caffeine-git]: https://github.com/ben-manes/caffeine
[quanta-crate]: https://crates.io/crates/quanta

[panic_in_quanta]: https://github.com/moka-rs/moka#integer-overflow-in-quanta-crate-on-some-x86_64-machines
[resolving-error-on-32bit]: https://github.com/moka-rs/moka#compile-errors-on-some-32-bit-platforms

[ghsa-qc84-gqf4-9926]: https://github.com/advisories/GHSA-qc84-gqf4-9926

[gh-issue-0162]: https://github.com/moka-rs/moka/issues/162/
[gh-issue-0155]: https://github.com/moka-rs/moka/issues/155/
[gh-issue-0123]: https://github.com/moka-rs/moka/issues/123/
[gh-issue-0119]: https://github.com/moka-rs/moka/issues/119/
[gh-issue-0107]: https://github.com/moka-rs/moka/issues/107/
[gh-issue-0072]: https://github.com/moka-rs/moka/issues/72/
[gh-issue-0059]: https://github.com/moka-rs/moka/issues/59/
[gh-issue-0043]: https://github.com/moka-rs/moka/issues/43/
[gh-issue-0038]: https://github.com/moka-rs/moka/issues/38/
[gh-issue-0034]: https://github.com/moka-rs/moka/issues/34/
[gh-issue-0031]: https://github.com/moka-rs/moka/issues/31/

[gh-pull-0169]: https://github.com/moka-rs/moka/pull/169/
[gh-pull-0167]: https://github.com/moka-rs/moka/pull/167/
[gh-pull-0159]: https://github.com/moka-rs/moka/pull/159/
[gh-pull-0157]: https://github.com/moka-rs/moka/pull/157/
[gh-pull-0145]: https://github.com/moka-rs/moka/pull/145/
[gh-pull-0143]: https://github.com/moka-rs/moka/pull/143/
[gh-pull-0138]: https://github.com/moka-rs/moka/pull/138/
[gh-pull-0137]: https://github.com/moka-rs/moka/pull/137/
[gh-pull-0133]: https://github.com/moka-rs/moka/pull/133/
[gh-pull-0129]: https://github.com/moka-rs/moka/pull/129/
[gh-pull-0127]: https://github.com/moka-rs/moka/pull/127/
[gh-pull-0126]: https://github.com/moka-rs/moka/pull/126/
[gh-pull-0121]: https://github.com/moka-rs/moka/pull/121/
[gh-pull-0117]: https://github.com/moka-rs/moka/pull/117/
[gh-pull-0116]: https://github.com/moka-rs/moka/pull/116/
[gh-pull-0114]: https://github.com/moka-rs/moka/pull/114/
[gh-pull-0105]: https://github.com/moka-rs/moka/pull/105/
[gh-pull-0104]: https://github.com/moka-rs/moka/pull/104/
[gh-pull-0103]: https://github.com/moka-rs/moka/pull/103/
[gh-pull-0101]: https://github.com/moka-rs/moka/pull/101/
[gh-pull-0100]: https://github.com/moka-rs/moka/pull/100/
[gh-pull-0099]: https://github.com/moka-rs/moka/pull/99/
[gh-pull-0090]: https://github.com/moka-rs/moka/pull/90/
[gh-pull-0088]: https://github.com/moka-rs/moka/pull/88/
[gh-pull-0086]: https://github.com/moka-rs/moka/pull/86/
[gh-pull-0084]: https://github.com/moka-rs/moka/pull/84/
[gh-pull-0083]: https://github.com/moka-rs/moka/pull/83/
[gh-pull-0082]: https://github.com/moka-rs/moka/pull/82/
[gh-pull-0080]: https://github.com/moka-rs/moka/pull/80/
[gh-pull-0079]: https://github.com/moka-rs/moka/pull/79/
[gh-pull-0077]: https://github.com/moka-rs/moka/pull/77/
[gh-pull-0076]: https://github.com/moka-rs/moka/pull/76/
[gh-pull-0075]: https://github.com/moka-rs/moka/pull/75/
[gh-pull-0067]: https://github.com/moka-rs/moka/pull/67/
[gh-pull-0065]: https://github.com/moka-rs/moka/pull/65/
[gh-pull-0056]: https://github.com/moka-rs/moka/pull/56/
[gh-pull-0053]: https://github.com/moka-rs/moka/pull/53/
[gh-pull-0047]: https://github.com/moka-rs/moka/pull/47/
[gh-pull-0042]: https://github.com/moka-rs/moka/pull/42/
[gh-pull-0037]: https://github.com/moka-rs/moka/pull/37/
[gh-pull-0033]: https://github.com/moka-rs/moka/pull/33/
[gh-pull-0030]: https://github.com/moka-rs/moka/pull/30/
[gh-pull-0028]: https://github.com/moka-rs/moka/pull/28/
[gh-pull-0024]: https://github.com/moka-rs/moka/pull/24/
[gh-pull-0023]: https://github.com/moka-rs/moka/pull/23/
[gh-pull-0022]: https://github.com/moka-rs/moka/pull/22/
[gh-pull-0020]: https://github.com/moka-rs/moka/pull/20/
[gh-pull-0019]: https://github.com/moka-rs/moka/pull/19/
[gh-pull-0016]: https://github.com/moka-rs/moka/pull/16/
[gh-pull-0012]: https://github.com/moka-rs/moka/pull/12/
[gh-pull-0011]: https://github.com/moka-rs/moka/pull/11/
[gh-pull-0009]: https://github.com/moka-rs/moka/pull/9/
[gh-pull-0007]: https://github.com/moka-rs/moka/pull/7/
