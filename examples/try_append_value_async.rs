//! This example demonstrates how to append an `i32` value to a cached `Vec<i32>`
//! value. It uses the `and_upsert_with` method of `Cache`.

use std::{io::Cursor, pin::Pin, sync::Arc};

use moka::{
    future::Cache,
    ops::compute::{CompResult, Op},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::RwLock,
};

/// The type of the cache key.
type Key = i32;

/// The type of the cache value.
///
/// We want to store a raw value `String` for each `i32` key. We are going to append
/// a `char` to the `String` value in the cache.
///
/// Note that we have to wrap the `String` in an `Arc<RwLock<_>>`. We need the `Arc`,
/// an atomic reference counted shared pointer, because `and_try_compute_with` method
/// of `Cache` passes a _clone_ of the value to our closure, instead of passing a
/// `&mut` reference. We do not want to clone the `String` every time we append a
/// `char` to it, so we wrap it in an `Arc`. Then we need the `RwLock` because we
/// mutate the `String` when we append a value to it.
///
/// The reason that `and_try_compute_with` cannot pass a `&mut String` to the closure
/// is because the internal concurrent hash table of `Cache` is a lock free data
/// structure and does not use any mutexes. So it cannot guarantee: (1) the
/// `&mut String` is unique, and (2) it is not accessed concurrently by other
/// threads.
type Value = Arc<RwLock<String>>;

#[tokio::main]
async fn main() -> Result<(), tokio::io::Error> {
    let cache: Cache<Key, Value> = Cache::new(100);

    let key = 0;

    // We are going read a byte at a time from a byte string (`[u8; 3]`).
    let reader = Cursor::new(b"abc");
    tokio::pin!(reader);

    // Read the first char 'a' from the reader, and insert a string "a" to the cache.
    let result = append_to_cached_string(&cache, key, &mut reader).await?;
    let CompResult::Inserted(entry) = result else {
        panic!("`Inserted` should be returned: {result:?}");
    };
    assert_eq!(*entry.into_value().read().await, "a");

    // Read next char 'b' from the reader, and append it the cached string.
    let result = append_to_cached_string(&cache, key, &mut reader).await?;
    let CompResult::ReplacedWith(entry) = result else {
        panic!("`ReplacedWith` should be returned: {result:?}");
    };
    assert_eq!(*entry.into_value().read().await, "ab");

    // Read next char 'c' from the reader, and append it the cached string.
    let result = append_to_cached_string(&cache, key, &mut reader).await?;
    let CompResult::ReplacedWith(entry) = result else {
        panic!("`ReplacedWith` should be returned: {result:?}");
    };
    assert_eq!(*entry.into_value().read().await, "abc");

    // Reading should fail as no more char left.
    let err = append_to_cached_string(&cache, key, &mut reader).await;
    assert_eq!(
        err.expect_err("An error should be returned").kind(),
        tokio::io::ErrorKind::UnexpectedEof
    );

    Ok(())
}

/// Reads a byte from the `reader``, convert it into a `char`, append it to the
/// cached `String` for the given `key`, and returns the resulting cached entry.
///
/// If reading from the `reader` fails with an IO error, it returns the error.
///
/// This method uses cache's `and_try_compute_with` method.
async fn append_to_cached_string(
    cache: &Cache<Key, Value>,
    key: Key,
    reader: &mut Pin<&mut impl AsyncRead>,
) -> Result<CompResult<Key, Value>, tokio::io::Error> {
    cache
        .entry(key)
        .and_try_compute_with(|maybe_entry| async {
            // Read a char from the reader.
            let byte = reader.read_u8().await?;
            let char =
                char::from_u32(byte as u32).expect("An ASCII byte should be converted into a char");

            // Check if the entry already exists.
            if let Some(entry) = maybe_entry {
                // The entry exists, append the char to the Vec.
                let v = entry.into_value();
                v.write().await.push(char);
                Ok(Op::Put(v))
            } else {
                // The entry does not exist, insert a new Vec containing
                // the char.
                let v = RwLock::new(String::from(char));
                Ok(Op::Put(Arc::new(v)))
            }
        })
        .await
}
