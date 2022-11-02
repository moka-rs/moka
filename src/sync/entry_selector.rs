use crate::Entry;

use super::Cache;

use std::{
    borrow::Borrow,
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
    /// use moka::sync::Cache;
    ///
    /// let cache: Cache<String, Option<u32>> = Cache::new(100);
    /// let key = "key1".to_string();
    ///
    /// let entry = cache.entry(key.clone()).or_default();
    /// assert!(entry.is_fresh());
    /// assert_eq!(entry.into_value(), None);
    ///
    /// let entry = cache.entry(key).or_default();
    /// // Not fresh because the value is already in the cache.
    /// assert!(!entry.is_fresh());
    /// ```
    pub fn or_default(self) -> Entry<K, V>
    where
        V: Default,
    {
        let key = Arc::new(self.owned_key);
        self.cache
            .get_or_insert_with_hash(key, self.hash, Default::default)
    }

    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    ///
    /// let cache: Cache<String, u32> = Cache::new(100);
    /// let key = "key1".to_string();
    ///
    /// let entry = cache.entry(key.clone()).or_insert(3);
    /// assert!(entry.is_fresh());
    /// assert_eq!(entry.into_value(), 3);
    ///
    /// let entry = cache.entry(key).or_insert(6);
    /// // Not fresh because the value is already in the cache.
    /// assert!(!entry.is_fresh());
    /// assert_eq!(entry.into_value(), 3);
    /// ```
    pub fn or_insert(self, default: V) -> Entry<K, V> {
        let key = Arc::new(self.owned_key);
        let init = || default;
        self.cache.get_or_insert_with_hash(key, self.hash, init)
    }

    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    ///
    /// let cache: Cache<String, String> = Cache::new(100);
    /// let key = "key1".to_string();
    ///
    /// let entry = cache
    ///     .entry(key.clone())
    ///     .or_insert_with(|| "value1".to_string());
    /// assert!(entry.is_fresh());
    /// assert_eq!(entry.into_value(), "value1");
    ///
    /// let entry = cache
    ///     .entry(key)
    ///     .or_insert_with(|| "value2".to_string());
    /// // Not fresh because the value is already in the cache.
    /// assert!(!entry.is_fresh());
    /// assert_eq!(entry.into_value(), "value1");
    /// ```
    pub fn or_insert_with(self, init: impl FnOnce() -> V) -> Entry<K, V> {
        let key = Arc::new(self.owned_key);
        let replace_if = None as Option<fn(&V) -> bool>;
        self.cache
            .get_or_insert_with_hash_and_fun(key, self.hash, init, replace_if, true)
    }

    /// Works like [`or_insert_with`](#method.or_insert_with), but takes an additional
    /// `replace_if` closure.
    ///
    /// This method will evaluate the `init` closure and insert the output to the
    /// cache when:
    ///
    /// - The key does not exist.
    /// - Or, `replace_if` closure returns `true`.
    pub fn or_insert_with_if(
        self,
        init: impl FnOnce() -> V,
        replace_if: impl FnMut(&V) -> bool,
    ) -> Entry<K, V> {
        let key = Arc::new(self.owned_key);
        self.cache
            .get_or_insert_with_hash_and_fun(key, self.hash, init, Some(replace_if), true)
    }

    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    ///
    /// let cache: Cache<String, u32> = Cache::new(100);
    /// let key = "key1".to_string();
    ///
    /// let none_entry = cache
    ///     .entry(key.clone())
    ///     .or_optionally_insert_with(|| None);
    /// assert!(none_entry.is_none());
    ///
    /// let some_entry = cache
    ///     .entry(key.clone())
    ///     .or_optionally_insert_with(|| Some(3));
    /// assert!(some_entry.is_some());
    /// let entry = some_entry.unwrap();
    /// assert!(entry.is_fresh());
    /// assert_eq!(entry.into_value(), 3);
    ///
    /// let some_entry = cache
    ///     .entry(key)
    ///     .or_optionally_insert_with(|| Some(6));
    /// let entry = some_entry.unwrap();
    /// // Not fresh because the value is already in the cache.
    /// assert!(!entry.is_fresh());
    /// assert_eq!(entry.into_value(), 3);
    /// ```
    pub fn or_optionally_insert_with(
        self,
        init: impl FnOnce() -> Option<V>,
    ) -> Option<Entry<K, V>> {
        let key = Arc::new(self.owned_key);
        self.cache
            .get_or_optionally_insert_with_hash_and_fun(key, self.hash, init, true)
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
    /// use moka::sync::Cache;
    ///
    /// let cache: Cache<String, Option<u32>> = Cache::new(100);
    /// let key = "key1".to_string();
    ///
    /// let entry = cache.entry_by_ref(&key).or_default();
    /// assert!(entry.is_fresh());
    /// assert_eq!(entry.into_value(), None);
    ///
    /// let entry = cache.entry_by_ref(&key).or_default();
    /// // Not fresh because the value is already in the cache.
    /// assert!(!entry.is_fresh());
    /// ```
    pub fn or_default(self) -> Entry<K, V>
    where
        V: Default,
    {
        self.cache
            .get_or_insert_with_hash_by_ref(self.ref_key, self.hash, Default::default)
    }

    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    ///
    /// let cache: Cache<String, u32> = Cache::new(100);
    /// let key = "key1".to_string();
    ///
    /// let entry = cache.entry_by_ref(&key).or_insert(3);
    /// assert!(entry.is_fresh());
    /// assert_eq!(entry.into_value(), 3);
    ///
    /// let entry = cache.entry_by_ref(&key).or_insert(6);
    /// // Not fresh because the value is already in the cache.
    /// assert!(!entry.is_fresh());
    /// assert_eq!(entry.into_value(), 3);
    /// ```
    pub fn or_insert(self, default: V) -> Entry<K, V> {
        let init = || default;
        self.cache
            .get_or_insert_with_hash_by_ref(self.ref_key, self.hash, init)
    }

    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    ///
    /// let cache: Cache<String, String> = Cache::new(100);
    /// let key = "key1".to_string();
    ///
    /// let entry = cache
    ///     .entry_by_ref(&key)
    ///     .or_insert_with(|| "value1".to_string());
    /// assert!(entry.is_fresh());
    /// assert_eq!(entry.into_value(), "value1");
    ///
    /// let entry = cache
    ///     .entry_by_ref(&key)
    ///     .or_insert_with(|| "value2".to_string());
    /// // Not fresh because the value is already in the cache.
    /// assert!(!entry.is_fresh());
    /// assert_eq!(entry.into_value(), "value1");
    /// ```
    pub fn or_insert_with(self, init: impl FnOnce() -> V) -> Entry<K, V> {
        let replace_if = None as Option<fn(&V) -> bool>;
        self.cache.get_or_insert_with_hash_by_ref_and_fun(
            self.ref_key,
            self.hash,
            init,
            replace_if,
            true,
        )
    }

    pub fn or_insert_with_if(
        self,
        init: impl FnOnce() -> V,
        replace_if: impl FnMut(&V) -> bool,
    ) -> Entry<K, V> {
        self.cache.get_or_insert_with_hash_by_ref_and_fun(
            self.ref_key,
            self.hash,
            init,
            Some(replace_if),
            true,
        )
    }

    /// # Example
    ///
    /// ```rust
    /// use moka::sync::Cache;
    ///
    /// let cache: Cache<String, u32> = Cache::new(100);
    /// let key = "key1".to_string();
    ///
    /// let none_entry = cache
    ///     .entry_by_ref(&key)
    ///     .or_optionally_insert_with(|| None);
    /// assert!(none_entry.is_none());
    ///
    /// let some_entry = cache
    ///     .entry_by_ref(&key)
    ///     .or_optionally_insert_with(|| Some(3));
    /// assert!(some_entry.is_some());
    /// let entry = some_entry.unwrap();
    /// assert!(entry.is_fresh());
    /// assert_eq!(entry.into_value(), 3);
    ///
    /// let some_entry = cache
    ///     .entry_by_ref(&key)
    ///     .or_optionally_insert_with(|| Some(6));
    /// let entry = some_entry.unwrap();
    /// // Not fresh because the value is already in the cache.
    /// assert!(!entry.is_fresh());
    /// assert_eq!(entry.into_value(), 3);
    /// ```
    pub fn or_optionally_insert_with(
        self,
        init: impl FnOnce() -> Option<V>,
    ) -> Option<Entry<K, V>> {
        self.cache
            .get_or_optionally_insert_with_hash_by_ref_and_fun(self.ref_key, self.hash, init, true)
    }
}
