//! This test reproduces a race condition where returning `None` from `Expiry`
//! methods (to unset expiration) can cause a use-after-free panic in the timer wheel.
//!
//! The bug occurs because:
//! 1. `expiration_time` is set atomically (immediately) when `Expiry` returns `None`
//! 2. `timer_node` is only updated during housekeeping (later)
//! 3. This creates a window where `expiration_time` is `None` but `timer_node` still
//!    points to a freed/invalid timer node
//!
//! Pattern that triggers the bug:
//! - Insert with `Err` value → gets TTL (timer node created)
//! - Update to `Ok` value → `Expiry` returns `None` (should remove timer node)
//! - Concurrent `get` or housekeeping reads stale `timer_node` pointer

#![cfg(feature = "sync")]

use moka::sync::Cache;
use moka::Expiry;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

type HandleResult<T> = Result<T, String>;

struct CustomExpiry {
    error_ttl: Duration,
}

impl Default for CustomExpiry {
    fn default() -> Self {
        Self {
            error_ttl: Duration::from_millis(100),
        }
    }
}

impl<K, V> Expiry<K, HandleResult<V>> for CustomExpiry {
    fn expire_after_create(&self, _: &K, value: &HandleResult<V>, _: Instant) -> Option<Duration> {
        match value {
            Ok(_) => None,
            Err(_) => Some(self.error_ttl),
        }
    }

    fn expire_after_update(
        &self,
        _: &K,
        value: &HandleResult<V>,
        _: Instant,
        _: Option<Duration>,
    ) -> Option<Duration> {
        match value {
            Ok(_) => None,
            Err(_) => Some(self.error_ttl),
        }
    }
}

/// This test runs for a short duration (5 seconds by default) to catch the race condition.
/// In production, the bug was observed within 2 months of operation, but with aggressive
/// concurrent operations, it can be triggered much faster.
///
/// The test pattern:
/// 1. Insert threads: repeatedly insert `Err` then `Ok` values (triggers timer node create/remove)
/// 2. Get threads: continuously read entries (triggers `expire_after_read` and housekeeping)
/// 3. Housekeeping thread: explicitly runs `run_pending_tasks()` to process timer wheel operations
#[test]
fn test_timer_wheel_panic() {
    // Use shorter duration for CI, increase for more thorough testing
    let test_duration = Duration::from_secs(5);
    const NUM_KEYS: u64 = if cfg!(miri) { 25 } else { 100 };

    let panics = Arc::new(AtomicUsize::new(0));

    let cache: Cache<u64, HandleResult<u64>> = Cache::builder()
        .name("test_cache")
        .expire_after(CustomExpiry::default())
        .time_to_idle(Duration::from_secs(240))
        .max_capacity(10000)
        .build();

    let cache = Arc::new(cache);
    let start = Instant::now();

    // Insert threads: Err -> Ok pattern
    let insert_handles: Vec<_> = (0..4)
        .map(|tid| {
            let cache = Arc::clone(&cache);
            let panics = Arc::clone(&panics);
            let duration = test_duration;
            thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut ops = 0u64;
                    while start.elapsed() < duration {
                        for key in 0..NUM_KEYS {
                            let key = key + (tid as u64 * 10000);
                            // Insert error first (creates timer node with TTL)
                            cache.insert(key, Err("error".into()));
                            // Update to success (Expiry returns None - removes TTL)
                            cache.insert(key, Ok(ops));
                            // Sometimes go back to error
                            if ops % 3 == 0 {
                                cache.insert(key, Err("retry".into()));
                            }
                            ops += 1;
                        }
                    }
                    println!("[Insert {}] done: {} ops", tid, ops);
                }))
                .unwrap_or_else(|_| {
                    panics.fetch_add(1, Ordering::Relaxed);
                    eprintln!("[Insert {}] PANICKED!", tid);
                });
            })
        })
        .collect();

    // Get threads - trigger expire_after_read and housekeeping
    let get_handles: Vec<_> = (0..4)
        .map(|tid| {
            let cache = Arc::clone(&cache);
            let panics = Arc::clone(&panics);
            let duration = test_duration;
            thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut ops = 0u64;
                    while start.elapsed() < duration {
                        for key in 0..NUM_KEYS {
                            // Read from different thread's key space to increase contention
                            let key = key + ((ops % 4) * 10000);
                            let _ = cache.get(&key);
                            ops += 1;
                        }
                    }
                    println!("[Get {}] done: {} ops", tid, ops);
                }))
                .unwrap_or_else(|_| {
                    panics.fetch_add(1, Ordering::Relaxed);
                    eprintln!("[Get {}] PANICKED!", tid);
                });
            })
        })
        .collect();

    let cache_hk = Arc::clone(&cache);
    let panics_hk = Arc::clone(&panics);
    let hk = thread::spawn(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while start.elapsed() < test_duration {
                cache_hk.run_pending_tasks();
                thread::sleep(Duration::from_millis(50));
            }
        }))
        .unwrap_or_else(|_| {
            panics_hk.fetch_add(1, Ordering::Relaxed);
            eprintln!("[HK] PANICKED!");
        });
    });

    for h in insert_handles {
        let _ = h.join();
    }
    for h in get_handles {
        let _ = h.join();
    }
    let _ = hk.join();

    let total_panics = panics.load(Ordering::Relaxed);
    assert_eq!(
        total_panics, 0,
        "Timer wheel panic detected! {} threads panicked",
        total_panics
    );
}

