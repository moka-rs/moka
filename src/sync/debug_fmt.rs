use std::{
    fmt,
    hash::{BuildHasher, Hash},
};

pub(crate) enum CacheRef<'a, K, V, S> {
    #[cfg(feature = "future")]
    Future(&'a crate::future::Cache<K, V, S>),
    Sync(&'a crate::sync::Cache<K, V, S>),
    SyncSeg(&'a crate::sync::SegmentedCache<K, V, S>),
}

pub struct DebugFmt<'a, K, V, S> {
    cache: CacheRef<'a, K, V, S>,
}

impl<'a, K, V, S> DebugFmt<'a, K, V, S> {
    pub(crate) fn new(cache: CacheRef<'a, K, V, S>) -> Self {
        Self { cache }
    }

    pub fn default_fmt(&self) -> DefaultDebugFmt {
        match self.cache {
            #[cfg(feature = "future")]
            CacheRef::Future(c) => DefaultDebugFmt::new(
                "Cache",
                c.policy().max_capacity(),
                c.entry_count(),
                c.weighted_size(),
            ),
            CacheRef::Sync(c) => DefaultDebugFmt::new(
                "Cache",
                c.policy().max_capacity(),
                c.entry_count(),
                c.weighted_size(),
            ),
            CacheRef::SyncSeg(c) => DefaultDebugFmt::new(
                "SegmentedCache",
                c.policy().max_capacity(),
                c.entry_count(),
                c.weighted_size(),
            ),
        }
    }

    pub fn entries(&self) -> EntriesDebugFmt<'_, 'a, K, V, S>
    where
        K: fmt::Debug,
        V: fmt::Debug,
    {
        EntriesDebugFmt { cache: &self.cache }
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

pub struct EntriesDebugFmt<'a, 'b, K, V, S> {
    cache: &'a CacheRef<'b, K, V, S>,
}

impl<'a, 'b, K, V, S> fmt::Debug for EntriesDebugFmt<'a, 'b, K, V, S>
where
    K: fmt::Debug + Hash + Eq + Send + Sync + 'static,
    V: fmt::Debug + Send + Sync + Clone + 'static,
    S: BuildHasher + Send + Sync + Clone + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d_map = f.debug_map();

        match self.cache {
            #[cfg(feature = "future")]
            CacheRef::Future(c) => {
                for (k, v) in c.iter() {
                    d_map.entry(&k, &v);
                }
            }
            CacheRef::Sync(c) => {
                for (k, v) in c.iter() {
                    d_map.entry(&k, &v);
                }
            }
            CacheRef::SyncSeg(c) => {
                for (k, v) in c.iter() {
                    d_map.entry(&k, &v);
                }
            }
        }

        d_map.finish()
    }
}
