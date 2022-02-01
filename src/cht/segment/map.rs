//! A lock-free hash map implemented with segmented bucket pointer arrays, open
//! addressing, and linear probing.

use crate::cht::map::{
    bucket::{self, BucketArray},
    bucket_array_ref::BucketArrayRef,
    DefaultHashBuilder,
};

use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    ptr,
    sync::atomic::{self, AtomicUsize, Ordering},
};

use crossbeam_epoch::Atomic;

/// A lock-free hash map implemented with segmented bucket pointer arrays, open
/// addressing, and linear probing.
///
/// By default, `Cache` uses a hashing algorithm selected to provide resistance
/// against HashDoS attacks.
///
/// The default hashing algorithm is the one used by `std::collections::HashMap`,
/// which is currently SipHash 1-3.
///
/// While its performance is very competitive for medium sized keys, other hashing
/// algorithms will outperform it for small keys such as integers as well as large
/// keys such as long strings. However those algorithms will typically not protect
/// against attacks such as HashDoS.
///
/// The hashing algorithm can be replaced on a per-`HashMap` basis using the
/// [`default`], [`with_hasher`], [`with_capacity_and_hasher`],
/// [`with_num_segments_and_hasher`], and
/// [`with_num_segments_capacity_and_hasher`] methods. Many alternative
/// algorithms are available on crates.io, such as the [`aHash`] crate.
///
/// The number of segments can be specified on a per-`HashMap` basis using the
/// [`with_num_segments`], [`with_num_segments_and_capacity`],
/// [`with_num_segments_and_hasher`], and
/// [`with_num_segments_capacity_and_hasher`] methods. By default, the
/// `num-cpus` feature is enabled and [`new`], [`with_capacity`],
/// [`with_hasher`], and [`with_capacity_and_hasher`] will create maps with
/// twice as many segments as the system has CPUs.
///
/// It is required that the keys implement the [`Eq`] and [`Hash`] traits,
/// although this can frequently be achieved by using
/// `#[derive(PartialEq, Eq, Hash)]`. If you implement these yourself, it is
/// important that the following property holds:
///
/// ```text
/// k1 == k2 -> hash(k1) == hash(k2)
/// ```
///
/// In other words, if two keys are equal, their hashes must be equal.
///
/// It is a logic error for a key to be modified in such a way that the key's
/// hash, as determined by the [`Hash`] trait, or its equality, as determined by
/// the [`Eq`] trait, changes while it is in the map. This is normally only
/// possible through [`Cell`], [`RefCell`], global state, I/O, or unsafe code.
///
/// [`aHash`]: https://crates.io/crates/ahash
/// [`default`]: #method.default
/// [`with_hasher`]: #method.with_hasher
/// [`with_capacity`]: #method.with_capacity
/// [`with_capacity_and_hasher`]: #method.with_capacity_and_hasher
/// [`with_num_segments_and_hasher`]: #method.with_num_segments_and_hasher
/// [`with_num_segments_capacity_and_hasher`]: #method.with_num_segments_capacity_and_hasher
/// [`with_num_segments`]: #method.with_num_segments
/// [`with_num_segments_and_capacity`]: #method.with_num_segments_and_capacity
/// [`new`]: #method.new
/// [`Eq`]: https://doc.rust-lang.org/std/cmp/trait.Eq.html
/// [`Hash`]: https://doc.rust-lang.org/std/hash/trait.Hash.html
/// [`Cell`]: https://doc.rust-lang.org/std/cell/struct.Ref.html
/// [`RefCell`]: https://doc.rust-lang.org/std/cell/struct.RefCell.html
pub(crate) struct HashMap<K, V, S = DefaultHashBuilder> {
    segments: Box<[Segment<K, V>]>,
    build_hasher: S,
    len: AtomicUsize,
    segment_shift: u32,
}

