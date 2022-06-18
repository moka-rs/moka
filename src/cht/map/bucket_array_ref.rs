use super::bucket::{self, Bucket, BucketArray, InsertOrModifyState, RehashOp};

use std::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    sync::atomic::{AtomicUsize, Ordering},
};

use crossbeam_epoch::{Atomic, CompareExchangeError, Guard, Owned, Shared};

pub(crate) struct BucketArrayRef<'a, K, V, S> {
    pub(crate) bucket_array: &'a Atomic<BucketArray<K, V>>,
    pub(crate) build_hasher: &'a S,
    pub(crate) len: &'a AtomicUsize,
}

impl<'a, K, V, S> BucketArrayRef<'a, K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher,
{
    pub(crate) fn get_key_value_and_then<Q, F, T>(
        &self,
        key: &Q,
        hash: u64,
        with_entry: F,
    ) -> Option<T>
    where
        Q: Hash + Eq + ?Sized,
        K: Borrow<Q>,
        F: FnOnce(&K, &V) -> Option<T>,
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
                    result = with_entry(key, unsafe { &*value.as_ptr() });
                    break;
                }
                Ok(None) => {
                    result = None;
                    break;
                }
                Err(_) => {
                    bucket_array_ref =
                        bucket_array_ref.rehash(guard, self.build_hasher, RehashOp::Expand);
                }
            }
        }

        self.swing(guard, current_ref, bucket_array_ref);

        result
    }

    pub(crate) fn remove_entry_if_and<Q, F, G, T>(
        &self,
        key: &Q,
        hash: u64,
        mut condition: F,
        with_previous_entry: G,
    ) -> Option<T>
    where
        Q: Hash + Eq + ?Sized,
        K: Borrow<Q>,
        F: FnMut(&K, &V) -> bool,
        G: FnOnce(&K, &V) -> T,
    {
        let guard = &crossbeam_epoch::pin();
        let current_ref = self.get(guard);
        let mut bucket_array_ref = current_ref;

        let result;

        loop {
            loop {
                let rehash_op = RehashOp::new(
                    bucket_array_ref.capacity(),
                    &bucket_array_ref.tombstone_count,
                    self.len,
                );
                if rehash_op.is_skip() {
                    break;
                }
                bucket_array_ref = bucket_array_ref.rehash(guard, self.build_hasher, rehash_op);
            }

            match bucket_array_ref.remove_if(guard, hash, key, condition) {
                Ok(previous_bucket_ptr) => {
                    if let Some(previous_bucket_ref) = unsafe { previous_bucket_ptr.as_ref() } {
                        let Bucket {
                            key,
                            maybe_value: value,
                        } = previous_bucket_ref;
                        self.len.fetch_sub(1, Ordering::Relaxed);
                        bucket_array_ref
                            .tombstone_count
                            .fetch_add(1, Ordering::Relaxed);
                        result = Some(with_previous_entry(key, unsafe { &*value.as_ptr() }));

                        unsafe { bucket::defer_destroy_tombstone(guard, previous_bucket_ptr) };
                    } else {
                        result = None;
                    }

                    break;
                }
                Err(c) => {
                    condition = c;
                    bucket_array_ref =
                        bucket_array_ref.rehash(guard, self.build_hasher, RehashOp::Expand);
                }
            }
        }

        self.swing(guard, current_ref, bucket_array_ref);

        result
    }

    pub(crate) fn insert_if_not_present_and<F, G, T>(
        &self,
        key: K,
        hash: u64,
        on_insert: F,
        with_existing_entry: G,
    ) -> Option<T>
    where
        F: FnOnce() -> V,
        G: FnOnce(&K, &V) -> T,
    {
        use bucket::InsertionResult;

        let guard = &crossbeam_epoch::pin();
        let current_ref = self.get(guard);
        let mut bucket_array_ref = current_ref;
        let mut state = InsertOrModifyState::New(key, on_insert);

        let result;

        loop {
            loop {
                let rehash_op = RehashOp::new(
                    bucket_array_ref.capacity(),
                    &bucket_array_ref.tombstone_count,
                    self.len,
                );
                if rehash_op.is_skip() {
                    break;
                }
                bucket_array_ref = bucket_array_ref.rehash(guard, self.build_hasher, rehash_op);
            }

            match bucket_array_ref.insert_if_not_present(guard, hash, state) {
                Ok(InsertionResult::AlreadyPresent(current_bucket_ptr)) => {
                    let current_bucket_ref = unsafe { current_bucket_ptr.as_ref() }.unwrap();
                    assert!(!bucket::is_tombstone(current_bucket_ptr));
                    let Bucket {
                        key,
                        maybe_value: value,
                    } = current_bucket_ref;
                    result = Some(with_existing_entry(key, unsafe { &*value.as_ptr() }));
                    break;
                }
                Ok(InsertionResult::Inserted) => {
                    self.len.fetch_add(1, Ordering::Relaxed);
                    result = None;
                    break;
                }
                Ok(InsertionResult::ReplacedTombstone(previous_bucket_ptr)) => {
                    assert!(bucket::is_tombstone(previous_bucket_ptr));
                    self.len.fetch_add(1, Ordering::Relaxed);
                    unsafe { bucket::defer_destroy_bucket(guard, previous_bucket_ptr) };
                    result = None;
                    break;
                }
                Err(s) => {
                    state = s;
                    bucket_array_ref =
                        bucket_array_ref.rehash(guard, self.build_hasher, RehashOp::Expand);
                }
            }
        }

        self.swing(guard, current_ref, bucket_array_ref);

        result
    }

    pub(crate) fn insert_with_or_modify_entry_and<T, F, G, H>(
        &self,
        key: K,
        hash: u64,
        on_insert: F,
        mut on_modify: G,
        with_old_entry: H,
    ) -> Option<T>
    where
        F: FnOnce() -> V,
        G: FnMut(&K, &V) -> V,
        H: FnOnce(&K, &V) -> T,
    {
        let guard = &crossbeam_epoch::pin();
        let current_ref = self.get(guard);
        let mut bucket_array_ref = current_ref;
        let mut state = InsertOrModifyState::New(key, on_insert);

        let result;

        loop {
            loop {
                let rehash_op = RehashOp::new(
                    bucket_array_ref.capacity(),
                    &bucket_array_ref.tombstone_count,
                    self.len,
                );
                if rehash_op.is_skip() {
                    break;
                }
                bucket_array_ref = bucket_array_ref.rehash(guard, self.build_hasher, rehash_op);
            }

            match bucket_array_ref.insert_or_modify(guard, hash, state, on_modify) {
                Ok(previous_bucket_ptr) => {
                    if let Some(previous_bucket_ref) = unsafe { previous_bucket_ptr.as_ref() } {
                        if bucket::is_tombstone(previous_bucket_ptr) {
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
                    bucket_array_ref =
                        bucket_array_ref.rehash(guard, self.build_hasher, RehashOp::Expand);
                }
            }
        }

        self.swing(guard, current_ref, bucket_array_ref);

        result
    }

    pub(crate) fn keys<F, T>(&self, mut with_key: F) -> Vec<T>
    where
        F: FnMut(&K) -> T,
    {
        let guard = &crossbeam_epoch::pin();
        let current_ref = self.get(guard);
        let mut bucket_array_ref = current_ref;

        let result;

        loop {
            match bucket_array_ref.keys(guard, &mut with_key) {
                Ok(keys) => {
                    result = keys;
                    break;
                }
                Err(_) => {
                    bucket_array_ref =
                        bucket_array_ref.rehash(guard, self.build_hasher, RehashOp::Expand);
                }
            }
        }

        self.swing(guard, current_ref, bucket_array_ref);

        result
    }
}

impl<'a, 'g, K, V, S> BucketArrayRef<'a, K, V, S> {
    fn get(&self, guard: &'g Guard) -> &'g BucketArray<K, V> {
        let mut maybe_new_bucket_array = None;

        loop {
            let bucket_array_ptr = self.bucket_array.load_consume(guard);

            if let Some(bucket_array_ref) = unsafe { bucket_array_ptr.as_ref() } {
                return bucket_array_ref;
            }

            let new_bucket_array =
                maybe_new_bucket_array.unwrap_or_else(|| Owned::new(BucketArray::default()));

            match self.bucket_array.compare_exchange(
                Shared::null(),
                new_bucket_array,
                Ordering::Release,
                Ordering::Relaxed,
                guard,
            ) {
                Ok(b) => return unsafe { b.as_ref() }.unwrap(),
                Err(CompareExchangeError { new, .. }) => maybe_new_bucket_array = Some(new),
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

            match self.bucket_array.compare_exchange(
                current_ptr,
                min_ptr,
                Ordering::Release,
                Ordering::Relaxed,
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
