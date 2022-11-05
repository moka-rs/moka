use super::{
    value_initializer::{InitResult, ValueInitializer},
    CacheBuilder, ConcurrentCacheExt,
};
use crate::{
    common::{
        concurrent::{
            constants::{MAX_SYNC_REPEATS, WRITE_RETRY_INTERVAL_MICROS},
            housekeeper::{self, InnerSync},
            Weigher, WriteOp,
        },
        time::Instant,
    },
    notification::{self, EvictionListener},
    sync::{Iter, PredicateId},
    sync_base::{
        base_cache::{BaseCache, HouseKeeperArc},
        iter::ScanningGet,
    },
    Policy, PredicateError,
};

use crossbeam_channel::{Sender, TrySendError};
use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    fmt,
    hash::{BuildHasher, Hash},
    sync::Arc,
    time::Duration,
};

/// A thread-safe concurrent synchronous in-memory cache.
///
/// `Cache` supports full concurrency of retrievals and a high expected concurrency
/// for updates.
///
/// `Cache` utilizes a lock-free concurrent hash table as the central key-value
/// storage. `Cache` performs a best-effort bounding of the map using an entry
/// replacement algorithm to determine which entries to evict when the capacity is
/// exceeded.
///
/// # Table of Contents
///
/// - [Example: `insert`, `get` and `invalidate`](#example-insert-get-and-invalidate)
/// - [Avoiding to clone the value at `get`](#avoiding-to-clone-the-value-at-get)
/// - [Example: Size-based Eviction](#example-size-based-eviction)
/// - [Example: Time-based Expirations](#example-time-based-expirations)
/// - [Example: Eviction Listener](#example-eviction-listener)
///     - [You should avoid eviction listener to panic](#you-should-avoid-eviction-listener-to-panic)
///     - [Delivery Modes for Eviction Listener](#delivery-modes-for-eviction-listener)
///         - [`Immediate` Mode](#immediate-mode)
///         - [`Queued` Mode](#queued-mode)
///     - [Example: `Queued` Delivery Mode](#example-queued-delivery-mode)
/// - [Thread Safety](#thread-safety)
/// - [Sharing a cache across threads](#sharing-a-cache-across-threads)
/// - [Hashing Algorithm](#hashing-algorithm)
///
/// # Example: `insert`, `get` and `invalidate`
///
/// Cache entries are manually added using [`insert`](#method.insert) or
/// [`get_with`](#method.get_with) methods, and are stored in the cache until either
/// evicted or manually invalidated.
///
/// Here's an example of reading and updating a cache by using multiple threads:
///
/// ```rust
/// use moka::sync::Cache;
///
/// use std::thread;
///
/// fn value(n: usize) -> String {
///     format!("value {}", n)
/// }
///
/// const NUM_THREADS: usize = 16;
/// const NUM_KEYS_PER_THREAD: usize = 64;
///
/// // Create a cache that can store up to 10,000 entries.
/// let cache = Cache::new(10_000);
///
/// // Spawn threads and read and update the cache simultaneously.
/// let threads: Vec<_> = (0..NUM_THREADS)
///     .map(|i| {
///         // To share the same cache across the threads, clone it.
///         // This is a cheap operation.
///         let my_cache = cache.clone();
///         let start = i * NUM_KEYS_PER_THREAD;
///         let end = (i + 1) * NUM_KEYS_PER_THREAD;
///
///         thread::spawn(move || {
///             // Insert 64 entries. (NUM_KEYS_PER_THREAD = 64)
///             for key in start..end {
///                 my_cache.insert(key, value(key));
///                 // get() returns Option<String>, a clone of the stored value.
///                 assert_eq!(my_cache.get(&key), Some(value(key)));
///             }
///
///             // Invalidate every 4 element of the inserted entries.
///             for key in (start..end).step_by(4) {
///                 my_cache.invalidate(&key);
///             }
///         })
///     })
///     .collect();
///
/// // Wait for all threads to complete.
/// threads.into_iter().for_each(|t| t.join().expect("Failed"));
///
/// // Verify the result.
/// for key in 0..(NUM_THREADS * NUM_KEYS_PER_THREAD) {
///     if key % 4 == 0 {
///         assert_eq!(cache.get(&key), None);
///     } else {
///         assert_eq!(cache.get(&key), Some(value(key)));
///     }
/// }
/// ```
///
/// If you want to atomically initialize and insert a value when the key is not
/// present, you might want to check other insertion methods
/// [`get_with`](#method.get_with) and [`try_get_with`](#method.try_get_with).
///
/// # Avoiding to clone the value at `get`
///
/// The return type of `get` method is `Option<V>` instead of `Option<&V>`. Every
/// time `get` is called for an existing key, it creates a clone of the stored value
/// `V` and returns it. This is because the `Cache` allows concurrent updates from
/// threads so a value stored in the cache can be dropped or replaced at any time by
/// any other thread. `get` cannot return a reference `&V` as it is impossible to
/// guarantee the value outlives the reference.
///
/// If you want to store values that will be expensive to clone, wrap them by
/// `std::sync::Arc` before storing in a cache. [`Arc`][rustdoc-std-arc] is a
/// thread-safe reference-counted pointer and its `clone()` method is cheap.
///
/// [rustdoc-std-arc]: https://doc.rust-lang.org/stable/std/sync/struct.Arc.html
///
/// # Example: Size-based Eviction
///
/// ```rust
/// use std::convert::TryInto;
/// use moka::sync::Cache;
///
/// // Evict based on the number of entries in the cache.
/// let cache = Cache::builder()
///     // Up to 10,000 entries.
///     .max_capacity(10_000)
///     // Create the cache.
///     .build();
/// cache.insert(1, "one".to_string());
///
/// // Evict based on the byte length of strings in the cache.
/// let cache = Cache::builder()
///     // A weigher closure takes &K and &V and returns a u32
///     // representing the relative size of the entry.
///     .weigher(|_key, value: &String| -> u32 {
///         value.len().try_into().unwrap_or(u32::MAX)
///     })
///     // This cache will hold up to 32MiB of values.
///     .max_capacity(32 * 1024 * 1024)
///     .build();
/// cache.insert(2, "two".to_string());
/// ```
///
/// If your cache should not grow beyond a certain size, use the `max_capacity`
/// method of the [`CacheBuilder`][builder-struct] to set the upper bound. The cache
/// will try to evict entries that have not been used recently or very often.
///
/// At the cache creation time, a weigher closure can be set by the `weigher` method
/// of the `CacheBuilder`. A weigher closure takes `&K` and `&V` as the arguments and
/// returns a `u32` representing the relative size of the entry:
///
/// - If the `weigher` is _not_ set, the cache will treat each entry has the same
///   size of `1`. This means the cache will be bounded by the number of entries.
/// - If the `weigher` is set, the cache will call the weigher to calculate the
///   weighted size (relative size) on an entry. This means the cache will be bounded
///   by the total weighted size of entries.
///
/// Note that weighted sizes are not used when making eviction selections.
///
/// [builder-struct]: ./struct.CacheBuilder.html
///
/// # Example: Time-based Expirations
///
/// `Cache` supports the following expiration policies:
///
/// - **Time to live**: A cached entry will be expired after the specified duration
///   past from `insert`.
/// - **Time to idle**: A cached entry will be expired after the specified duration
///   past from `get` or `insert`.
///
/// ```rust
/// use moka::sync::Cache;
/// use std::time::Duration;
///
/// let cache = Cache::builder()
///     // Time to live (TTL): 30 minutes
///     .time_to_live(Duration::from_secs(30 * 60))
///     // Time to idle (TTI):  5 minutes
///     .time_to_idle(Duration::from_secs( 5 * 60))
///     // Create the cache.
///     .build();
///
/// // This entry will expire after 5 minutes (TTI) if there is no get().
/// cache.insert(0, "zero");
///
/// // This get() will extend the entry life for another 5 minutes.
/// cache.get(&0);
///
/// // Even though we keep calling get(), the entry will expire
/// // after 30 minutes (TTL) from the insert().
/// ```
///
/// # Example: Eviction Listener
///
/// A `Cache` can be configured with an eviction listener, a closure that is called
/// every time there is a cache eviction. The listener takes three parameters: the
/// key and value of the evicted entry, and the
/// [`RemovalCause`](../notification/enum.RemovalCause.html) to indicate why the
/// entry was evicted.
///
/// An eviction listener can be used to keep other data structures in sync with the
/// cache, for example.
///
/// The following example demonstrates how to use an eviction listener with
/// time-to-live expiration to manage the lifecycle of temporary files on a
/// filesystem. The cache stores the paths of the files, and when one of them has
/// expired, the eviction lister will be called with the path, so it can remove the
/// file from the filesystem.
///
/// ```rust
/// // Cargo.toml
/// //
/// // [dependencies]
/// // anyhow = "1.0"
///
/// use moka::{sync::Cache, notification};
///
/// use anyhow::{anyhow, Context};
/// use std::{
///     fs, io,
///     path::{Path, PathBuf},
///     sync::{Arc, RwLock},
///     time::Duration,
/// };
///
/// /// The DataFileManager writes, reads and removes data files.
/// struct DataFileManager {
///     base_dir: PathBuf,
///     file_count: usize,
/// }
///
/// impl DataFileManager {
///     fn new(base_dir: PathBuf) -> Self {
///         Self {
///             base_dir,
///             file_count: 0,
///         }
///     }
///
///     fn write_data_file(
///         &mut self,
///         key: impl AsRef<str>,
///         contents: String
///     ) -> io::Result<PathBuf> {
///         // Use the key as a part of the filename.
///         let mut path = self.base_dir.to_path_buf();
///         path.push(key.as_ref());
///
///         assert!(!path.exists(), "Path already exists: {:?}", path);
///
///         // create the file at the path and write the contents to the file.
///         fs::write(&path, contents)?;
///         self.file_count += 1;
///         println!("Created a data file at {:?} (file count: {})", path, self.file_count);
///         Ok(path)
///     }
///
///     fn read_data_file(&self, path: impl AsRef<Path>) -> io::Result<String> {
///         // Reads the contents of the file at the path, and return the contents.
///         fs::read_to_string(path)
///     }
///
///     fn remove_data_file(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
///         // Remove the file at the path.
///         fs::remove_file(path.as_ref())?;
///         self.file_count -= 1;
///         println!(
///             "Removed a data file at {:?} (file count: {})",
///             path.as_ref(),
///             self.file_count
///         );
///
///         Ok(())
///     }
/// }
///
/// fn main() -> anyhow::Result<()> {
///     // Create an instance of the DataFileManager and wrap it with
///     // Arc<RwLock<_>> so it can be shared across threads.
///     let file_mgr = DataFileManager::new(std::env::temp_dir());
///     let file_mgr = Arc::new(RwLock::new(file_mgr));
///
///     let file_mgr1 = Arc::clone(&file_mgr);
///
///     // Create an eviction lister closure.
///     let listener = move |k, v: PathBuf, cause| {
///         // Try to remove the data file at the path `v`.
///         println!(
///             "\n== An entry has been evicted. k: {:?}, v: {:?}, cause: {:?}",
///             k, v, cause
///         );
///
///         // Acquire the write lock of the DataFileManager. We must handle
///         // error cases here to prevent the listener from panicking.
///         match file_mgr1.write() {
///             Err(_e) => {
///                 eprintln!("The lock has been poisoned");
///             }
///             Ok(mut mgr) => {
///                 // Remove the data file using the DataFileManager.
///                 if let Err(_e) = mgr.remove_data_file(v.as_path()) {
///                     eprintln!("Failed to remove a data file at {:?}", v);
///                 }
///             }
///         }
///     };
///
///     // Create the cache. Set time to live for two seconds and set the
///     // eviction listener.
///     let cache = Cache::builder()
///         .max_capacity(100)
///         .time_to_live(Duration::from_secs(2))
///         .eviction_listener(listener)
///         .build();
///
///     // Insert an entry to the cache.
///     // This will create and write a data file for the key "user1", store the
///     // path of the file to the cache, and return it.
///     println!("== try_get_with()");
///     let key = "user1";
///     let path = cache
///         .try_get_with(key, || -> anyhow::Result<_> {
///             let mut mgr = file_mgr
///                 .write()
///                 .map_err(|_e| anyhow::anyhow!("The lock has been poisoned"))?;
///             let path = mgr
///                 .write_data_file(key, "user data".into())
///                 .with_context(|| format!("Failed to create a data file"))?;
///             Ok(path)
///         })
///         .map_err(|e| anyhow!("{}", e))?;
///
///     // Read the data file at the path and print the contents.
///     println!("\n== read_data_file()");
///     {
///         let mgr = file_mgr
///             .read()
///             .map_err(|_e| anyhow::anyhow!("The lock has been poisoned"))?;
///         let contents = mgr
///             .read_data_file(path.as_path())
///             .with_context(|| format!("Failed to read data from {:?}", path))?;
///         println!("contents: {}", contents);
///     }
///
///     // Sleep for five seconds. While sleeping, the cache entry for key "user1"
///     // will be expired and evicted, so the eviction lister will be called to
///     // remove the file.
///     std::thread::sleep(Duration::from_secs(5));
///
///     Ok(())
/// }
/// ```
///
/// ## You should avoid eviction listener to panic
///
/// It is very important to make an eviction listener closure not to panic.
/// Otherwise, the cache will stop calling the listener after a panic. This is an
/// intended behavior because the cache cannot know whether it is memory safe or not
/// to call the panicked lister again.
///
/// When a listener panics, the cache will swallow the panic and disable the
/// listener. If you want to know when a listener panics and the reason of the panic,
/// you can enable an optional `logging` feature of Moka and check error-level logs.
///
/// To enable the `logging`, do the followings:
///
/// 1. In `Cargo.toml`, add the crate feature `logging` for `moka`.
/// 2. Set the logging level for `moka` to `error` or any lower levels (`warn`,
///    `info`, ...):
///     - If you are using the `env_logger` crate, you can achieve this by setting
///       `RUST_LOG` environment variable to `moka=error`.
/// 3. If you have more than one caches, you may want to set a distinct name for each
///    cache by using cache builder's [`name`][builder-name-method] method. The name
///    will appear in the log.
///
/// [builder-name-method]: ./struct.CacheBuilder.html#method.name
///
/// ## Delivery Modes for Eviction Listener
///
/// The [`DeliveryMode`][delivery-mode] specifies how and when an eviction
/// notifications should be delivered to an eviction listener. The `sync` caches
/// (`Cache` and `SegmentedCache`) support two delivery modes: `Immediate` and
/// `Queued` modes.
///
/// [delivery-mode]: ../notification/enum.DeliveryMode.html
///
/// ### `Immediate` Mode
///
/// Tne `Immediate` mode is the default delivery mode for the `sync` caches. Use this
/// mode when it is import to keep the order of write operations and eviction
/// notifications.
///
/// This mode has the following characteristics:
///
/// - The listener is called immediately after an entry was evicted.
/// - The listener is called by the thread who evicted the entry:
///    - The calling thread can be a background eviction thread or a user thread
///      invoking a cache write operation such as `insert`, `get_with` or
///      `invalidate`.
///    - The calling thread is blocked until the listener returns.
/// - This mode guarantees that write operations and eviction notifications for a
///   given cache key are ordered by the time when they occurred.
/// - This mode adds some performance overhead to cache write operations as it uses
///   internal per-key lock to guarantee the ordering.
///
/// ### `Queued` Mode
///
/// Use this mode when write performance is more important than preserving the order
/// of write operations and eviction notifications.
///
/// - The listener will be called some time after an entry was evicted.
/// - A notification will be stashed in a queue. The queue will be processed by
///   dedicated notification thread(s) and that thread will call the listener.
/// - This mode does not preserve the order of write operations and eviction
///   notifications.
/// - This mode adds almost no performance overhead to cache write operations as it
///   does not use the per-key lock.
///
/// ### Example: `Queued` Delivery Mode
///
/// Because the `Immediate` mode is the default mode for `sync` caches, the previous
/// example was using it implicitly.
///
/// The following is the same example but modified for the `Queued` delivery mode.
/// (Showing changed lines only)
///
/// ```rust
/// // Cargo.toml
/// //
/// // [dependencies]
/// // anyhow = "1.0"
/// // uuid = { version = "1.1", features = ["v4"] }
///
/// use moka::{sync::Cache, notification};
///
/// # use anyhow::{anyhow, Context};
/// # use std::{
/// #     fs, io,
/// #     path::{Path, PathBuf},
/// #     sync::{Arc, RwLock},
/// #     time::Duration,
/// # };
/// // Use UUID crate to generate a random file name.
/// use uuid::Uuid;
///
/// # struct DataFileManager {
/// #     base_dir: PathBuf,
/// #     file_count: usize,
/// # }
/// #
/// impl DataFileManager {
/// #   fn new(base_dir: PathBuf) -> Self {
/// #       Self {
/// #           base_dir,
/// #           file_count: 0,
/// #       }
/// #   }
/// #
///     fn write_data_file(
///         &mut self,
///         _key: impl AsRef<str>,
///         contents: String
///     ) -> io::Result<PathBuf> {
///         // We do not use the key for the filename anymore. Instead, we
///         // use UUID to generate a unique filename for each call.
///         loop {
///             // Generate a file path with unique file name.
///             let mut path = self.base_dir.to_path_buf();
///             path.push(Uuid::new_v4().as_hyphenated().to_string());
///
///             if path.exists() {
///                 continue; // This path is already taken by others. Retry.
///             }
///
///             // We have got a unique file path, so create the file at
///             // the path and write the contents to the file.
///             fs::write(&path, contents)?;
///             self.file_count += 1;
///             println!("Created a data file at {:?} (file count: {})", path, self.file_count);
///
///             // Return the path.
///             return Ok(path);
///         }
///     }
///
///     // Other associate functions and methods are unchanged.
/// #
/// #   fn read_data_file(&self, path: impl AsRef<Path>) -> io::Result<String> {
/// #       fs::read_to_string(path)
/// #   }
/// #
/// #   fn remove_data_file(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
/// #       fs::remove_file(path.as_ref())?;
/// #       self.file_count -= 1;
/// #       println!(
/// #           "Removed a data file at {:?} (file count: {})",
/// #           path.as_ref(),
/// #           self.file_count
/// #       );
/// #
/// #       Ok(())
/// #   }
/// }
///
/// fn main() -> anyhow::Result<()> {
///     // (Omitted unchanged lines)
///
/// #   let file_mgr = DataFileManager::new(std::env::temp_dir());
/// #   let file_mgr = Arc::new(RwLock::new(file_mgr));
/// #
/// #   let file_mgr1 = Arc::clone(&file_mgr);
/// #
///     // Create an eviction lister closure.
///     // let listener = ...
///
/// #   let listener = move |k, v: PathBuf, cause| {
/// #       println!(
/// #           "\n== An entry has been evicted. k: {:?}, v: {:?}, cause: {:?}",
/// #           k, v, cause
/// #       );
/// #
/// #       match file_mgr1.write() {
/// #           Err(_e) => {
/// #               eprintln!("The lock has been poisoned");
/// #           }
/// #           Ok(mut mgr) => {
/// #               if let Err(_e) = mgr.remove_data_file(v.as_path()) {
/// #                   eprintln!("Failed to remove a data file at {:?}", v);
/// #               }
/// #           }
/// #       }
/// #   };
/// #
///     // Create a listener configuration with Queued delivery mode.
///     let listener_conf = notification::Configuration::builder()
///         .delivery_mode(notification::DeliveryMode::Queued)
///         .build();
///
///     // Create the cache.
///     let cache = Cache::builder()
///         .max_capacity(100)
///         .time_to_live(Duration::from_secs(2))
///         // Set the eviction listener with the configuration.
///         .eviction_listener_with_conf(listener, listener_conf)
///         .build();
///
///     // Insert an entry to the cache.
///     // ...
/// #   println!("== try_get_with()");
/// #   let key = "user1";
/// #   let path = cache
/// #       .try_get_with(key, || -> anyhow::Result<_> {
/// #           let mut mgr = file_mgr
/// #               .write()
/// #               .map_err(|_e| anyhow::anyhow!("The lock has been poisoned"))?;
/// #           let path = mgr
/// #               .write_data_file(key, "user data".into())
/// #               .with_context(|| format!("Failed to create a data file"))?;
/// #           Ok(path)
/// #       })
/// #       .map_err(|e| anyhow!("{}", e))?;
/// #
///     // Read the data file at the path and print the contents.
///     // ...
/// #   println!("\n== read_data_file()");
/// #   {
/// #       let mgr = file_mgr
/// #           .read()
/// #           .map_err(|_e| anyhow::anyhow!("The lock has been poisoned"))?;
/// #       let contents = mgr
/// #           .read_data_file(path.as_path())
/// #           .with_context(|| format!("Failed to read data from {:?}", path))?;
/// #       println!("contents: {}", contents);
/// #   }
/// #
///     // Sleep for five seconds.
///     // ...
/// #   std::thread::sleep(Duration::from_secs(5));
///
///     Ok(())
/// }
/// ```
///
/// As you can see, `DataFileManager::write_data_file` method no longer uses the
/// cache key for the file name. Instead, it generates a UUID-based unique file name
/// on each call. This kind of treatment will be needed for `Queued` mode because
/// notifications will be delivered with some delay.
///
/// For example, a user thread could do the followings:
///
/// 1. `insert` an entry, and create a file.
/// 2. The entry is evicted due to size constraint:
///     - This will trigger an eviction notification but it will be fired some time
///       later.
///     - The notification listener will remove the file when it is called, but we
///       cannot predict when the call would be made.
/// 3. `insert` the entry again, and create the file again.
///
/// In `Queued` mode, the notification of the eviction at step 2 can be delivered
/// either before or after the re-`insert` at step 3. If the `write_data_file` method
/// does not generate unique file name on each call and the notification has not been
/// delivered before step 3, the user thread could overwrite the file created at
/// step 1. And then the notification will be delivered and the eviction listener
/// will remove a wrong file created at step 3 (instead of the correct one created at
/// step 1). This will cause the cache entires and the files on the filesystem to
/// become out of sync.
///
/// Generating unique file names prevents this problem, as the user thread will never
/// overwrite the file created at step 1 and the eviction lister will never remove a
/// wrong file.
///
/// # Thread Safety
///
/// All methods provided by the `Cache` are considered thread-safe, and can be safely
/// accessed by multiple concurrent threads.
///
/// - `Cache<K, V, S>` requires trait bounds `Send`, `Sync` and `'static` for `K`
///   (key), `V` (value) and `S` (hasher state).
/// - `Cache<K, V, S>` will implement `Send` and `Sync`.
///
/// # Sharing a cache across threads
///
/// To share a cache across threads, do one of the followings:
///
/// - Create a clone of the cache by calling its `clone` method and pass it to other
///   thread.
/// - Wrap the cache by a `sync::OnceCell` or `sync::Lazy` from
///   [once_cell][once-cell-crate] create, and set it to a `static` variable.
///
/// Cloning is a cheap operation for `Cache` as it only creates thread-safe
/// reference-counted pointers to the internal data structures.
///
/// [once-cell-crate]: https://crates.io/crates/once_cell
///
/// # Hashing Algorithm
///
/// By default, `Cache` uses a hashing algorithm selected to provide resistance
/// against HashDoS attacks. It will be the same one used by
/// `std::collections::HashMap`, which is currently SipHash 1-3.
///
/// While SipHash's performance is very competitive for medium sized keys, other
/// hashing algorithms will outperform it for small keys such as integers as well as
/// large keys such as long strings. However those algorithms will typically not
/// protect against attacks such as HashDoS.
///
/// The hashing algorithm can be replaced on a per-`Cache` basis using the
/// [`build_with_hasher`][build-with-hasher-method] method of the `CacheBuilder`.
/// Many alternative algorithms are available on crates.io, such as the
/// [aHash][ahash-crate] crate.
///
/// [build-with-hasher-method]: ./struct.CacheBuilder.html#method.build_with_hasher
/// [ahash-crate]: https://crates.io/crates/ahash
///
pub struct Cache<K, V, S = RandomState> {
    base: BaseCache<K, V, S>,
    value_initializer: Arc<ValueInitializer<K, V, S>>,
}