// impl<K, V> HashMap<K, V, DefaultHashBuilder> {
//     /// Creates an empty `HashMap` with the specified number of segments.
//     ///
//     /// The hash map is initially created with a capacity of 0, so it will not
//     /// allocate bucket pointer arrays until it is first inserted into. However,
//     /// it will always allocate memory for segment pointers and lengths.
//     ///
//     /// # Panics
//     ///
//     /// Panics if `num_segments` is 0.
//     pub(crate) fn with_num_segments(num_segments: usize) -> Self {
//         Self::with_num_segments_capacity_and_hasher(num_segments, 0, DefaultHashBuilder::default())
//     }

//     /// Creates an empty `HashMap` with the specified number of segments and
//     /// capacity.
//     ///
//     /// The hash map will be able to hold at least `capacity` elements without
//     /// reallocating any bucket pointer arrays. If `capacity` is 0, the hash map
//     /// will not allocate any bucket pointer arrays. However, it will always
//     /// allocate memory for segment pointers and lengths.
//     ///
//     /// # Panics
//     ///
//     /// Panics if `num_segments` is 0.
//     pub(crate) fn with_num_segments_and_capacity(num_segments: usize, capacity: usize) -> Self {
//         Self::with_num_segments_capacity_and_hasher(
//             num_segments,
//             capacity,
//             DefaultHashBuilder::default(),
//         )
//     }
// }

impl<K, V, S> HashMap<K, V, S> {
    /// Creates an empty `HashMap` with the specified number of segments, using
    /// `build_hasher` to hash the keys.
    ///
    /// The hash map is initially created with a capacity of 0, so it will not
    /// allocate bucket pointer arrays until it is first inserted into. However,
    /// it will always allocate memory for segment pointers and lengths.
    ///
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    pub(crate) fn with_num_segments_and_hasher(num_segments: usize, build_hasher: S) -> Self {
        Self::with_num_segments_capacity_and_hasher(num_segments, 0, build_hasher)
    }

    /// Creates an empty `HashMap` with the specified number of segments and
    /// capacity, using `build_hasher` to hash the keys.
    ///
    /// The hash map will be able to hold at least `capacity` elements without
    /// reallocating any bucket pointer arrays. If `capacity` is 0, the hash map
    /// will not allocate any bucket pointer arrays. However, it will always
    /// allocate memory for segment pointers and lengths.
    ///
    /// # Panics
    ///
    /// Panics if `num_segments` is 0.
    pub(crate) fn with_num_segments_capacity_and_hasher(
        num_segments: usize,
        capacity: usize,
        build_hasher: S,
    ) -> Self {
        assert!(num_segments > 0);

        let actual_num_segments = num_segments.next_power_of_two();
        let segment_shift = 64 - actual_num_segments.trailing_zeros();

        let mut segments = Vec::with_capacity(actual_num_segments);

        if capacity == 0 {
            unsafe {
                ptr::write_bytes(segments.as_mut_ptr(), 0, actual_num_segments);
                segments.set_len(actual_num_segments);
            }
        } else {
            let actual_capacity = (capacity * 2).next_power_of_two();

            for _ in 0..actual_num_segments {
                segments.push(Segment {
                    bucket_array: Atomic::new(BucketArray::with_length(0, actual_capacity)),
                    len: AtomicUsize::new(0),
                });
            }
        }

        let segments = segments.into_boxed_slice();

        Self {
            segments,
            build_hasher,
            len: AtomicUsize::new(0),
            segment_shift,
        }
    }

    // /// Returns the number of elements in the map.
    // ///
    // /// # Safety
    // ///
    // /// This method on its own is safe, but other threads can add or remove
    // /// elements at any time.
    // pub(crate) fn len(&self) -> usize {
    //     self.len.load(Ordering::Relaxed)
    // }

    // /// Returns `true` if the map contains no elements.
    // ///
    // /// # Safety
    // ///
    // /// This method on its own is safe, but other threads can add or remove
    // /// elements at any time.
    // pub(crate) fn is_empty(&self) -> bool {
    //     self.len() == 0
    // }

    // /// Returns the number of elements the map can hold without reallocating any
    // /// bucket pointer arrays.
    // ///
    // /// Note that all mutating operations except removal will result in a bucket
    // /// being allocated or reallocated.
    // ///
    // /// # Safety
    // ///
    // /// This method on its own is safe, but other threads can increase the
    // /// capacity of each segment at any time by adding elements.
    // pub(crate) fn capacity(&self) -> usize {
    //     let guard = &crossbeam_epoch::pin();

