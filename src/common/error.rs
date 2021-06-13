/// The error type for the functionalities around
/// [`Cache#invalidate_entries_if`][invalidate-if] method.
///
/// [invalidate-if]: ./sync/struct.Cache.html#method.invalidate_entries_if
#[derive(thiserror::Error, Debug)]
pub enum PredicateError {
    /// This cache does not have a necessary configuration enabled for supporting
    /// invalidation closures.
    ///
    /// You can enable the configuration by calling the
    /// [`CacheBuilder::enable_invalidation_with_closures`][enable-invalidation-closures]
    /// method at the cache creation time.
    ///
    /// [enable-invalidation-closures]: ./sync/struct.CacheBuilder.html#method.enable_invalidation_with_closures
    #[error(
        "Support for invalidation closures is disabled in this cache. \
    Please enable it by calling the enable_invalidation_with_closures method \
    of the builder at the cache creation time"
    )]
    InvalidationClosuresDisabled,
}
