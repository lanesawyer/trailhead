//! A feed field's type: a kind, optionally a list, optionally absent. Depends on serde alone so the engine can share it.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    String,
    Integer,
    Number,
    Boolean,
    Timestamp,
    Day,
    Url,
    Json,
}

impl FieldKind {
    const ALL: [(&'static str, FieldKind); 8] = [
        ("string", FieldKind::String),
        ("integer", FieldKind::Integer),
        ("number", FieldKind::Number),
        ("boolean", FieldKind::Boolean),
        ("timestamp", FieldKind::Timestamp),
        ("day", FieldKind::Day),
        ("url", FieldKind::Url),
        ("json", FieldKind::Json),
    ];

    fn name(self) -> &'static str {
        Self::ALL.iter().find(|(_, k)| *k == self).map_or("", |(n, _)| n)
    }

    fn check(self, value: &Value) -> Result<(), String> {
        let ok = match self {
            FieldKind::String => value.is_string(),
            FieldKind::Integer => value.is_i64() || value.is_u64(),
            FieldKind::Number => value.is_number(),
            FieldKind::Boolean => value.is_boolean(),
            FieldKind::Json => !value.is_null(),
            FieldKind::Timestamp => {
                return text(value, self).and_then(|s| {
                    if is_rfc3339(s) { Ok(()) } else { Err(format!("`{s}` is not an RFC 3339 timestamp")) }
                });
            }
            FieldKind::Day => {
                return text(value, self).and_then(|s| {
                    if is_day(s) { Ok(()) } else { Err(format!("`{s}` is not a day written YYYY-MM-DD")) }
                });
            }
            FieldKind::Url => {
                return text(value, self).and_then(|s| {
                    if is_http_url(s) { Ok(()) } else { Err(format!("`{s}` is not an http or https URL")) }
                });
            }
        };
        if ok { Ok(()) } else { Err(format!("expected {}, got {}", self.name(), describe(value))) }
    }
}

impl fmt::Display for FieldKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldType {
    pub kind: FieldKind,
    pub list: bool,
    pub optional: bool,
}

impl FieldType {
    pub fn parse(text: &str) -> Result<FieldType, String> {
        let (rest, optional) = match text.strip_suffix('?') {
            Some(rest) => (rest, true),
            None => (text, false),
        };
        let (name, list) = match rest.strip_suffix("[]") {
            Some(name) => (name, true),
            None => (rest, false),
        };
        match FieldKind::ALL.iter().find(|(n, _)| *n == name) {
            Some((_, kind)) => Ok(FieldType { kind: *kind, list, optional }),
            None => Err(format!(
                "`{text}` is not a field type: use one of {}, then `[]` for a list and `?` for optional",
                FieldKind::ALL.map(|(n, _)| n).join(", ")
            )),
        }
    }

    pub fn check(&self, value: &Value) -> Result<(), String> {
        if value.is_null() {
            return if self.optional { Ok(()) } else { Err(format!("a {self} is required, got null")) };
        }
        if !self.list {
            return self.kind.check(value);
        }
        let Some(items) = value.as_array() else {
            return Err(format!("expected a list ({self}), got {}", describe(value)));
        };
        for (i, item) in items.iter().enumerate() {
            self.kind.check(item).map_err(|e| format!("item {}: {e}", i + 1))?;
        }
        Ok(())
    }
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}{}", self.kind, if self.list { "[]" } else { "" }, if self.optional { "?" } else { "" })
    }
}

impl Serialize for FieldType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FieldType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        FieldType::parse(&text).map_err(de::Error::custom)
    }
}

