//! What a person set, checked against the manifest's `[settings]` and read back typed.

use serde_json::{Map, Value};

use crate::manifest::{Manifest, SettingKind, SettingSpec};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
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

#[derive(Debug, Clone)]
pub struct Settings {
    specs: std::collections::BTreeMap<String, SettingSpec>,
    values: Map<String, Value>,
}

fn matches_kind(kind: SettingKind, v: &Value) -> bool {
    match kind {
        SettingKind::String => v.is_string(),
        SettingKind::List => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
        SettingKind::Boolean => v.is_boolean(),
        SettingKind::Integer => v.is_i64() || v.is_u64(),
    }
}

impl Settings {
    /// `values` is the JSON object the server holds for the connector; `null` is the same as nothing set.
    pub fn new(manifest: &Manifest, values: Value) -> Result<Settings, SettingsError> {
        let mut values = match values {
            Value::Null => Map::new(),
            Value::Object(m) => m,
            _ => return Err(SettingsError::NotAnObject),
        };
        values.retain(|_, v| !v.is_null());
        if let Some(name) = values.keys().find(|k| !manifest.settings.contains_key(*k)) {
            return Err(SettingsError::Unknown(name.clone()));
        }
        for (name, spec) in &manifest.settings {
            match values.get(name) {
                None if spec.required => return Err(SettingsError::Missing(name.clone())),
                Some(v) if !matches_kind(spec.kind, v) => {
                    return Err(SettingsError::Mistyped { name: name.clone(), kind: spec.kind });
                }
                _ => {}
            }
        }
        Ok(Settings { specs: manifest.settings.clone(), values })
    }

    fn value(&self, name: &str, asked: SettingKind) -> Result<Option<&Value>, SettingsError> {
        let spec = self.specs.get(name).ok_or_else(|| SettingsError::Unknown(name.to_string()))?;
        if spec.kind != asked {
            return Err(SettingsError::WrongGetter { name: name.to_string(), declared: spec.kind, asked });
        }
        Ok(self.values.get(name))
    }

    /// `None` only for an optional setting nobody set; a required one is always there.
    pub fn string(&self, name: &str) -> Result<Option<&str>, SettingsError> {
        Ok(self.value(name, SettingKind::String)?.and_then(Value::as_str))
    }

    pub fn list(&self, name: &str) -> Result<Option<Vec<&str>>, SettingsError> {
        Ok(self
            .value(name, SettingKind::List)?
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect()))
    }

    pub fn boolean(&self, name: &str) -> Result<Option<bool>, SettingsError> {
        Ok(self.value(name, SettingKind::Boolean)?.and_then(Value::as_bool))
    }

    pub fn integer(&self, name: &str) -> Result<Option<i64>, SettingsError> {
        Ok(self.value(name, SettingKind::Integer)?.and_then(Value::as_i64))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn manifest() -> Manifest {
        let text = crate::manifest::tests::GITHUB.replace(
            "[feeds.pull_requests]",
            "[settings.site]\nkind = \"string\"\n[settings.limit]\nkind = \"integer\"\n[settings.mine]\nkind = \"boolean\"\n\n[feeds.pull_requests]",
        );
        Manifest::parse(&text).unwrap()
    }

    #[test]
    fn typed_getters() {
        let s = Settings::new(&manifest(), json!({"repos": ["a/b", "c/d"], "site": "x", "limit": 5, "mine": true}))
            .unwrap();
        assert_eq!(s.list("repos").unwrap().unwrap(), ["a/b", "c/d"]);
        assert_eq!(s.string("site").unwrap(), Some("x"));
        assert_eq!(s.integer("limit").unwrap(), Some(5));
        assert_eq!(s.boolean("mine").unwrap(), Some(true));
        assert!(matches!(s.string("limit"), Err(SettingsError::WrongGetter { .. })));
        assert_eq!(s.string("nope"), Err(SettingsError::Unknown("nope".into())));
    }

    #[test]
    fn optional_settings_may_be_absent_or_null() {
        let s = Settings::new(&manifest(), json!({"repos": [], "site": null})).unwrap();
        assert_eq!(s.string("site").unwrap(), None);
        assert_eq!(s.integer("limit").unwrap(), None);
    }

    #[test]
    fn refuses_missing_unknown_and_mistyped() {
        let m = manifest();
        assert_eq!(Settings::new(&m, json!({})).unwrap_err(), SettingsError::Missing("repos".into()));
        assert_eq!(Settings::new(&m, Value::Null).unwrap_err(), SettingsError::Missing("repos".into()));
        assert_eq!(Settings::new(&m, json!([])).unwrap_err(), SettingsError::NotAnObject);
        assert_eq!(Settings::new(&m, json!({"repos": [], "x": 1})).unwrap_err(), SettingsError::Unknown("x".into()));
        assert_eq!(
            Settings::new(&m, json!({"repos": "a/b"})).unwrap_err(),
            SettingsError::Mistyped { name: "repos".into(), kind: SettingKind::List }
        );
        assert!(Settings::new(&m, json!({"repos": [1]})).is_err());
        assert!(Settings::new(&m, json!({"repos": [], "limit": 1.5})).is_err());
    }
}
