use crate::sync::base_cache::BaseCache;

use std::{
    hash::{BuildHasher, Hash},
    sync::{Arc, Weak},
};

pub struct Iter<'i, K, V, S> {
    keys: Option<Vec<Weak<K>>>,
    cache_segments: Box<[&'i BaseCache<K, V, S>]>,
    num_cht_segments: usize,
    cache_seg_index: usize,
    cht_seg_index: usize,
    is_done: bool,
}

impl<'i, K, V, S> Iter<'i, K, V, S> {
    pub(crate) fn with_single_cache_segment(
        cache: &'i BaseCache<K, V, S>,
        num_cht_segments: usize,
    ) -> Self {
        Self {
            keys: None,
            cache_segments: Box::new([cache]),
            num_cht_segments,
            cache_seg_index: 0,
            cht_seg_index: 0,
            is_done: false,
        }
    }

    // pub(crate) fn with_multiple_cache_segments(
    //     cache_segments: Box<[&'i BaseCache<K, V, S>]>,
    //     num_cht_segments: usize,
    // ) -> Self {
    //     Self {
    //         keys: None,
    //         cache_segments,
    //         num_cht_segments,
    //         cache_seg_index: 0,
    //         cht_seg_index: 0,
    //         is_done: false,
    //     }
    // }
}

impl<'i, K, V, S> Iterator for Iter<'i, K, V, S>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    type Item = (Arc<K>, V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.is_done {
            return None;
        }

        while let Some(key) = self.next_key() {
            if let Some(v) = self.cache().get_for_iter(&key) {
                return Some((key, v));
            }
        }

        self.is_done = true;
        None
    }
}

impl<'i, K, V, S> Iter<'i, K, V, S>
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    S: BuildHasher + Clone + Send + Sync + 'static,
{
    fn cache(&self) -> &'i BaseCache<K, V, S> {
        self.cache_segments[self.cache_seg_index]
    }

    fn next_key(&mut self) -> Option<Arc<K>> {
        while let Some(keys) = self.current_keys() {
            if let key @ Some(_) = keys.pop().and_then(|k| k.upgrade()) {
                return key;
            }
        }
        None
    }

    fn current_keys(&mut self) -> Option<&mut Vec<Weak<K>>> {
        // If keys is none or some but empty, try to get next keys.
        while self.keys.as_ref().map_or(true, Vec::is_empty) {
            // Adjust indices.
            if self.cht_seg_index >= self.num_cht_segments {
                self.cache_seg_index += 1;
                self.cht_seg_index = 0;
                if self.cache_seg_index >= self.cache_segments.len() {
                    // No more cache segments left.
                    return None;
                }
            }

            self.keys = self.cache_segments[self.cache_seg_index].keys(dbg!(self.cht_seg_index));
            self.cht_seg_index += 1;
            dbg!(self.keys.as_ref().map_or(0, |ks| ks.len()));
        }

        self.keys.as_mut()
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
