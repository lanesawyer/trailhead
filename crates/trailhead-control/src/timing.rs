use std::time::Duration;

pub(crate) use trailhead_factory::clock::unix_now;

/// The wait after the `failures`th failure in a row: `first`, doubling up to `max`.
pub(crate) fn backoff(failures: u32, first: Duration, max: Duration) -> Duration {
    first.saturating_mul(1 << failures.saturating_sub(1).min(8)).min(max)
}

/// How long until a unix time, plus a second so a timer set for a reset fires after it.
pub(crate) fn until(unix: i64) -> Duration {
    Duration::from_secs(u64::try_from(unix - unix_now()).unwrap_or(0) + 1)
}
