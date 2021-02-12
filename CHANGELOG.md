# Moka &mdash; Release Notes

## Version 0.2.0

### Features

- Introduce an asynchronous (futures aware) cache.


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
