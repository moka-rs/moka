pub(crate) mod base_cache;
mod invalidator;
pub(crate) mod iter;
mod key_lock;

/// The type of the unique ID to identify a predicate used by
/// [`Cache::invalidate_entries_if`][invalidate-if] method.
///
/// A `PredicateId` is a `String` of UUID (version 4).
///
/// [invalidate-if]: ./struct.Cache.html#method.invalidate_entries_if
pub type PredicateId = String;

pub(crate) type PredicateIdStr<'a> = &'a str;
