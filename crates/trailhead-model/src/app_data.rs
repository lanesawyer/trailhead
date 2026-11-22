//! The app data store's shapes: an app's declared collections (its `collections.toml`), the records kept in them and
//! the events that change them, with the rules a write is checked by. The engine keeps each app's records in a file of
//! its own; nothing here needs it.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::ParseError;

/// How long an app's id may be, as the shell names apps.
pub const APP_ID_CHARS: std::ops::RangeInclusive<usize> = 2..=32;
/// The longest collection or field name.
pub const MAX_NAME: usize = 64;
/// The longest record key, in characters.
pub const MAX_KEY: usize = 512;

fn err(msg: impl Into<String>) -> ParseError {
    ParseError(msg.into())
}

/// An app's id: lowercase words of letters and digits joined by `-`, starting with a letter, as the shell makes them
/// from an app's name.
pub fn check_app_id(id: &str) -> Result<(), ParseError> {
    let words_ok =
        id.split('-').all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    if !words_ok || !id.starts_with(|c: char| c.is_ascii_lowercase()) || !APP_ID_CHARS.contains(&id.len()) {
        return Err(err(format!(
            "`{id}` isn't an app's id: lowercase words of letters and digits joined by `-`, starting with a letter, \
             {} to {} characters",
            APP_ID_CHARS.start(),
            APP_ID_CHARS.end()
        )));
    }
    Ok(())
}

/// A collection's or field's name: lowercase letters, digits and `_`, starting with a letter. `what` names it in the
/// error.
pub fn check_name(what: &str, name: &str) -> Result<(), ParseError> {
    let ok = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.len() <= MAX_NAME
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(err(format!(
            "{what} name `{name}` must be lowercase letters, digits and `_`, start with a letter, and be at most \
             {MAX_NAME} characters"
        )))
    }
}

/// A record's key: the name something outside gave it, such as `lanesawyer/trailhead#123`.
pub fn check_key(key: &str) -> Result<(), ParseError> {
    if key.is_empty() || key.chars().count() > MAX_KEY || key.chars().any(char::is_control) {
        return Err(err(format!("a record's key is 1 to {MAX_KEY} characters with no control characters")));
    }
    Ok(())
}

/// What one value of a field is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    String,
    Integer,
    Number,
    Boolean,
    /// An instant in RFC 3339, like `2026-11-22T09:00:00Z`.
    Timestamp,
    /// A calendar day, like `2026-11-22`.
    Day,
    /// An absolute URL: a scheme, `:`, and the rest, with no spaces.
    Url,
    /// Any JSON value.
    Json,
}

impl FieldKind {
    pub const ALL: [FieldKind; 8] = [
        FieldKind::String,
        FieldKind::Integer,
        FieldKind::Number,
        FieldKind::Boolean,
        FieldKind::Timestamp,
        FieldKind::Day,
        FieldKind::Url,
        FieldKind::Json,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::String => "string",
            FieldKind::Integer => "integer",
            FieldKind::Number => "number",
            FieldKind::Boolean => "boolean",
            FieldKind::Timestamp => "timestamp",
            FieldKind::Day => "day",
            FieldKind::Url => "url",
            FieldKind::Json => "json",
        }
    }

    /// What a value of this kind looks like, for an error.
    fn described(self) -> &'static str {
        match self {
            FieldKind::String => "a string",
            FieldKind::Integer => "a whole number",
            FieldKind::Number => "a number",
            FieldKind::Boolean => "true or false",
            FieldKind::Timestamp => "a timestamp in RFC 3339, like 2026-11-22T09:00:00Z",
            FieldKind::Day => "a day, like 2026-11-22",
            FieldKind::Url => "a URL, like https://example.com",
            FieldKind::Json => "any JSON value",
        }
    }

    fn accepts(self, v: &Value) -> bool {
        match self {
            FieldKind::String => v.is_string(),
            FieldKind::Integer => v.as_i64().is_some(),
            FieldKind::Number => v.is_number(),
            FieldKind::Boolean => v.is_boolean(),
            FieldKind::Timestamp => v.as_str().is_some_and(|s| s.parse::<Timestamp>().is_ok()),
            FieldKind::Day => v.as_str().is_some_and(|s| s.parse::<jiff::civil::Date>().is_ok()),
            FieldKind::Url => v.as_str().is_some_and(is_url),
            FieldKind::Json => true,
        }
    }
}

fn is_url(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else { return false };
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !rest.is_empty()
        && !s.chars().any(char::is_whitespace)
}

impl FromStr for FieldKind {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        FieldKind::ALL.into_iter().find(|k| k.as_str() == s).ok_or_else(|| {
            let all: Vec<&str> = FieldKind::ALL.iter().map(|k| k.as_str()).collect();
            err(format!("unknown kind `{s}` (expected one of: {})", all.join(", ")))
        })
    }
}

