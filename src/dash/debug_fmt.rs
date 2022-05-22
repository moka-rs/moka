use std::{
    fmt,
    hash::{BuildHasher, Hash},
};

use super::Cache;

pub struct DebugFmt<'a, K, V, S> {
    cache: &'a Cache<K, V, S>,
}

impl<'a, K, V, S> DebugFmt<'a, K, V, S> {
    pub(crate) fn new(cache: &'a Cache<K, V, S>) -> Self {
        Self { cache }
    }

    pub fn default_fmt(&self) -> DefaultDebugFmt {
        DefaultDebugFmt::new(
            "Cache",
            self.cache.policy().max_capacity(),
            self.cache.entry_count(),
            self.cache.weighted_size(),
        )
    }

    pub fn entries(&self) -> EntriesDebugFmt<'a, K, V, S>
    where
        K: fmt::Debug,
        V: fmt::Debug,
    {
        EntriesDebugFmt { cache: self.cache }
    }
}

pub struct DefaultDebugFmt {
    name: &'static str,
    max_capacity: Option<u64>,
    entry_count: u64,
    weighted_size: u64,
}

impl DefaultDebugFmt {
    fn new(
        name: &'static str,
        max_capacity: Option<u64>,
        entry_count: u64,
        weighted_size: u64,
    ) -> Self {
        Self {
            name,
            max_capacity,
            entry_count,
            weighted_size,
        }
    }
}

impl fmt::Debug for DefaultDebugFmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(self.name)
            .field("max_capacity", &self.max_capacity)
            .field("entry_count", &self.entry_count)
            .field("weighted_size", &self.weighted_size)
            .finish()
    }
}

pub struct EntriesDebugFmt<'a, K, V, S> {
    cache: &'a Cache<K, V, S>,
}

impl<'a, K, V, S> fmt::Debug for EntriesDebugFmt<'a, K, V, S>
where
    K: fmt::Debug + Hash + Eq,
    V: fmt::Debug,
    S: BuildHasher + Clone,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d_map = f.debug_map();

        for r in self.cache.iter() {
            let (k, v) = r.pair();
            d_map.entry(k, v);
        }

        d_map.finish()
    }
}
