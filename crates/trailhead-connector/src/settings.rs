//! What a person set, checked against the manifest's `[settings]` and read back typed.

use serde_json::{Map, Value};

use crate::{Manifest, SettingKind, SettingSpec, SettingsError};

#[derive(Debug, Clone)]
pub struct Settings {
    specs: std::collections::BTreeMap<String, SettingSpec>,
    values: Map<String, Value>,
}

impl Settings {
    /// `values` is the JSON object the server holds for the connector; `null` is the same as nothing set. Every
    /// required setting has to be set.
    pub fn new(manifest: &Manifest, values: Value) -> Result<Settings, SettingsError> {
        let values = manifest.check_settings(values)?;
        if let Some(name) = manifest.missing_settings(&values).into_iter().next() {
            return Err(SettingsError::Missing(name));
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
        let text = crate::fixtures::GITHUB.replace(
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
