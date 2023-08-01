# Moka Cache &mdash; Migration Guide

## Migrating to Moka v0.12

Moka v0.12 had major breaking changes on the API and internal behavior.

### `future::Cache`

The thread-pool was removed from `future::Cache`.

It caused the following API changes:

1. `future::Cache::get` method is now `async fn`, so you must `.await` the result.
2. `future::Cache::blocking()` method was removed.
   - Use async runtime's blocking API instead.
   - See [Replacing the Blocking API](#replacing-the-blocking-api) for more details.
3. Now `or_insert_with_if` method of the entry API requires `Send` bound for the
   `replace_if` closure.
4. `future::CacheBuilder::eviction_listener_with_queued_delivery_mode` was removed.
   - Use `future::CacheBuilder::eviction_listener` instead.
5. The signature of eviction listener closure was changed. Now it should return
   `future::ListenerFuture` instead of `()`.
   - `ListenerFuture` is a type alias of `Pin<Box<dyn Future<Output = ()> + Send>>`.
   - See [Updating the Eviction Listener](#updating-the-eviction-listener) for more
     details.
6. **TODO** `future::ConcurrentCacheExt::sync` is renamed to `future::Cache::flush`
   and became `async fn`.

Removing the thread-pool caused the following internal behavior changes:

1. Housekeeping tasks such as evictions (removing expired entries) may not be
   executed periodically anymore.
   - See [Housekeeping Tasks](#housekeeping-tasks) for more details.
2. Now `future::Cache` only supports `Immediate` delivery mode for eviction listener.
   - With older versions, it only supported `Queued` delivery mode.
   - If you need `Queued` delivery mode back, please file an issue.
   - See [Updating the Eviction Listener](#updating-the-eviction-listener) for more
     details.

#### Replacing the Blocking API

TODO

#### Updating the Eviction Listener

TODO

#### Housekeeping Tasks

Housekeeping are now executed only when certain cache APIs are called:

- Cache writes: `insert`, `get_with`, `invalidate`, etc.
- `get`
- `flush`

You can manually trigger the tasks by calling `flush` method.

Housekeeping tasks include:

TODO


### `sync::Cache` and `sync::SegmentedCache`

1. `sync` module is no longer enabled by default. Use `sync` feature to enable it.
2. ... **TODO**