    //     self.segments
    //         .iter()
    //         .map(|s| s.bucket_array.load_consume(guard))
    //         .map(|p| unsafe { p.as_ref() })
    //         .map(|a| a.map(BucketArray::capacity).unwrap_or(0))
    //         .sum::<usize>()
    // }

    // /// Returns the number of elements the `index`-th segment of the map can
    // /// hold without reallocating a bucket pointer array.
    // ///
    // /// Note that all mutating operations, with the exception of removing
    // /// elements, will result in an allocation for a new bucket.
    // ///
    // /// # Safety
    // ///
    // /// This method on its own is safe, but other threads can increase the
    // /// capacity of a segment at any time by adding elements.
    // pub(crate) fn segment_capacity(&self, index: usize) -> usize {
    //     assert!(index < self.segments.len());

    //     let guard = &crossbeam_epoch::pin();

    //     unsafe {
    //         self.segments[index]
    //             .bucket_array
    //             .load_consume(guard)
    //             .as_ref()
    //     }
    //     .map(BucketArray::capacity)
    //     .unwrap_or(0)
    // }

    // /// Returns the number of segments in the map.
    // pub(crate) fn num_segments(&self) -> usize {
    //     self.segments.len()
    // }
}

// impl<K, V, S: BuildHasher> HashMap<K, V, S> {
//     /// Returns the index of the segment that `key` would belong to if inserted
//     /// into the map.
//     pub(crate) fn segment_index<Q: Hash>(&self, key: &Q) -> usize
//     where
//         K: Borrow<Q>,
//     {
//         let hash = bucket::hash(&self.build_hasher, key);

//         self.segment_index_from_hash(hash)
//     }
// }

impl<K: Hash + Eq, V, S: BuildHasher> HashMap<K, V, S> {
    /// Returns a clone of the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// [`Hash`]: https://doc.rust-lang.org/std/hash/trait.Hash.html
    /// [`Eq`]: https://doc.rust-lang.org/std/cmp/trait.Eq.html
    #[inline]
    pub(crate) fn get<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        V: Clone,
    {
        self.get_key_value_and(key, |_, v| v.clone())
    }

    /// Returns a clone of the the key-value pair corresponding to the supplied
    /// key.
    ///
    /// The supplied key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for the key
    /// type.
    ///
    /// [`Hash`]: https://doc.rust-lang.org/std/hash/trait.Hash.html
    /// [`Eq`]: https://doc.rust-lang.org/std/cmp/trait.Eq.html
    #[inline]
    pub(crate) fn get_key_value<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q> + Clone,
        V: Clone,
    {
        self.get_key_value_and(key, |k, v| (k.clone(), v.clone()))
    }

    /// Returns the result of invoking a function with a reference to the
    /// key-value pair corresponding to the supplied key.
    ///
    /// The supplied key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for the key
    /// type.
    ///
    /// [`Hash`]: https://doc.rust-lang.org/std/hash/trait.Hash.html
    /// [`Eq`]: https://doc.rust-lang.org/std/cmp/trait.Eq.html
    #[inline]
    pub(crate) fn get_key_value_and<Q: Hash + Eq + ?Sized, F: FnOnce(&K, &V) -> T, T>(
        &self,
        key: &Q,
        with_entry: F,
    ) -> Option<T>
    where
        K: Borrow<Q>,
    {
        let hash = bucket::hash(&self.build_hasher, &key);

        self.bucket_array_ref(hash)
            .get_key_value_and(key, hash, with_entry)
    }

    /// Removes a key from the map, returning a clone of the value previously
    /// corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// [`Hash`]: https://doc.rust-lang.org/std/hash/trait.Hash.html
    /// [`Eq`]: https://doc.rust-lang.org/std/cmp/trait.Eq.html
    #[inline]
    pub(crate) fn remove<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        V: Clone,
    {
        self.remove_entry_if_and(key, |_, _| true, |_, v| v.clone())
    }

