//! Days and times typed on the command line, read in the local zone.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::output;

/// RFC 3339 passes through; a bare date means the end of that day in the local
/// zone, using that day's offset (DST included).
pub(crate) fn parse_when(s: &str) -> Result<String> {
    parse_when_in(s, output::local_tz())
}

fn parse_when_in(s: &str, tz: &jiff::tz::TimeZone) -> Result<String> {
    if s.parse::<jiff::Timestamp>().is_ok() {
        return Ok(s.to_string());
    }
    let date: jiff::civil::Date = s.parse().with_context(|| format!("{s:?} isn't YYYY-MM-DD or RFC 3339"))?;
    let end_of_day = date.at(23, 59, 59, 0).to_zoned(tz.clone())?;
    Ok(end_of_day.timestamp().to_string())
}

pub(crate) fn local_today() -> jiff::civil::Date {
    jiff::Zoned::now().with_time_zone(output::local_tz().clone()).date()
}

/// Fields for `--on` (`scheduled_*`) or `--due` (`due_*`): a day, a day and a time,
/// a time today, or an RFC 3339 timestamp, in the local zone.
pub(crate) fn parse_day_time(field: Field, words: &[String]) -> Result<Value> {
    parse_day_time_in(field, &words.join(" "), local_today(), output::local_tz())
}

#[derive(Clone, Copy)]
pub(crate) enum Field {
    Scheduled,
    Due,
}

fn parse_day_time_in(field: Field, s: &str, today: jiff::civil::Date, tz: &jiff::tz::TimeZone) -> Result<Value> {
    let f = match field {
        Field::Scheduled => "scheduled",
        Field::Due => "due",
    };
    let at = |at: jiff::Zoned| {
        json!({
            format!("{f}_on"): at.date().to_string(),
            format!("{f}_at"): at.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
            format!("{f}_tz"): tz.iana_name(),
        })
    };
    if let Ok(ts) = s.trim().parse::<jiff::Timestamp>() {
        return Ok(at(ts.to_zoned(tz.clone())));
    }
    let lower = s.to_lowercase();
    let mut words: Vec<&str> = lower.split_whitespace().filter(|w| *w != "at").collect();
    let day = match words.first().map(|w| parse_day(w, today)) {
        Some(Ok(day)) => {
            words.remove(0);
            day
        }
        _ => today,
    };
    if !words.is_empty() {
        return Ok(at(day.to_datetime(parse_clock(&words.concat())?).to_zoned(tz.clone())?));
    }
    let mut out = json!({ format!("{f}_on"): day.to_string() });
    // A day-only deadline ends at midnight in a zone; a scheduled day has no instant.
    if let (Field::Due, Some(name)) = (field, tz.iana_name()) {
        out["due_tz"] = json!(name);
    }
    Ok(out)
}

/// `YYYY-MM-DD`, `today`, `tomorrow`, or a weekday (`fri`, `friday`): the next one,
/// counting today.
fn parse_day(s: &str, today: jiff::civil::Date) -> Result<jiff::civil::Date> {
    use jiff::civil::Weekday::*;
    let weekday = match s {
        "mon" | "monday" => Some(Monday),
        "tue" | "tues" | "tuesday" => Some(Tuesday),
        "wed" | "wednesday" => Some(Wednesday),
        "thu" | "thur" | "thurs" | "thursday" => Some(Thursday),
        "fri" | "friday" => Some(Friday),
        "sat" | "saturday" => Some(Saturday),
        "sun" | "sunday" => Some(Sunday),
        _ => None,
    };
    if let Some(w) = weekday {
        return Ok(today.checked_add(jiff::Span::new().days(today.weekday().until(w)))?);
    }
    match s {
        "today" => Ok(today),
        "tomorrow" => Ok(today.tomorrow()?),
        _ => s.parse().with_context(|| format!("{s:?} isn't YYYY-MM-DD, today, tomorrow or a weekday")),
    }
}

