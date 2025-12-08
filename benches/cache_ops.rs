//! Benchmark suite for moka cache operations.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use moka::sync::Cache;
use std::sync::Arc;
use std::time::Duration;

/// Benchmark insertion of new entries into an empty cache.
///
/// Tests cache sizes: 100, 1,000, and 10,000 entries.
fn insert_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert");

    for size in [100, 1000, 10000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                let cache: Cache<u64, String> = Cache::new(size);
                for i in 0..size {
                    cache.insert(black_box(i), black_box(format!("value-{}", i)));
                }
            });
        });
    }
    group.finish();
}

/// Benchmark read operations on a pre-populated cache.
///
/// Measures the performance of `get()` operations across different cache sizes.
fn get_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("get");

    for size in [100, 1000, 10000].iter() {
        let cache: Cache<u64, String> = Cache::new(*size);
        for i in 0..*size {
            cache.insert(i, format!("value-{}", i));
        }

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                for i in 0..size {
                    let _ = cache.get(&black_box(i));
                }
            });
        });
    }
    group.finish();
}

/// Benchmark mixed cache operations representing a realistic workload.
///
/// Distribution: 33% inserts, 33% gets, 33% contains_key operations.
fn mixed_operations_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_ops");

    for size in [100, 1000, 10000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                let cache: Cache<u64, String> = Cache::new(size);
                for i in 0..size {
                    // Mix of insert, get, and contains_key operations
                    if i % 3 == 0 {
                        cache.insert(black_box(i), black_box(format!("value-{}", i)));
                    } else if i % 3 == 1 {
                        let _ = cache.get(&black_box(i));
                    } else {
                        let _ = cache.contains_key(&black_box(i));
                    }
                }
            });
        });
    }
    group.finish();
}

/// Benchmark LRU eviction performance.
///
/// Inserts 2,000 entries into a cache with capacity of 1,000 to trigger evictions.
fn eviction_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("eviction");

    // Create a small cache and insert more items than its capacity to trigger evictions
    let cache_size = 1000;
    let insert_count = 2000;

    group.throughput(Throughput::Elements(insert_count));
    group.bench_function("lru_eviction", |b| {
        b.iter(|| {
            let cache: Cache<u64, String> = Cache::new(cache_size);
            for i in 0..insert_count {
                cache.insert(black_box(i), black_box(format!("value-{}", i)));
            }
        });
    });

    group.finish();
}

/// Benchmark update operations with Arc-wrapped keys.
///
/// Tests the performance of updating existing cache entries when keys are `Arc<String>`.
/// This measures the modify path performance with reference-counted keys.
fn update_arc_keys_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("update_arc_keys");

    for size in [100, 1000, 10000].iter() {
        let cache: Cache<Arc<String>, String> = Cache::new(*size);

        // Pre-populate the cache with Arc-wrapped keys
        for i in 0..*size {
            let key = Arc::new(format!("key-{}", i));
            cache.insert(key, format!("value-{}", i));
        }

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                // Update all existing keys (exercises the modify path with Arc-wrapped keys)
                for i in 0..size {
                    let key = Arc::new(format!("key-{}", i));
                    cache.insert(black_box(key), black_box(format!("updated-{}", i)));
                }
            });
        });
    }
    group.finish();
}

/// Benchmark update operations with large Arc-wrapped keys.
///
/// Uses 10KB `Arc<Vec<u8>>` keys to measure performance and memory characteristics
/// of updating cache entries with large reference-counted keys.
fn update_large_arc_keys_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("update_large_arc_keys");

    let size = 100;
    let key_size = 10 * 1024; // 10KB per key
    let cache: Cache<Arc<Vec<u8>>, u64> = Cache::new(size);

    // Pre-populate with large keys
    for i in 0..size {
        let key = Arc::new(vec![i as u8; key_size]);
        cache.insert(key, i);
    }

    group.throughput(Throughput::Elements(size as u64));
    group.bench_function("10kb_keys", |b| {
        b.iter(|| {
            for i in 0..size {
                let key = Arc::new(vec![i as u8; key_size]);
                cache.insert(black_box(key), black_box(i * 2));
            }
        });
    });

    group.finish();
}

/// Benchmark update operations on pre-populated cache with regular keys.
///
/// Tests pure update performance (no inserts) with standard `u64` keys.
/// Provides baseline comparison for Arc-wrapped key benchmarks.
fn update_existing_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("update_existing");

    for size in [100, 1000, 10000].iter() {
        let cache: Cache<u64, String> = Cache::new(*size);

        // Pre-populate the cache
        for i in 0..*size {
            cache.insert(i, format!("value-{}", i));
        }

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                // Update all existing entries
                for i in 0..size {
                    cache.insert(black_box(i), black_box(format!("updated-{}", i)));
                }
            });
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(10))
        .sample_size(100);
    targets = insert_benchmark, get_benchmark, mixed_operations_benchmark, eviction_benchmark,
              update_arc_keys_benchmark, update_large_arc_keys_benchmark, update_existing_benchmark
}

criterion_main!(benches);
