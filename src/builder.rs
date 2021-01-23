use crate::{Cache, SegmentedCache};

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    time::Duration,
};

pub struct Builder<C> {
    capacity: usize,
    num_segments: Option<usize>,
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
    cache_type: PhantomData<C>,
}

impl<K, V> Builder<Cache<K, V, RandomState>>
where
    K: Eq + Hash,
{
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            num_segments: None,
            time_to_live: None,
            time_to_idle: None,
            cache_type: PhantomData::default(),
        }
    }

    pub fn segment(self, num_segments: usize) -> Builder<SegmentedCache<K, V, RandomState>> {
        Builder {
            capacity: self.capacity,
            num_segments: Some(num_segments),
            time_to_live: self.time_to_live,
            time_to_idle: self.time_to_idle,
            cache_type: PhantomData::default(),
        }
    }

    pub fn build(self) -> Cache<K, V, RandomState> {
        let build_hasher = RandomState::default();
        Cache::with_everything(
            self.capacity,
            build_hasher,
            self.time_to_live,
            self.time_to_idle,
        )
    }

    pub fn build_with_hasher<S>(self, hasher: S) -> Cache<K, V, S>
    where
        S: BuildHasher + Clone,
    {
        Cache::with_everything(self.capacity, hasher, self.time_to_live, self.time_to_idle)
    }
}

impl<K, V> Builder<SegmentedCache<K, V, RandomState>>
where
    K: Eq + Hash,
{
    pub fn build(self) -> SegmentedCache<K, V, RandomState> {
        let build_hasher = RandomState::default();
        SegmentedCache::with_everything(
            self.capacity,
            self.num_segments.unwrap(),
            build_hasher,
            self.time_to_live,
            self.time_to_idle,
        )
    }

    pub fn build_with_hasher<S>(self, hasher: S) -> SegmentedCache<K, V, S>
    where
        S: BuildHasher + Clone,
    {
        SegmentedCache::with_everything(
            self.capacity,
            self.num_segments.unwrap(),
            hasher,
            self.time_to_live,
            self.time_to_idle,
        )
    }
}

impl<C> Builder<C> {
    pub fn time_to_live(self, duration: Duration) -> Self {
        Self {
            time_to_live: Some(duration),
            ..self
        }
    }

    pub fn time_to_idle(self, duration: Duration) -> Self {
        Self {
            time_to_idle: Some(duration),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Builder;
    use crate::ConcurrentCache;

    use std::{sync::Arc, time::Duration};

    #[test]
    fn build_cache() {
        // Cache<char, String>
        let cache = Builder::new(100).build();

        assert_eq!(cache.capacity(), 100);
        assert_eq!(cache.time_to_live(), None);
        assert_eq!(cache.time_to_idle(), None);
        assert_eq!(cache.num_segments(), 1);

        cache.insert('a', "Alice");
        assert_eq!(cache.get(&'a'), Some(Arc::new("Alice")));

        let cache = Builder::new(100)
            .time_to_live(Duration::from_secs(45 * 60))
            .time_to_idle(Duration::from_secs(15 * 60))
            .build();

        assert_eq!(cache.capacity(), 100);
        assert_eq!(cache.time_to_live(), Some(Duration::from_secs(45 * 60)));
        assert_eq!(cache.time_to_idle(), Some(Duration::from_secs(15 * 60)));
        assert_eq!(cache.num_segments(), 1);

        cache.insert('a', "Alice");
        assert_eq!(cache.get(&'a'), Some(Arc::new("Alice")));
    }

    #[test]
    fn build_segmented_cache() {
        // SegmentCache<char, String>
        let cache = Builder::new(100).segment(16).build();

        assert_eq!(cache.capacity(), 100);
        assert_eq!(cache.time_to_live(), None);
        assert_eq!(cache.time_to_idle(), None);
        assert_eq!(cache.num_segments(), 16_usize.next_power_of_two());

        cache.insert('b', "Bob");
        assert_eq!(cache.get(&'b'), Some(Arc::new("Bob")));

        let cache = Builder::new(100)
            .segment(16)
            .time_to_live(Duration::from_secs(45 * 60))
            .time_to_idle(Duration::from_secs(15 * 60))
            .build();

        assert_eq!(cache.capacity(), 100);
        assert_eq!(cache.time_to_live(), Some(Duration::from_secs(45 * 60)));
        assert_eq!(cache.time_to_idle(), Some(Duration::from_secs(15 * 60)));
        assert_eq!(cache.num_segments(), 16_usize.next_power_of_two());

        cache.insert('b', "Bob");
        assert_eq!(cache.get(&'b'), Some(Arc::new("Bob")));
    }
}
