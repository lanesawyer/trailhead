use std::time::Duration;

/// The wait after the `failures`th failure in a row: `first`, doubling up to `max`.
pub(crate) fn backoff(failures: u32, first: Duration, max: Duration) -> Duration {
    first.saturating_mul(1 << failures.saturating_sub(1).min(8)).min(max)
}

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// How long until a unix time, plus a second so a timer set for a reset fires after it.
pub(crate) fn until(unix: i64) -> Duration {
    Duration::from_secs(u64::try_from(unix - unix_now()).unwrap_or(0) + 1)
}
