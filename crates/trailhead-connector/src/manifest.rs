//! `connector.toml`: what a connector is, what it may reach, what it takes and what it hands over.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;

use crate::field::FieldType;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
#[error("connector.toml: {0}")]
pub struct ManifestError(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
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

pub(crate) fn valid_name(s: &str, allow_dash: bool) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || (allow_dash && c == '-'))
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
        if !valid_name(&raw.id, true) {
            return err(format!(
                "id `{}` must be lowercase letters, digits, `-` or `_`, starting with a letter",
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
            if !valid_name(secret, false) {
                return err(format!(
                    "secret `{secret}` must be lowercase letters, digits or `_`, starting with a letter"
                ));
            }
        }
        let mut settings = BTreeMap::new();
        for (name, s) in raw.settings {
            if !valid_name(&name, false) {
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
            if !valid_name(&name, false) {
                return err(format!("feed `{name}` must be lowercase letters, digits or `_`, starting with a letter"));
            }
            if feed.fields.is_empty() {
                return err(format!("feed `{name}` has no fields"));
            }
            if let Some(field) = feed.fields.keys().find(|f| !valid_name(f, false)) {
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
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::field::FieldKind;

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
        fails(&GITHUB.replace("pull_requests", "pull-requests"), "feed `pull-requests`");
        fails(&GITHUB.replace("title =", "Title ="), "field `Title`");
        fails(&GITHUB.replace("\"timestamp\"", "\"date\""), "not a field type");
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
}
