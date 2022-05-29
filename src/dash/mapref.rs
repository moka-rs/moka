use crate::common::concurrent::ValueEntry;

use std::{
    hash::{BuildHasher, Hash},
    sync::Arc,
};
use triomphe::Arc as TrioArc;

type DashMapRef<'a, K, V, S> =
    dashmap::mapref::multiple::RefMulti<'a, Arc<K>, TrioArc<ValueEntry<K, V>>, S>;

pub struct EntryRef<'a, K, V, S>(DashMapRef<'a, K, V, S>);

unsafe impl<'a, K, V, S> Sync for EntryRef<'a, K, V, S>
where
    K: Eq + Hash + Send + Sync,
    V: Send + Sync,
    S: BuildHasher,
{
}

impl<'a, K, V, S> EntryRef<'a, K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    pub(crate) fn new(map_ref: DashMapRef<'a, K, V, S>) -> Self {
        Self(map_ref)
    }

    pub fn key(&self) -> &K {
        self.0.key()
    }

    pub fn value(&self) -> &V {
        &self.0.value().value
    }

    pub fn pair(&self) -> (&K, &V) {
        (self.key(), self.value())
    }
}

impl<'a, K, V, S> std::ops::Deref for EntryRef<'a, K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    type Target = V;

    fn deref(&self) -> &V {
        self.value()
    }
}