fn describe(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

fn text(value: &Value, kind: FieldKind) -> Result<&str, String> {
    value.as_str().ok_or_else(|| format!("expected {kind} as a string, got {}", describe(value)))
}

fn digits(s: &[u8], from: usize, n: usize) -> Option<u32> {
    let part = s.get(from..from + n)?;
    if !part.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(part.iter().fold(0, |acc, d| acc * 10 + u32::from(d - b'0')))
}

fn is_day(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let (Some(y), Some(m), Some(d)) = (digits(b, 0, 4), digits(b, 5, 2), digits(b, 8, 2)) else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

fn is_rfc3339(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 || !is_day(&s[..10]) || !matches!(b[10], b'T' | b't') {
        return false;
    }
    let (Some(h), Some(m), Some(sec)) = (digits(b, 11, 2), digits(b, 14, 2), digits(b, 17, 2)) else {
        return false;
    };
    if b[13] != b':' || b[16] != b':' || h > 23 || m > 59 || sec > 60 {
        return false;
    }
    let mut i = 19;
    if b[i] == b'.' {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    match b.get(i) {
        Some(b'Z' | b'z') => i + 1 == b.len(),
        Some(b'+' | b'-') => {
            b.len() == i + 6
                && b[i + 3] == b':'
                && matches!((digits(b, i + 1, 2), digits(b, i + 4, 2)), (Some(h), Some(m)) if h <= 23 && m <= 59)
        }
        _ => false,
    }
}

fn is_http_url(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")) else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ty(s: &str) -> FieldType {
        FieldType::parse(s).unwrap()
    }

    #[test]
    fn parses_and_displays() {
        for s in ["string", "url", "string[]", "string?", "string[]?", "json", "day?"] {
            assert_eq!(ty(s).to_string(), s);
        }
        assert_eq!(ty("integer[]?"), FieldType { kind: FieldKind::Integer, list: true, optional: true });
        assert!(FieldType::parse("text").unwrap_err().contains("not a field type"));
        assert!(FieldType::parse("string?[]").is_err());
        assert!(FieldType::parse("").is_err());
    }

    #[test]
    fn checks_values() {
        assert!(ty("string").check(&json!("x")).is_ok());
        assert!(ty("string").check(&json!(1)).unwrap_err().contains("got a number"));
        assert!(ty("string").check(&json!(null)).is_err());
        assert!(ty("string?").check(&json!(null)).is_ok());
        assert!(ty("integer").check(&json!(3)).is_ok());
        assert!(ty("integer").check(&json!(3.5)).is_err());
        assert!(ty("number").check(&json!(3.5)).is_ok());
        assert!(ty("boolean").check(&json!(false)).is_ok());
        assert!(ty("json").check(&json!({"a": [1]})).is_ok());
        assert!(ty("string[]").check(&json!(["a", "b"])).is_ok());
        assert!(ty("string[]").check(&json!(["a", 2])).unwrap_err().starts_with("item 2"));
        assert!(ty("string[]").check(&json!("a")).is_err());
        assert!(ty("string[]?").check(&json!(null)).is_ok());
    }

    #[test]
    fn checks_timestamps_days_and_urls() {
        let ts = ty("timestamp");
        for ok in ["2026-11-22T10:00:00Z", "2026-11-22T10:00:00.123+02:00", "2026-11-22t10:00:00-07:00"] {
            assert!(ts.check(&json!(ok)).is_ok(), "{ok}");
        }
        for bad in [
            "2026-11-22",
            "2026-11-22T10:00:00",
            "2026-13-22T10:00:00Z",
            "2026-11-22T25:00:00Z",
            "yesterday",
            "2026-11-22T10:00:00.Z",
        ] {
            assert!(ts.check(&json!(bad)).is_err(), "{bad}");
        }
        let day = ty("day");
        assert!(day.check(&json!("2024-02-29")).is_ok());
        assert!(day.check(&json!("2026-02-29")).is_err());
        assert!(day.check(&json!("2026-1-2")).is_err());
        let url = ty("url");
        assert!(url.check(&json!("https://example.com/a?b=1")).is_ok());
        assert!(url.check(&json!("http://example.com")).is_ok());
        assert!(url.check(&json!("ftp://example.com")).is_err());
        assert!(url.check(&json!("https://")).is_err());
        assert!(url.check(&json!("https://a b")).is_err());
    }

    #[test]
    fn round_trips_through_serde() {
        let t: FieldType = serde_json::from_str("\"string[]?\"").unwrap();
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"string[]?\"");
        assert!(serde_json::from_str::<FieldType>("\"nope\"").is_err());
    }
}
