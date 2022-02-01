use super::bucket::{self, Bucket, BucketArray, InsertOrModifyState};

use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    sync::atomic::{AtomicUsize, Ordering},
};

use crossbeam_epoch::{Atomic, CompareAndSetError, Guard, Owned, Shared};

pub(crate) struct BucketArrayRef<'a, K, V, S> {
    pub(crate) bucket_array: &'a Atomic<BucketArray<K, V>>,
    pub(crate) build_hasher: &'a S,
    pub(crate) len: &'a AtomicUsize,
}

impl<'a, K: Hash + Eq, V, S: BuildHasher> BucketArrayRef<'a, K, V, S> {
    pub(crate) fn get_key_value_and<Q: Hash + Eq + ?Sized, F: FnOnce(&K, &V) -> T, T>(
        &self,
        key: &Q,
        hash: u64,
        with_entry: F,
    ) -> Option<T>
    where
        K: Borrow<Q>,
    {
        let guard = &crossbeam_epoch::pin();
        let current_ref = self.get(guard);
        let mut bucket_array_ref = current_ref;

        let result;

        loop {
            match bucket_array_ref
                .get(guard, hash, key)
                .map(|p| unsafe { p.as_ref() })
            {
                Ok(Some(Bucket {
                    key,
                    maybe_value: value,
                })) => {
                    result = Some(with_entry(key, unsafe { &*value.as_ptr() }));

                    break;
                }
                Ok(None) => {
                    result = None;

                    break;
                }
                Err(_) => {
                    bucket_array_ref = bucket_array_ref.rehash(guard, self.build_hasher);
                }
            }
        }

        self.swing(guard, current_ref, bucket_array_ref);

        result
    }

    pub(crate) fn remove_entry_if_and<
        Q: Hash + Eq + ?Sized,
        F: FnMut(&K, &V) -> bool,
        G: FnOnce(&K, &V) -> T,
        T,
    >(
        &self,
        key: &Q,
        hash: u64,
        mut condition: F,
        with_previous_entry: G,
    ) -> Option<T>
    where
        K: Borrow<Q>,
    {
        let guard = &crossbeam_epoch::pin();
        let current_ref = self.get(guard);
        let mut bucket_array_ref = current_ref;

        let result;

        loop {
            match bucket_array_ref.remove_if(guard, hash, key, condition) {
                Ok(previous_bucket_ptr) => {
                    if let Some(previous_bucket_ref) = unsafe { previous_bucket_ptr.as_ref() } {
                        let Bucket {
                            key,
                            maybe_value: value,
                        } = previous_bucket_ref;
                        self.len.fetch_sub(1, Ordering::Relaxed);
                        result = Some(with_previous_entry(key, unsafe { &*value.as_ptr() }));

                        unsafe { bucket::defer_destroy_tombstone(guard, previous_bucket_ptr) };
                    } else {
                        result = None;
                    }

                    break;
                }
                Err(c) => {
                    condition = c;
                    bucket_array_ref = bucket_array_ref.rehash(guard, self.build_hasher);
                }
            }
        }

        self.swing(guard, current_ref, bucket_array_ref);

        result
    }

    pub(crate) fn insert_with_or_modify_entry_and<
        F: FnOnce() -> V,
        G: FnMut(&K, &V) -> V,
        H: FnOnce(&K, &V) -> T,
        T,
    >(
        &self,
        key: K,
        hash: u64,
        on_insert: F,
        mut on_modify: G,
        with_old_entry: H,
    ) -> Option<T> {
        let guard = &crossbeam_epoch::pin();
        let current_ref = self.get(guard);
        let mut bucket_array_ref = current_ref;
        let mut state = InsertOrModifyState::New(key, on_insert);

        let result;

        loop {
            while self.len.load(Ordering::Relaxed) > bucket_array_ref.capacity() {
                bucket_array_ref = bucket_array_ref.rehash(guard, self.build_hasher);
            }

            match bucket_array_ref.insert_or_modify(guard, hash, state, on_modify) {
                Ok(previous_bucket_ptr) => {
                    if let Some(previous_bucket_ref) = unsafe { previous_bucket_ptr.as_ref() } {
                        if previous_bucket_ptr.tag() & bucket::TOMBSTONE_TAG != 0 {
                            self.len.fetch_add(1, Ordering::Relaxed);
                            result = None;
                        } else {
                            let Bucket {
                                key,
                                maybe_value: value,
                            } = previous_bucket_ref;
                            result = Some(with_old_entry(key, unsafe { &*value.as_ptr() }));
                        }

                        unsafe { bucket::defer_destroy_bucket(guard, previous_bucket_ptr) };
                    } else {
                        self.len.fetch_add(1, Ordering::Relaxed);
                        result = None;
                    }

                    break;
                }
                Err((s, f)) => {
                    state = s;
                    on_modify = f;
                    bucket_array_ref = bucket_array_ref.rehash(guard, self.build_hasher);
                }
            }
        }

        self.swing(guard, current_ref, bucket_array_ref);

        result
    }
}

impl<'a, 'g, K, V, S> BucketArrayRef<'a, K, V, S> {
    fn get(&self, guard: &'g Guard) -> &'g BucketArray<K, V> {
        const DEFAULT_LENGTH: usize = 128;

        let mut maybe_new_bucket_array = None;

        loop {
            let bucket_array_ptr = self.bucket_array.load_consume(guard);

            if let Some(bucket_array_ref) = unsafe { bucket_array_ptr.as_ref() } {
                return bucket_array_ref;
            }

            let new_bucket_array = maybe_new_bucket_array
                .unwrap_or_else(|| Owned::new(BucketArray::with_length(0, DEFAULT_LENGTH)));

            match self.bucket_array.compare_and_set_weak(
                Shared::null(),
                new_bucket_array,
                (Ordering::Release, Ordering::Relaxed),
                guard,
            ) {
                Ok(b) => return unsafe { b.as_ref() }.unwrap(),
                Err(CompareAndSetError { new, .. }) => maybe_new_bucket_array = Some(new),
            }
        }
    }

    fn swing(
        &self,
        guard: &'g Guard,
        mut current_ref: &'g BucketArray<K, V>,
        min_ref: &'g BucketArray<K, V>,
    ) {
        let min_epoch = min_ref.epoch;

        let mut current_ptr = (current_ref as *const BucketArray<K, V>).into();
        let min_ptr: Shared<'g, _> = (min_ref as *const BucketArray<K, V>).into();

        loop {
            if current_ref.epoch >= min_epoch {
                return;
            }

            match self.bucket_array.compare_and_set_weak(
                current_ptr,
                min_ptr,
                (Ordering::Release, Ordering::Relaxed),
                guard,
            ) {
                Ok(_) => unsafe { bucket::defer_acquire_destroy(guard, current_ptr) },
                Err(_) => {
                    let new_ptr = self.bucket_array.load_consume(guard);
                    assert!(!new_ptr.is_null());

                    current_ptr = new_ptr;
                    current_ref = unsafe { new_ptr.as_ref() }.unwrap();
                }
            }
        }
    }
}