/// Extended stress test - runs longer for more thorough testing.
///
/// To run this test, do one of the following:
///
/// ```console
/// ## Normal cargo test command for release build
/// $ cargo test --release -p moka -F sync --test timer_wheel_panic_test stress -- \
///     --ignored --no-capture
///
/// ## Miri command with appropriate flags
/// ##
/// ## - `miri-ignore-leaks` for ignoring `crossbeam-epoch`'s not yet reclaimed memory.
/// ## - `miri-permissive-provenance` to avoid provenance-related warnings for `tagptr`
/// ##    crate.
/// ## - `miri-disable-isolation` to use the wall-clock time for the test duration,
/// ##     instead of Miri's emulated virtual clock, which may advance significantly
/// ##     slower.
/// $ MIRIFLAGS='-Zmiri-tree-borrows -Zmiri-ignore-leaks -Zmiri-permissive-provenance -Zmiri-disable-isolation' \
///     cargo +nightly miri test stress_test_timer_wheel_panic -F sync -- \
///       --ignored --no-capture
/// ```
#[test]
#[ignore]
fn stress_test_timer_wheel_panic() {
    // If Miri is used, extend the duration significantly.
    const DURATION_MINUTES: u64 = if cfg!(miri) { 20 } else { 1 };
    let test_duration = Duration::from_secs(DURATION_MINUTES * 60);

    let panics = Arc::new(AtomicUsize::new(0));

    let cache: Cache<u64, HandleResult<u64>> = Cache::builder()
        .name("stress_test_cache")
        .expire_after(CustomExpiry::default())
        .time_to_idle(Duration::from_secs(240))
        .max_capacity(10000)
        .build();

    let cache = Arc::new(cache);
    let start = Instant::now();

    // More aggressive thread count for stress testing
    const NUM_INSERT_THREADS: i32 = if cfg!(miri) { 6 } else { 8 };
    const NUM_GET_THREADS: i32 = if cfg!(miri) { 4 } else { 8 };

    // Insert threads
    let insert_handles: Vec<_> = (0..NUM_INSERT_THREADS)
        .map(|tid| {
            let cache = Arc::clone(&cache);
            let panics = Arc::clone(&panics);
            let duration = test_duration;
            thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut ops = 0u64;
                    const REPORT_INTERVAL: u64 = if cfg!(miri) { 5 } else { 100_000 };
                    'l: loop {
                        for key in 0..200u64 {
                            let key = key + (tid as u64 * 10000);
                            cache.insert(key, Err("error".into()));
                            cache.insert(key, Ok(ops));
                            if ops % 3 == 0 {
                                cache.insert(key, Err("retry".into()));
                            }
                            if ops % 7 == 0 {
                                cache.invalidate(&key);
                            }
                            ops += 1;
                            if ops % REPORT_INTERVAL == 0 {
                                println!("[Insert {}] running: {} ops", tid, ops);
                            }

                            if start.elapsed() >= duration {
                                break 'l;
                            }
                        }
                    }
                    println!("[Insert {}] done: {} ops", tid, ops);
                }))
                .unwrap_or_else(|e| {
                    panics.fetch_add(1, Ordering::Relaxed);
                    eprintln!("[Insert {}] PANICKED: {:?}", tid, e);
                });
            })
        })
        .collect();

    // Get threads
    let get_handles: Vec<_> = (0..NUM_GET_THREADS)
        .map(|tid| {
            let cache = Arc::clone(&cache);
            let panics = Arc::clone(&panics);
            let duration = test_duration;
            thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut ops = 0u64;
                    const REPORT_INTERVAL: u64 = if cfg!(miri) { 50 } else { 100_000 };
                    'l: loop {
                        for key in 0..200u64 {
                            let key = key + ((ops % NUM_GET_THREADS as u64) * 10000);
                            let _ = cache.get(&key);
                            ops += 1;
                            if ops % REPORT_INTERVAL == 0 {
                                println!("[Get {}] running: {} ops", tid, ops);
                            }

                            if start.elapsed() >= duration {
                                break 'l;
                            }
                        }
                    }
                    println!("[Get {}] done: {} ops", tid, ops);
                }))
                .unwrap_or_else(|e| {
                    panics.fetch_add(1, Ordering::Relaxed);
                    eprintln!("[Get {}] PANICKED: {:?}", tid, e);
                });
            })
        })
        .collect();

    let hk_handles = if cfg!(miri) {
        None
    } else {
        let handles = (0..2)
            .map(|tid| {
                let cache = Arc::clone(&cache);
                let panics = Arc::clone(&panics);
                let duration = test_duration;
                thread::spawn(move || {
                    let mut ops = 0u64;
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        while start.elapsed() < duration {
                            cache.run_pending_tasks();
                            thread::sleep(Duration::from_millis(25));

                            ops += 1;
                            if ops % 50 == 0 {
                                println!("[HK {}] running: {} ops", tid, ops);
                            }
                        }
                    }))
                    .unwrap_or_else(|e| {
                        panics.fetch_add(1, Ordering::Relaxed);
                        eprintln!("[HK {}] PANICKED: {:?}", tid, e);
                    });
                })
            })
            .collect::<Vec<_>>();
        Some(handles)
    };

    // Wait for all threads
    for h in insert_handles {
        let _ = h.join();
    }
    for h in get_handles {
        let _ = h.join();
    }
    if let Some(hk_handles) = hk_handles {
        for h in hk_handles {
            let _ = h.join();
        }
    }

    let total_panics = panics.load(Ordering::Relaxed);
    assert_eq!(
        total_panics, 0,
        "Timer wheel panic detected! {} threads panicked during stress test.",
        total_panics
    );
}