/// `1pm`, `1:30pm`, `13:00` or `noon`.
fn parse_clock(s: &str) -> Result<jiff::civil::Time> {
    let bad = || anyhow::anyhow!("{s:?} isn't a day or a time like 1pm, 1:30pm or 13:00");
    if s == "noon" {
        return Ok(jiff::civil::time(12, 0, 0, 0));
    }
    let (clock, pm) = match (s.strip_suffix("am"), s.strip_suffix("pm")) {
        (Some(c), _) => (c, Some(false)),
        (_, Some(c)) => (c, Some(true)),
        _ => (s, None),
    };
    let (h, m) = clock.split_once(':').unwrap_or((clock, "0"));
    let mut hour: i8 = h.parse().map_err(|_| bad())?;
    let minute: i8 = m.parse().map_err(|_| bad())?;
    if let Some(pm) = pm {
        if !(1..=12).contains(&hour) {
            return Err(bad());
        }
        hour = hour % 12 + if pm { 12 } else { 0 };
    }
    jiff::civil::Time::new(hour, minute, 0, 0).map_err(|_| bad())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Field, parse_day_time_in, parse_when_in};

    #[test]
    fn dates_end_at_local_midnight_with_that_days_offset() {
        let la = jiff::tz::TimeZone::get("America/Los_Angeles").unwrap();
        // DST ends 2026-11-01 in the US.
        assert_eq!(parse_when_in("2026-10-31", &la).unwrap(), "2026-11-01T06:59:59Z");
        assert_eq!(parse_when_in("2026-11-01", &la).unwrap(), "2026-11-02T07:59:59Z");
        assert_eq!(parse_when_in("2026-11-01T12:00:00Z", &la).unwrap(), "2026-11-01T12:00:00Z");
        assert!(parse_when_in("next tuesday", &la).is_err());
    }

    #[test]
    fn schedules_are_a_day_with_an_optional_local_time() {
        let la = jiff::tz::TimeZone::get("America/Los_Angeles").unwrap();
        let today = jiff::civil::date(2026, 10, 31);
        let on = |s| parse_day_time_in(Field::Scheduled, s, today, &la);
        assert_eq!(on("today").unwrap(), json!({"scheduled_on": "2026-10-31"}));
        assert_eq!(on("tomorrow").unwrap(), json!({"scheduled_on": "2026-11-01"}));
        assert_eq!(on("2026-12-01").unwrap(), json!({"scheduled_on": "2026-12-01"}));
        let at = |day: &str, at: &str| json!({"scheduled_on": day, "scheduled_at": at, "scheduled_tz": "America/Los_Angeles"});
        assert_eq!(on("today at 1pm").unwrap(), at("2026-10-31", "2026-10-31T13:00:00-07:00"));
        assert_eq!(on("1:30 PM").unwrap(), at("2026-10-31", "2026-10-31T13:30:00-07:00"));
        // DST ends 2026-11-01 in the US, so tomorrow's offset differs.
        assert_eq!(on("tomorrow 9am").unwrap(), at("2026-11-01", "2026-11-01T09:00:00-08:00"));
        assert_eq!(on("2026-12-01 13:00").unwrap(), at("2026-12-01", "2026-12-01T13:00:00-08:00"));
        assert_eq!(on("12am").unwrap(), at("2026-10-31", "2026-10-31T00:00:00-07:00"));
        assert_eq!(on("noon").unwrap(), at("2026-10-31", "2026-10-31T12:00:00-07:00"));
        assert!(on("13pm").is_err());
        assert!(on("next tuesday").is_err());
    }

    #[test]
    fn deadlines_take_the_same_input_and_keep_a_zone_for_day_only() {
        let la = jiff::tz::TimeZone::get("America/Los_Angeles").unwrap();
        // A Saturday.
        let today = jiff::civil::date(2026, 10, 31);
        let due = |s| parse_day_time_in(Field::Due, s, today, &la).unwrap();
        let day = |d: &str| json!({"due_on": d, "due_tz": "America/Los_Angeles"});
        assert_eq!(due("fri"), day("2026-11-06"));
        assert_eq!(due("saturday"), day("2026-10-31"));
        assert_eq!(due("2026-12-01"), day("2026-12-01"));
        assert_eq!(
            due("friday 5pm"),
            json!({"due_on": "2026-11-06", "due_at": "2026-11-06T17:00:00-08:00", "due_tz": "America/Los_Angeles"})
        );
        assert_eq!(
            due("2026-11-01T12:00:00Z"),
            json!({"due_on": "2026-11-01", "due_at": "2026-11-01T04:00:00-08:00", "due_tz": "America/Los_Angeles"})
        );
    }
}
