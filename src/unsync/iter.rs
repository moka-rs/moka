use super::{Cache, ValueEntry};

use std::{
    hash::{BuildHasher, Hash},
    rc::Rc,
};

type HashMapIter<'i, K, V> = std::collections::hash_map::Iter<'i, Rc<K>, ValueEntry<K, V>>;

pub struct Iter<'i, K, V, S> {
    cache: &'i Cache<K, V, S>,
    iter: HashMapIter<'i, K, V>,
}

impl<'i, K, V, S> Iter<'i, K, V, S> {
    pub(crate) fn new(cache: &'i Cache<K, V, S>, iter: HashMapIter<'i, K, V>) -> Self {
        Self { cache, iter }
    }
}

impl<'i, K, V, S> Iterator for Iter<'i, K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher + Clone,
{
    type Item = (&'i K, &'i V);

    fn next(&mut self) -> Option<Self::Item> {
        for (k, entry) in self.iter.by_ref() {
            if !self.cache.is_expired_entry(entry) {
                return Some((k, &entry.value));
            }
        }
        None
    }
}
