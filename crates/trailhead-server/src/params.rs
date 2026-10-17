//! Parsing for query parameters and tool arguments, shared by REST and MCP.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use trailhead_core::{Error, Result};

pub fn parse_time(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|_| Error::Invalid(format!("{s:?} is not an RFC 3339 timestamp")))
}

pub fn parse_opt_time(s: Option<&str>) -> Result<Option<OffsetDateTime>> {
    s.map(parse_time).transpose()
}

/// A comma-separated list of free-form strings, blanks dropped.
pub fn split_list(s: Option<&str>) -> Vec<String> {
    s.unwrap_or_default().split(',').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect()
}

/// A comma-separated list of values like statuses, each parsed.
pub fn parse_list<T: std::str::FromStr<Err = Error>>(s: Option<&str>) -> Result<Vec<T>> {
    split_list(s).iter().map(|p| p.parse()).collect()
}
