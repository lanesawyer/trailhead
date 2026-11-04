//! Log timestamps shared by the server, the factory and the daemon.

use tracing_subscriber::fmt::time::ChronoLocal;

/// RFC 3339 with milliseconds and the UTC offset, e.g. `2026-11-03T10:56:19.184-08:00`.
pub const LOG_TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3f%:z";

/// A timer that stamps log lines in the machine's local time zone (`TZ` or `/etc/localtime`),
/// with the offset shown. `TZ=UTC` gives UTC.
pub fn local_timer() -> ChronoLocal {
    ChronoLocal::new(LOG_TIME_FORMAT.to_string())
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