// TODO: https://github.com/moka-rs/moka/issues/54
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<K, V, S> Send for Cache<K, V, S>
where
    K: Send + Sync,
    V: Send + Sync,
    S: Send,
{
}

unsafe impl<K, V, S> Sync for Cache<K, V, S>
where
    K: Send + Sync,
    V: Send + Sync,
    S: Sync,
{
}

// NOTE: We cannot do `#[derive(Clone)]` because it will add `Clone` bound to `K`.
impl<K, V, S> Clone for Cache<K, V, S> {
    /// Makes a clone of this shared cache.
    ///
    /// This operation is cheap as it only creates thread-safe reference counted
    /// pointers to the shared internal data structures.
    fn clone(&self) -> Self {
        Self {
            base: self.base.clone(),
            value_initializer: Arc::clone(&self.value_initializer),
        }
    }
}

impl<K, V, S> fmt::Debug for Cache<K, V, S>
where
    K: fmt::Debug + Eq + Hash + Send + Sync + 'static,
    V: fmt::Debug + Clone + Send + Sync + 'static,
    // TODO: Remove these bounds from S.
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d_map = f.debug_map();

        for (k, v) in self.iter() {
            d_map.entry(&k, &v);
        }

        d_map.finish()
    }
}

impl<K, V, S> Cache<K, V, S> {
    /// Returns cache’s name.
    pub fn name(&self) -> Option<&str> {
        self.base.name()
    }

    /// Returns a read-only cache policy of this cache.
    ///
    /// At this time, cache policy cannot be modified after cache creation.
    /// A future version may support to modify it.
    pub fn policy(&self) -> Policy {
        self.base.policy()
    }

    /// Returns an approximate number of entries in this cache.
    ///
    /// The value returned is _an estimate_; the actual count may differ if there are
    /// concurrent insertions or removals, or if some entries are pending removal due
    /// to expiration. This inaccuracy can be mitigated by performing a `sync()`
    /// first.
    ///
    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    ///
    /// let cache = Cache::new(10);
    /// cache.insert('n', "Netherland Dwarf");
    /// cache.insert('l', "Lop Eared");
    /// cache.insert('d', "Dutch");
    ///
    /// // Ensure an entry exists.
    /// assert!(cache.contains_key(&'n'));
    ///
    /// // However, followings may print stale number zeros instead of threes.
    /// println!("{}", cache.entry_count());   // -> 0
    /// println!("{}", cache.weighted_size()); // -> 0
    ///
    /// // To mitigate the inaccuracy, bring `ConcurrentCacheExt` trait to
    /// // the scope so we can use `sync` method.
    /// use moka::sync::ConcurrentCacheExt;
    /// // Call `sync` to run pending internal tasks.
    /// cache.sync();
    ///
    /// // Followings will print the actual numbers.
    /// println!("{}", cache.entry_count());   // -> 3
    /// println!("{}", cache.weighted_size()); // -> 3
    /// ```
    ///
    pub fn entry_count(&self) -> u64 {
        self.base.entry_count()
    }

    /// Returns an approximate total weighted size of entries in this cache.
    ///
    /// The value returned is _an estimate_; the actual size may differ if there are
    /// concurrent insertions or removals, or if some entries are pending removal due
    /// to expiration. This inaccuracy can be mitigated by performing a `sync()`
    /// first. See [`entry_count`](#method.entry_count) for a sample code.
    pub fn weighted_size(&self) -> u64 {
        self.base.weighted_size()
    }
}

impl<K, V> Cache<K, V, RandomState>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Constructs a new `Cache<K, V>` that will store up to the `max_capacity`.
    ///
    /// To adjust various configuration knobs such as `initial_capacity` or
    /// `time_to_live`, use the [`CacheBuilder`][builder-struct].
    ///
    /// [builder-struct]: ./struct.CacheBuilder.html
    pub fn new(max_capacity: u64) -> Self {
        let build_hasher = RandomState::default();
        let housekeeper_conf = housekeeper::Configuration::new_thread_pool(true);
        Self::with_everything(
            None,
            Some(max_capacity),
            None,
            build_hasher,
            None,
            None,
            None,
            None,
            None,
            false,
            housekeeper_conf,
        )
    }

    /// Returns a [`CacheBuilder`][builder-struct], which can builds a `Cache` or
    /// `SegmentedCache` with various configuration knobs.
    ///
    /// [builder-struct]: ./struct.CacheBuilder.html
    pub fn builder() -> CacheBuilder<K, V, Cache<K, V, RandomState>> {
        CacheBuilder::default()
    }
}

impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    // https://rust-lang.github.io/rust-clippy/master/index.html#too_many_arguments
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_everything(
        name: Option<String>,
        max_capacity: Option<u64>,
        initial_capacity: Option<usize>,
        build_hasher: S,
        weigher: Option<Weigher<K, V>>,
        eviction_listener: Option<EvictionListener<K, V>>,
        eviction_listener_conf: Option<notification::Configuration>,
        time_to_live: Option<Duration>,
        time_to_idle: Option<Duration>,
        invalidator_enabled: bool,
        housekeeper_conf: housekeeper::Configuration,
    ) -> Self {
        Self {
            base: BaseCache::new(
                name,
                max_capacity,
                initial_capacity,
                build_hasher.clone(),
                weigher,
                eviction_listener,
                eviction_listener_conf,
                time_to_live,
                time_to_idle,
                invalidator_enabled,
                housekeeper_conf,
            ),
            value_initializer: Arc::new(ValueInitializer::with_hasher(build_hasher)),
        }
    }

    /// Returns `true` if the cache contains a value for the key.
    ///
    /// Unlike the `get` method, this method is not considered a cache read operation,
    /// so it does not update the historic popularity estimator or reset the idle
    /// timer for the key.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.base.contains_key_with_hash(key, self.base.hash(key))
    }

    pub(crate) fn contains_key_with_hash<Q>(&self, key: &Q, hash: u64) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.base.contains_key_with_hash(key, hash)
    }

    /// Returns a _clone_ of the value corresponding to the key.
    ///
    /// If you want to store values that will be expensive to clone, wrap them by
    /// `std::sync::Arc` before storing in a cache. [`Arc`][rustdoc-std-arc] is a
    /// thread-safe reference-counted pointer and its `clone()` method is cheap.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    ///
    /// [rustdoc-std-arc]: https://doc.rust-lang.org/stable/std/sync/struct.Arc.html
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.base.get_with_hash(key, self.base.hash(key))
    }

    pub(crate) fn get_with_hash<Q>(&self, key: &Q, hash: u64) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.base.get_with_hash(key, hash)
    }

    /// Deprecated, replaced with [`get_with`](#method.get_with)
    #[deprecated(since = "0.8.0", note = "Replaced with `get_with`")]
    pub fn get_or_insert_with(&self, key: K, init: impl FnOnce() -> V) -> V {
        self.get_with(key, init)
    }

    /// Deprecated, replaced with [`try_get_with`](#method.try_get_with)
    #[deprecated(since = "0.8.0", note = "Replaced with `try_get_with`")]
    pub fn get_or_try_insert_with<F, E>(&self, key: K, init: F) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
    {
        self.try_get_with(key, init)
    }

    /// Ensures the value of the key exists by inserting the output of the `init`
    /// closure if not exist, and returns a _clone_ of the value.
    ///
    /// This method prevents to evaluate the `init` closure multiple times on the
    /// same key even if the method is concurrently called by many threads; only one
    /// of the calls evaluates its closure, and other calls wait for that closure to
    /// complete.
    ///
    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    /// use std::{sync::Arc, thread};
    ///
    /// const TEN_MIB: usize = 10 * 1024 * 1024; // 10MiB
    /// let cache = Cache::new(100);
    ///
    /// // Spawn four threads.
    /// let threads: Vec<_> = (0..4_u8)
    ///     .map(|task_id| {
    ///         let my_cache = cache.clone();
    ///         thread::spawn(move || {
    ///             println!("Thread {} started.", task_id);
    ///
    ///             // Try to insert and get the value for key1. Although all four
    ///             // threads will call `get_with` at the same time, the `init` closure
    ///             // must be evaluated only once.
    ///             let value = my_cache.get_with("key1", || {
    ///                 println!("Thread {} inserting a value.", task_id);
    ///                 Arc::new(vec![0u8; TEN_MIB])
    ///             });
    ///
    ///             // Ensure the value exists now.
    ///             assert_eq!(value.len(), TEN_MIB);
    ///             assert!(my_cache.get(&"key1").is_some());
    ///
    ///             println!("Thread {} got the value. (len: {})", task_id, value.len());
    ///         })
    ///     })
    ///     .collect();
    ///
    /// // Wait all threads to complete.
    /// threads
    ///     .into_iter()
    ///     .for_each(|t| t.join().expect("Thread failed"));
    /// ```
    ///
    /// **Result**
    ///
    /// - The `init` closure was called exactly once by thread 1.
    /// - Other threads were blocked until thread 1 inserted the value.
    ///
    /// ```console
    /// Thread 1 started.
    /// Thread 0 started.
    /// Thread 3 started.
    /// Thread 2 started.
    /// Thread 1 inserting a value.
    /// Thread 2 got the value. (len: 10485760)
    /// Thread 1 got the value. (len: 10485760)
    /// Thread 0 got the value. (len: 10485760)
    /// Thread 3 got the value. (len: 10485760)
    /// ```
    ///
    /// # Panics
    ///
    /// This method panics when the `init` closure has panicked. When it happens,
    /// only the caller whose `init` closure panicked will get the panic (e.g. only
    /// thread 1 in the above sample). If there are other calls in progress (e.g.
    /// thread 0, 2 and 3 above), this method will restart and resolve one of the
    /// remaining `init` closure.
    ///
    pub fn get_with(&self, key: K, init: impl FnOnce() -> V) -> V {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        let replace_if = None as Option<fn(&V) -> bool>;
        self.get_or_insert_with_hash_and_fun(key, hash, init, replace_if)
    }

    /// Similar to [`get_with`](#method.get_with), but instead of passing an owned
    /// key, you can pass a reference to the key. If the key does not exist in the
    /// cache, the key will be cloned to create new entry in the cache.
    pub fn get_with_by_ref<Q>(&self, key: &Q, init: impl FnOnce() -> V) -> V
    where
        K: Borrow<Q>,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    {
        let hash = self.base.hash(key);
        let replace_if = None as Option<fn(&V) -> bool>;

        self.get_or_insert_with_hash_by_ref_and_fun(key, hash, init, replace_if)
    }

    /// Works like [`get_with`](#method.get_with), but takes an additional
    /// `replace_if` closure.
    ///
    /// This method will evaluate the `init` closure and insert the output to the
    /// cache when:
    ///
    /// - The key does not exist.
    /// - Or, `replace_if` closure returns `true`.
    pub fn get_with_if(
        &self,
        key: K,
        init: impl FnOnce() -> V,
        replace_if: impl FnMut(&V) -> bool,
    ) -> V {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        self.get_or_insert_with_hash_and_fun(key, hash, init, Some(replace_if))
    }

    // We will provide this API under the new `entry` API.
    //
    // /// Similar to [`get_with_if`](#method.get_with_if), but instead of passing an
    // /// owned key, you can pass a reference to the key. If the key does not exist in
    // /// the cache, the key will be cloned to create new entry in the cache.
    // pub fn get_with_if_by_ref<Q>(
    //     &self,
    //     key: &Q,
    //     init: impl FnOnce() -> V,
    //     replace_if: impl FnMut(&V) -> bool,
    // ) -> V
    // where
    //     K: Borrow<Q>,
    //     Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    // {
    //     let hash = self.base.hash(key);
    //     self.get_or_insert_with_hash_by_ref_and_fun(key, hash, init, Some(replace_if))
    // }

    pub(crate) fn get_or_insert_with_hash_and_fun(
        &self,
        key: Arc<K>,
        hash: u64,
        init: impl FnOnce() -> V,
        mut replace_if: Option<impl FnMut(&V) -> bool>,
    ) -> V {
        self.base
            .get_with_hash_but_ignore_if(&key, hash, replace_if.as_mut())
            .unwrap_or_else(|| self.insert_with_hash_and_fun(key, hash, init, replace_if))
    }

    // Need to create new function instead of using the existing
    // `get_or_insert_with_hash_and_fun`. The reason is `by_ref` function will
    // require key reference to have `ToOwned` trait. If we modify the existing
    // `get_or_insert_with_hash_and_fun` function, it will require all the existing
    // apis that depends on it to make the `K` to have `ToOwned` trait.
    pub(crate) fn get_or_insert_with_hash_by_ref_and_fun<Q>(
        &self,
        key: &Q,
        hash: u64,
        init: impl FnOnce() -> V,
        mut replace_if: Option<impl FnMut(&V) -> bool>,
    ) -> V
    where
        K: Borrow<Q>,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    {
        self.base
            .get_with_hash_but_ignore_if(key, hash, replace_if.as_mut())
            .unwrap_or_else(|| {
                let key = Arc::new(key.to_owned());
                self.insert_with_hash_and_fun(key, hash, init, replace_if)
            })
    }

    pub(crate) fn insert_with_hash_and_fun(
        &self,
        key: Arc<K>,
        hash: u64,
        init: impl FnOnce() -> V,
        mut replace_if: Option<impl FnMut(&V) -> bool>,
    ) -> V {
        let get = || {
            self.base
                .get_with_hash_but_no_recording(&key, hash, replace_if.as_mut())
        };
        let insert = |v| self.insert_with_hash(key.clone(), hash, v);

        match self
            .value_initializer
            .init_or_read(Arc::clone(&key), get, init, insert)
        {
            InitResult::Initialized(v) => {
                crossbeam_epoch::pin().flush();
                v
            }
            InitResult::ReadExisting(v) => v,
            InitResult::InitErr(_) => unreachable!(),
        }
    }

    /// Try to ensure the value of the key exists by inserting an `Ok` result of the
    /// init closure if not exist, and returns a _clone_ of the value or the `Err`
    /// returned by the closure.
    ///
    /// This method prevents to evaluate the init closure multiple times on the same
    /// key even if the method is concurrently called by many threads; only one of
    /// the calls evaluates its closure (as long as these closures return the same
    /// error type), and other calls wait for that closure to complete.
    ///
    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    /// use std::{path::Path, thread};
    ///
    /// /// This function tries to get the file size in bytes.
    /// fn get_file_size(thread_id: u8, path: impl AsRef<Path>) -> Result<u64, std::io::Error> {
    ///     println!("get_file_size() called by thread {}.", thread_id);
    ///     Ok(std::fs::metadata(path)?.len())
    /// }
    ///
    /// let cache = Cache::new(100);
    ///
    /// // Spawn four threads.
    /// let threads: Vec<_> = (0..4_u8)
    ///     .map(|thread_id| {
    ///         let my_cache = cache.clone();
    ///         thread::spawn(move || {
    ///             println!("Thread {} started.", thread_id);
    ///
    ///             // Try to insert and get the value for key1. Although all four
    ///             // threads will call `try_get_with` at the same time,
    ///             // get_file_size() must be called only once.
    ///             let value = my_cache.try_get_with(
    ///                 "key1",
    ///                 || get_file_size(thread_id, "./Cargo.toml"),
    ///             );
    ///
    ///             // Ensure the value exists now.
    ///             assert!(value.is_ok());
    ///             assert!(my_cache.get(&"key1").is_some());
    ///
    ///             println!(
    ///                 "Thread {} got the value. (len: {})",
    ///                 thread_id,
    ///                 value.unwrap()
    ///             );
    ///         })
    ///     })
    ///     .collect();
    ///
    /// // Wait all threads to complete.
    /// threads
    ///     .into_iter()
    ///     .for_each(|t| t.join().expect("Thread failed"));
    /// ```
    ///
    /// **Result**
    ///
    /// - `get_file_size()` was called exactly once by thread 1.
    /// - Other threads were blocked until thread 1 inserted the value.
    ///
    /// ```console
    /// Thread 1 started.
    /// Thread 2 started.
    /// get_file_size() called by thread 1.
    /// Thread 3 started.
    /// Thread 0 started.
    /// Thread 2 got the value. (len: 1466)
    /// Thread 0 got the value. (len: 1466)
    /// Thread 1 got the value. (len: 1466)
    /// Thread 3 got the value. (len: 1466)
    /// ```
    ///
    /// # Panics
    ///
    /// This method panics when the `init` closure has panicked. When it happens,
    /// only the caller whose `init` closure panicked will get the panic (e.g. only
    /// thread 1 in the above sample). If there are other calls in progress (e.g.
    /// thread 0, 2 and 3 above), this method will restart and resolve one of the
    /// remaining `init` closure.
    ///
    pub fn try_get_with<F, E>(&self, key: K, init: F) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
    {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        self.get_or_try_insert_with_hash_and_fun(key, hash, init)
    }

    /// Similar to [`try_get_with`](#method.try_get_with), but instead of passing an
    /// owned key, you can pass a reference to the key. If the key does not exist in
    /// the cache, the key will be cloned to create new entry in the cache.
    pub fn try_get_with_by_ref<F, E, Q>(&self, key: &Q, init: F) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
        K: Borrow<Q>,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    {
        let hash = self.base.hash(key);
        self.get_or_try_insert_with_hash_by_ref_and_fun(key, hash, init)
    }

    pub(crate) fn get_or_try_insert_with_hash_and_fun<F, E>(
        &self,
        key: Arc<K>,
        hash: u64,
        init: F,
    ) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
    {
        if let Some(v) = self.get_with_hash(&key, hash) {
            return Ok(v);
        }

        self.try_insert_with_hash_and_fun(key, hash, init)
    }

    pub(crate) fn get_or_try_insert_with_hash_by_ref_and_fun<F, Q, E>(
        &self,
        key: &Q,
        hash: u64,
        init: F,
    ) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
        K: Borrow<Q>,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    {
        if let Some(v) = self.get_with_hash(key, hash) {
            return Ok(v);
        }

        let key = Arc::new(key.to_owned());
        self.try_insert_with_hash_and_fun(key, hash, init)
    }

    pub(crate) fn try_insert_with_hash_and_fun<F, E>(
        &self,
        key: Arc<K>,
        hash: u64,
        init: F,
    ) -> Result<V, Arc<E>>
    where
        F: FnOnce() -> Result<V, E>,
        E: Send + Sync + 'static,
    {
        let get = || {
            let ignore_if = None as Option<&mut fn(&V) -> bool>;
            self.base
                .get_with_hash_but_no_recording(&key, hash, ignore_if)
        };
        let insert = |v| self.insert_with_hash(key.clone(), hash, v);

        match self
            .value_initializer
            .try_init_or_read(Arc::clone(&key), get, init, insert)
        {
            InitResult::Initialized(v) => {
                crossbeam_epoch::pin().flush();
                Ok(v)
            }
            InitResult::ReadExisting(v) => Ok(v),
            InitResult::InitErr(e) => {
                crossbeam_epoch::pin().flush();
                Err(e)
            }
        }
    }

    /// Try to ensure the value of the key exists by inserting an `Some` result of
    /// the init closure if not exist, and returns a _clone_ of the value or `None`
    /// returned by the closure.
    ///
    /// This method prevents to evaluate the init closure multiple times on the same
    /// key even if the method is concurrently called by many threads; only one of
    /// the calls evaluates its closure (as long as these closures return the same
    /// Option type), and other calls wait for that closure to complete.
    ///
    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    /// use std::{path::Path, thread};
    ///
    /// /// This function tries to get the file size in bytes.
    /// fn get_file_size(thread_id: u8, path: impl AsRef<Path>) -> Option<u64> {
    ///     println!("get_file_size() called by thread {}.", thread_id);
    ///     std::fs::metadata(path).ok().map(|m| m.len())
    /// }
    ///
    /// let cache = Cache::new(100);
    ///
    /// // Spawn four threads.
    /// let threads: Vec<_> = (0..4_u8)
    ///     .map(|thread_id| {
    ///         let my_cache = cache.clone();
    ///         thread::spawn(move || {
    ///             println!("Thread {} started.", thread_id);
    ///
    ///             // Try to insert and get the value for key1. Although all four
    ///             // threads will call `optionally_get_with` at the same time,
    ///             // get_file_size() must be called only once.
    ///             let value = my_cache.optionally_get_with(
    ///                 "key1",
    ///                 || get_file_size(thread_id, "./Cargo.toml"),
    ///             );
    ///
    ///             // Ensure the value exists now.
    ///             assert!(value.is_some());
    ///             assert!(my_cache.get(&"key1").is_some());
    ///
    ///             println!(
    ///                 "Thread {} got the value. (len: {})",
    ///                 thread_id,
    ///                 value.unwrap()
    ///             );
    ///         })
    ///     })
    ///     .collect();
    ///
    /// // Wait all threads to complete.
    /// threads
    ///     .into_iter()
    ///     .for_each(|t| t.join().expect("Thread failed"));
    /// ```
    ///
    /// **Result**
    ///
    /// - `get_file_size()` was called exactly once by thread 0.
    /// - Other threads were blocked until thread 0 inserted the value.
    ///
    /// ```console
    /// Thread 0 started.
    /// Thread 1 started.
    /// Thread 2 started.
    /// get_file_size() called by thread 0.
    /// Thread 3 started.
    /// Thread 2 got the value. (len: 1466)
    /// Thread 0 got the value. (len: 1466)
    /// Thread 1 got the value. (len: 1466)
    /// Thread 3 got the value. (len: 1466)
    /// ```
    ///
    /// # Panics
    ///
    /// This method panics when the `init` closure has panicked. When it happens,
    /// only the caller whose `init` closure panicked will get the panic (e.g. only
    /// thread 1 in the above sample). If there are other calls in progress (e.g.
    /// thread 0, 2 and 3 above), this method will restart and resolve one of the
    /// remaining `init` closure.
    ///
    pub fn optionally_get_with<F>(&self, key: K, init: F) -> Option<V>
    where
        F: FnOnce() -> Option<V>,
    {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);

        self.get_or_optionally_insert_with_hash_and_fun(key, hash, init)
    }

    /// Similar to [`optionally_get_with`](#method.optionally_get_with), but instead
    /// of passing an owned key, you can pass a reference to the key. If the key does
    /// not exist in the cache, the key will be cloned to create new entry in the
    /// cache.
    pub fn optionally_get_with_by_ref<F, Q>(&self, key: &Q, init: F) -> Option<V>
    where
        F: FnOnce() -> Option<V>,
        K: Borrow<Q>,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    {
        let hash = self.base.hash(key);
        self.get_or_optionally_insert_with_hash_by_ref_and_fun(key, hash, init)
    }

    pub(super) fn get_or_optionally_insert_with_hash_and_fun<F>(
        &self,
        key: Arc<K>,
        hash: u64,
        init: F,
    ) -> Option<V>
    where
        F: FnOnce() -> Option<V>,
    {
        let res = self.get_with_hash(&key, hash);
        if res.is_some() {
            return res;
        }

        self.optionally_insert_with_hash_and_fun(key, hash, init)
    }

    pub(super) fn get_or_optionally_insert_with_hash_by_ref_and_fun<F, Q>(
        &self,
        key: &Q,
        hash: u64,
        init: F,
    ) -> Option<V>
    where
        F: FnOnce() -> Option<V>,
        K: Borrow<Q>,
        Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    {
        let res = self.get_with_hash(key, hash);
        if res.is_some() {
            return res;
        }

        let key = Arc::new(key.to_owned());
        self.optionally_insert_with_hash_and_fun(key, hash, init)
    }

    pub(super) fn optionally_insert_with_hash_and_fun<F>(
        &self,
        key: Arc<K>,
        hash: u64,
        init: F,
    ) -> Option<V>
    where
        F: FnOnce() -> Option<V>,
    {
        let get = || {
            let ignore_if = None as Option<&mut fn(&V) -> bool>;
            self.base
                .get_with_hash_but_no_recording(&key, hash, ignore_if)
        };
        let insert = |v| self.insert_with_hash(key.clone(), hash, v);

        match self
            .value_initializer
            .optionally_init_or_read(Arc::clone(&key), get, init, insert)
        {
            InitResult::Initialized(v) => {
                crossbeam_epoch::pin().flush();
                Some(v)
            }
            InitResult::ReadExisting(v) => Some(v),
            InitResult::InitErr(_) => {
                crossbeam_epoch::pin().flush();
                None
            }
        }
    }

    /// Inserts a key-value pair into the cache.
    ///
    /// If the cache has this key present, the value is updated.
    pub fn insert(&self, key: K, value: V) {
        let hash = self.base.hash(&key);
        let key = Arc::new(key);
        self.insert_with_hash(key, hash, value)
    }

    pub(crate) fn insert_with_hash(&self, key: Arc<K>, hash: u64, value: V) {
        let (op, now) = self.base.do_insert_with_hash(key, hash, value);
        let hk = self.base.housekeeper.as_ref();
        Self::schedule_write_op(
            self.base.inner.as_ref(),
            &self.base.write_op_ch,
            op,
            now,
            hk,
        )
        .expect("Failed to insert");
    }

    /// Discards any cached value for the key.
    ///
    /// The key may be any borrowed form of the cache's key type, but `Hash` and `Eq`
    /// on the borrowed form _must_ match those for the key type.
    pub fn invalidate<Q>(&self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.base.hash(key);
        self.invalidate_with_hash(key, hash);
    }

    pub(crate) fn invalidate_with_hash<Q>(&self, key: &Q, hash: u64)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        // Lock the key for removal if blocking removal notification is enabled.
        let mut kl = None;
        let mut klg = None;
        if self.base.is_removal_notifier_enabled() && self.base.is_blocking_removal_notification() {
            // To lock the key, we have to get Arc<K> for key (&Q).
            //
            // TODO: Enhance this if possible. This is rather hack now because
            // it cannot prevent race conditions like this:
            //
            // 1. We miss the key because it does not exist. So we do not lock
            //    the key.
            // 2. Somebody else (other thread) inserts the key.
            // 3. We remove the entry for the key, but without the key lock!
            //
            if let Some(arc_key) = self.base.get_key_with_hash(key, hash) {
                kl = self.base.maybe_key_lock(&arc_key);
                klg = kl.as_ref().map(|kl| kl.lock());
            }
        }

        if let Some(kv) = self.base.remove_entry(key, hash) {
            if self.base.is_removal_notifier_enabled() {
                self.base.notify_invalidate(&kv.key, &kv.entry)
            }
            // Drop the locks before scheduling write op to avoid a potential dead lock.
            // (Scheduling write can do spin lock when the queue is full, and queue will
            // be drained by the housekeeping thread that can lock the same key)
            std::mem::drop(klg);
            std::mem::drop(kl);

            let op = WriteOp::Remove(kv);
            let now = self.base.current_time_from_expiration_clock();
            let hk = self.base.housekeeper.as_ref();
            Self::schedule_write_op(
                self.base.inner.as_ref(),
                &self.base.write_op_ch,
                op,
                now,
                hk,
            )
            .expect("Failed to remove");
            crossbeam_epoch::pin().flush();
        }
    }

    /// Discards all cached values.
    ///
    /// This method returns immediately and a background thread will evict all the
    /// cached values inserted before the time when this method was called. It is
    /// guaranteed that the `get` method must not return these invalidated values
    /// even if they have not been evicted.
    ///
    /// Like the `invalidate` method, this method does not clear the historic
    /// popularity estimator of keys so that it retains the client activities of
    /// trying to retrieve an item.
    pub fn invalidate_all(&self) {
        self.base.invalidate_all();
    }

    /// Discards cached values that satisfy a predicate.
    ///
    /// `invalidate_entries_if` takes a closure that returns `true` or `false`. This
    /// method returns immediately and a background thread will apply the closure to
    /// each cached value inserted before the time when `invalidate_entries_if` was
    /// called. If the closure returns `true` on a value, that value will be evicted
    /// from the cache.
    ///
    /// Also the `get` method will apply the closure to a value to determine if it
    /// should have been invalidated. Therefore, it is guaranteed that the `get`
    /// method must not return invalidated values.
    ///
    /// Note that you must call
    /// [`CacheBuilder::support_invalidation_closures`][support-invalidation-closures]
    /// at the cache creation time as the cache needs to maintain additional internal
    /// data structures to support this method. Otherwise, calling this method will
    /// fail with a
    /// [`PredicateError::InvalidationClosuresDisabled`][invalidation-disabled-error].
    ///
    /// Like the `invalidate` method, this method does not clear the historic
    /// popularity estimator of keys so that it retains the client activities of
    /// trying to retrieve an item.
    ///
    /// [support-invalidation-closures]: ./struct.CacheBuilder.html#method.support_invalidation_closures
    /// [invalidation-disabled-error]: ../enum.PredicateError.html#variant.InvalidationClosuresDisabled
    pub fn invalidate_entries_if<F>(&self, predicate: F) -> Result<PredicateId, PredicateError>
    where
        F: Fn(&K, &V) -> bool + Send + Sync + 'static,
    {
        self.base.invalidate_entries_if(Arc::new(predicate))
    }

    pub(crate) fn invalidate_entries_with_arc_fun<F>(
        &self,
        predicate: Arc<F>,
    ) -> Result<PredicateId, PredicateError>
    where
        F: Fn(&K, &V) -> bool + Send + Sync + 'static,
    {
        self.base.invalidate_entries_if(predicate)
    }

    /// Creates an iterator visiting all key-value pairs in arbitrary order. The
    /// iterator element type is `(Arc<K>, V)`, where `V` is a clone of a stored
    /// value.
    ///
    /// Iterators do not block concurrent reads and writes on the cache. An entry can
    /// be inserted to, invalidated or evicted from a cache while iterators are alive
    /// on the same cache.
    ///
    /// Unlike the `get` method, visiting entries via an iterator do not update the
    /// historic popularity estimator or reset idle timers for keys.
    ///
    /// # Guarantees
    ///
    /// In order to allow concurrent access to the cache, iterator's `next` method
    /// does _not_ guarantee the following:
    ///
    /// - It does not guarantee to return a key-value pair (an entry) if its key has
    ///   been inserted to the cache _after_ the iterator was created.
    ///   - Such an entry may or may not be returned depending on key's hash and
    ///     timing.
    ///
    /// and the `next` method guarantees the followings:
    ///
    /// - It guarantees not to return the same entry more than once.
    /// - It guarantees not to return an entry if it has been removed from the cache
    ///   after the iterator was created.
    ///     - Note: An entry can be removed by following reasons:
    ///         - Manually invalidated.
    ///         - Expired (e.g. time-to-live).
    ///         - Evicted as the cache capacity exceeded.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use moka::sync::Cache;
    ///
    /// let cache = Cache::new(100);
    /// cache.insert("Julia", 14);
    ///
    /// let mut iter = cache.iter();
    /// let (k, v) = iter.next().unwrap(); // (Arc<K>, V)
    /// assert_eq!(*k, "Julia");
    /// assert_eq!(v, 14);
    ///
    /// assert!(iter.next().is_none());
    /// ```
    ///
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter::with_single_cache_segment(&self.base, self.num_cht_segments())
    }
}

