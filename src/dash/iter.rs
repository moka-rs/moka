use super::{base_cache::BaseCache, mapref::EntryRef};
use crate::common::concurrent::ValueEntry;

use std::{
    hash::{BuildHasher, Hash},
    sync::Arc,
};
use triomphe::Arc as TrioArc;

pub(crate) type DashMapIter<'a, K, V, S> =
    dashmap::iter::Iter<'a, Arc<K>, TrioArc<ValueEntry<K, V>>, S>;

pub struct Iter<'a, K, V, S> {
    cache: &'a BaseCache<K, V, S>,
    map_iter: DashMapIter<'a, K, V, S>,
}

impl<'a, K, V, S> Iter<'a, K, V, S> {
    pub(crate) fn new(cache: &'a BaseCache<K, V, S>, map_iter: DashMapIter<'a, K, V, S>) -> Self {
        Self { cache, map_iter }
    }
}

impl<'a, K, V, S> Iterator for Iter<'a, K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Clone,
{
    type Item = EntryRef<'a, K, V, S>;

    fn next(&mut self) -> Option<Self::Item> {
        for map_ref in &mut self.map_iter {
            if !self.cache.is_expired_entry(map_ref.value()) {
                return Some(EntryRef::new(map_ref));
            }
        }

        None
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
