//! Connectors (`docs/app-platform.md`, "Connectors"): `connector.toml`, what a connector is, what it may reach, what
//! it takes and what it hands over; the checks of a feed's set and of a connector's settings, which the connector kit
//! and the server both make; and how the server shows a connector and its runs.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::app_data::{FieldType, check_app_id};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// The agent a connector runs as: `connector/<id>`, made by the server with no parent agent.
pub fn agent_name(id: &str) -> String {
    format!("{AGENT_PREFIX}{id}")
}

/// What every connector's agent's name starts with.
pub const AGENT_PREFIX: &str = "connector/";

/// The notification the server raises when a connector's runs keep failing, about `connector/<id>`.
pub const FAILING: &str = "connector.failing";

/// The environment variable a connector reads its secret `name` from.
pub fn secret_var(name: &str) -> String {
    format!("TRAILHEAD_CONNECTOR_SECRET_{}", name.to_ascii_uppercase())
}

/// The name a connector's secret is saved under in the server's credential store (`trailhead connector login`).
pub fn secret_cred(id: &str, name: &str) -> String {
    format!("connector.{id}.{name}")
}

/// The variable in the server's own environment that, when set, gives a connector's secret in place of the one saved:
/// `TRAILHEAD_CONNECTOR_GITHUB_TOKEN` for `github`'s `token`.
pub fn secret_override_var(id: &str, name: &str) -> String {
    format!("TRAILHEAD_CONNECTOR_{}_{}", id.replace('-', "_"), name).to_ascii_uppercase()
}