impl<'a, K, V, S> IntoIterator for &'a Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    type Item = (Arc<K>, V);

    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<K, V, S> ConcurrentCacheExt<K, V> for Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn sync(&self) {
        self.base.inner.sync(MAX_SYNC_REPEATS);
    }
}

//
// Iterator support
//
impl<K, V, S> ScanningGet<K, V> for Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn num_cht_segments(&self) -> usize {
        self.base.num_cht_segments()
    }

    fn scanning_get(&self, key: &Arc<K>) -> Option<V> {
        self.base.scanning_get(key)
    }

    fn keys(&self, cht_segment: usize) -> Option<Vec<Arc<K>>> {
        self.base.keys(cht_segment)
    }
}

//
// private methods
//
impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    #[inline]
    fn schedule_write_op(
        inner: &impl InnerSync,
        ch: &Sender<WriteOp<K, V>>,
        op: WriteOp<K, V>,
        now: Instant,
        housekeeper: Option<&HouseKeeperArc<K, V, S>>,
    ) -> Result<(), TrySendError<WriteOp<K, V>>> {
        let mut op = op;

        // NOTES:
        // - This will block when the channel is full.
        // - We are doing a busy-loop here. We were originally calling `ch.send(op)?`,
        //   but we got a notable performance degradation.
        loop {
            BaseCache::apply_reads_writes_if_needed(inner, ch, now, housekeeper);
            match ch.try_send(op) {
                Ok(()) => break,
                Err(TrySendError::Full(op1)) => {
                    op = op1;
                    std::thread::sleep(Duration::from_micros(WRITE_RETRY_INTERVAL_MICROS));
                }
                Err(e @ TrySendError::Disconnected(_)) => return Err(e),
            }
        }
        Ok(())
    }
}

// For unit tests.
#[cfg(test)]
impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    pub(crate) fn is_table_empty(&self) -> bool {
        self.entry_count() == 0
    }

    pub(crate) fn invalidation_predicate_count(&self) -> usize {
        self.base.invalidation_predicate_count()
    }

    pub(crate) fn reconfigure_for_testing(&mut self) {
        self.base.reconfigure_for_testing();
    }

    pub(crate) fn set_expiration_clock(&self, clock: Option<crate::common::time::Clock>) {
        self.base.set_expiration_clock(clock);
    }
}

// To see the debug prints, run test as `cargo test -- --nocapture`
#[cfg(test)]
mod tests {
    use super::{Cache, ConcurrentCacheExt};
    use crate::{
        common::time::Clock,
        notification::{
            self,
            macros::{assert_eq_with_mode, assert_with_mode},
            DeliveryMode, RemovalCause,
        },
    };

    use parking_lot::Mutex;
    use std::{convert::Infallible, sync::Arc, time::Duration};

    #[test]
    fn basic_single_thread() {
        run_test(DeliveryMode::Immediate);
        run_test(DeliveryMode::Queued);

        fn run_test(delivery_mode: DeliveryMode) {
            // The following `Vec`s will hold actual and expected notifications.
            let actual = Arc::new(Mutex::new(Vec::new()));
            let mut expected = Vec::new();

            // Create an eviction listener.
            let a1 = Arc::clone(&actual);
            let listener = move |k, v, cause| a1.lock().push((k, v, cause));
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(delivery_mode)
                .build();

            // Create a cache with the eviction listener.
            let mut cache = Cache::builder()
                .max_capacity(3)
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.reconfigure_for_testing();

            // Make the cache exterior immutable.
            let cache = cache;

            cache.insert("a", "alice");
            cache.insert("b", "bob");
            assert_eq_with_mode!(cache.get(&"a"), Some("alice"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), Some("bob"), delivery_mode);
            cache.sync();
            // counts: a -> 1, b -> 1

            cache.insert("c", "cindy");
            assert_eq_with_mode!(cache.get(&"c"), Some("cindy"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"c"), delivery_mode);
            // counts: a -> 1, b -> 1, c -> 1
            cache.sync();

            assert_with_mode!(cache.contains_key(&"a"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"a"), Some("alice"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), Some("bob"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            cache.sync();
            // counts: a -> 2, b -> 2, c -> 1

            // "d" should not be admitted because its frequency is too low.
            cache.insert("d", "david"); //   count: d -> 0
            expected.push((Arc::new("d"), "david", RemovalCause::Size));
            cache.sync();
            assert_eq_with_mode!(cache.get(&"d"), None, delivery_mode); //   d -> 1
            assert_with_mode!(!cache.contains_key(&"d"), delivery_mode);

            cache.insert("d", "david");
            expected.push((Arc::new("d"), "david", RemovalCause::Size));
            cache.sync();
            assert_with_mode!(!cache.contains_key(&"d"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"d"), None, delivery_mode); //   d -> 2

            // "d" should be admitted and "c" should be evicted
            // because d's frequency is higher than c's.
            cache.insert("d", "dennis");
            expected.push((Arc::new("c"), "cindy", RemovalCause::Size));
            cache.sync();
            assert_eq_with_mode!(cache.get(&"a"), Some("alice"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), Some("bob"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"c"), None, delivery_mode);
            assert_eq_with_mode!(cache.get(&"d"), Some("dennis"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"c"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"d"), delivery_mode);

            cache.invalidate(&"b");
            expected.push((Arc::new("b"), "bob", RemovalCause::Explicit));
            cache.sync();
            assert_eq_with_mode!(cache.get(&"b"), None, delivery_mode);
            assert_with_mode!(!cache.contains_key(&"b"), delivery_mode);

            verify_notification_vec(&cache, actual, &expected, delivery_mode);
        }
    }

    #[test]
    fn size_aware_eviction() {
        run_test(DeliveryMode::Immediate);
        run_test(DeliveryMode::Queued);

        fn run_test(delivery_mode: DeliveryMode) {
            let weigher = |_k: &&str, v: &(&str, u32)| v.1;

            let alice = ("alice", 10);
            let bob = ("bob", 15);
            let bill = ("bill", 20);
            let cindy = ("cindy", 5);
            let david = ("david", 15);
            let dennis = ("dennis", 15);

            // The following `Vec`s will hold actual and expected notifications.
            let actual = Arc::new(Mutex::new(Vec::new()));
            let mut expected = Vec::new();

            // Create an eviction listener.
            let a1 = Arc::clone(&actual);
            let listener = move |k, v, cause| a1.lock().push((k, v, cause));
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(delivery_mode)
                .build();

            // Create a cache with the eviction listener.
            let mut cache = Cache::builder()
                .max_capacity(31)
                .weigher(weigher)
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.reconfigure_for_testing();

            // Make the cache exterior immutable.
            let cache = cache;

            cache.insert("a", alice);
            cache.insert("b", bob);
            assert_eq_with_mode!(cache.get(&"a"), Some(alice), delivery_mode);
            assert_with_mode!(cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), Some(bob), delivery_mode);
            cache.sync();
            // order (LRU -> MRU) and counts: a -> 1, b -> 1

            cache.insert("c", cindy);
            assert_eq_with_mode!(cache.get(&"c"), Some(cindy), delivery_mode);
            assert_with_mode!(cache.contains_key(&"c"), delivery_mode);
            // order and counts: a -> 1, b -> 1, c -> 1
            cache.sync();

            assert_with_mode!(cache.contains_key(&"a"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"a"), Some(alice), delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), Some(bob), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            cache.sync();
            // order and counts: c -> 1, a -> 2, b -> 2

            // To enter "d" (weight: 15), it needs to evict "c" (w: 5) and "a" (w: 10).
            // "d" must have higher count than 3, which is the aggregated count
            // of "a" and "c".
            cache.insert("d", david); //   count: d -> 0
            expected.push((Arc::new("d"), david, RemovalCause::Size));
            cache.sync();
            assert_eq_with_mode!(cache.get(&"d"), None, delivery_mode); //   d -> 1
            assert_with_mode!(!cache.contains_key(&"d"), delivery_mode);

            cache.insert("d", david);
            expected.push((Arc::new("d"), david, RemovalCause::Size));
            cache.sync();
            assert_with_mode!(!cache.contains_key(&"d"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"d"), None, delivery_mode); //   d -> 2

            cache.insert("d", david);
            expected.push((Arc::new("d"), david, RemovalCause::Size));
            cache.sync();
            assert_eq_with_mode!(cache.get(&"d"), None, delivery_mode); //   d -> 3
            assert_with_mode!(!cache.contains_key(&"d"), delivery_mode);

            cache.insert("d", david);
            expected.push((Arc::new("d"), david, RemovalCause::Size));
            cache.sync();
            assert_with_mode!(!cache.contains_key(&"d"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"d"), None, delivery_mode); //   d -> 4

            // Finally "d" should be admitted by evicting "c" and "a".
            cache.insert("d", dennis);
            expected.push((Arc::new("c"), cindy, RemovalCause::Size));
            expected.push((Arc::new("a"), alice, RemovalCause::Size));
            cache.sync();
            assert_eq_with_mode!(cache.get(&"a"), None, delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), Some(bob), delivery_mode);
            assert_eq_with_mode!(cache.get(&"c"), None, delivery_mode);
            assert_eq_with_mode!(cache.get(&"d"), Some(dennis), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"c"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"d"), delivery_mode);

            // Update "b" with "bill" (w: 15 -> 20). This should evict "d" (w: 15).
            cache.insert("b", bill);
            expected.push((Arc::new("b"), bob, RemovalCause::Replaced));
            expected.push((Arc::new("d"), dennis, RemovalCause::Size));
            cache.sync();
            assert_eq_with_mode!(cache.get(&"b"), Some(bill), delivery_mode);
            assert_eq_with_mode!(cache.get(&"d"), None, delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"d"), delivery_mode);

