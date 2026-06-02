//! Driving moka's TTL/TTI from an external clock via [`moka::ExternalClock`].
//!
//! Run with:
//!
//! ```sh
//! cargo run --example external_time --features sync
//! ```
//!
//! This demonstrates the `external_clock` builder seam using a real-world
//! `TimeProvider` abstraction (adapted from the Hashiverse project — see the
//! `TimeProvider` / `RealTimeProvider` / `ScaledTimeProvider` section below;
//! the only change is using `std::time` instead of `chrono`). A thin adapter
//! implements `moka::ExternalClock` on top of any `TimeProvider`, so a cache's
//! native `time_to_live` is measured against that provider's clock instead of
//! `std::time::Instant`.
//!
//! The payoff: with a `ScaledTimeProvider` running, say, 1,000,000× faster than
//! wall time, a 1-hour TTL elapses after a few milliseconds of real sleeping —
//! which is exactly what makes time-dependent cache logic testable without
//! actually waiting.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use moka::sync::Cache;
use moka::ExternalClock;

/// Milliseconds since the Unix epoch, via `std` (so this example needs no
/// `chrono` dependency). Hashiverse's originals use `chrono::Utc::now()`; the
/// providers below are adapted to call this instead.
fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Minimal time newtypes (trimmed copies of Hashiverse's wire-format types,
// providing only what the verbatim providers below reference).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeMillis(pub i64);

impl std::fmt::Display for TimeMillis {
    // Simplified vs. Hashiverse's `YYYYMMDD.HHMMSS.mmm`; only used by the
    // `current_time_str` default method, which this example never calls.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy)]
pub struct DurationMillis(pub i64);

impl From<DurationMillis> for Duration {
    fn from(value: DurationMillis) -> Self {
        if value.0 <= 0 {
            Duration::ZERO
        } else {
            Duration::from_millis(value.0 as u64)
        }
    }
}

// ===========================================================================
// TimeProvider / RealTimeProvider / ScaledTimeProvider — adapted from Hashiverse
// (hashiverse-lib/src/tools/time_provider/time_provider.rs). The only change vs.
// the originals is swapping `chrono::Utc::now()` for `now_unix_millis()` (std),
// and dropping the `current_datetime()` default method (it returns a chrono
// type and is unused here) — so this example needs no chrono dependency.
// ===========================================================================

/// A stubbable abstraction over wall-clock time and asynchronous sleeping.
pub trait TimeProvider: Send + Sync {
    /// Returns the current time in milliseconds since the UNIX epoch
    fn current_time_millis(&self) -> TimeMillis;

    fn current_time_str(&self) -> String {
        self.current_time_millis().to_string()
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;

    fn sleep_millis(&self, millis: DurationMillis) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.sleep(Duration::from(millis))
    }
}

/// Implementation of TimeProvider that uses the system clock
#[derive(Default, Clone)]
pub struct RealTimeProvider;

impl TimeProvider for RealTimeProvider {
    fn current_time_millis(&self) -> TimeMillis {
        TimeMillis(now_unix_millis())
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Implementation of TimeProvider that scales time by a given factor
///
/// This provider allows you to make time pass faster or slower than real time.
/// For example, a scale factor of 2.0 means time passes twice as fast,
/// and a scale factor of 0.5 means time passes at half speed.
#[derive(Clone)]
pub struct ScaledTimeProvider {
    scale_factor: f64,
    start_real_time: i64,
}

impl ScaledTimeProvider {
    /// Create a new ScaledTimeProvider with the specified scale factor
    ///
    /// # Arguments
    /// * `scale_factor` - How much faster (>1.0) or slower (<1.0) time should pass compared to real time
    pub fn new(scale_factor: f64) -> Self {
        Self {
            scale_factor,
            start_real_time: now_unix_millis(),
        }
    }
}

impl TimeProvider for ScaledTimeProvider {
    fn current_time_millis(&self) -> TimeMillis {
        // Calculate how much real time has passed since we started
        let real_now = now_unix_millis();
        let real_elapsed = real_now.saturating_sub(self.start_real_time);

        // Scale the elapsed time and add it to our starting point
        let scaled_elapsed = (real_elapsed as f64 * self.scale_factor) as i64;
        TimeMillis(scaled_elapsed)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        // Calculate the scaled duration and create the sleep future once
        let scaled_duration = Duration::from_secs_f64(duration.as_secs_f64() / self.scale_factor);
        Box::pin(tokio::time::sleep(scaled_duration))
    }
}

// ===========================================================================
// The adapter: any TimeProvider becomes a moka ExternalClock.
// ===========================================================================

/// Bridges a [`TimeProvider`] to moka's [`ExternalClock`]. moka measures time as
/// a `Duration` elapsed since the clock's origin, so we capture the provider's
/// reading at construction and report the difference on every call.
struct TimeProviderClock {
    time_provider: Arc<dyn TimeProvider>,
    origin_millis: i64,
}

impl TimeProviderClock {
    fn new(time_provider: Arc<dyn TimeProvider>) -> Self {
        let origin_millis = time_provider.current_time_millis().0;
        Self { time_provider, origin_millis }
    }
}

impl ExternalClock for TimeProviderClock {
    fn elapsed_since_origin(&self) -> Duration {
        let now = self.time_provider.current_time_millis().0;
        Duration::from_millis(now.saturating_sub(self.origin_millis).max(0) as u64)
    }
}

fn main() {
    // A one-hour TTL. Under wall time you'd wait an hour to see expiry.
    let ttl = Duration::from_secs(60 * 60);

    // Cache A — driven by RealTimeProvider through the external clock. Behaves
    // like an ordinary wall-clock cache: the entry survives a short real sleep.
    let real_cache: Cache<&str, &str> = Cache::builder()
        .time_to_live(ttl)
        .external_clock(Arc::new(TimeProviderClock::new(Arc::new(RealTimeProvider))))
        .build();

    // Cache B — driven by a ScaledTimeProvider running 1,000,000× faster, so a
    // few real milliseconds become hours of cache time and the entry expires.
    let scale_factor = 1_000_000.0;
    let scaled_cache: Cache<&str, &str> = Cache::builder()
        .time_to_live(ttl)
        .external_clock(Arc::new(TimeProviderClock::new(Arc::new(ScaledTimeProvider::new(scale_factor)))))
        .build();

    real_cache.insert("k", "v");
    scaled_cache.insert("k", "v");
    println!("After insert: real_cache={:?}, scaled_cache={:?}", real_cache.get(&"k"), scaled_cache.get(&"k"));

    // Sleep a tiny bit of *real* time. For the scaled cache this is
    // 0.1s × 1,000,000 ≈ 27.7 hours of cache time — well past the 1-hour TTL.
    let real_sleep = Duration::from_millis(100);
    println!(
        "\nSleeping {:?} of real time (= {:?} of scaled cache time)...\n",
        real_sleep,
        real_sleep.mul_f64(scale_factor),
    );
    std::thread::sleep(real_sleep);

    // moka does maintenance when poked; force it so expiry is applied now.
    real_cache.run_pending_tasks();
    scaled_cache.run_pending_tasks();

    let real_after = real_cache.get(&"k");
    let scaled_after = scaled_cache.get(&"k");
    println!("After 100ms real sleep: real_cache={real_after:?}, scaled_cache={scaled_after:?}");

    assert_eq!(real_after, Some("v"), "wall-clock entry should NOT expire after only 100ms of real time");
    assert_eq!(scaled_after, None, "scaled entry SHOULD expire: 100ms × 1e6 is well past the 1-hour TTL");

    println!("\nOK: moka's TTL honoured the injected TimeProvider clock for both providers.");
}
