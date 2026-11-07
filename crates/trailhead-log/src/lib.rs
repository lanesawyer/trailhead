//! The log setup the server, the factory host and the daemon share, so the timestamp format and
//! the subscriber's shape live in one place.

use std::io::IsTerminal;

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::time::ChronoLocal;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// RFC 3339 with milliseconds and the UTC offset, e.g. `2026-11-03T10:56:19.184-08:00`.
pub const LOG_TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3f%:z";

/// A timer that stamps log lines in the machine's local time zone (`TZ` or `/etc/localtime`),
/// with the offset shown. `TZ=UTC` gives UTC.
pub fn local_timer() -> ChronoLocal {
    ChronoLocal::new(LOG_TIME_FORMAT.to_string())
}

/// Installs the global subscriber: lines go to stdout (colored only on a terminal), and to `file`
/// as well, uncolored, when there is one. `extra` is one more layer (the host's log buffer), fed
/// what the filter lets through. Panics if a global subscriber is already set.
pub fn init<W, L>(filter: EnvFilter, file: Option<W>, extra: L)
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
    L: Layer<Registry> + Send + Sync + 'static,
{
    let stdout = tracing_subscriber::fmt::layer().with_timer(local_timer()).with_ansi(std::io::stdout().is_terminal());
    let file = file.map(|w| tracing_subscriber::fmt::layer().with_timer(local_timer()).with_ansi(false).with_writer(w));
    tracing_subscriber::registry()
        .with(extra.with_filter(filter.clone()))
        .with(file.with_filter(filter.clone()))
        .with(stdout.with_filter(filter))
        .init();
}

/// [`init`] with stdout only.
pub fn init_stdout(filter: EnvFilter) {
    init(filter, None::<fn() -> std::io::Sink>, tracing_subscriber::layer::Identity::new());
}

/// The filter from `RUST_LOG`, or `default` when it is unset or invalid.
pub fn env_filter_or(default: &str) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| default.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};

    #[test]
    fn formats_with_offset() {
        let pacific = FixedOffset::west_opt(8 * 3600).unwrap();
        let t = pacific.with_ymd_and_hms(2026, 11, 3, 10, 56, 19).unwrap() + chrono::Duration::milliseconds(184);
        assert_eq!(t.format(LOG_TIME_FORMAT).to_string(), "2026-11-03T10:56:19.184-08:00");
        assert_eq!(t.with_timezone(&chrono::Utc).format(LOG_TIME_FORMAT).to_string(), "2026-11-03T18:56:19.184+00:00");
    }
}
