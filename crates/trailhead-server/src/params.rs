//! Parsing for query parameters and tool arguments, shared by REST and MCP.

use jiff::civil::Date;
use jiff::{Timestamp, Zoned};
use trailhead_core::{Error, Result, parse_at};

pub fn parse_time(s: &str) -> Result<Timestamp> {
    parse_at(s).map(|z| z.timestamp())
}

pub fn parse_opt_time(s: Option<&str>) -> Result<Option<Timestamp>> {
    s.map(parse_time).transpose()
}

/// A task's time, keeping the offset it was written with.
pub fn parse_opt_at(s: Option<&str>) -> Result<Option<Zoned>> {
    s.map(parse_at).transpose()
}

pub fn parse_date(s: &str) -> Result<Date> {
    s.parse().map_err(|_| Error::Invalid(format!("{s:?} is not a date like 2026-10-17")))
}

pub fn parse_opt_date(s: Option<&str>) -> Result<Option<Date>> {
    s.map(parse_date).transpose()
}

/// A comma-separated list of free-form strings, blanks dropped.
pub fn split_list(s: Option<&str>) -> Vec<String> {
    s.unwrap_or_default().split(',').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect()
}

/// A comma-separated list of values like statuses, each parsed.
pub fn parse_list<T: std::str::FromStr<Err = Error>>(s: Option<&str>) -> Result<Vec<T>> {
    split_list(s).iter().map(|p| p.parse()).collect()
}
