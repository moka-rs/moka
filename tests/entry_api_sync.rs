#![cfg(all(test, feature = "sync"))]

use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    thread,
};

use moka::sync::{Cache, SegmentedCache};
use paste::paste;

const NUM_THREADS: u8 = 16;
const FILE: &str = "./Cargo.toml";

fn get_file_size(thread_id: u8, path: impl AsRef<Path>, call_counter: &AtomicUsize) -> Option<u64> {
    println!("get_file_size() called by thread {}.", thread_id);
    call_counter.fetch_add(1, Ordering::AcqRel);
    std::fs::metadata(path).ok().map(|m| m.len())
}

macro_rules! generate_test_get_with {
    ($name:ident, $cache_init:expr) => {
        paste! {
            #[test]
            fn [<test_ $name _get_with>]() {
                let cache = $cache_init;
                let call_counter = Arc::new(AtomicUsize::default());

                let threads: Vec<_> = (0..NUM_THREADS)
                    .map(|thread_id| {
                        let my_cache = cache.clone();
                        let my_call_counter = Arc::clone(&call_counter);
                        thread::spawn(move || {
                            println!("Thread {} started.", thread_id);

                            let key = "key1".to_string();
                            let value = match thread_id % 2 {
                                0 => my_cache.optionally_get_with(key.clone(), || {
                                    get_file_size(thread_id, FILE, &my_call_counter)
                                }),
                                1 => my_cache.optionally_get_with_by_ref(key.as_str(), || {
                                    get_file_size(thread_id, FILE, &my_call_counter)
                                }),
                                _ => unreachable!(),
                            };

                            assert!(value.is_some());
                            assert!(my_cache.get(key.as_str()).is_some());

                            println!(
                                "Thread {} got the value. (len: {})",
                                thread_id,
                                value.unwrap()
                            );
                        })
                    })
                    .collect();

                threads
                    .into_iter()
                    .for_each(|t| t.join().expect("Thread failed"));

                assert_eq!(call_counter.load(Ordering::Acquire), 1);
            }
        }
    };
}

generate_test_get_with!(cache, Cache::<String, u64>::new(100));
generate_test_get_with!(seg_cache, SegmentedCache::<String, u64>::new(100, 4));
