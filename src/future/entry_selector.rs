use crate::Entry;

use super::Cache;

use std::{
    borrow::Borrow,
    future::Future,
    hash::{BuildHasher, Hash},
    sync::Arc,
};

/// Provides advanced methods to select or insert an entry of the cache.
///
/// Many methods here return an [`Entry`], a snapshot of a single key-value pair in
/// the cache, carrying additional information like `is_fresh`.
///
/// `OwnedKeyEntrySelector` is constructed from the [`entry`][entry-method] method on
/// the cache.
///
/// [`Entry`]: ../struct.Entry.html
/// [entry-method]: ./struct.Cache.html#method.entry
pub struct OwnedKeyEntrySelector<'a, K, V, S> {
    owned_key: K,
    hash: u64,
    cache: &'a Cache<K, V, S>,
}

impl<'a, K, V, S> OwnedKeyEntrySelector<'a, K, V, S>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(owned_key: K, hash: u64, cache: &'a Cache<K, V, S>) -> Self {
        Self {
            owned_key,
            hash,
            cache,
        }
    }

    /// Returns the corresponding [`Entry`] for the key given when this entry
    /// selector was constructed. If the entry does not exist, inserts one by calling
    /// the [`default`][std-default-function] function of the value type `V`.
    ///
    /// [`Entry`]: ../struct.Entry.html
    /// [std-default-function]: https://doc.rust-lang.org/stable/std/default/trait.Default.html#tymethod.default
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, Option<u32>> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let entry = cache.entry(key.clone()).or_default().await;
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), None);
    ///
    ///     let entry = cache.entry(key).or_default().await;
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    /// }
    /// ```
    pub async fn or_default(self) -> Entry<K, V>
    where
        V: Default,
    {
        let key = Arc::new(self.owned_key);
        self.cache
            .get_or_insert_with_hash(key, self.hash, Default::default)
            .await
    }

    /// Returns the corresponding [`Entry`] for the key given when this entry
    /// selector was constructed. If the entry does not exist, inserts one by using
    /// the the given `default` value for `V`.
    ///
    /// [`Entry`]: ../struct.Entry.html
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, u32> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let entry = cache.entry(key.clone()).or_insert(3).await;
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), 3);
    ///
    ///     let entry = cache.entry(key).or_insert(6).await;
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    ///     assert_eq!(entry.into_value(), 3);
    /// }
    /// ```
    pub async fn or_insert(self, default: V) -> Entry<K, V> {
        let key = Arc::new(self.owned_key);
        let init = || default;
        self.cache
            .get_or_insert_with_hash(key, self.hash, init)
            .await
    }

    /// Returns the corresponding [`Entry`] for the key given when this entry
    /// selector was constructed. If the entry does not exist, resolves the `init`
    /// future and inserts the output.
    ///
    /// [`Entry`]: ../struct.Entry.html
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, String> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let entry = cache
    ///         .entry(key.clone())
    ///         .or_insert_with(async { "value1".to_string() })
    ///         .await;
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), "value1");
    ///
    ///     let entry = cache
    ///         .entry(key)
    ///         .or_insert_with(async { "value2".to_string() })
    ///         .await;
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    ///     assert_eq!(entry.into_value(), "value1");
    /// }
    /// ```
    ///
    /// # Concurrent calls on the same key
    ///
    /// This method guarantees that concurrent calls on the same not-existing entry
    /// are coalesced into one evaluation of the `init` future. Only one of the calls
    /// evaluates its future (thus returned entry's `is_fresh` method returns
    /// `true`), and other calls wait for that future to resolve (and their
    /// `is_fresh` return `false`).
    ///
    /// For more detail about the coalescing behavior, see
    /// [`Cache::get_with`][get-with-method].
    ///
    /// [get-with-method]: ./struct.Cache.html#method.get_with
    pub async fn or_insert_with(self, init: impl Future<Output = V>) -> Entry<K, V> {
        futures_util::pin_mut!(init);
        let key = Arc::new(self.owned_key);
        let replace_if = None as Option<fn(&V) -> bool>;
        self.cache
            .get_or_insert_with_hash_and_fun(key, self.hash, init, replace_if, true)
            .await
    }

    /// Works like [`or_insert_with`](#method.or_insert_with), but takes an additional
    /// `replace_if` closure.
    ///
    /// This method will resolve the `init` future and insert the output to the
    /// cache when:
    ///
    /// - The key does not exist.
    /// - Or, `replace_if` closure returns `true`.
    pub async fn or_insert_with_if(
        self,
        init: impl Future<Output = V>,
        replace_if: impl FnMut(&V) -> bool + Send,
    ) -> Entry<K, V> {
        futures_util::pin_mut!(init);
        let key = Arc::new(self.owned_key);
        self.cache
            .get_or_insert_with_hash_and_fun(key, self.hash, init, Some(replace_if), true)
            .await
    }

    /// Returns the corresponding [`Entry`] for the key given when this entry
    /// selector was constructed. If the entry does not exist, resolves the `init`
    /// future, and inserts an entry if `Some(value)` was returned. If `None` was
    /// returned from the future, this method does not insert an entry and returns
    /// `None`.
    ///
    /// [`Entry`]: ../struct.Entry.html
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, u32> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let none_entry = cache
    ///         .entry(key.clone())
    ///         .or_optionally_insert_with(async { None })
    ///         .await;
    ///     assert!(none_entry.is_none());
    ///
    ///     let some_entry = cache
    ///         .entry(key.clone())
    ///         .or_optionally_insert_with(async { Some(3) })
    ///         .await;
    ///     assert!(some_entry.is_some());
    ///     let entry = some_entry.unwrap();
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), 3);
    ///
    ///     let some_entry = cache
    ///         .entry(key)
    ///         .or_optionally_insert_with(async { Some(6) })
    ///         .await;
    ///     let entry = some_entry.unwrap();
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    ///     assert_eq!(entry.into_value(), 3);
    /// }
    /// ```
    ///
    /// # Concurrent calls on the same key
    ///
    /// This method guarantees that concurrent calls on the same not-existing entry
    /// are coalesced into one evaluation of the `init` future. Only one of the calls
    /// evaluates its future (thus returned entry's `is_fresh` method returns
    /// `true`), and other calls wait for that future to resolve (and their
    /// `is_fresh` return `false`).
    ///
    /// For more detail about the coalescing behavior, see
    /// [`Cache::optionally_get_with`][opt-get-with-method].
    ///
    /// [opt-get-with-method]: ./struct.Cache.html#method.optionally_get_with
    pub async fn or_optionally_insert_with(
        self,
        init: impl Future<Output = Option<V>>,
    ) -> Option<Entry<K, V>> {
        futures_util::pin_mut!(init);
        let key = Arc::new(self.owned_key);
        self.cache
            .get_or_optionally_insert_with_hash_and_fun(key, self.hash, init, true)
            .await
    }

    /// Returns the corresponding [`Entry`] for the key given when this entry
    /// selector was constructed. If the entry does not exist, resolves the `init`
    /// future, and inserts an entry if `Ok(value)` was returned. If `Err(_)` was
    /// returned from the future, this method does not insert an entry and returns
    /// the `Err` wrapped by [`std::sync::Arc`][std-arc].
    ///
    /// [`Entry`]: ../struct.Entry.html
    /// [std-arc]: https://doc.rust-lang.org/stable/std/sync/struct.Arc.html
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, u32> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let error_entry = cache
    ///         .entry(key.clone())
    ///         .or_try_insert_with(async { Err("error") })
    ///         .await;
    ///     assert!(error_entry.is_err());
    ///
    ///     let ok_entry = cache
    ///         .entry(key.clone())
    ///         .or_try_insert_with(async { Ok::<u32, &str>(3) })
    ///         .await;
    ///     assert!(ok_entry.is_ok());
    ///     let entry = ok_entry.unwrap();
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), 3);
    ///
    ///     let ok_entry = cache
    ///         .entry(key)
    ///         .or_try_insert_with(async { Ok::<u32, &str>(6) })
    ///         .await;
    ///     let entry = ok_entry.unwrap();
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    ///     assert_eq!(entry.into_value(), 3);
    /// }
    /// ```
    ///
    /// # Concurrent calls on the same key
    ///
    /// This method guarantees that concurrent calls on the same not-existing entry
    /// are coalesced into one evaluation of the `init` future (as long as these
    /// futures return the same error type). Only one of the calls evaluates its
    /// future (thus returned entry's `is_fresh` method returns `true`), and other
    /// calls wait for that future to resolve (and their `is_fresh` return `false`).
    ///
    /// For more detail about the coalescing behavior, see
    /// [`Cache::try_get_with`][try-get-with-method].
    ///
    /// [try-get-with-method]: ./struct.Cache.html#method.try_get_with
    pub async fn or_try_insert_with<F, E>(self, init: F) -> Result<Entry<K, V>, Arc<E>>
    where
        F: Future<Output = Result<V, E>>,
        E: Send + Sync + 'static,
    {
        futures_util::pin_mut!(init);
        let key = Arc::new(self.owned_key);
        self.cache
            .get_or_try_insert_with_hash_and_fun(key, self.hash, init, true)
            .await
    }
}