            // Re-add "a" (w: 10) and update "b" with "bob" (w: 20 -> 15).
            cache.insert("a", alice);
            cache.insert("b", bob);
            expected.push((Arc::new("b"), bill, RemovalCause::Replaced));
            cache.sync();
            assert_eq_with_mode!(cache.get(&"a"), Some(alice), delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), Some(bob), delivery_mode);
            assert_eq_with_mode!(cache.get(&"d"), None, delivery_mode);
            assert_with_mode!(cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"d"), delivery_mode);

            // Verify the sizes.
            assert_eq_with_mode!(cache.entry_count(), 2, delivery_mode);
            assert_eq_with_mode!(cache.weighted_size(), 25, delivery_mode);

            verify_notification_vec(&cache, actual, &expected, delivery_mode);
        }
    }

    #[test]
    fn basic_multi_threads() {
        let num_threads = 4;
        let cache = Cache::new(100);

        // https://rust-lang.github.io/rust-clippy/master/index.html#needless_collect
        #[allow(clippy::needless_collect)]
        let handles = (0..num_threads)
            .map(|id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    cache.insert(10, format!("{}-100", id));
                    cache.get(&10);
                    cache.insert(20, format!("{}-200", id));
                    cache.invalidate(&10);
                })
            })
            .collect::<Vec<_>>();

        handles.into_iter().for_each(|h| h.join().expect("Failed"));

        assert!(cache.get(&10).is_none());
        assert!(cache.get(&20).is_some());
        assert!(!cache.contains_key(&10));
        assert!(cache.contains_key(&20));
    }

    #[test]
    fn invalidate_all() {
        run_test(DeliveryMode::Immediate);
        run_test(DeliveryMode::Queued);

        fn run_test(delivery_mode: DeliveryMode) {
            // The following `Vec`s will hold actual and expected notifications.
            let actual = Arc::new(Mutex::new(Vec::new()));
            let mut expected = Vec::new();

            // Create an eviction listener.
            let a1 = Arc::clone(&actual);
            let listener = move |k, v, cause| a1.lock().push((k, v, cause));
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(delivery_mode)
                .build();

            // Create a cache with the eviction listener.
            let mut cache = Cache::builder()
                .max_capacity(100)
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.reconfigure_for_testing();

            // Make the cache exterior immutable.
            let cache = cache;

            cache.insert("a", "alice");
            cache.insert("b", "bob");
            cache.insert("c", "cindy");
            assert_eq_with_mode!(cache.get(&"a"), Some("alice"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), Some("bob"), delivery_mode);
            assert_eq_with_mode!(cache.get(&"c"), Some("cindy"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"c"), delivery_mode);

            // `cache.sync()` is no longer needed here before invalidating. The last
            // modified timestamp of the entries were updated when they were inserted.
            // https://github.com/moka-rs/moka/issues/155

            cache.invalidate_all();
            expected.push((Arc::new("a"), "alice", RemovalCause::Explicit));
            expected.push((Arc::new("b"), "bob", RemovalCause::Explicit));
            expected.push((Arc::new("c"), "cindy", RemovalCause::Explicit));
            cache.sync();

            cache.insert("d", "david");
            cache.sync();

            assert_with_mode!(cache.get(&"a").is_none(), delivery_mode);
            assert_with_mode!(cache.get(&"b").is_none(), delivery_mode);
            assert_with_mode!(cache.get(&"c").is_none(), delivery_mode);
            assert_eq_with_mode!(cache.get(&"d"), Some("david"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"b"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"c"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"d"), delivery_mode);

            verify_notification_vec(&cache, actual, &expected, delivery_mode);
        }
    }

    #[test]
    fn invalidate_entries_if() -> Result<(), Box<dyn std::error::Error>> {
        run_test(DeliveryMode::Immediate)?;
        run_test(DeliveryMode::Queued)?;

        fn run_test(delivery_mode: DeliveryMode) -> Result<(), Box<dyn std::error::Error>> {
            use std::collections::HashSet;

            // The following `Vec`s will hold actual and expected notifications.
            let actual = Arc::new(Mutex::new(Vec::new()));
            let mut expected = Vec::new();

            // Create an eviction listener.
            let a1 = Arc::clone(&actual);
            let listener = move |k, v, cause| a1.lock().push((k, v, cause));
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(delivery_mode)
                .build();

            // Create a cache with the eviction listener.
            let mut cache = Cache::builder()
                .max_capacity(100)
                .support_invalidation_closures()
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.reconfigure_for_testing();

            let (clock, mock) = Clock::mock();
            cache.set_expiration_clock(Some(clock));

            // Make the cache exterior immutable.
            let cache = cache;

            cache.insert(0, "alice");
            cache.insert(1, "bob");
            cache.insert(2, "alex");
            cache.sync();

            mock.increment(Duration::from_secs(5)); // 5 secs from the start.
            cache.sync();

            assert_eq_with_mode!(cache.get(&0), Some("alice"), delivery_mode);
            assert_eq_with_mode!(cache.get(&1), Some("bob"), delivery_mode);
            assert_eq_with_mode!(cache.get(&2), Some("alex"), delivery_mode);
            assert_with_mode!(cache.contains_key(&0), delivery_mode);
            assert_with_mode!(cache.contains_key(&1), delivery_mode);
            assert_with_mode!(cache.contains_key(&2), delivery_mode);

            let names = ["alice", "alex"].iter().cloned().collect::<HashSet<_>>();
            cache.invalidate_entries_if(move |_k, &v| names.contains(v))?;
            assert_eq_with_mode!(cache.base.invalidation_predicate_count(), 1, delivery_mode);
            expected.push((Arc::new(0), "alice", RemovalCause::Explicit));
            expected.push((Arc::new(2), "alex", RemovalCause::Explicit));

            mock.increment(Duration::from_secs(5)); // 10 secs from the start.

            cache.insert(3, "alice");

            // Run the invalidation task and wait for it to finish. (TODO: Need a better way than sleeping)
            cache.sync(); // To submit the invalidation task.
            std::thread::sleep(Duration::from_millis(200));
            cache.sync(); // To process the task result.
            std::thread::sleep(Duration::from_millis(200));

            assert_with_mode!(cache.get(&0).is_none(), delivery_mode);
            assert_with_mode!(cache.get(&2).is_none(), delivery_mode);
            assert_eq_with_mode!(cache.get(&1), Some("bob"), delivery_mode);
            // This should survive as it was inserted after calling invalidate_entries_if.
            assert_eq_with_mode!(cache.get(&3), Some("alice"), delivery_mode);

            assert_with_mode!(!cache.contains_key(&0), delivery_mode);
            assert_with_mode!(cache.contains_key(&1), delivery_mode);
            assert_with_mode!(!cache.contains_key(&2), delivery_mode);
            assert_with_mode!(cache.contains_key(&3), delivery_mode);

            assert_eq_with_mode!(cache.entry_count(), 2, delivery_mode);
            assert_eq_with_mode!(cache.invalidation_predicate_count(), 0, delivery_mode);

            mock.increment(Duration::from_secs(5)); // 15 secs from the start.

            cache.invalidate_entries_if(|_k, &v| v == "alice")?;
            cache.invalidate_entries_if(|_k, &v| v == "bob")?;
            assert_eq_with_mode!(cache.invalidation_predicate_count(), 2, delivery_mode);
            // key 1 was inserted before key 3.
            expected.push((Arc::new(1), "bob", RemovalCause::Explicit));
            expected.push((Arc::new(3), "alice", RemovalCause::Explicit));

            // Run the invalidation task and wait for it to finish. (TODO: Need a better way than sleeping)
            cache.sync(); // To submit the invalidation task.
            std::thread::sleep(Duration::from_millis(200));
            cache.sync(); // To process the task result.
            std::thread::sleep(Duration::from_millis(200));

            assert_with_mode!(cache.get(&1).is_none(), delivery_mode);
            assert_with_mode!(cache.get(&3).is_none(), delivery_mode);

            assert_with_mode!(!cache.contains_key(&1), delivery_mode);
            assert_with_mode!(!cache.contains_key(&3), delivery_mode);

            assert_eq_with_mode!(cache.entry_count(), 0, delivery_mode);
            assert_eq_with_mode!(cache.invalidation_predicate_count(), 0, delivery_mode);

            verify_notification_vec(&cache, actual, &expected, delivery_mode);

            Ok(())
        }

        Ok(())
    }

    #[test]
    fn time_to_live() {
        run_test(DeliveryMode::Immediate);
        run_test(DeliveryMode::Queued);

        fn run_test(delivery_mode: DeliveryMode) {
            // The following `Vec`s will hold actual and expected notifications.
            let actual = Arc::new(Mutex::new(Vec::new()));
            let mut expected = Vec::new();

            // Create an eviction listener.
            let a1 = Arc::clone(&actual);
            let listener = move |k, v, cause| a1.lock().push((k, v, cause));
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(delivery_mode)
                .build();

            // Create a cache with the eviction listener.
            let mut cache = Cache::builder()
                .max_capacity(100)
                .time_to_live(Duration::from_secs(10))
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.reconfigure_for_testing();

            let (clock, mock) = Clock::mock();
            cache.set_expiration_clock(Some(clock));

            // Make the cache exterior immutable.
            let cache = cache;

            cache.insert("a", "alice");
            cache.sync();

            mock.increment(Duration::from_secs(5)); // 5 secs from the start.
            cache.sync();

            assert_eq_with_mode!(cache.get(&"a"), Some("alice"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"a"), delivery_mode);

            mock.increment(Duration::from_secs(5)); // 10 secs.
            expected.push((Arc::new("a"), "alice", RemovalCause::Expired));
            assert_eq_with_mode!(cache.get(&"a"), None, delivery_mode);
            assert_with_mode!(!cache.contains_key(&"a"), delivery_mode);

            assert_eq_with_mode!(cache.iter().count(), 0, delivery_mode);

            cache.sync();
            assert_with_mode!(cache.is_table_empty(), delivery_mode);

            cache.insert("b", "bob");
            cache.sync();

            assert_eq_with_mode!(cache.entry_count(), 1, delivery_mode);

            mock.increment(Duration::from_secs(5)); // 15 secs.
            cache.sync();

            assert_eq_with_mode!(cache.get(&"b"), Some("bob"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            assert_eq_with_mode!(cache.entry_count(), 1, delivery_mode);

            cache.insert("b", "bill");
            expected.push((Arc::new("b"), "bob", RemovalCause::Replaced));
            cache.sync();

            mock.increment(Duration::from_secs(5)); // 20 secs
            cache.sync();

            assert_eq_with_mode!(cache.get(&"b"), Some("bill"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            assert_eq_with_mode!(cache.entry_count(), 1, delivery_mode);

            mock.increment(Duration::from_secs(5)); // 25 secs
            expected.push((Arc::new("b"), "bill", RemovalCause::Expired));

            assert_eq_with_mode!(cache.get(&"a"), None, delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), None, delivery_mode);
            assert_with_mode!(!cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"b"), delivery_mode);

            assert_eq_with_mode!(cache.iter().count(), 0, delivery_mode);

            cache.sync();
            assert_with_mode!(cache.is_table_empty(), delivery_mode);

            verify_notification_vec(&cache, actual, &expected, delivery_mode);
        }
    }

    #[test]
    fn time_to_idle() {
        run_test(DeliveryMode::Immediate);
        run_test(DeliveryMode::Queued);

        fn run_test(delivery_mode: DeliveryMode) {
            // The following `Vec`s will hold actual and expected notifications.
            let actual = Arc::new(Mutex::new(Vec::new()));
            let mut expected = Vec::new();

            // Create an eviction listener.
            let a1 = Arc::clone(&actual);
            let listener = move |k, v, cause| a1.lock().push((k, v, cause));
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(delivery_mode)
                .build();

            // Create a cache with the eviction listener.
            let mut cache = Cache::builder()
                .max_capacity(100)
                .time_to_idle(Duration::from_secs(10))
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.reconfigure_for_testing();

            let (clock, mock) = Clock::mock();
            cache.set_expiration_clock(Some(clock));

            // Make the cache exterior immutable.
            let cache = cache;

            cache.insert("a", "alice");
            cache.sync();

            mock.increment(Duration::from_secs(5)); // 5 secs from the start.
            cache.sync();

            assert_eq_with_mode!(cache.get(&"a"), Some("alice"), delivery_mode);

            mock.increment(Duration::from_secs(5)); // 10 secs.
            cache.sync();

            cache.insert("b", "bob");
            cache.sync();

            assert_eq_with_mode!(cache.entry_count(), 2, delivery_mode);

            mock.increment(Duration::from_secs(2)); // 12 secs.
            cache.sync();

            // contains_key does not reset the idle timer for the key.
            assert_with_mode!(cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);
            cache.sync();

            assert_eq_with_mode!(cache.entry_count(), 2, delivery_mode);

            mock.increment(Duration::from_secs(3)); // 15 secs.
            expected.push((Arc::new("a"), "alice", RemovalCause::Expired));

            assert_eq_with_mode!(cache.get(&"a"), None, delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), Some("bob"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(cache.contains_key(&"b"), delivery_mode);

            assert_eq_with_mode!(cache.iter().count(), 1, delivery_mode);

            cache.sync();
            assert_eq_with_mode!(cache.entry_count(), 1, delivery_mode);

            mock.increment(Duration::from_secs(10)); // 25 secs
            expected.push((Arc::new("b"), "bob", RemovalCause::Expired));

            assert_eq_with_mode!(cache.get(&"a"), None, delivery_mode);
            assert_eq_with_mode!(cache.get(&"b"), None, delivery_mode);
            assert_with_mode!(!cache.contains_key(&"a"), delivery_mode);
            assert_with_mode!(!cache.contains_key(&"b"), delivery_mode);

            assert_eq_with_mode!(cache.iter().count(), 0, delivery_mode);

            cache.sync();
            assert_with_mode!(cache.is_table_empty(), delivery_mode);

            verify_notification_vec(&cache, actual, &expected, delivery_mode);
        }
    }

    #[test]
    fn test_iter() {
        const NUM_KEYS: usize = 50;

        fn make_value(key: usize) -> String {
            format!("val: {}", key)
        }

        let cache = Cache::builder()
            .max_capacity(100)
            .time_to_idle(Duration::from_secs(10))
            .build();

        for key in 0..NUM_KEYS {
            cache.insert(key, make_value(key));
        }

        let mut key_set = std::collections::HashSet::new();

        for (key, value) in &cache {
            assert_eq!(value, make_value(*key));

            key_set.insert(*key);
        }

        // Ensure there are no missing or duplicate keys in the iteration.
        assert_eq!(key_set.len(), NUM_KEYS);
    }

    /// Runs 16 threads at the same time and ensures no deadlock occurs.
    ///
    /// - Eight of the threads will update key-values in the cache.
    /// - Eight others will iterate the cache.
    ///
    #[test]
    fn test_iter_multi_threads() {
        use std::collections::HashSet;

        const NUM_KEYS: usize = 1024;
        const NUM_THREADS: usize = 16;

        fn make_value(key: usize) -> String {
            format!("val: {}", key)
        }

        let cache = Cache::builder()
            .max_capacity(2048)
            .time_to_idle(Duration::from_secs(10))
            .build();

        // Initialize the cache.
        for key in 0..NUM_KEYS {
            cache.insert(key, make_value(key));
        }

        let rw_lock = Arc::new(std::sync::RwLock::<()>::default());
        let write_lock = rw_lock.write().unwrap();

        // https://rust-lang.github.io/rust-clippy/master/index.html#needless_collect
        #[allow(clippy::needless_collect)]
        let handles = (0..NUM_THREADS)
            .map(|n| {
                let cache = cache.clone();
                let rw_lock = Arc::clone(&rw_lock);

                if n % 2 == 0 {
                    // This thread will update the cache.
                    std::thread::spawn(move || {
                        let read_lock = rw_lock.read().unwrap();
                        for key in 0..NUM_KEYS {
                            // TODO: Update keys in a random order?
                            cache.insert(key, make_value(key));
                        }
                        std::mem::drop(read_lock);
                    })
                } else {
                    // This thread will iterate the cache.
                    std::thread::spawn(move || {
                        let read_lock = rw_lock.read().unwrap();
                        let mut key_set = HashSet::new();
                        for (key, value) in &cache {
                            assert_eq!(value, make_value(*key));
                            key_set.insert(*key);
                        }
                        // Ensure there are no missing or duplicate keys in the iteration.
                        assert_eq!(key_set.len(), NUM_KEYS);
                        std::mem::drop(read_lock);
                    })
                }
            })
            .collect::<Vec<_>>();

        // Let these threads to run by releasing the write lock.
        std::mem::drop(write_lock);

        handles.into_iter().for_each(|h| h.join().expect("Failed"));

        // Ensure there are no missing or duplicate keys in the iteration.
        let key_set = cache.iter().map(|(k, _v)| *k).collect::<HashSet<_>>();
        assert_eq!(key_set.len(), NUM_KEYS);
    }

    #[test]
    fn get_with() {
        use std::thread::{sleep, spawn};

        let cache = Cache::new(100);
        const KEY: u32 = 0;

        // This test will run five threads:
        //
        // Thread1 will be the first thread to call `get_with` for a key, so its init
        // closure will be evaluated and then a &str value "thread1" will be inserted
        // to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `get_with` immediately.
                let v = cache1.get_with(KEY, || {
                    // Wait for 300 ms and return a &str value.
                    sleep(Duration::from_millis(300));
                    "thread1"
                });
                assert_eq!(v, "thread1");
            })
        };

        // Thread2 will be the second thread to call `get_with` for the same key, so
        // its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the value inserted by thread1's init closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `get_with`.
                sleep(Duration::from_millis(100));
                let v = cache2.get_with(KEY, || unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread3 will be the third thread to call `get_with` for the same key. By
        // the time it calls, thread1's init closure should have finished already and
        // the value should be already inserted to the cache. So its init closure
        // will not be evaluated and will get the value insert by thread1's init
        // closure immediately.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get_with`.
                sleep(Duration::from_millis(400));
                let v = cache3.get_with(KEY, || unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread4 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache4.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread5 will call `get` for the same key. It will call after thread1's init
        // closure finished, so it will get the value insert by thread1's init closure.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache5.get(&KEY);
                assert_eq!(maybe_v, Some("thread1"));
            })
        };

        for t in vec![thread1, thread2, thread3, thread4, thread5] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn get_with_by_ref() {
        use std::thread::{sleep, spawn};

        let cache = Cache::new(100);
        const KEY: &u32 = &0;

        // This test will run five threads:
        //
        // Thread1 will be the first thread to call `get_with_by_ref` for a key, so its init
        // closure will be evaluated and then a &str value "thread1" will be inserted
        // to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `get_with_by_ref` immediately.
                let v = cache1.get_with_by_ref(KEY, || {
                    // Wait for 300 ms and return a &str value.
                    sleep(Duration::from_millis(300));
                    "thread1"
                });
                assert_eq!(v, "thread1");
            })
        };

        // Thread2 will be the second thread to call `get_with_by_ref` for the same key, so
        // its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the value inserted by thread1's init closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `get_with_by_ref`.
                sleep(Duration::from_millis(100));
                let v = cache2.get_with_by_ref(KEY, || unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread3 will be the third thread to call `get_with_by_ref` for the same key. By
        // the time it calls, thread1's init closure should have finished already and
        // the value should be already inserted to the cache. So its init closure
        // will not be evaluated and will get the value insert by thread1's init
        // closure immediately.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get_with_by_ref`.
                sleep(Duration::from_millis(400));
                let v = cache3.get_with_by_ref(KEY, || unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread4 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache4.get(KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread5 will call `get` for the same key. It will call after thread1's init
        // closure finished, so it will get the value insert by thread1's init closure.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache5.get(KEY);
                assert_eq!(maybe_v, Some("thread1"));
            })
        };

        for t in vec![thread1, thread2, thread3, thread4, thread5] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn get_with_if() {
        use std::thread::{sleep, spawn};

        let cache = Cache::new(100);
        const KEY: u32 = 0;

        // This test will run seven threads:
        //
        // Thread1 will be the first thread to call `get_with_if` for a key, so its
        // init closure will be evaluated and then a &str value "thread1" will be
        // inserted to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `get_with` immediately.
                let v = cache1.get_with_if(
                    KEY,
                    || {
                        // Wait for 300 ms and return a &str value.
                        sleep(Duration::from_millis(300));
                        "thread1"
                    },
                    |_v| unreachable!(),
                );
                assert_eq!(v, "thread1");
            })
        };

        // Thread2 will be the second thread to call `get_with_if` for the same key,
        // so its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the value inserted by thread1's init closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `get_with`.
                sleep(Duration::from_millis(100));
                let v = cache2.get_with_if(KEY, || unreachable!(), |_v| unreachable!());
                assert_eq!(v, "thread1");
            })
        };

        // Thread3 will be the third thread to call `get_with_if` for the same
        // key. By the time it calls, thread1's init closure should have finished
        // already and the value should be already inserted to the cache. Also
        // thread3's `replace_if` closure returns `false`. So its init closure will
        // not be evaluated and will get the value inserted by thread1's init closure
        // immediately.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 350 ms before calling `get_with_if`.
                sleep(Duration::from_millis(350));
                let v = cache3.get_with_if(
                    KEY,
                    || unreachable!(),
                    |v| {
                        assert_eq!(v, &"thread1");
                        false
                    },
                );
                assert_eq!(v, "thread1");
            })
        };

        // Thread4 will be the fourth thread to call `get_with_if` for the same
        // key. The value should have been already inserted to the cache by
        // thread1. However thread4's `replace_if` closure returns `true`. So its
        // init closure will be evaluated to replace the current value.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get_with_if`.
                sleep(Duration::from_millis(400));
                let v = cache4.get_with_if(
                    KEY,
                    || "thread4",
                    |v| {
                        assert_eq!(v, &"thread1");
                        true
                    },
                );
                assert_eq!(v, "thread4");
            })
        };

        // Thread5 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache5.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread6 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread6 = {
            let cache6 = cache.clone();
            spawn(move || {
                // Wait for 350 ms before calling `get`.
                sleep(Duration::from_millis(350));
                let maybe_v = cache6.get(&KEY);
                assert_eq!(maybe_v, Some("thread1"));
            })
        };

        // Thread7 will call `get` for the same key. It will call after thread1's init
        // closure finished, so it will get the value insert by thread1's init closure.
        let thread7 = {
            let cache7 = cache.clone();
            spawn(move || {
                // Wait for 450 ms before calling `get`.
                sleep(Duration::from_millis(450));
                let maybe_v = cache7.get(&KEY);
                assert_eq!(maybe_v, Some("thread4"));
            })
        };

        for t in vec![
            thread1, thread2, thread3, thread4, thread5, thread6, thread7,
        ] {
            t.join().expect("Failed to join");
        }
    }

    // #[test]
    // fn get_with_if_by_ref() {
    //     use std::thread::{sleep, spawn};

    //     let cache: Cache<u32, &str> = Cache::new(100);
    //     const KEY: &u32 = &0;

    //     // This test will run seven threads:
    //     //
    //     // Thread1 will be the first thread to call `get_with_if_by_ref` for a key, so its
    //     // init closure will be evaluated and then a &str value "thread1" will be
    //     // inserted to the cache.
    //     let thread1 = {
    //         let cache1 = cache.clone();
    //         spawn(move || {
    //             // Call `get_with` immediately.
    //             let v = cache1.get_with_if_by_ref(
    //                 KEY,
    //                 || {
    //                     // Wait for 300 ms and return a &str value.
    //                     sleep(Duration::from_millis(300));
    //                     "thread1"
    //                 },
    //                 |_v| unreachable!(),
    //             );
    //             assert_eq!(v, "thread1");
    //         })
    //     };

    //     // Thread2 will be the second thread to call `get_with_if_by_ref` for the same key,
    //     // so its init closure will not be evaluated. Once thread1's init closure
    //     // finishes, it will get the value inserted by thread1's init closure.
    //     let thread2 = {
    //         let cache2 = cache.clone();
    //         spawn(move || {
    //             // Wait for 100 ms before calling `get_with`.
    //             sleep(Duration::from_millis(100));
    //             let v = cache2.get_with_if_by_ref(KEY, || unreachable!(), |_v| unreachable!());
    //             assert_eq!(v, "thread1");
    //         })
    //     };

    //     // Thread3 will be the third thread to call `get_with_if_by_ref` for the same
    //     // key. By the time it calls, thread1's init closure should have finished
    //     // already and the value should be already inserted to the cache. Also
    //     // thread3's `replace_if` closure returns `false`. So its init closure will
    //     // not be evaluated and will get the value inserted by thread1's init closure
    //     // immediately.
    //     let thread3 = {
    //         let cache3 = cache.clone();
    //         spawn(move || {
    //             // Wait for 350 ms before calling `get_with_if_by_ref`.
    //             sleep(Duration::from_millis(350));
    //             let v = cache3.get_with_if_by_ref(
    //                 KEY,
    //                 || unreachable!(),
    //                 |v| {
    //                     assert_eq!(v, &"thread1");
    //                     false
    //                 },
    //             );
    //             assert_eq!(v, "thread1");
    //         })
    //     };

    //     // Thread4 will be the fourth thread to call `get_with_if_by_ref` for the same
    //     // key. The value should have been already inserted to the cache by
    //     // thread1. However thread4's `replace_if` closure returns `true`. So its
    //     // init closure will be evaluated to replace the current value.
    //     let thread4 = {
    //         let cache4 = cache.clone();
    //         spawn(move || {
    //             // Wait for 400 ms before calling `get_with_if_by_ref`.
    //             sleep(Duration::from_millis(400));
    //             let v = cache4.get_with_if_by_ref(
    //                 KEY,
    //                 || "thread4",
    //                 |v| {
    //                     assert_eq!(v, &"thread1");
    //                     true
    //                 },
    //             );
    //             assert_eq!(v, "thread4");
    //         })
    //     };

    //     // Thread5 will call `get` for the same key. It will call when thread1's init
    //     // closure is still running, so it will get none for the key.
    //     let thread5 = {
    //         let cache5 = cache.clone();
    //         spawn(move || {
    //             // Wait for 200 ms before calling `get`.
    //             sleep(Duration::from_millis(200));
    //             let maybe_v = cache5.get(KEY);
    //             assert!(maybe_v.is_none());
    //         })
    //     };

    //     // Thread6 will call `get` for the same key. It will call when thread1's init
    //     // closure is still running, so it will get none for the key.
    //     let thread6 = {
    //         let cache6 = cache.clone();
    //         spawn(move || {
    //             // Wait for 350 ms before calling `get`.
    //             sleep(Duration::from_millis(350));
    //             let maybe_v = cache6.get(KEY);
    //             assert_eq!(maybe_v, Some("thread1"));
    //         })
    //     };

    //     // Thread7 will call `get` for the same key. It will call after thread1's init
    //     // closure finished, so it will get the value insert by thread1's init closure.
    //     let thread7 = {
    //         let cache7 = cache.clone();
    //         spawn(move || {
    //             // Wait for 450 ms before calling `get`.
    //             sleep(Duration::from_millis(450));
    //             let maybe_v = cache7.get(KEY);
    //             assert_eq!(maybe_v, Some("thread4"));
    //         })
    //     };

    //     for t in vec![
    //         thread1, thread2, thread3, thread4, thread5, thread6, thread7,
    //     ] {
    //         t.join().expect("Failed to join");
    //     }
    // }

    #[test]
    fn try_get_with() {
        use std::{
            sync::Arc,
            thread::{sleep, spawn},
        };

        // Note that MyError does not implement std::error::Error trait like
        // anyhow::Error.
        #[derive(Debug)]
        pub struct MyError(String);

        type MyResult<T> = Result<T, Arc<MyError>>;

        let cache = Cache::new(100);
        const KEY: u32 = 0;

        // This test will run eight threads:
        //
        // Thread1 will be the first thread to call `try_get_with` for a key, so its
        // init closure will be evaluated and then an error will be returned. Nothing
        // will be inserted to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `try_get_with` immediately.
                let v = cache1.try_get_with(KEY, || {
                    // Wait for 300 ms and return an error.
                    sleep(Duration::from_millis(300));
                    Err(MyError("thread1 error".into()))
                });
                assert!(v.is_err());
            })
        };

        // Thread2 will be the second thread to call `try_get_with` for the same key,
        // so its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the same error value returned by thread1's init
        // closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `try_get_with`.
                sleep(Duration::from_millis(100));
                let v: MyResult<_> = cache2.try_get_with(KEY, || unreachable!());
                assert!(v.is_err());
            })
        };

        // Thread3 will be the third thread to call `get_with` for the same key. By
        // the time it calls, thread1's init closure should have finished already,
        // but the key still does not exist in the cache. So its init closure will be
        // evaluated and then an okay &str value will be returned. That value will be
        // inserted to the cache.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `try_get_with`.
                sleep(Duration::from_millis(400));
                let v: MyResult<_> = cache3.try_get_with(KEY, || {
                    // Wait for 300 ms and return an Ok(&str) value.
                    sleep(Duration::from_millis(300));
                    Ok("thread3")
                });
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // thread4 will be the fourth thread to call `try_get_with` for the same
        // key. So its init closure will not be evaluated. Once thread3's init
        // closure finishes, it will get the same okay &str value.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 500 ms before calling `try_get_with`.
                sleep(Duration::from_millis(500));
                let v: MyResult<_> = cache4.try_get_with(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread5 will be the fifth thread to call `try_get_with` for the same
        // key. So its init closure will not be evaluated. By the time it calls,
        // thread3's init closure should have finished already, so its init closure
        // will not be evaluated and will get the value insert by thread3's init
        // closure immediately.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `try_get_with`.
                sleep(Duration::from_millis(800));
                let v: MyResult<_> = cache5.try_get_with(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread6 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread6 = {
            let cache6 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache6.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread7 will call `get` for the same key. It will call after thread1's init
        // closure finished with an error. So it will get none for the key.
        let thread7 = {
            let cache7 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache7.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread8 will call `get` for the same key. It will call after thread3's init
        // closure finished, so it will get the value insert by thread3's init closure.
        let thread8 = {
            let cache8 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `get`.
                sleep(Duration::from_millis(800));
                let maybe_v = cache8.get(&KEY);
                assert_eq!(maybe_v, Some("thread3"));
            })
        };

        for t in vec![
            thread1, thread2, thread3, thread4, thread5, thread6, thread7, thread8,
        ] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn try_get_with_by_ref() {
        use std::{
            sync::Arc,
            thread::{sleep, spawn},
        };

        // Note that MyError does not implement std::error::Error trait like
        // anyhow::Error.
        #[derive(Debug)]
        pub struct MyError(String);

        type MyResult<T> = Result<T, Arc<MyError>>;

        let cache = Cache::new(100);
        const KEY: &u32 = &0;

        // This test will run eight threads:
        //
        // Thread1 will be the first thread to call `try_get_with_by_ref` for a key, so its
        // init closure will be evaluated and then an error will be returned. Nothing
        // will be inserted to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `try_get_with_by_ref` immediately.
                let v = cache1.try_get_with_by_ref(KEY, || {
                    // Wait for 300 ms and return an error.
                    sleep(Duration::from_millis(300));
                    Err(MyError("thread1 error".into()))
                });
                assert!(v.is_err());
            })
        };

        // Thread2 will be the second thread to call `try_get_with_by_ref` for the same key,
        // so its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the same error value returned by thread1's init
        // closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `try_get_with_by_ref`.
                sleep(Duration::from_millis(100));
                let v: MyResult<_> = cache2.try_get_with_by_ref(KEY, || unreachable!());
                assert!(v.is_err());
            })
        };

        // Thread3 will be the third thread to call `get_with` for the same key. By
        // the time it calls, thread1's init closure should have finished already,
        // but the key still does not exist in the cache. So its init closure will be
        // evaluated and then an okay &str value will be returned. That value will be
        // inserted to the cache.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `try_get_with_by_ref`.
                sleep(Duration::from_millis(400));
                let v: MyResult<_> = cache3.try_get_with_by_ref(KEY, || {
                    // Wait for 300 ms and return an Ok(&str) value.
                    sleep(Duration::from_millis(300));
                    Ok("thread3")
                });
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // thread4 will be the fourth thread to call `try_get_with_by_ref` for the same
        // key. So its init closure will not be evaluated. Once thread3's init
        // closure finishes, it will get the same okay &str value.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 500 ms before calling `try_get_with_by_ref`.
                sleep(Duration::from_millis(500));
                let v: MyResult<_> = cache4.try_get_with_by_ref(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread5 will be the fifth thread to call `try_get_with_by_ref` for the same
        // key. So its init closure will not be evaluated. By the time it calls,
        // thread3's init closure should have finished already, so its init closure
        // will not be evaluated and will get the value insert by thread3's init
        // closure immediately.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `try_get_with_by_ref`.
                sleep(Duration::from_millis(800));
                let v: MyResult<_> = cache5.try_get_with_by_ref(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread6 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread6 = {
            let cache6 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache6.get(KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread7 will call `get` for the same key. It will call after thread1's init
        // closure finished with an error. So it will get none for the key.
        let thread7 = {
            let cache7 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache7.get(KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread8 will call `get` for the same key. It will call after thread3's init
        // closure finished, so it will get the value insert by thread3's init closure.
        let thread8 = {
            let cache8 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `get`.
                sleep(Duration::from_millis(800));
                let maybe_v = cache8.get(KEY);
                assert_eq!(maybe_v, Some("thread3"));
            })
        };

        for t in vec![
            thread1, thread2, thread3, thread4, thread5, thread6, thread7, thread8,
        ] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn optionally_get_with() {
        use std::thread::{sleep, spawn};

        let cache = Cache::new(100);
        const KEY: u32 = 0;

        // This test will run eight threads:
        //
        // Thread1 will be the first thread to call `optionally_get_with` for a key, so its
        // init closure will be evaluated and then an error will be returned. Nothing
        // will be inserted to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `optionally_get_with` immediately.
                let v = cache1.optionally_get_with(KEY, || {
                    // Wait for 300 ms and return an error.
                    sleep(Duration::from_millis(300));
                    None
                });
                assert!(v.is_none());
            })
        };

        // Thread2 will be the second thread to call `optionally_get_with` for the same key,
        // so its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the same error value returned by thread1's init
        // closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `optionally_get_with`.
                sleep(Duration::from_millis(100));
                let v = cache2.optionally_get_with(KEY, || unreachable!());
                assert!(v.is_none());
            })
        };

        // Thread3 will be the third thread to call `get_with` for the same key. By
        // the time it calls, thread1's init closure should have finished already,
        // but the key still does not exist in the cache. So its init closure will be
        // evaluated and then an okay &str value will be returned. That value will be
        // inserted to the cache.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `optionally_get_with`.
                sleep(Duration::from_millis(400));
                let v = cache3.optionally_get_with(KEY, || {
                    // Wait for 300 ms and return an Ok(&str) value.
                    sleep(Duration::from_millis(300));
                    Some("thread3")
                });
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // thread4 will be the fourth thread to call `optionally_get_with` for the same
        // key. So its init closure will not be evaluated. Once thread3's init
        // closure finishes, it will get the same okay &str value.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 500 ms before calling `optionally_get_with`.
                sleep(Duration::from_millis(500));
                let v = cache4.optionally_get_with(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread5 will be the fifth thread to call `optionally_get_with` for the same
        // key. So its init closure will not be evaluated. By the time it calls,
        // thread3's init closure should have finished already, so its init closure
        // will not be evaluated and will get the value insert by thread3's init
        // closure immediately.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `optionally_get_with`.
                sleep(Duration::from_millis(800));
                let v = cache5.optionally_get_with(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread6 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread6 = {
            let cache6 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache6.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread7 will call `get` for the same key. It will call after thread1's init
        // closure finished with an error. So it will get none for the key.
        let thread7 = {
            let cache7 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache7.get(&KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread8 will call `get` for the same key. It will call after thread3's init
        // closure finished, so it will get the value insert by thread3's init closure.
        let thread8 = {
            let cache8 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `get`.
                sleep(Duration::from_millis(800));
                let maybe_v = cache8.get(&KEY);
                assert_eq!(maybe_v, Some("thread3"));
            })
        };

        for t in vec![
            thread1, thread2, thread3, thread4, thread5, thread6, thread7, thread8,
        ] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    fn optionally_get_with_by_ref() {
        use std::thread::{sleep, spawn};

        let cache = Cache::new(100);
        const KEY: &u32 = &0;

        // This test will run eight threads:
        //
        // Thread1 will be the first thread to call `optionally_get_with_by_ref` for a key, so its
        // init closure will be evaluated and then an error will be returned. Nothing
        // will be inserted to the cache.
        let thread1 = {
            let cache1 = cache.clone();
            spawn(move || {
                // Call `optionally_get_with_by_ref` immediately.
                let v = cache1.optionally_get_with_by_ref(KEY, || {
                    // Wait for 300 ms and return an error.
                    sleep(Duration::from_millis(300));
                    None
                });
                assert!(v.is_none());
            })
        };

        // Thread2 will be the second thread to call `optionally_get_with_by_ref` for the same key,
        // so its init closure will not be evaluated. Once thread1's init closure
        // finishes, it will get the same error value returned by thread1's init
        // closure.
        let thread2 = {
            let cache2 = cache.clone();
            spawn(move || {
                // Wait for 100 ms before calling `optionally_get_with_by_ref`.
                sleep(Duration::from_millis(100));
                let v = cache2.optionally_get_with_by_ref(KEY, || unreachable!());
                assert!(v.is_none());
            })
        };

        // Thread3 will be the third thread to call `get_with` for the same key. By
        // the time it calls, thread1's init closure should have finished already,
        // but the key still does not exist in the cache. So its init closure will be
        // evaluated and then an okay &str value will be returned. That value will be
        // inserted to the cache.
        let thread3 = {
            let cache3 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `optionally_get_with_by_ref`.
                sleep(Duration::from_millis(400));
                let v = cache3.optionally_get_with_by_ref(KEY, || {
                    // Wait for 300 ms and return an Ok(&str) value.
                    sleep(Duration::from_millis(300));
                    Some("thread3")
                });
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // thread4 will be the fourth thread to call `optionally_get_with_by_ref` for the same
        // key. So its init closure will not be evaluated. Once thread3's init
        // closure finishes, it will get the same okay &str value.
        let thread4 = {
            let cache4 = cache.clone();
            spawn(move || {
                // Wait for 500 ms before calling `optionally_get_with_by_ref`.
                sleep(Duration::from_millis(500));
                let v = cache4.optionally_get_with_by_ref(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread5 will be the fifth thread to call `optionally_get_with_by_ref` for the same
        // key. So its init closure will not be evaluated. By the time it calls,
        // thread3's init closure should have finished already, so its init closure
        // will not be evaluated and will get the value insert by thread3's init
        // closure immediately.
        let thread5 = {
            let cache5 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `optionally_get_with_by_ref`.
                sleep(Duration::from_millis(800));
                let v = cache5.optionally_get_with_by_ref(KEY, || unreachable!());
                assert_eq!(v.unwrap(), "thread3");
            })
        };

        // Thread6 will call `get` for the same key. It will call when thread1's init
        // closure is still running, so it will get none for the key.
        let thread6 = {
            let cache6 = cache.clone();
            spawn(move || {
                // Wait for 200 ms before calling `get`.
                sleep(Duration::from_millis(200));
                let maybe_v = cache6.get(KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread7 will call `get` for the same key. It will call after thread1's init
        // closure finished with an error. So it will get none for the key.
        let thread7 = {
            let cache7 = cache.clone();
            spawn(move || {
                // Wait for 400 ms before calling `get`.
                sleep(Duration::from_millis(400));
                let maybe_v = cache7.get(KEY);
                assert!(maybe_v.is_none());
            })
        };

        // Thread8 will call `get` for the same key. It will call after thread3's init
        // closure finished, so it will get the value insert by thread3's init closure.
        let thread8 = {
            let cache8 = cache.clone();
            spawn(move || {
                // Wait for 800 ms before calling `get`.
                sleep(Duration::from_millis(800));
                let maybe_v = cache8.get(KEY);
                assert_eq!(maybe_v, Some("thread3"));
            })
        };

        for t in vec![
            thread1, thread2, thread3, thread4, thread5, thread6, thread7, thread8,
        ] {
            t.join().expect("Failed to join");
        }
    }

    #[test]
    // https://github.com/moka-rs/moka/issues/43
    fn handle_panic_in_get_with() {
        use std::{sync::Barrier, thread};

        let cache = Cache::new(16);
        let barrier = Arc::new(Barrier::new(2));
        {
            let cache_ref = cache.clone();
            let barrier_ref = barrier.clone();
            thread::spawn(move || {
                let _ = cache_ref.get_with(1, || {
                    barrier_ref.wait();
                    thread::sleep(Duration::from_millis(50));
                    panic!("Panic during get_with");
                });
            });
        }

        barrier.wait();
        assert_eq!(cache.get_with(1, || 5), 5);
    }

    #[test]
    // https://github.com/moka-rs/moka/issues/43
    fn handle_panic_in_try_get_with() {
        use std::{sync::Barrier, thread};

        let cache = Cache::new(16);
        let barrier = Arc::new(Barrier::new(2));
        {
            let cache_ref = cache.clone();
            let barrier_ref = barrier.clone();
            thread::spawn(move || {
                let _ = cache_ref.try_get_with(1, || {
                    barrier_ref.wait();
                    thread::sleep(Duration::from_millis(50));
                    panic!("Panic during try_get_with");
                }) as Result<_, Arc<Infallible>>;
            });
        }

        barrier.wait();
        assert_eq!(
            cache.try_get_with(1, || Ok(5)) as Result<_, Arc<Infallible>>,
            Ok(5)
        );
    }

    #[test]
    fn test_removal_notifications() {
        run_test(DeliveryMode::Immediate);
        run_test(DeliveryMode::Queued);

        fn run_test(delivery_mode: DeliveryMode) {
            // The following `Vec`s will hold actual and expected notifications.
            let actual = Arc::new(Mutex::new(Vec::new()));
            let mut expected = Vec::new();

            // Create an eviction listener.
            let a1 = Arc::clone(&actual);
            let listener = move |k, v, cause| a1.lock().push((k, v, cause));
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(delivery_mode)
                .build();

            // Create a cache with the eviction listener.
            let mut cache = Cache::builder()
                .max_capacity(3)
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.reconfigure_for_testing();

            // Make the cache exterior immutable.
            let cache = cache;

            cache.insert('a', "alice");
            cache.invalidate(&'a');
            expected.push((Arc::new('a'), "alice", RemovalCause::Explicit));

            cache.sync();
            assert_eq_with_mode!(cache.entry_count(), 0, delivery_mode);

            cache.insert('b', "bob");
            cache.insert('c', "cathy");
            cache.insert('d', "david");
            cache.sync();
            assert_eq_with_mode!(cache.entry_count(), 3, delivery_mode);

            // This will be rejected due to the size constraint.
            cache.insert('e', "emily");
            expected.push((Arc::new('e'), "emily", RemovalCause::Size));
            cache.sync();
            assert_eq_with_mode!(cache.entry_count(), 3, delivery_mode);

            // Raise the popularity of 'e' so it will be accepted next time.
            cache.get(&'e');
            cache.sync();

            // Retry.
            cache.insert('e', "eliza");
            // and the LRU entry will be evicted.
            expected.push((Arc::new('b'), "bob", RemovalCause::Size));
            cache.sync();
            assert_eq_with_mode!(cache.entry_count(), 3, delivery_mode);

            // Replace an existing entry.
            cache.insert('d', "dennis");
            expected.push((Arc::new('d'), "david", RemovalCause::Replaced));
            cache.sync();
            assert_eq_with_mode!(cache.entry_count(), 3, delivery_mode);

            verify_notification_vec(&cache, actual, &expected, delivery_mode);
        }
    }

    #[test]
    fn test_immediate_removal_notifications_with_updates() {
        // The following `Vec` will hold actual notifications.
        let actual = Arc::new(Mutex::new(Vec::new()));

        // Create an eviction listener.
        let a1 = Arc::clone(&actual);
        let listener = move |k, v, cause| a1.lock().push((k, v, cause));
        let listener_conf = notification::Configuration::builder()
            .delivery_mode(DeliveryMode::Immediate)
            .build();

        // Create a cache with the eviction listener and also TTL and TTI.
        let mut cache = Cache::builder()
            .eviction_listener_with_conf(listener, listener_conf)
            .time_to_live(Duration::from_secs(7))
            .time_to_idle(Duration::from_secs(5))
            .build();
        cache.reconfigure_for_testing();

        let (clock, mock) = Clock::mock();
        cache.set_expiration_clock(Some(clock));

        // Make the cache exterior immutable.
        let cache = cache;

        cache.insert("alice", "a0");
        cache.sync();

        // Now alice (a0) has been expired by the idle timeout (TTI).
        mock.increment(Duration::from_secs(6));
        assert_eq!(cache.get(&"alice"), None);

        // We have not ran sync after the expiration of alice (a0), so it is
        // still in the cache.
        assert_eq!(cache.entry_count(), 1);

        // Re-insert alice with a different value. Since alice (a0) is still
        // in the cache, this is actually a replace operation rather than an
        // insert operation. We want to verify that the RemovalCause of a0 is
        // Expired, not Replaced.
        cache.insert("alice", "a1");
        {
            let mut a = actual.lock();
            assert_eq!(a.len(), 1);
            assert_eq!(a[0], (Arc::new("alice"), "a0", RemovalCause::Expired));
            a.clear();
        }

        cache.sync();

        mock.increment(Duration::from_secs(4));
        assert_eq!(cache.get(&"alice"), Some("a1"));
        cache.sync();

        // Now alice has been expired by time-to-live (TTL).
        mock.increment(Duration::from_secs(4));
        assert_eq!(cache.get(&"alice"), None);

        // But, again, it is still in the cache.
        assert_eq!(cache.entry_count(), 1);

        // Re-insert alice with a different value and verify that the
        // RemovalCause of a1 is Expired (not Replaced).
        cache.insert("alice", "a2");
        {
            let mut a = actual.lock();
            assert_eq!(a.len(), 1);
            assert_eq!(a[0], (Arc::new("alice"), "a1", RemovalCause::Expired));
            a.clear();
        }

        cache.sync();

        assert_eq!(cache.entry_count(), 1);

        // Now alice (a2) has been expired by the idle timeout.
        mock.increment(Duration::from_secs(6));
        assert_eq!(cache.get(&"alice"), None);
        assert_eq!(cache.entry_count(), 1);

        // This invalidate will internally remove alice (a2).
        cache.invalidate(&"alice");
        cache.sync();
        assert_eq!(cache.entry_count(), 0);

        {
            let mut a = actual.lock();
            assert_eq!(a.len(), 1);
            assert_eq!(a[0], (Arc::new("alice"), "a2", RemovalCause::Expired));
            a.clear();
        }

        // Re-insert, and this time, make it expired by the TTL.
        cache.insert("alice", "a3");
        cache.sync();
        mock.increment(Duration::from_secs(4));
        assert_eq!(cache.get(&"alice"), Some("a3"));
        cache.sync();
        mock.increment(Duration::from_secs(4));
        assert_eq!(cache.get(&"alice"), None);
        assert_eq!(cache.entry_count(), 1);

        // This invalidate will internally remove alice (a2).
        cache.invalidate(&"alice");
        cache.sync();

        assert_eq!(cache.entry_count(), 0);

        {
            let mut a = actual.lock();
            assert_eq!(a.len(), 1);
            assert_eq!(a[0], (Arc::new("alice"), "a3", RemovalCause::Expired));
            a.clear();
        }
    }

    // This test ensures the key-level lock for the immediate notification
    // delivery mode is working so that the notifications for a given key
    // should always be ordered. This is true even if multiple client threads
    // try to modify the entries for the key at the same time. (This test will
    // run three client threads)
    #[test]
    fn test_key_lock_used_by_immediate_removal_notifications() {
        use std::thread::{sleep, spawn};

        const KEY: &str = "alice";

        type Val = &'static str;

        #[derive(PartialEq, Eq, Debug)]
        enum Event {
            Insert(Val),
            Invalidate(Val),
            BeginNotify(Val, RemovalCause),
            EndNotify(Val, RemovalCause),
        }

        // The following `Vec will hold actual notifications.
        let actual = Arc::new(Mutex::new(Vec::new()));

        // Create an eviction listener.
        // Note that this listener is slow and will take ~100 ms to complete.
        let a0 = Arc::clone(&actual);
        let listener = move |_k, v, cause| {
            a0.lock().push(Event::BeginNotify(v, cause));
            sleep(Duration::from_millis(100));
            a0.lock().push(Event::EndNotify(v, cause));
        };
        let listener_conf = notification::Configuration::builder()
            .delivery_mode(DeliveryMode::Immediate)
            .build();

        // Create a cache with the eviction listener and also TTL.
        let mut cache = Cache::builder()
            .eviction_listener_with_conf(listener, listener_conf)
            .time_to_live(Duration::from_millis(200))
            .build();
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        // - Notifications for the same key must not overlap.

        // Time  Event
        // ----- -------------------------------------
        // 0000: Insert value a0
        // 0200: a0 expired
        // 0210: Insert value a1 -> expired a0 (N-A0)
        // 0220: Insert value a2 (waiting) (A-A2)
        // 0310: N-A0 processed
        //       A-A2 finished waiting -> replace a1 (N-A1)
        // 0320: Invalidate (waiting) (R-A2)
        // 0410: N-A1 processed
        //       R-A2 finished waiting -> explicit a2 (N-A2)
        // 0510: N-A2 processed

        let expected = vec![
            Event::Insert("a0"),
            Event::Insert("a1"),
            Event::BeginNotify("a0", RemovalCause::Expired),
            Event::Insert("a2"),
            Event::EndNotify("a0", RemovalCause::Expired),
            Event::BeginNotify("a1", RemovalCause::Replaced),
            Event::Invalidate("a2"),
            Event::EndNotify("a1", RemovalCause::Replaced),
            Event::BeginNotify("a2", RemovalCause::Explicit),
            Event::EndNotify("a2", RemovalCause::Explicit),
        ];

        // 0000: Insert value a0
        actual.lock().push(Event::Insert("a0"));
        cache.insert(KEY, "a0");
        // Call `sync` to set the last modified for the KEY immediately so that
        // this entry should expire in 200 ms from now.
        cache.sync();

        // 0210: Insert value a1 -> expired a0 (N-A0)
        let thread1 = {
            let a1 = Arc::clone(&actual);
            let c1 = cache.clone();
            spawn(move || {
                sleep(Duration::from_millis(210));
                a1.lock().push(Event::Insert("a1"));
                c1.insert(KEY, "a1");
            })
        };

        // 0220: Insert value a2 (waiting) (A-A2)
        let thread2 = {
            let a2 = Arc::clone(&actual);
            let c2 = cache.clone();
            spawn(move || {
                sleep(Duration::from_millis(220));
                a2.lock().push(Event::Insert("a2"));
                c2.insert(KEY, "a2");
            })
        };

        // 0320: Invalidate (waiting) (R-A2)
        let thread3 = {
            let a3 = Arc::clone(&actual);
            let c3 = cache.clone();
            spawn(move || {
                sleep(Duration::from_millis(320));
                a3.lock().push(Event::Invalidate("a2"));
                c3.invalidate(&KEY);
            })
        };

        for t in vec![thread1, thread2, thread3] {
            t.join().expect("Failed to join");
        }

        let actual = actual.lock();
        assert_eq!(actual.len(), expected.len());

        for (i, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(actual, expected, "expected[{}]", i);
        }
    }

    // NOTE: To enable the panic logging, run the following command:
    //
    // RUST_LOG=moka=info cargo test --features 'logging' -- \
    //   sync::cache::tests::recover_from_panicking_eviction_listener --exact --nocapture
    //
    #[test]
    fn recover_from_panicking_eviction_listener() {
        #[cfg(feature = "logging")]
        let _ = env_logger::builder().is_test(true).try_init();

        run_test(DeliveryMode::Immediate);
        run_test(DeliveryMode::Queued);

        fn run_test(delivery_mode: DeliveryMode) {
            // The following `Vec`s will hold actual and expected notifications.
            let actual = Arc::new(Mutex::new(Vec::new()));
            let mut expected = Vec::new();

            // Create an eviction listener that panics when it see
            // a value "panic now!".
            let a1 = Arc::clone(&actual);
            let listener = move |k, v, cause| {
                if v == "panic now!" {
                    panic!("Panic now!");
                }
                a1.lock().push((k, v, cause))
            };
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(delivery_mode)
                .build();

            // Create a cache with the eviction listener.
            let mut cache = Cache::builder()
                .name("My Sync Cache")
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.reconfigure_for_testing();

            // Make the cache exterior immutable.
            let cache = cache;

            // Insert an okay value.
            cache.insert("alice", "a0");
            cache.sync();

            // Insert a value that will cause the eviction listener to panic.
            cache.insert("alice", "panic now!");
            expected.push((Arc::new("alice"), "a0", RemovalCause::Replaced));
            cache.sync();

            // Insert an okay value. This will replace the previous
            // value "panic now!" so the eviction listener will panic.
            cache.insert("alice", "a2");
            cache.sync();
            // No more removal notification should be sent.

            // Invalidate the okay value.
            cache.invalidate(&"alice");
            cache.sync();

            verify_notification_vec(&cache, actual, &expected, delivery_mode);
        }
    }

    // This test ensures that the `contains_key`, `get` and `invalidate` can use
    // borrowed form `&[u8]` for key with type `Vec<u8>`.
    // https://github.com/moka-rs/moka/issues/166
    #[test]
    fn borrowed_forms_of_key() {
        let cache: Cache<Vec<u8>, ()> = Cache::new(1);

        let key = vec![1_u8];
        cache.insert(key.clone(), ());

        // key as &Vec<u8>
        let key_v: &Vec<u8> = &key;
        assert!(cache.contains_key(key_v));
        assert_eq!(cache.get(key_v), Some(()));
        cache.invalidate(key_v);

        cache.insert(key, ());

        // key as &[u8]
        let key_s: &[u8] = &[1_u8];
        assert!(cache.contains_key(key_s));
        assert_eq!(cache.get(key_s), Some(()));
        cache.invalidate(key_s);
    }

    #[test]
    fn drop_value_immediately_after_eviction() {
        use crate::common::test_utils::{Counters, Value};

        const MAX_CAPACITY: u32 = 500;
        const KEYS: u32 = ((MAX_CAPACITY as f64) * 1.2) as u32;

        let counters = Arc::new(Counters::default());
        let counters1 = Arc::clone(&counters);

        let listener = move |_k, _v, cause| match cause {
            RemovalCause::Size => counters1.incl_evicted(),
            RemovalCause::Explicit => counters1.incl_invalidated(),
            _ => (),
        };

        let mut cache = Cache::builder()
            .max_capacity(MAX_CAPACITY as u64)
            .eviction_listener(listener)
            .build();
        cache.reconfigure_for_testing();

        // Make the cache exterior immutable.
        let cache = cache;

        for key in 0..KEYS {
            let value = Arc::new(Value::new(vec![0u8; 1024], &counters));
            cache.insert(key, value);
            counters.incl_inserted();
            cache.sync();
        }

        let eviction_count = KEYS - MAX_CAPACITY;

        cache.sync();
        assert_eq!(counters.inserted(), KEYS, "inserted");
        assert_eq!(counters.value_created(), KEYS, "value_created");
        assert_eq!(counters.evicted(), eviction_count, "evicted");
        assert_eq!(counters.invalidated(), 0, "invalidated");
        assert_eq!(counters.value_dropped(), eviction_count, "value_dropped");

        for key in 0..KEYS {
            cache.invalidate(&key);
            cache.sync();
        }

        cache.sync();
        assert_eq!(counters.inserted(), KEYS, "inserted");
        assert_eq!(counters.value_created(), KEYS, "value_created");
        assert_eq!(counters.evicted(), eviction_count, "evicted");
        assert_eq!(counters.invalidated(), MAX_CAPACITY, "invalidated");
        assert_eq!(counters.value_dropped(), KEYS, "value_dropped");

        std::mem::drop(cache);
        assert_eq!(counters.value_dropped(), KEYS, "value_dropped");
    }

    // Ignored by default. This test cannot run in parallel with other tests.
    #[test]
    #[ignore]
    fn enabling_and_disabling_thread_pools() {
        use crate::common::concurrent::thread_pool::{PoolName::*, ThreadPoolRegistry};

        // Enable the housekeeper pool.
        {
            let cache = Cache::builder().thread_pool_enabled(true).build();
            cache.insert('a', "a");
            let enabled_pools = ThreadPoolRegistry::enabled_pools();
            assert_eq!(enabled_pools, &[Housekeeper]);
        }

        // Enable the housekeeper and invalidator pools.
        {
            let cache = Cache::builder()
                .thread_pool_enabled(true)
                .support_invalidation_closures()
                .build();
            cache.insert('a', "a");
            let enabled_pools = ThreadPoolRegistry::enabled_pools();
            assert_eq!(enabled_pools, &[Housekeeper, Invalidator]);
        }

        // Queued delivery mode: Enable the housekeeper and removal notifier pools.
        {
            let listener = |_k, _v, _cause| {};
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(DeliveryMode::Queued)
                .build();
            let cache = Cache::builder()
                .thread_pool_enabled(true)
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.insert('a', "a");
            let enabled_pools = ThreadPoolRegistry::enabled_pools();
            assert_eq!(enabled_pools, &[Housekeeper, RemovalNotifier]);
        }

        // Immediate delivery mode: Enable only the housekeeper pool.
        {
            let listener = |_k, _v, _cause| {};
            let listener_conf = notification::Configuration::builder()
                .delivery_mode(DeliveryMode::Immediate)
                .build();
            let cache = Cache::builder()
                .thread_pool_enabled(true)
                .eviction_listener_with_conf(listener, listener_conf)
                .build();
            cache.insert('a', "a");
            let enabled_pools = ThreadPoolRegistry::enabled_pools();
            assert_eq!(enabled_pools, &[Housekeeper]);
        }

        // Disable all pools.
        {
            let cache = Cache::builder().thread_pool_enabled(false).build();
            cache.insert('a', "a");
            let enabled_pools = ThreadPoolRegistry::enabled_pools();
            assert!(enabled_pools.is_empty());
        }
    }

    #[test]
    fn test_debug_format() {
        let cache = Cache::new(10);
        cache.insert('a', "alice");
        cache.insert('b', "bob");
        cache.insert('c', "cindy");

        let debug_str = format!("{:?}", cache);
        assert!(debug_str.starts_with('{'));
        assert!(debug_str.contains(r#"'a': "alice""#));
        assert!(debug_str.contains(r#"'b': "bob""#));
        assert!(debug_str.contains(r#"'c': "cindy""#));
        assert!(debug_str.ends_with('}'));
    }

    type NotificationTuple<K, V> = (Arc<K>, V, RemovalCause);

    fn verify_notification_vec<K, V, S>(
        cache: &Cache<K, V, S>,
        actual: Arc<Mutex<Vec<NotificationTuple<K, V>>>>,
        expected: &[NotificationTuple<K, V>],
        delivery_mode: DeliveryMode,
    ) where
        K: std::hash::Hash + Eq + std::fmt::Debug + Send + Sync + 'static,
        V: Eq + std::fmt::Debug + Clone + Send + Sync + 'static,
        S: std::hash::BuildHasher + Clone + Send + Sync + 'static,
    {
        // Retries will be needed when testing in a QEMU VM.
        const MAX_RETRIES: usize = 5;
        let mut retries = 0;
        loop {
            // Ensure all scheduled notifications have been processed.
            cache.sync();
            std::thread::sleep(Duration::from_millis(500));

            let actual = &*actual.lock();
            if actual.len() != expected.len() {
                if retries <= MAX_RETRIES {
                    retries += 1;
                    continue;
                } else {
                    assert_eq!(
                        actual.len(),
                        expected.len(),
                        "Retries exhausted (delivery mode: {:?})",
                        delivery_mode
                    );
                }
            }

            for (i, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                assert_eq!(
                    actual, expected,
                    "expected[{}] (delivery mode: {:?})",
                    i, delivery_mode
                );
            }

            break;
        }
    }
}
