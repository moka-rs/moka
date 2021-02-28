# Moka &mdash; Change Log

## Version 0.3.0

- Add an unsync cache (`moka::unsync::Cache`) and its builder.
- Add `invalidate_all` method to `sync`, `future` and `unsync` caches.

## Version 0.2.0

- Add an asynchronous, futures aware cache (`moka::future::Cache`) and its builder.

## Version 0.1.0

### Features

- Thread-safe, highly concurrent in-memory cache implementations.
- Caches are bounded by the maximum number of elements.
- Maintains good hit rate by using entry replacement algorithms inspired by
  [Caffeine][caffeine-git]:
    - Admission to a cache is controlled by the Least Frequently Used (LFU) policy.
    - Eviction from a cache is controlled by the Least Recently Used (LRU) policy.
- Supports expiration policies:
    - Time to live
    - Time to idle

[caffeine-git]: https://github.com/ben-manes/caffeine
