pub(crate) mod iter;

#[cfg(feature = "sync")]
pub(crate) mod base_cache;

#[cfg(feature = "sync")]
mod invalidator;

#[cfg(feature = "sync")]
mod key_lock;

/// The type of the unique ID to identify a predicate used by
/// [`Cache::invalidate_entries_if`][invalidate-if] method.
///
/// A `PredicateId` is a `String` of UUID (version 4).
///
/// [invalidate-if]: ./struct.Cache.html#method.invalidate_entries_if
#[cfg(feature = "sync")]
pub type PredicateId = String;

#[cfg(feature = "sync")]
pub(crate) type PredicateIdStr<'a> = &'a str;