/// Provides advanced methods to select or insert an entry of the cache.
///
/// Many methods here return an [`Entry`], a snapshot of a single key-value pair in
/// the cache, carrying additional information like `is_fresh`.
///
/// `RefKeyEntrySelector` is constructed from the
/// [`entry_by_ref`][entry-by-ref-method] method on the cache.
///
/// [`Entry`]: ../struct.Entry.html
/// [entry-by-ref-method]: ./struct.Cache.html#method.entry_by_ref
pub struct RefKeyEntrySelector<'a, K, Q, V, S>
where
    Q: ?Sized,
{
    ref_key: &'a Q,
    hash: u64,
    cache: &'a Cache<K, V, S>,
}

impl<'a, K, Q, V, S> RefKeyEntrySelector<'a, K, Q, V, S>
where
    K: Borrow<Q> + Hash + Eq + Send + Sync + 'static,
    Q: ToOwned<Owned = K> + Hash + Eq + ?Sized,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(ref_key: &'a Q, hash: u64, cache: &'a Cache<K, V, S>) -> Self {
        Self {
            ref_key,
            hash,
            cache,
        }
    }

    /// Returns the corresponding [`Entry`] for the reference of the key given when
    /// this entry selector was constructed. If the entry does not exist, inserts one
    /// by cloning the key and calling the [`default`][std-default-function] function
    /// of the value type `V`.
    ///
    /// [`Entry`]: ../struct.Entry.html
    /// [std-default-function]: https://doc.rust-lang.org/stable/std/default/trait.Default.html#tymethod.default
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, Option<u32>> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let entry = cache.entry_by_ref(&key).or_default().await;
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), None);
    ///
    ///     let entry = cache.entry_by_ref(&key).or_default().await;
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    /// }
    /// ```
    pub async fn or_default(self) -> Entry<K, V>
    where
        V: Default,
    {
        self.cache
            .get_or_insert_with_hash_by_ref(self.ref_key, self.hash, Default::default)
            .await
    }

    /// Returns the corresponding [`Entry`] for the reference of the key given when
    /// this entry selector was constructed. If the entry does not exist, inserts one
    /// by cloning the key and using the given `default` value for `V`.
    ///
    /// [`Entry`]: ../struct.Entry.html
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, u32> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let entry = cache.entry_by_ref(&key).or_insert(3).await;
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), 3);
    ///
    ///     let entry = cache.entry_by_ref(&key).or_insert(6).await;
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    ///     assert_eq!(entry.into_value(), 3);
    /// }
    /// ```
    pub async fn or_insert(self, default: V) -> Entry<K, V> {
        let init = || default;
        self.cache
            .get_or_insert_with_hash_by_ref(self.ref_key, self.hash, init)
            .await
    }

    /// Returns the corresponding [`Entry`] for the reference of the key given when
    /// this entry selector was constructed. If the entry does not exist, inserts one
    /// by cloning the key and resolving the `init` future for the value.
    ///
    /// [`Entry`]: ../struct.Entry.html
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, String> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let entry = cache
    ///         .entry_by_ref(&key)
    ///         .or_insert_with(async { "value1".to_string() })
    ///         .await;
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), "value1");
    ///
    ///     let entry = cache
    ///         .entry_by_ref(&key)
    ///         .or_insert_with(async { "value2".to_string() })
    ///         .await;
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    ///     assert_eq!(entry.into_value(), "value1");
    /// }
    /// ```
    ///
    /// # Concurrent calls on the same key
    ///
    /// This method guarantees that concurrent calls on the same not-existing entry
    /// are coalesced into one evaluation of the `init` future. Only one of the calls
    /// evaluates its future (thus returned entry's `is_fresh` method returns
    /// `true`), and other calls wait for that future to resolve (and their
    /// `is_fresh` return `false`).
    ///
    /// For more detail about the coalescing behavior, see
    /// [`Cache::get_with`][get-with-method].
    ///
    /// [get-with-method]: ./struct.Cache.html#method.get_with
    pub async fn or_insert_with(self, init: impl Future<Output = V>) -> Entry<K, V> {
        futures_util::pin_mut!(init);
        let replace_if = None as Option<fn(&V) -> bool>;
        self.cache
            .get_or_insert_with_hash_by_ref_and_fun(self.ref_key, self.hash, init, replace_if, true)
            .await
    }

    /// Works like [`or_insert_with`](#method.or_insert_with), but takes an additional
    /// `replace_if` closure.
    ///
    /// This method will resolve the `init` future and insert the output to the
    /// cache when:
    ///
    /// - The key does not exist.
    /// - Or, `replace_if` closure returns `true`.
    pub async fn or_insert_with_if(
        self,
        init: impl Future<Output = V>,
        replace_if: impl FnMut(&V) -> bool + Send,
    ) -> Entry<K, V> {
        futures_util::pin_mut!(init);
        self.cache
            .get_or_insert_with_hash_by_ref_and_fun(
                self.ref_key,
                self.hash,
                init,
                Some(replace_if),
                true,
            )
            .await
    }

    /// Returns the corresponding [`Entry`] for the reference of the key given when
    /// this entry selector was constructed. If the entry does not exist, clones the
    /// key and resolves the `init` future. If `Some(value)` was returned by the
    /// future, inserts an entry with the value . If `None` was returned, this method
    /// does not insert an entry and returns `None`.
    ///
    /// [`Entry`]: ../struct.Entry.html
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, u32> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let none_entry = cache
    ///         .entry_by_ref(&key)
    ///         .or_optionally_insert_with(async { None })
    ///         .await;
    ///     assert!(none_entry.is_none());
    ///
    ///     let some_entry = cache
    ///         .entry_by_ref(&key)
    ///         .or_optionally_insert_with(async { Some(3) })
    ///         .await;
    ///     assert!(some_entry.is_some());
    ///     let entry = some_entry.unwrap();
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), 3);
    ///
    ///     let some_entry = cache
    ///         .entry_by_ref(&key)
    ///         .or_optionally_insert_with(async { Some(6) })
    ///         .await;
    ///     let entry = some_entry.unwrap();
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    ///     assert_eq!(entry.into_value(), 3);
    /// }
    /// ```
    ///
    /// # Concurrent calls on the same key
    /// This method guarantees that concurrent calls on the same not-existing entry
    /// are coalesced into one evaluation of the `init` future. Only one of the calls
    /// evaluates its future (thus returned entry's `is_fresh` method returns
    /// `true`), and other calls wait for that future to resolve (and their
    /// `is_fresh` return `false`).
    ///
    /// For more detail about the coalescing behavior, see
    /// [`Cache::optionally_get_with`][opt-get-with-method].
    ///
    /// [opt-get-with-method]: ./struct.Cache.html#method.optionally_get_with
    pub async fn or_optionally_insert_with(
        self,
        init: impl Future<Output = Option<V>>,
    ) -> Option<Entry<K, V>> {
        futures_util::pin_mut!(init);
        self.cache
            .get_or_optionally_insert_with_hash_by_ref_and_fun(self.ref_key, self.hash, init, true)
            .await
    }

    /// Returns the corresponding [`Entry`] for the reference of the key given when
    /// this entry selector was constructed. If the entry does not exist, clones the
    /// key and resolves the `init` future. If `Ok(value)` was returned from the
    /// future, inserts an entry with the value. If `Err(_)` was returned, this
    /// method does not insert an entry and returns the `Err` wrapped by
    /// [`std::sync::Arc`][std-arc].
    ///
    /// [`Entry`]: ../struct.Entry.html
    /// [std-arc]: https://doc.rust-lang.org/stable/std/sync/struct.Arc.html
    ///
    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.12", features = ["future"] }
    /// // tokio = { version = "1", features = ["rt-multi-thread", "macros" ] }
    ///
    /// use moka::future::Cache;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let cache: Cache<String, u32> = Cache::new(100);
    ///     let key = "key1".to_string();
    ///
    ///     let error_entry = cache
    ///         .entry_by_ref(&key)
    ///         .or_try_insert_with(async { Err("error") })
    ///         .await;
    ///     assert!(error_entry.is_err());
    ///
    ///     let ok_entry = cache
    ///         .entry_by_ref(&key)
    ///         .or_try_insert_with(async { Ok::<u32, &str>(3) })
    ///         .await;
    ///     assert!(ok_entry.is_ok());
    ///     let entry = ok_entry.unwrap();
    ///     assert!(entry.is_fresh());
    ///     assert_eq!(entry.key(), &key);
    ///     assert_eq!(entry.into_value(), 3);
    ///
    ///     let ok_entry = cache
    ///         .entry_by_ref(&key)
    ///         .or_try_insert_with(async { Ok::<u32, &str>(6) })
    ///         .await;
    ///     let entry = ok_entry.unwrap();
    ///     // Not fresh because the value was already in the cache.
    ///     assert!(!entry.is_fresh());
    ///     assert_eq!(entry.into_value(), 3);
    /// }
    /// ```
    ///
    /// # Concurrent calls on the same key
    ///
    /// This method guarantees that concurrent calls on the same not-existing entry
    /// are coalesced into one evaluation of the `init` future (as long as these
    /// futures return the same error type). Only one of the calls evaluates its
    /// future (thus returned entry's `is_fresh` method returns `true`), and other
    /// calls wait for that future to resolve (and their `is_fresh` return `false`).
    ///
    /// For more detail about the coalescing behavior, see
    /// [`Cache::try_get_with`][try-get-with-method].
    ///
    /// [try-get-with-method]: ./struct.Cache.html#method.try_get_with
    pub async fn or_try_insert_with<F, E>(self, init: F) -> Result<Entry<K, V>, Arc<E>>
    where
        F: Future<Output = Result<V, E>>,
        E: Send + Sync + 'static,
    {
        futures_util::pin_mut!(init);
        self.cache
            .get_or_try_insert_with_hash_by_ref_and_fun(self.ref_key, self.hash, init, true)
            .await
    }
}