/// A field's type as written in `collections.toml`: a kind, then `[]` for a list of them, then `?` for optional, so
/// `string`, `string?`, `string[]` and `string[]?`. A list's items are never missing, so `string?[]` is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FieldType {
    pub kind: FieldKind,
    pub list: bool,
    pub optional: bool,
}

impl FieldType {
    pub fn required(kind: FieldKind) -> Self {
        FieldType { kind, list: false, optional: false }
    }

    pub fn optional(kind: FieldKind) -> Self {
        FieldType { kind, list: false, optional: true }
    }

    /// Whether `v` (not null) is a value of this type; `Err` says what it should be.
    fn check(self, field: &str, v: &Value) -> Result<(), ParseError> {
        let ok = if self.list {
            v.as_array().is_some_and(|items| items.iter().all(|i| !i.is_null() && self.kind.accepts(i)))
        } else {
            self.kind.accepts(v)
        };
        if ok {
            return Ok(());
        }
        let should = if self.list {
            format!("a list of {}", self.kind.described().trim_start_matches("a ").trim_start_matches("an "))
        } else {
            self.kind.described().to_string()
        };
        Err(err(format!("field `{field}` should be {should}, not {}", shown(v))))
    }
}

/// A value for an error message, cut short.
fn shown(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 60 { format!("{}…", s.chars().take(59).collect::<String>()) } else { s }
}

impl FromStr for FieldType {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        let t = s.trim();
        let (t, optional) = t.strip_suffix('?').map_or((t, false), |t| (t, true));
        let (t, list) = t.strip_suffix("[]").map_or((t, false), |t| (t, true));
        if let Some(kind) = t.strip_suffix('?') {
            return Err(err(format!(
                "`{s}`: write `{kind}[]?` for an optional list; a list's items are never missing"
            )));
        }
        Ok(FieldType { kind: t.parse()?, list, optional })
    }
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}{}", self.kind.as_str(), if self.list { "[]" } else { "" }, if self.optional { "?" } else { "" })
    }
}

impl TryFrom<String> for FieldType {
    type Error = ParseError;
    fn try_from(s: String) -> Result<Self, ParseError> {
        s.parse()
    }
}

impl From<FieldType> for String {
    fn from(t: FieldType) -> String {
        t.to_string()
    }
}

pub type Fields = BTreeMap<String, FieldType>;

/// One collection: its fields, and for one filled from a connector's feed, which feed (`github.pull_requests`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub fields: Fields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl Collection {
    /// Checks a record's data against the fields, naming the field that is missing, unknown or of the wrong type, and
    /// returns it with optional fields given as `null` left out, so `null` and absent are the same thing.
    pub fn check_data(&self, data: Value) -> Result<Map<String, Value>, ParseError> {
        let Value::Object(mut data) = data else {
            return Err(err(format!("a record's data is a JSON object of its fields, not {}", shown(&data))));
        };
        if let Some(unknown) = data.keys().find(|k| !self.fields.contains_key(*k)) {
            let known: Vec<&str> = self.fields.keys().map(String::as_str).collect();
            return Err(err(format!("unknown field `{unknown}` (the fields are: {})", known.join(", "))));
        }
        for (name, ty) in &self.fields {
            match data.get(name) {
                None | Some(Value::Null) if ty.optional => {
                    data.remove(name);
                }
                None | Some(Value::Null) => return Err(err(format!("field `{name}` is missing"))),
                Some(v) => ty.check(name, v)?,
            }
        }
        Ok(data)
    }
}

/// An app's collections, as its `collections.toml` declares them. A fed collection's fields are its feed's, which
/// [`Declaration::with_feeds`] fills in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub collections: BTreeMap<String, Collection>,
}

impl Declaration {
    /// Parses a `collections.toml`:
    ///
    /// ```toml
    /// [collections.snoozes]
    /// fields = { item = "string", until = "timestamp" }
    ///
    /// [collections.pull_requests]
    /// from = "github.pull_requests"
    /// ```
    pub fn parse(text: &str) -> Result<Declaration, ParseError> {
        let table: toml::Table =
            text.parse().map_err(|e: toml::de::Error| err(format!("collections.toml: {}", e.message())))?;
        if let Some(other) = table.keys().find(|k| *k != "collections") {
            return Err(err(format!("unknown table `{other}`: collections.toml holds only [collections.<name>]")));
        }
        let mut decl = Declaration::default();
        let Some(collections) = table.get("collections") else { return Ok(decl) };
        let toml::Value::Table(collections) = collections else {
            return Err(err("`collections` is a table: [collections.<name>]"));
        };
        for (name, body) in collections {
            let c = parse_collection(name, body).map_err(|e| err(format!("collection `{name}`: {e}")))?;
            decl.collections.insert(name.clone(), c);
        }
        Ok(decl)
    }