    /// Removes a key from the map, returning a clone of the key-value pair
    /// previously corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// [`Hash`]: https://doc.rust-lang.org/std/hash/trait.Hash.html
    /// [`Eq`]: https://doc.rust-lang.org/std/cmp/trait.Eq.html
    #[inline]
    pub(crate) fn remove_entry<Q: Hash + Eq + ?Sized>(&self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q> + Clone,
        V: Clone,
    {
        self.remove_entry_if_and(key, |_, _| true, |k, v| (k.clone(), v.clone()))
    }

    /// Removes a key from the map if a condition is met, returning a clone of
    /// the value previously corresponding to the key.
    ///
    /// `condition` will be invoked at least once if [`Some`] is returned. It
    /// may also be invoked one or more times if [`None`] is returned.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// [`Hash`]: https://doc.rust-lang.org/std/hash/trait.Hash.html
    /// [`Eq`]: https://doc.rust-lang.org/std/cmp/trait.Eq.html
    /// [`Some`]: https://doc.rust-lang.org/std/option/enum.Option.html#variant.Some
    /// [`None`]: https://doc.rust-lang.org/std/option/enum.Option.html#variant.None
    pub(crate) fn remove_if<Q: Hash + Eq + ?Sized, F: FnMut(&K, &V) -> bool>(
        &self,
        key: &Q,
        condition: F,
    ) -> Option<V>
    where
        K: Borrow<Q>,
        V: Clone,
    {
        self.remove_entry_if_and(key, condition, move |_, v| v.clone())
    }

    /// Removes a key from the map if a condition is met, returning the result
    /// of invoking a function with a reference to the key-value pair previously
    /// corresponding to the key.
    ///
    /// `condition` will be invoked at least once if [`Some`] is returned. It
    /// may also be invoked one or more times if [`None`] is returned.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`Hash`] and [`Eq`] on the borrowed form *must* match those for
    /// the key type.
    ///
    /// [`Hash`]: https://doc.rust-lang.org/std/hash/trait.Hash.html
    /// [`Eq`]: https://doc.rust-lang.org/std/cmp/trait.Eq.html
    /// [`Some`]: https://doc.rust-lang.org/std/option/enum.Option.html#variant.Some
    /// [`None`]: https://doc.rust-lang.org/std/option/enum.Option.html#variant.None
    #[inline]
    pub(crate) fn remove_entry_if_and<
        Q: Hash + Eq + ?Sized,
        F: FnMut(&K, &V) -> bool,
        G: FnOnce(&K, &V) -> T,
        T,
    >(
        &self,
        key: &Q,
        condition: F,
        with_previous_entry: G,
    ) -> Option<T>
    where
        K: Borrow<Q>,
    {
        let hash = bucket::hash(&self.build_hasher, &key);

        self.bucket_array_ref(hash)
            .remove_entry_if_and(key, hash, condition, move |k, v| {
                self.len.fetch_sub(1, Ordering::Relaxed);

                with_previous_entry(k, v)
            })
    }

    /// If no value corresponds to the key, invoke a default function to insert
    /// a new key-value pair into the map. Otherwise, modify the existing value
    /// and return a clone of the value previously corresponding to the key.
    ///
    /// `on_insert` may be invoked, even if [`None`] is returned.
    ///
    /// `on_modify` will be invoked at least once if [`Some`] is returned. It
    /// may also be invoked one or more times if [`None`] is returned.
    ///
    /// [`Some`]: https://doc.rust-lang.org/std/option/enum.Option.html#variant.Some
    /// [`None`]: https://doc.rust-lang.org/std/option/enum.Option.html#variant.None
    ///
    /// Moka
    #[inline]
    pub(crate) fn insert_with_or_modify<F: FnOnce() -> V, G: FnMut(&K, &V) -> V>(
        &self,
        key: K,
        on_insert: F,
        on_modify: G,
    ) -> Option<V>
    where
        V: Clone,
    {
        self.insert_with_or_modify_entry_and(key, on_insert, on_modify, |_, v| v.clone())
    }

    /// If no value corresponds to the key, invoke a default function to insert
    /// a new key-value pair into the map. Otherwise, modify the existing value
    /// and return the result of invoking a function with a reference to the
    /// key-value pair previously corresponding to the supplied key.
    ///
    /// `on_insert` may be invoked, even if [`None`] is returned.
    ///
    /// `on_modify` will be invoked at least once if [`Some`] is returned. It
    /// may also be invoked one or more times if [`None`] is returned.
    ///
    /// [`Some`]: https://doc.rust-lang.org/std/option/enum.Option.html#variant.Some
    /// [`None`]: https://doc.rust-lang.org/std/option/enum.Option.html#variant.None
    #[inline]
    pub(crate) fn insert_with_or_modify_entry_and<
        F: FnOnce() -> V,
        G: FnMut(&K, &V) -> V,
        H: FnOnce(&K, &V) -> T,
        T,
    >(
        &self,
        key: K,
        on_insert: F,
        on_modify: G,
        with_old_entry: H,
    ) -> Option<T> {
        let hash = bucket::hash(&self.build_hasher, &key);

        let result = self.bucket_array_ref(hash).insert_with_or_modify_entry_and(
            key,
            hash,
            on_insert,
            on_modify,
            with_old_entry,
        );

        if result.is_none() {
            self.len.fetch_add(1, Ordering::Relaxed);
        }

        result
    }
}

impl<K, V, S> Drop for HashMap<K, V, S> {
    fn drop(&mut self) {
        let guard = unsafe { &crossbeam_epoch::unprotected() };
        atomic::fence(Ordering::Acquire);

        for Segment {
            bucket_array: this_bucket_array,
            ..
        } in self.segments.iter()
        {
            let mut current_ptr = this_bucket_array.load(Ordering::Relaxed, guard);

            while let Some(current_ref) = unsafe { current_ptr.as_ref() } {
                let next_ptr = current_ref.next.load(Ordering::Relaxed, guard);

                for this_bucket_ptr in current_ref
                    .buckets
                    .iter()
                    .map(|b| b.load(Ordering::Relaxed, guard))
                    .filter(|p| !p.is_null())
                    .filter(|p| next_ptr.is_null() || p.tag() & bucket::TOMBSTONE_TAG == 0)
                {
                    // only delete tombstones from the newest bucket array
                    // the only way this becomes a memory leak is if there was a panic during a rehash,
                    // in which case i'm going to say that running destructors and freeing memory is
                    // best-effort, and my best effort is to not do it
                    unsafe { bucket::defer_acquire_destroy(guard, this_bucket_ptr) };
                }

                unsafe { bucket::defer_acquire_destroy(guard, current_ptr) };

                current_ptr = next_ptr;
            }
        }
    }
}

impl<K, V, S> HashMap<K, V, S> {
    #[inline]
    fn bucket_array_ref(&'_ self, hash: u64) -> BucketArrayRef<'_, K, V, S> {
        let index = self.segment_index_from_hash(hash);

        let Segment {
            ref bucket_array,
            ref len,
        } = self.segments[index];

        BucketArrayRef {
            bucket_array,
            build_hasher: &self.build_hasher,
            len,
        }
    }

    #[inline]
    fn segment_index_from_hash(&'_ self, hash: u64) -> usize {
        if self.segment_shift == 64 {
            0
        } else {
            (hash >> self.segment_shift) as usize
        }
    }
}

struct Segment<K, V> {
    bucket_array: Atomic<BucketArray<K, V>>,
    len: AtomicUsize,
}

// #[cfg(test)]
// mod tests {
//     use crate::write_test_cases_for_me;

//     use super::*;

//     write_test_cases_for_me!(HashMap);

//     #[test]
//     fn single_segment() {
//         let map = HashMap::with_num_segments(1);

//         assert!(map.is_empty());
//         assert_eq!(map.len(), 0);

//         assert_eq!(map.insert("foo", 5), None);
//         assert_eq!(map.get("foo"), Some(5));

//         assert!(!map.is_empty());
//         assert_eq!(map.len(), 1);

//         assert_eq!(map.remove("foo"), Some(5));
//         assert!(map.is_empty());
//         assert_eq!(map.len(), 0);
//     }
// }