/// What the server gives a connector's run, besides one [`secret_var`] per secret.
pub mod env {
    pub const URL: &str = "TRAILHEAD_URL";
    pub const TOKEN: &str = "TRAILHEAD_TOKEN";
    /// The settings as a JSON object.
    pub const SETTINGS: &str = "TRAILHEAD_CONNECTOR_SETTINGS";
    /// The file the connector's saved state is kept in.
    pub const STATE: &str = "TRAILHEAD_CONNECTOR_STATE";
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("connector.toml: {0}")]
pub struct ManifestError(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum SettingKind {
    String,
    List,
    Boolean,
    Integer,
}

impl std::fmt::Display for SettingKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SettingKind::String => "string",
            SettingKind::List => "list",
            SettingKind::Boolean => "boolean",
            SettingKind::Integer => "integer",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingSpec {
    pub kind: SettingKind,
    pub label: String,
    pub help: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedSpec {
    pub fields: BTreeMap<String, FieldType>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub every: Duration,
    pub timeout: Duration,
    pub hosts: Vec<String>,
    pub secrets: Vec<String>,
    pub settings: BTreeMap<String, SettingSpec>,
    pub feeds: BTreeMap<String, FeedSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    id: String,
    name: String,
    every: String,
    timeout: Option<String>,
    hosts: Vec<String>,
    #[serde(default)]
    secrets: Vec<String>,
    #[serde(default)]
    settings: BTreeMap<String, RawSetting>,
    #[serde(default)]
    feeds: BTreeMap<String, RawFeed>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSetting {
    kind: SettingKind,
    label: Option<String>,
    help: Option<String>,
    #[serde(default)]
    required: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFeed {
    fields: BTreeMap<String, FieldType>,
}

fn err<T>(msg: impl Into<String>) -> Result<T, ManifestError> {
    Err(ManifestError(msg.into()))
}

fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `30s`, `5m` or `2h`: a whole number and a unit, nothing else.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let bad = || format!("`{text}` is not a duration: write a whole number and s, m or h, like `30s`, `5m` or `2h`");
    let split = text.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?;
    let n: u64 = text[..split].parse().map_err(|_| bad())?;
    let secs = match &text[split..] {
        "s" => n,
        "m" => n.checked_mul(60).ok_or_else(bad)?,
        "h" => n.checked_mul(3600).ok_or_else(bad)?,
        _ => return Err(bad()),
    };
    if secs == 0 {
        return Err(format!("`{text}` is zero: a duration must be longer than that"));
    }
    Ok(Duration::from_secs(secs))
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let raw: Raw = toml::from_str(text).map_err(|e| ManifestError(e.message().to_string()))?;
        if check_app_id(&raw.id).is_err() {
            return err(format!(
                "id `{}` must be lowercase words of letters and digits joined by `-`, starting with a letter, 2 to 32 \
                 characters",
                raw.id
            ));
        }
        if raw.name.trim().is_empty() {
            return err("name is empty");
        }
        let every = parse_duration(&raw.every).or_else(|e| err(format!("every: {e}")))?;
        let timeout = match &raw.timeout {
            Some(t) => parse_duration(t).or_else(|e| err(format!("timeout: {e}")))?,
            None => DEFAULT_TIMEOUT,
        };
        if raw.hosts.is_empty() {
            return err("hosts is empty: a connector has to declare the hosts it reaches");
        }
        for host in &raw.hosts {
            if !valid_host(host) {
                return err(format!(
                    "host `{host}` must be a bare lowercase host name like `api.github.com`, with no scheme, port, path or wildcard"
                ));
            }
        }
        for secret in &raw.secrets {
            if !valid_name(secret) {
                return err(format!(
                    "secret `{secret}` must be lowercase letters, digits or `_`, starting with a letter"
                ));
            }
        }
        let mut settings = BTreeMap::new();
        for (name, s) in raw.settings {
            if !valid_name(&name) {
                return err(format!(
                    "setting `{name}` must be lowercase letters, digits or `_`, starting with a letter"
                ));
            }
            let label = s.label.unwrap_or_else(|| name.clone());
            settings.insert(name, SettingSpec { kind: s.kind, label, help: s.help, required: s.required });
        }
        if raw.feeds.is_empty() {
            return err("there are no feeds: declare at least one under [feeds.<name>]");
        }
        let mut feeds = BTreeMap::new();
        for (name, feed) in raw.feeds {
            if !valid_name(&name) {
                return err(format!("feed `{name}` must be lowercase letters, digits or `_`, starting with a letter"));
            }
            if feed.fields.is_empty() {
                return err(format!("feed `{name}` has no fields"));
            }
            if let Some(field) = feed.fields.keys().find(|f| !valid_name(f)) {
                return err(format!(
                    "field `{field}` of feed `{name}` must be lowercase letters, digits or `_`, starting with a letter"
                ));
            }
            feeds.insert(name, FeedSpec { fields: feed.fields });
        }
        Ok(Manifest {
            id: raw.id,
            name: raw.name,
            every,
            timeout,
            hosts: raw.hosts,
            secrets: raw.secrets,
            settings,
            feeds,
        })
    }

    pub fn check_feed(&self, feed: &str, records: &[FeedRecord]) -> Result<(), FeedError> {
        self.feeds.get(feed).ok_or_else(|| FeedError::UnknownFeed(feed.to_string()))?.check(feed, records)
    }

    /// Checks what a person set against `[settings]`: each a declared setting of its kind. `null` is the same as not
    /// set, and is left out of what comes back. A required setting left unset is not an error here, since a person
    /// sets them one at a time; [`Manifest::missing_settings`] says which.
    pub fn check_settings(&self, values: Value) -> Result<Map<String, Value>, SettingsError> {
        let mut values = match values {
            Value::Null => Map::new(),
            Value::Object(m) => m,
            _ => return Err(SettingsError::NotAnObject),
        };
        values.retain(|_, v| !v.is_null());
        if let Some(name) = values.keys().find(|k| !self.settings.contains_key(*k)) {
            return Err(SettingsError::Unknown(name.clone()));
        }
        for (name, spec) in &self.settings {
            if let Some(v) = values.get(name)
                && !spec.kind.matches(v)
            {
                return Err(SettingsError::Mistyped { name: name.clone(), kind: spec.kind });
            }
        }
        Ok(values)
    }

    /// The required settings `values` leaves unset, in name order.
    pub fn missing_settings(&self, values: &Map<String, Value>) -> Vec<String> {
        self.settings
            .iter()
            .filter(|(name, spec)| spec.required && values.get(*name).is_none_or(Value::is_null))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

impl SettingKind {
    pub fn matches(self, v: &Value) -> bool {
        match self {
            SettingKind::String => v.is_string(),
            SettingKind::List => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
            SettingKind::Boolean => v.is_boolean(),
            SettingKind::Integer => v.is_i64() || v.is_u64(),
        }
    }

    /// A value of this kind from what a person typed: a list from each word given (or from one JSON array), a
    /// boolean from `true`/`false`, `yes`/`no` or `on`/`off`, an integer from its digits.
    pub fn from_words(self, words: &[String]) -> Result<Value, String> {
        let one = || match words {
            [w] => Ok(w.as_str()),
            _ => Err(format!("a {self} takes one value, not {}", words.len())),
        };
        match self {
            SettingKind::String => Ok(Value::String(words.join(" "))),
            SettingKind::List => match words {
                [w] if w.trim_start().starts_with('[') => {
                    let v: Value = serde_json::from_str(w).map_err(|e| format!("not a JSON list: {e}"))?;
                    if SettingKind::List.matches(&v) { Ok(v) } else { Err("a list holds strings only".into()) }
                }
                _ => Ok(Value::Array(words.iter().cloned().map(Value::String).collect())),
            },
            SettingKind::Boolean => match one()?.to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" => Ok(Value::Bool(true)),
                "false" | "no" | "off" => Ok(Value::Bool(false)),
                other => Err(format!("`{other}` isn't true or false")),
            },
            SettingKind::Integer => {
                let w = one()?;
                w.parse::<i64>().map(Value::from).map_err(|_| format!("`{w}` isn't a whole number"))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("settings must be a JSON object")]
    NotAnObject,
    #[error("setting `{0}` is required")]
    Missing(String),
    #[error("`{0}` is not a setting this connector declares")]
    Unknown(String),
    #[error("setting `{name}` must be a {kind}")]
    Mistyped { name: String, kind: SettingKind },
    #[error("setting `{name}` is a {declared}, not a {asked}")]
    WrongGetter { name: String, declared: SettingKind, asked: SettingKind },
}

/// One record of a feed's set: its key, unique in the set, and its fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FeedRecord {
    pub key: String,
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub data: Map<String, Value>,
}

impl FeedRecord {
    pub fn new(key: impl Into<String>, data: Map<String, Value>) -> FeedRecord {
        FeedRecord { key: key.into(), data }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FeedError {
    #[error("the connector declares no feed `{0}`")]
    UnknownFeed(String),
    #[error("feed `{feed}`: a record has an empty key")]
    EmptyKey { feed: String },
    #[error("feed `{feed}`: key `{key}` appears more than once")]
    DuplicateKey { feed: String, key: String },
    #[error("feed `{feed}`, record `{key}`: missing field `{field}`")]
    Missing { feed: String, key: String, field: String },
    #[error("feed `{feed}`, record `{key}`: `{field}` is not a declared field")]
    Unknown { feed: String, key: String, field: String },
    #[error("feed `{feed}`, record `{key}`, field `{field}`: {problem}")]
    Mistyped { feed: String, key: String, field: String, problem: String },
}

impl FeedSpec {
    pub fn check(&self, feed: &str, records: &[FeedRecord]) -> Result<(), FeedError> {
        let mut seen = std::collections::BTreeSet::new();
        for record in records {
            let at = |field: &str| (feed.to_string(), record.key.clone(), field.to_string());
            if record.key.is_empty() {
                return Err(FeedError::EmptyKey { feed: feed.to_string() });
            }
            if !seen.insert(record.key.as_str()) {
                return Err(FeedError::DuplicateKey { feed: feed.to_string(), key: record.key.clone() });
            }
            if let Some(field) = record.data.keys().find(|f| !self.fields.contains_key(*f)) {
                let (feed, key, field) = at(field);
                return Err(FeedError::Unknown { feed, key, field });
            }
            for (field, ty) in &self.fields {
                match record.data.get(field) {
                    None | Some(Value::Null) if ty.optional => {}
                    None | Some(Value::Null) => {
                        let (feed, key, field) = at(field);
                        return Err(FeedError::Missing { feed, key, field });
                    }
                    Some(value) => {
                        if let Err(problem) = ty.check(value).map_err(|e| e.0) {
                            let (feed, key, field) = at(field);
                            return Err(FeedError::Mistyped { feed, key, field, problem });
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// `PUT /v1/connectors/{id}/feeds/{feed}`: the feed's whole current set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FeedSet {
    pub records: Vec<FeedRecord>,
}

/// What handing a feed over did: the set, written into each app collection filled from the feed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HandedOver {
    pub feed: String,
    /// How many records the set held.
    pub records: usize,
    /// Each app collection filled from the feed, in app order. One that failed says why and stopped none of the others.
    pub apps: Vec<FedCollection>,
}

/// One app collection a feed's set was written into.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FedCollection {
    pub app: String,
    pub collection: String,
    pub created: usize,
    pub changed: usize,
    pub removed: usize,
    pub unchanged: usize,
    /// Why writing into this app failed; the counts are then zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One run of a connector, as the server keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorRun {
    pub started_at: Timestamp,
    pub duration_ms: u64,
    pub ok: bool,
    /// Why it failed: the end of what it wrote to stderr, or what the server saw (a time limit, a start that failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What each feed handed over in the run did, by feed.
    #[serde(default)]
    pub feeds: BTreeMap<String, FeedRun>,
}

/// What one feed's handovers in a run did, added up over the apps it fills.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FeedRun {
    pub records: usize,
    pub created: usize,
    pub changed: usize,
    pub removed: usize,
    pub unchanged: usize,
    /// Each app that refused the set, as `<app>: <why>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

impl FeedRun {
    pub fn add(&mut self, handed: &HandedOver) {
        self.records = handed.records;
        for app in &handed.apps {
            self.created += app.created;
            self.changed += app.changed;
            self.removed += app.removed;
            self.unchanged += app.unchanged;
            if let Some(e) = &app.error {
                self.errors.push(format!("{}: {e}", app.app));
            }
        }
    }
}

/// A connector as `GET /v1/connectors` shows it: what its manifest declares, how a person set it, and its runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorInfo {
    pub id: String,
    /// Its `name`, or its id when its manifest is broken.
    pub name: String,
    /// Why its `connector.toml` couldn't be read; nothing else is known of it then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How often it runs, in seconds.
    pub every_secs: u64,
    /// How long a run may take, in seconds.
    pub timeout_secs: u64,
    pub hosts: Vec<String>,
    pub settings: Vec<SettingInfo>,
    pub secrets: Vec<SecretInfo>,
    pub feeds: Vec<FeedInfo>,
    pub enabled: bool,
    /// What it still needs before it can run, in plain words; empty when it can.
    pub missing: Vec<String>,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<ConnectorRun>,
    /// When it runs next, if it is on, can run and the server schedules runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SettingInfo {
    pub name: String,
    pub kind: SettingKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    pub required: bool,
    /// As set; absent when not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// A secret by name, and whether it is set. Its value never leaves the server's machine but to the connector's run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SecretInfo {
    pub name: String,
    pub set: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FeedInfo {
    pub name: String,
    /// Each field's type, as `collections.toml` writes them.
    #[cfg_attr(feature = "openapi", schema(value_type = BTreeMap<String, String>))]
    pub fields: BTreeMap<String, FieldType>,
    /// The app collections filled from it, as `<app>.<collection>`.
    pub collections: Vec<String>,
}

/// `PATCH /v1/connectors/{id}`: turn it on or off, and change its settings. `settings` is merged into what is set; a
/// setting given as `null` is unset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConnectorPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub settings: Option<Map<String, Value>>,
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;
    use crate::app_data::FieldKind;

    pub(crate) const GITHUB: &str = r#"
id = "github"
name = "GitHub"
every = "5m"
hosts = ["api.github.com"]
secrets = ["token"]

[settings.repos]
kind = "list"
label = "Repositories"
help = "owner/name, one per line"
required = true

[feeds.pull_requests]
fields = { title = "string", url = "url", author = "string", draft = "boolean", updated = "timestamp", reviewers = "string[]", ticket = "string?" }
"#;

    #[test]
    fn parses_the_example() {
        let m = Manifest::parse(GITHUB).unwrap();
        assert_eq!(m.id, "github");
        assert_eq!(m.every, Duration::from_secs(300));
        assert_eq!(m.timeout, DEFAULT_TIMEOUT);
        assert_eq!(m.secrets, ["token"]);
        let repos = &m.settings["repos"];
        assert_eq!(repos.kind, SettingKind::List);
        assert!(repos.required);
        assert_eq!(repos.help.as_deref(), Some("owner/name, one per line"));
        let fields = &m.feeds["pull_requests"].fields;
        assert_eq!(fields.len(), 7);
        assert_eq!(fields["reviewers"].kind, FieldKind::String);
        assert!(fields["reviewers"].list && fields["ticket"].optional);
    }

    fn fails(text: &str, needle: &str) {
        let e = Manifest::parse(text).unwrap_err().to_string();
        assert!(e.contains(needle), "`{e}` should mention `{needle}`");
    }

    #[test]
    fn refuses_what_is_wrong_in_plain_words() {
        fails(&GITHUB.replace("every = \"5m\"", "every = \"5 minutes\""), "every: `5 minutes` is not a duration");
        fails(&GITHUB.replace("every = \"5m\"", "every = \"0s\""), "is zero");
        fails(&GITHUB.replace("every = \"5m\"", "every = \"5m\"\ntimeout = \"soon\""), "timeout:");
        fails(&GITHUB.replace("api.github.com", "https://api.github.com"), "bare lowercase host");
        fails(&GITHUB.replace("api.github.com", "*.github.com"), "bare lowercase host");
        fails(&GITHUB.replace("api.github.com", "api.github.com:443"), "bare lowercase host");
        fails(&GITHUB.replace("id = \"github\"", "id = \"GitHub\""), "id `GitHub`");
        fails(&GITHUB.replace("id = \"github\"", "id = \"git_hub\""), "id `git_hub`");
        fails(&GITHUB.replace("pull_requests", "pull-requests"), "feed `pull-requests`");
        fails(&GITHUB.replace("title =", "Title ="), "field `Title`");
        fails(&GITHUB.replace("\"timestamp\"", "\"date\""), "`date` isn't a field type");
        fails(&GITHUB.replace("secrets = [\"token\"]", "secrets = [\"Token\"]"), "secret `Token`");
        fails(&GITHUB.replace("kind = \"list\"", "kind = \"set\""), "set");
        fails(&GITHUB.replace("hosts = [\"api.github.com\"]", "hosts = []"), "hosts is empty");
        fails(
            &GITHUB.replace("[feeds.pull_requests]", "[feeds.x]\nfields = {}\n[feeds.pull_requests]"),
            "has no fields",
        );
    }

    #[test]
    fn refuses_unknown_and_missing_keys() {
        fails(&format!("{GITHUB}\nretries = 3\n"), "unknown field `retries`");
        fails(&GITHUB.replace("required = true", "required = true\nrequried = true"), "unknown field `requried`");
        fails(&GITHUB.replace("name = \"GitHub\"\n", ""), "missing field `name`");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("m").is_err());
        assert!(parse_duration("5d").is_err());
        assert!(parse_duration("-5m").is_err());
    }

    fn record(key: &str, data: Value) -> FeedRecord {
        FeedRecord::new(key, data.as_object().unwrap().clone())
    }

    fn good() -> Value {
        json!({"title": "t", "url": "https://github.com/a/b/pull/1", "author": "lane", "draft": false,
               "updated": "2026-11-22T10:00:00Z", "reviewers": ["x"]})
    }

    fn check(records: &[FeedRecord]) -> Result<(), FeedError> {
        Manifest::parse(GITHUB).unwrap().check_feed("pull_requests", records)
    }

    #[test]
    fn a_feed_accepts_a_good_set_with_optional_fields_absent_or_null() {
        let mut with_null = good();
        with_null["ticket"] = Value::Null;
        assert_eq!(check(&[record("1", good()), record("2", with_null)]), Ok(()));
        assert_eq!(check(&[]), Ok(()));
    }

    #[test]
    fn a_feed_check_names_the_feed_key_and_field() {
        let mut missing = good();
        missing.as_object_mut().unwrap().remove("title");
        let e = check(&[record("7", missing)]).unwrap_err();
        assert_eq!(e.to_string(), "feed `pull_requests`, record `7`: missing field `title`");

        let mut extra = good();
        extra["stars"] = json!(1);
        let e = check(&[record("7", extra)]).unwrap_err();
        assert_eq!(e.to_string(), "feed `pull_requests`, record `7`: `stars` is not a declared field");

        let mut wrong = good();
        wrong["draft"] = json!("no");
        let e = check(&[record("7", wrong)]).unwrap_err().to_string();
        assert!(e.starts_with("feed `pull_requests`, record `7`, field `draft`: should be true or false"), "{e}");
    }

    #[test]
    fn a_feed_refuses_empty_and_duplicate_keys_and_unknown_feeds() {
        assert_eq!(check(&[record("", good())]), Err(FeedError::EmptyKey { feed: "pull_requests".into() }));
        assert_eq!(
            check(&[record("1", good()), record("1", good())]),
            Err(FeedError::DuplicateKey { feed: "pull_requests".into(), key: "1".into() })
        );
        let m = Manifest::parse(GITHUB).unwrap();
        assert_eq!(m.check_feed("issues", &[]), Err(FeedError::UnknownFeed("issues".into())));
    }

    fn with_more_settings() -> Manifest {
        let text = GITHUB.replace(
            "[feeds.pull_requests]",
            "[settings.site]\nkind = \"string\"\n[settings.limit]\nkind = \"integer\"\n[settings.mine]\nkind = \"boolean\"\n\n[feeds.pull_requests]",
        );
        Manifest::parse(&text).unwrap()
    }

    #[test]
    fn settings_are_checked_by_kind_and_required_ones_are_reported_missing() {
        let m = with_more_settings();
        let set = m.check_settings(json!({"site": "x", "limit": null})).unwrap();
        assert_eq!(Value::Object(set.clone()), json!({"site": "x"}));
        assert_eq!(m.missing_settings(&set), ["repos"]);
        assert_eq!(m.check_settings(json!([])).unwrap_err(), SettingsError::NotAnObject);
        assert_eq!(m.check_settings(json!({"x": 1})).unwrap_err(), SettingsError::Unknown("x".into()));
        assert_eq!(
            m.check_settings(json!({"repos": "a/b"})).unwrap_err(),
            SettingsError::Mistyped { name: "repos".into(), kind: SettingKind::List }
        );
        assert!(m.check_settings(json!({"repos": [1]})).is_err());
        assert!(m.check_settings(json!({"limit": 1.5})).is_err());
        assert!(m.missing_settings(&m.check_settings(json!({"repos": []})).unwrap()).is_empty());
    }

    #[test]
    fn a_setting_is_read_from_words_by_its_kind() {
        let words = |w: &[&str]| w.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(SettingKind::List.from_words(&words(&["a/b", "c/d"])).unwrap(), json!(["a/b", "c/d"]));
        assert_eq!(SettingKind::List.from_words(&words(&["[\"a b\"]"])).unwrap(), json!(["a b"]));
        assert_eq!(SettingKind::String.from_words(&words(&["two", "words"])).unwrap(), json!("two words"));
        assert_eq!(SettingKind::Boolean.from_words(&words(&["Yes"])).unwrap(), json!(true));
        assert!(SettingKind::Boolean.from_words(&words(&["maybe"])).is_err());
        assert_eq!(SettingKind::Integer.from_words(&words(&["42"])).unwrap(), json!(42));
        assert!(SettingKind::Integer.from_words(&words(&["4", "2"])).is_err());
    }
}