    /// Fills in each fed collection's fields from its feed's shape. `Err` names a feed `shape` doesn't know.
    pub fn with_feeds(mut self, shape: impl Fn(&str) -> Option<Fields>) -> Result<Declaration, ParseError> {
        for (name, c) in &mut self.collections {
            if let Some(feed) = &c.from {
                c.fields = shape(feed).ok_or_else(|| err(format!("collection `{name}`: no feed `{feed}` is known")))?;
            }
        }
        Ok(self)
    }

    /// Checks names and that every collection has fields to check writes by. A fed collection with none hasn't had its
    /// feed's shape filled in.
    pub fn check(&self) -> Result<(), ParseError> {
        for (name, c) in &self.collections {
            check_name("collection", name)?;
            for field in c.fields.keys() {
                check_name("field", field).map_err(|e| err(format!("collection `{name}`: {e}")))?;
            }
            if let Some(feed) = &c.from {
                check_feed(feed).map_err(|e| err(format!("collection `{name}`: {e}")))?;
                if c.fields.is_empty() {
                    return Err(err(format!("collection `{name}`: the shape of feed `{feed}` isn't known")));
                }
            } else if c.fields.is_empty() {
                return Err(err(format!("collection `{name}` has no fields")));
            }
        }
        Ok(())
    }

    /// What changed from `old` other than by addition (a new collection, or a new optional field), each as a
    /// collection of `old` and why. Such a change is only taken while that collection holds no records.
    pub fn changes_from(&self, old: &Declaration) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (name, was) in &old.collections {
            let Some(now) = self.collections.get(name) else {
                out.push((name.clone(), format!("collection `{name}` is gone")));
                continue;
            };
            if was.from != now.from {
                let said = |f: &Option<String>| f.as_ref().map_or("declared fields".into(), |f| format!("feed `{f}`"));
                out.push((
                    name.clone(),
                    format!("collection `{name}` was filled from {} and now {}", said(&was.from), said(&now.from)),
                ));
            }
            for (field, ty) in &was.fields {
                match now.fields.get(field) {
                    None => out.push((name.clone(), format!("field `{name}.{field}` is gone"))),
                    Some(t) if t != ty => {
                        out.push((name.clone(), format!("field `{name}.{field}` was `{ty}` and is now `{t}`")));
                    }
                    Some(_) => {}
                }
            }
            for (field, ty) in &now.fields {
                if !was.fields.contains_key(field) && !ty.optional {
                    out.push((name.clone(), format!("new field `{name}.{field}` isn't optional (`{ty}?`)")));
                }
            }
        }
        out
    }
}

fn check_feed(feed: &str) -> Result<(), ParseError> {
    let Some((connector, name)) = feed.split_once('.') else {
        return Err(err(format!("`from = \"{feed}\"` names a feed as <connector>.<feed>, like github.pull_requests")));
    };
    check_app_id(connector)
        .map_err(|_| err(format!("`{connector}` isn't a connector's id: lowercase words joined by `-`")))?;
    check_name("feed", name)
}

fn parse_collection(name: &str, body: &toml::Value) -> Result<Collection, ParseError> {
    check_name("collection", name)?;
    let toml::Value::Table(body) = body else {
        return Err(err("is a table with `fields` or `from`"));
    };
    if let Some(other) = body.keys().find(|k| *k != "fields" && *k != "from") {
        return Err(err(format!("unknown key `{other}` (a collection has `fields` or `from`)")));
    }
    match (body.get("fields"), body.get("from")) {
        (Some(_), Some(_)) => {
            Err(err("has both `fields` and `from`; a collection filled from a feed takes the feed's fields"))
        }
        (None, None) => Err(err("needs `fields = { name = \"kind\" }` or `from = \"<connector>.<feed>\"`")),
        (None, Some(from)) => {
            let from = from.as_str().ok_or_else(|| err("`from` is a string, like \"github.pull_requests\""))?;
            check_feed(from)?;
            Ok(Collection { fields: Fields::new(), from: Some(from.to_string()) })
        }
        (Some(fields), None) => {
            let toml::Value::Table(fields) = fields else {
                return Err(err("`fields` is a table, like { item = \"string\", until = \"timestamp?\" }"));
            };
            if fields.is_empty() {
                return Err(err("has no fields"));
            }
            let mut out = Fields::new();
            for (field, ty) in fields {
                check_name("field", field)?;
                let ty = ty
                    .as_str()
                    .ok_or_else(|| err(format!("field `{field}`: its type is a string, like \"string?\"")))?;
                out.insert(field.clone(), ty.parse().map_err(|e: ParseError| err(format!("field `{field}`: {e}")))?);
            }
            Ok(Collection { fields: out, from: None })
        }
    }
}

