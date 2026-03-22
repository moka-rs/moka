//! Expiry checking helper functions for concurrent caches.
//!
//! These are pure functions that check entry expiration status. They do not
//! acquire locks or perform async operations.

use crate::common::{
    concurrent::{arc::MiniArc, entry_info::EntryInfo, AccessTime},
    time::Instant,
};

use std::time::Duration;

/// Returns `true` if this entry is expired by its per-entry TTL.
#[inline]
pub(crate) fn is_expired_by_per_entry_ttl<K>(
    entry_info: &MiniArc<EntryInfo<K>>,
    now: Instant,
) -> bool {
    if let Some(ts) = entry_info.expiration_state().0 {
        ts <= now
    } else {
        false
    }
}

/// Returns `true` when one of the followings conditions is met:
///
/// - This entry is expired by the time-to-idle config of this cache instance.
/// - Or, it is invalidated by the `invalidate_all` method.
#[inline]
pub(crate) fn is_expired_entry_ao(
    time_to_idle: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry: &impl AccessTime,
    now: Instant,
) -> bool {
    if let Some(ts) = entry.last_accessed() {
        is_invalid_entry(valid_after, ts) || is_expired_by_tti(time_to_idle, ts, now)
    } else {
        false
    }
}

/// Returns `true` when one of the following conditions is met:
///
/// - This entry is expired by the time-to-live (TTL) config of this cache instance.
/// - Or, it is invalidated by the `invalidate_all` method.
#[inline]
pub(crate) fn is_expired_entry_wo(
    time_to_live: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry: &impl AccessTime,
    now: Instant,
) -> bool {
    if let Some(ts) = entry.last_modified() {
        is_invalid_entry(valid_after, ts) || is_expired_by_ttl(time_to_live, ts, now)
    } else {
        false
    }
}

/// Returns a tuple of (is_expired, is_invalid) for access-order expiration checking.
#[inline]
pub(crate) fn is_entry_expired_ao_or_invalid(
    time_to_idle: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry_last_accessed: Instant,
    now: Instant,
) -> (bool, bool) {
    let ts = entry_last_accessed;
    let expired = is_expired_by_tti(time_to_idle, ts, now);
    let invalid = is_invalid_entry(valid_after, ts);
    (expired, invalid)
}

/// Returns a tuple of (is_expired, is_invalid) for write-order expiration checking.
#[inline]
pub(crate) fn is_entry_expired_wo_or_invalid(
    time_to_live: &Option<Duration>,
    valid_after: &Option<Instant>,
    entry_last_modified: Instant,
    now: Instant,
) -> (bool, bool) {
    let ts = entry_last_modified;
    let expired = is_expired_by_ttl(time_to_live, ts, now);
    let invalid = is_invalid_entry(valid_after, ts);
    (expired, invalid)
}

/// Returns `true` if the entry timestamp is before the valid_after time.
#[inline]
pub(crate) fn is_invalid_entry(valid_after: &Option<Instant>, entry_ts: Instant) -> bool {
    if let Some(va) = valid_after {
        entry_ts < *va
    } else {
        false
    }
}

/// Returns `true` if the entry is expired by time-to-idle.
#[inline]
pub(crate) fn is_expired_by_tti(
    time_to_idle: &Option<Duration>,
    entry_last_accessed: Instant,
    now: Instant,
) -> bool {
    if let Some(tti) = time_to_idle {
        let expiration = entry_last_accessed.saturating_add(*tti);
        expiration <= now
    } else {
        false
    }
}

/// Returns `true` if the entry is expired by time-to-live.
#[inline]
pub(crate) fn is_expired_by_ttl(
    time_to_live: &Option<Duration>,
    entry_last_modified: Instant,
    now: Instant,
) -> bool {
    if let Some(ttl) = time_to_live {
        let expiration = entry_last_modified.saturating_add(*ttl);
        expiration <= now
    } else {
        false
    }
}
