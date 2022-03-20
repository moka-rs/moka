use super::mapref::EntryRef;
use crate::sync::ValueEntry;

use std::{
    hash::{BuildHasher, Hash},
    sync::Arc,
};
use triomphe::Arc as TrioArc;

type DashMapIter<'a, K, V, S> = dashmap::iter::Iter<'a, Arc<K>, TrioArc<ValueEntry<K, V>>, S>;

pub struct Iter<'a, K, V, S>(DashMapIter<'a, K, V, S>);

impl<'a, K, V, S> Iter<'a, K, V, S> {
    pub(crate) fn new(map_iter: DashMapIter<'a, K, V, S>) -> Self {
        Self(map_iter)
    }
}

impl<'a, K, V, S> Iterator for Iter<'a, K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    type Item = EntryRef<'a, K, V, S>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|map_ref| EntryRef::new(map_ref))
    }
}

unsafe impl<'a, 'i, K, V, S> Send for Iter<'i, K, V, S>
where
    K: 'a + Eq + Hash + Send,
    V: 'a + Send,
    S: 'a + BuildHasher + Clone,
{
}

unsafe impl<'a, 'i, K, V, S> Sync for Iter<'i, K, V, S>
where
    K: 'a + Eq + Hash + Sync,
    V: 'a + Sync,
    S: 'a + BuildHasher + Clone,
{
}