/// One record in an app's collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: Uuid,
    pub collection: String,
    /// The name something outside gave it, unique in its collection.
    #[serde(default)]
    pub key: Option<String>,
    pub data: Map<String, Value>,
    /// 1 when created, and one more each change.
    pub version: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// The actor that created it.
    pub created_by: Uuid,
}

/// What a write did to its record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Created,
    Changed,
    /// The record already looked like that: nothing was written, no event, same version.
    Unchanged,
    Deleted,
}

/// A record and what a write did to it; for [`Outcome::Deleted`], the record as it was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Written {
    pub outcome: Outcome,
    pub record: Record,
}

/// What replacing a collection's set did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceCounts {
    pub created: usize,
    pub changed: usize,
    pub removed: usize,
    pub unchanged: usize,
}

/// One change in an app's own log. Shaped like the task log's [`crate::Event`], with the collection as the entity and
/// the record's id as the entity id; each app's file numbers its own events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppEvent {
    pub id: i64,
    pub batch_id: Uuid,
    pub actor_id: Uuid,
    pub on_behalf_of: Uuid,
    pub collection: String,
    pub record_id: Uuid,
    /// `record.created`, `record.updated`, `record.deleted` or `record.reverted`.
    pub kind: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub undoes: Option<i64>,
    pub at: Timestamp,
}

/// What an undo or revert in one app's file did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppUndoReport {
    pub batch_id: Uuid,
    pub reverted: Vec<AppReverted>,
    pub skipped: Vec<AppSkipped>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppReverted {
    pub collection: String,
    pub record_id: Uuid,
    /// The event that recorded the revert, if anything actually changed.
    pub event_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSkipped {
    pub collection: String,
    pub record_id: Uuid,
    /// A later event by someone else that would be overwritten, if that is why it was skipped.
    pub later_event: Option<i64>,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn types_parse_and_print() {
        for s in ["string", "integer?", "timestamp[]", "url[]?", "json"] {
            assert_eq!(s.parse::<FieldType>().unwrap().to_string(), s);
        }
        let t: FieldType = "day[]?".parse().unwrap();
        assert_eq!(t, FieldType { kind: FieldKind::Day, list: true, optional: true });
        assert!("string?[]".parse::<FieldType>().unwrap_err().0.contains("string[]?"));
        assert!("time".parse::<FieldType>().unwrap_err().0.contains("unknown kind `time`"));
    }

    #[test]
    fn app_ids() {
        for ok in ["cairn", "trail-notes", "a2", "x1-y2"] {
            check_app_id(ok).unwrap();
        }
        for bad in ["", "a", "Cairn", "2go", "a--b", "a-", "-a", "a_b", &"a".repeat(33)] {
            assert!(check_app_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn data_is_checked() {
        let c = Collection {
            fields: [
                ("item".to_string(), "string".parse().unwrap()),
                ("until".to_string(), "timestamp?".parse().unwrap()),
                ("tags".to_string(), "string[]?".parse().unwrap()),
                ("n".to_string(), "integer?".parse().unwrap()),
                ("link".to_string(), "url?".parse().unwrap()),
                ("on".to_string(), "day?".parse().unwrap()),
            ]
            .into(),
            from: None,
        };
        let ok = c
            .check_data(
                json!({"item": "x", "until": null, "tags": ["a"], "n": 3, "link": "https://a.b/c", "on": "2026-11-22"}),
            )
            .unwrap();
        assert!(!ok.contains_key("until"));
        let refused = |v: Value| c.check_data(v).unwrap_err().0;
        assert!(refused(json!({})).contains("`item` is missing"));
        assert!(refused(json!({"item": null})).contains("`item` is missing"));
        assert!(refused(json!({"item": "x", "what": 1})).contains("unknown field `what`"));
        assert!(refused(json!({"item": 1})).contains("field `item` should be a string"));
        assert!(refused(json!({"item": "x", "until": "tomorrow"})).contains("field `until` should be a timestamp"));
        assert!(refused(json!({"item": "x", "tags": ["a", 1]})).contains("a list of string"));
        assert!(refused(json!({"item": "x", "n": 1.5})).contains("whole number"));
        assert!(refused(json!({"item": "x", "link": "not a url"})).contains("URL"));
        assert!(refused(json!({"item": "x", "on": "22/11/2026"})).contains("a day"));
        assert!(refused(json!(["item"])).contains("JSON object"));
    }
}
