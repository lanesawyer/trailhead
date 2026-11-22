//! A small saved map between runs: a cursor, an ETag. The caller says where the file is.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("state file {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("state file {path} is not a JSON object: {message}")]
    Corrupt { path: PathBuf, message: String },
    #[error("state `{key}`: {message}")]
    Value { key: String, message: String },
}

#[derive(Debug, Clone)]
pub struct State {
    path: PathBuf,
    map: BTreeMap<String, Value>,
}

impl State {
    /// A missing file is an empty state.
    pub fn load(path: impl Into<PathBuf>) -> Result<State, StateError> {
        let path = path.into();
        let map = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| StateError::Corrupt { path: path.clone(), message: e.to_string() })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(source) => return Err(StateError::Io { path, source }),
        };
        Ok(State { path, map })
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StateError> {
        self.map
            .get(key)
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|e| StateError::Value { key: key.to_string(), message: e.to_string() })
    }

    pub fn set<T: Serialize>(&mut self, key: &str, value: &T) -> Result<(), StateError> {
        let value = serde_json::to_value(value)
            .map_err(|e| StateError::Value { key: key.to_string(), message: e.to_string() })?;
        self.map.insert(key.to_string(), value);
        Ok(())
    }

    pub fn remove(&mut self, key: &str) {
        self.map.remove(key);
    }

    /// Writes a sibling file and renames it over the real one, so a crash leaves the old state or the new, never half.
    pub fn save(&self) -> Result<(), StateError> {
        let io = |source| StateError::Io { path: self.path.clone(), source };
        let dir = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(dir).map_err(io)?;
        let mut tmp_name = self.path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".tmp");
        let tmp = dir.join(tmp_name);
        let text = serde_json::to_string_pretty(&self.map).expect("a map of JSON values serializes");
        let mut file = fs::File::create(&tmp).map_err(io)?;
        file.write_all(text.as_bytes()).map_err(io)?;
        file.sync_all().map_err(io)?;
        fs::rename(&tmp, &self.path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/state.json");
        let mut s = State::load(&path).unwrap();
        assert_eq!(s.get::<String>("etag").unwrap(), None);
        s.set("etag", &"abc").unwrap();
        s.set("since", &42).unwrap();
        s.save().unwrap();
        assert!(!dir.path().join("sub/state.json.tmp").exists());

        let mut s = State::load(&path).unwrap();
        assert_eq!(s.get::<String>("etag").unwrap().as_deref(), Some("abc"));
        assert_eq!(s.get::<u32>("since").unwrap(), Some(42));
        assert!(s.get::<bool>("since").is_err());
        s.remove("etag");
        s.save().unwrap();
        assert_eq!(State::load(&path).unwrap().get::<String>("etag").unwrap(), None);
    }

    #[test]
    fn a_corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, "[1, 2").unwrap();
        assert!(matches!(State::load(&path), Err(StateError::Corrupt { .. })));
    }
}
