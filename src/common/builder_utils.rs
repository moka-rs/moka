use std::time::Duration;

#[cfg(any(feature = "sync", feature = "future"))]
use super::concurrent::housekeeper;

const YEAR_SECONDS: u64 = 365 * 24 * 3600;

pub(crate) fn ensure_expirations_or_panic(
    time_to_live: Option<Duration>,
    time_to_idle: Option<Duration>,
) {
    let max_duration = Duration::from_secs(1_000 * YEAR_SECONDS);
    if let Some(d) = time_to_live {
        assert!(d <= max_duration, "time_to_live is longer than 1000 years");
    }
    if let Some(d) = time_to_idle {
        assert!(d <= max_duration, "time_to_idle is longer than 1000 years");
    }
}

#[cfg(any(feature = "sync", feature = "future"))]
pub(crate) fn housekeeper_conf(thread_pool_enabled: bool) -> housekeeper::Configuration {
    if thread_pool_enabled {
        housekeeper::Configuration::new_thread_pool(true)
    } else {
        housekeeper::Configuration::new_blocking()
    }
}
