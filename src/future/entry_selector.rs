use crate::Entry;

use super::Cache;

use std::{
    borrow::Borrow,
    future::Future,
    hash::{BuildHasher, Hash},
    sync::Arc,
};

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

    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.10", features = ["future"] }
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
    ///     assert_eq!(entry.is_fresh(), true);
    ///     assert_eq!(entry.into_value(), None);
    ///
    ///     let entry = cache.entry(key).or_default().await;
    ///     assert_eq!(entry.is_fresh(), false);
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

    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.10", features = ["future"] }
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
    ///     assert_eq!(entry.is_fresh(), true);
    ///     assert_eq!(entry.into_value(), 3);
    ///
    ///     let entry = cache.entry(key).or_insert(6).await;
    ///     assert_eq!(entry.is_fresh(), false);
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

    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.10", features = ["future"] }
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
    ///     assert_eq!(entry.is_fresh(), true);
    ///     assert_eq!(entry.into_value(), "value1");
    ///
    ///     let entry = cache
    ///         .entry(key)
    ///         .or_insert_with(async { "value2".to_string() })
    ///         .await;
    ///     assert_eq!(entry.is_fresh(), false);
    ///     assert_eq!(entry.into_value(), "value1");
    /// }
    /// ```
    pub async fn or_insert_with(self, init: impl Future<Output = V>) -> Entry<K, V> {
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
        replace_if: impl FnMut(&V) -> bool,
    ) -> Entry<K, V> {
        let key = Arc::new(self.owned_key);
        self.cache
            .get_or_insert_with_hash_and_fun(key, self.hash, init, Some(replace_if), true)
            .await
    }
}

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

    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.10", features = ["future"] }
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
    ///     assert_eq!(entry.is_fresh(), true);
    ///     assert_eq!(entry.into_value(), None);
    ///
    ///     let entry = cache.entry_by_ref(&key).or_default().await;
    ///     assert_eq!(entry.is_fresh(), false);
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

    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.10", features = ["future"] }
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
    ///     assert_eq!(entry.is_fresh(), true);
    ///     assert_eq!(entry.into_value(), 3);
    ///
    ///     let entry = cache.entry_by_ref(&key).or_insert(6).await;
    ///     assert_eq!(entry.is_fresh(), false);
    ///     assert_eq!(entry.into_value(), 3);
    /// }
    /// ```
    pub async fn or_insert(self, default: V) -> Entry<K, V> {
        let init = || default;
        self.cache
            .get_or_insert_with_hash_by_ref(self.ref_key, self.hash, init)
            .await
    }

    /// # Example
    ///
    /// ```rust
    /// // Cargo.toml
    /// //
    /// // [dependencies]
    /// // moka = { version = "0.10", features = ["future"] }
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
    ///     assert_eq!(entry.is_fresh(), true);
    ///     assert_eq!(entry.into_value(), "value1");
    ///
    ///     let entry = cache
    ///         .entry_by_ref(&key)
    ///         .or_insert_with(async { "value2".to_string() })
    ///         .await;
    ///     assert_eq!(entry.is_fresh(), false);
    ///     assert_eq!(entry.into_value(), "value1");
    /// }
    /// ```
    pub async fn or_insert_with(self, init: impl Future<Output = V>) -> Entry<K, V> {
        let owned_key: K = self.ref_key.to_owned();
        let key = Arc::new(owned_key);
        let replace_if = None as Option<fn(&V) -> bool>;
        self.cache
            .get_or_insert_with_hash_and_fun(key, self.hash, init, replace_if, true)
            .await
    }

    pub async fn or_insert_with_if(
        self,
        init: impl Future<Output = V>,
        replace_if: impl FnMut(&V) -> bool,
    ) -> Entry<K, V> {
        let owned_key: K = self.ref_key.to_owned();
        let key = Arc::new(owned_key);
        self.cache
            .get_or_insert_with_hash_and_fun(key, self.hash, init, Some(replace_if), true)
            .await
    }
}
