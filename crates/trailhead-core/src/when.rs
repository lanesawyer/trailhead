//! Days with an optional time, as used by due dates and scheduling: `<f>_on` is the
//! day, `<f>_at` an optional instant on it, `<f>_tz` the IANA zone it was set in.

use time::{Date, OffsetDateTime};

use crate::error::{Error, Result};

pub(crate) fn zone(tz: &str) -> Result<jiff::tz::TimeZone> {
    jiff::tz::TimeZone::get(tz)
        .map_err(|_| Error::Invalid(format!("{tz:?} isn't a known IANA time zone, like America/Los_Angeles")))
}

fn to_jiff(t: OffsetDateTime) -> jiff::Timestamp {
    jiff::Timestamp::from_second(t.unix_timestamp()).expect("in range")
}

/// The local date and time of `at` in `tz`, or at its own offset without a zone.
fn local(at: OffsetDateTime, tz: Option<&str>) -> Result<(Date, (u8, u8, u8))> {
    let Some(tz) = tz else { return Ok((at.date(), (at.hour(), at.minute(), at.second()))) };
    let z = to_jiff(at).to_zoned(zone(tz)?);
    let date =
        Date::from_calendar_date(z.year().into(), (z.month() as u8).try_into().expect("valid month"), z.day() as u8)
            .map_err(|e| Error::Invalid(e.to_string()))?;
    Ok((date, (z.hour() as u8, z.minute() as u8, z.second() as u8)))
}

/// The last second of `day` in `tz` (UTC without one): when a day-only deadline passes.
pub(crate) fn end_of_day(day: Date, tz: Option<&str>) -> Result<OffsetDateTime> {
    let civil = jiff::civil::date(day.year() as i16, day.month() as i8, day.day() as i8).at(23, 59, 59, 0);
    let zone = match tz {
        Some(tz) => zone(tz)?,
        None => jiff::tz::TimeZone::UTC,
    };
    let ts = civil.to_zoned(zone).map_err(|e| Error::Invalid(e.to_string()))?.timestamp();
    Ok(OffsetDateTime::from_unix_timestamp(ts.as_second()).expect("in range"))
}

/// The moment a deadline passes: its time, else the end of its day.
pub(crate) fn deadline(
    on: Option<Date>,
    at: Option<OffsetDateTime>,
    tz: Option<&str>,
) -> Result<Option<OffsetDateTime>> {
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
    at: Option<OffsetDateTime>,
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
pub(crate) fn split_legacy_deadline(at: OffsetDateTime, tz: Option<&str>) -> Result<(Date, bool)> {
    let tz = tz.filter(|z| zone(z).is_ok()).unwrap_or("UTC");
    let (day, hms) = local(at, Some(tz))?;
    Ok((day, hms == (23, 59, 59) && at.nanosecond() == 0))
}

#[cfg(test)]
mod tests {
    use time::macros::{date, datetime};

    use super::*;

    #[test]
    fn legacy_end_of_day_deadlines_become_day_only() {
        let la = Some("America/Los_Angeles");
        // What `--due 2026-10-31` stored: 23:59:59 that day in Los Angeles.
        assert_eq!(
            split_legacy_deadline(datetime!(2026-11-01 6:59:59 UTC), la).unwrap(),
            (date!(2026 - 10 - 31), true)
        );
        assert_eq!(split_legacy_deadline(datetime!(2026-11-01 6:00 UTC), la).unwrap(), (date!(2026 - 10 - 31), false));
        assert_eq!(
            split_legacy_deadline(datetime!(2026-11-01 6:59:59 UTC), None).unwrap(),
            (date!(2026 - 11 - 01), false)
        );
        assert!(split_legacy_deadline(datetime!(2026-11-01 23:59:59 UTC), Some("Mars/Base")).unwrap().1);
        assert_eq!(end_of_day(date!(2026 - 10 - 31), la).unwrap(), datetime!(2026-11-01 6:59:59 UTC));
    }
}
