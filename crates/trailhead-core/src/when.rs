//! Days with an optional time, as used by due dates and scheduling: `<f>_on` is the
//! day, `<f>_at` an optional instant on it, `<f>_tz` the IANA zone it was set in.

use jiff::civil::Date;
use jiff::tz::{Offset, TimeZone};
use jiff::{Timestamp, Zoned};

use crate::error::{Error, Result};

pub(crate) fn zone(tz: &str) -> Result<TimeZone> {
    TimeZone::get(tz)
        .map_err(|_| Error::Invalid(format!("{tz:?} isn't a known IANA time zone, like America/Los_Angeles")))
}

/// Parses an RFC 3339 timestamp, keeping the offset it was written with, so a time
/// given without a zone still falls on the day its writer meant.
pub fn parse_at(s: &str) -> Result<Zoned> {
    let bad = |_| Error::Invalid(format!("{s:?} is not an RFC 3339 timestamp"));
    let ts: Timestamp = s.parse().map_err(bad)?;
    let pieces = jiff::fmt::temporal::Pieces::parse(s).map_err(bad)?;
    let offset = pieces.to_numeric_offset().unwrap_or(Offset::UTC);
    Ok(ts.to_zoned(TimeZone::fixed(offset)))
}

/// A stored instant, which is always UTC.
pub(crate) fn utc(t: Timestamp) -> Zoned {
    t.to_zoned(TimeZone::UTC)
}

/// The local date and time of `at` in `tz`, or at its own offset without a zone.
fn local(at: &Zoned, tz: Option<&str>) -> Result<(Date, (i8, i8, i8))> {
    let z = match tz {
        Some(tz) => at.timestamp().to_zoned(zone(tz)?),
        None => at.clone(),
    };
    Ok((z.date(), (z.hour(), z.minute(), z.second())))
}

/// The last second of `day` in `tz` (UTC without one): when a day-only deadline passes.
pub(crate) fn end_of_day(day: Date, tz: Option<&str>) -> Result<Timestamp> {
    let zone = match tz {
        Some(tz) => zone(tz)?,
        None => TimeZone::UTC,
    };
    Ok(day.at(23, 59, 59, 0).to_zoned(zone).map_err(|e| Error::Invalid(e.to_string()))?.timestamp())
}

/// The moment a deadline passes: its time, else the end of its day.
pub(crate) fn deadline(on: Option<Date>, at: Option<Timestamp>, tz: Option<&str>) -> Result<Option<Timestamp>> {
    match (at, on) {
        (Some(at), _) => Ok(Some(at)),
        (None, Some(day)) => end_of_day(day, tz).map(Some),
        (None, None) => Ok(None),
    }
}

/// Fills in the day from a time and checks day, time and zone agree. `field` names
/// them in errors (`due`, `scheduled`). Only call it when the time or zone was just
/// given: stored times are UTC, so a time without a zone can't be placed again later.
pub(crate) fn settle(
    field: &str,
    on: &mut Option<Date>,
    at: Option<&Zoned>,
    tz: &Option<String>,
    zone_needs_time: bool,
) -> Result<()> {
    if let Some(tz) = tz {
        zone(tz)?;
        if at.is_none() && (zone_needs_time || on.is_none()) {
            let needs = if zone_needs_time { format!("{field}_at") } else { format!("{field}_on or {field}_at") };
            return Err(Error::Invalid(format!("{field}_tz needs {needs}")));
        }
    }
    if let Some(at) = at {
        let (day, _) = local(at, tz.as_deref())?;
        match on {
            Some(d) if *d != day => {
                return Err(Error::Invalid(format!("{field}_at falls on {day}, not {field}_on {d}")));
            }
            _ => *on = Some(day),
        }
    }
    Ok(())
}

/// For migrating timestamp-only deadlines: the day, and whether the time was the
/// 23:59:59 the CLI used to stand in for "some time that day".
pub(crate) fn split_legacy_deadline(at: Timestamp, tz: Option<&str>) -> Result<(Date, bool)> {
    let tz = tz.filter(|z| zone(z).is_ok()).unwrap_or("UTC");
    let (day, hms) = local(&utc(at), Some(tz))?;
    Ok((day, hms == (23, 59, 59) && at.subsec_nanosecond() == 0))
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn legacy_end_of_day_deadlines_become_day_only() {
        let la = Some("America/Los_Angeles");
        // What `--due 2026-10-31` stored: 23:59:59 that day in Los Angeles.
        assert_eq!(split_legacy_deadline(ts("2026-11-01T06:59:59Z"), la).unwrap(), (date(2026, 10, 31), true));
        assert_eq!(split_legacy_deadline(ts("2026-11-01T06:00:00Z"), la).unwrap(), (date(2026, 10, 31), false));
        assert_eq!(split_legacy_deadline(ts("2026-11-01T06:59:59Z"), None).unwrap(), (date(2026, 11, 1), false));
        assert!(split_legacy_deadline(ts("2026-11-01T23:59:59Z"), Some("Mars/Base")).unwrap().1);
        assert_eq!(end_of_day(date(2026, 10, 31), la).unwrap(), ts("2026-11-01T06:59:59Z"));
    }

    #[test]
    fn a_time_without_a_zone_falls_on_its_own_offsets_day() {
        let at = parse_at("2026-11-06T23:00:00-08:00").unwrap();
        assert_eq!(at.timestamp(), ts("2026-11-07T07:00:00Z"));
        let mut on = None;
        settle("due", &mut on, Some(&at), &None, false).unwrap();
        assert_eq!(on, Some(date(2026, 11, 6)));
        assert_eq!(parse_at("2026-11-06T23:00:00Z").unwrap().date(), date(2026, 11, 6));
        assert!(parse_at("2026-11-06").is_err());
    }
}
