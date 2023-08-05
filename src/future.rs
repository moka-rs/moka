//! Provides a thread-safe, concurrent asynchronous (futures aware) cache
//! implementation.
//!
//! To use this module, enable a crate feature called "future".

use futures_util::future::BoxFuture;
use std::{future::Future, hash::Hash, sync::Arc};

mod base_cache;
mod builder;
mod cache;
mod entry_selector;
mod housekeeper;
mod invalidator;
mod key_lock;
mod notifier;
mod value_initializer;

pub use {
    builder::CacheBuilder,
    // cache::{BlockingOp, Cache},
    cache::Cache,
    entry_selector::{OwnedKeyEntrySelector, RefKeyEntrySelector},
};

/// The type of the unique ID to identify a predicate used by
/// [`Cache::invalidate_entries_if`][invalidate-if] method.
///
/// A `PredicateId` is a `String` of UUID (version 4).
///
/// [invalidate-if]: ./struct.Cache.html#method.invalidate_entries_if
pub type PredicateId = String;

pub(crate) type PredicateIdStr<'a> = &'a str;

// Empty struct to be used in InitResult::InitErr to represent the Option None.
pub(crate) struct OptionallyNone;

impl<T: ?Sized> FutureExt for T where T: Future {}

pub trait FutureExt: Future {
    fn boxed<'a, T>(self) -> BoxFuture<'a, T>
    where
        Self: Future<Output = T> + Sized + Send + 'a,
    {
        Box::pin(self)
    }
}

pub struct Iter<'i, K, V>(crate::sync_base::iter::Iter<'i, K, V>);

impl<'i, K, V> Iter<'i, K, V> {
    pub(crate) fn new(inner: crate::sync_base::iter::Iter<'i, K, V>) -> Self {
        Self(inner)
    }
}

impl<'i, K, V> Iterator for Iter<'i, K, V>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    type Item = (Arc<K>, V);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}
