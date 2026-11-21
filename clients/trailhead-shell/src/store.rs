//! What an app keeps on this machine so that it outlives a switch to another app and a restart of the shell: the
//! view it was on, an unsent draft. It is not the app's data, which lives on the server; losing this file loses
//! nothing a person can't put back in a moment.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use bevy::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::AppId;

/// Each app's own keys and values, kept in one JSON file an app (`<dir>/<app id>.json`) and written back after a
/// frame in which one changed. Opened without a directory it holds them only while the shell runs.
#[derive(Resource, Default)]
pub struct LocalStore {
    dir: Option<PathBuf>,
    apps: HashMap<&'static str, Map<String, Value>>,
    unsaved: HashSet<&'static str>,
}

impl LocalStore {
    /// Reads what each of `apps` kept last time. A file that is missing or isn't JSON is an app with nothing kept.
    pub fn open(dir: Option<PathBuf>, apps: &[AppId]) -> Self {
        let mut store = LocalStore { dir, ..default() };
        let Some(dir) = &store.dir else { return store };
        for app in apps {
            let kept = std::fs::read_to_string(dir.join(file_name(*app)))
                .ok()
                .and_then(|text| serde_json::from_str::<Map<String, Value>>(&text).ok());
            if let Some(kept) = kept {
                store.apps.insert(app.0, kept);
            }
        }
        store
    }

    /// What `app` kept under `key`, if it is there and still reads as a `T`.
    pub fn get<T: DeserializeOwned>(&self, app: AppId, key: &str) -> Option<T> {
        let value = self.apps.get(app.0)?.get(key)?;
        serde_json::from_value(value.clone()).ok()
    }

    /// Keeps `value` for `app` under `key`. Setting what is already there writes nothing.
    pub fn set<T: Serialize>(&mut self, app: AppId, key: &str, value: T) {
        let Ok(value) = serde_json::to_value(value) else { return };
        let kept = self.apps.entry(app.0).or_default();
        if kept.get(key) != Some(&value) {
            kept.insert(key.to_string(), value);
            self.unsaved.insert(app.0);
        }
    }

    /// Forgets what `app` kept under `key`.
    pub fn remove(&mut self, app: AppId, key: &str) {
        if self.apps.get_mut(app.0).is_some_and(|kept| kept.remove(key).is_some()) {
            self.unsaved.insert(app.0);
        }
    }
}

fn file_name(app: AppId) -> String {
    format!("{}.json", app.0)
}

impl LocalStore {
    /// Writes the files of the apps that changed something. A write that fails is logged and tried again the next
    /// time that app changes something: this is a convenience, and never worth stopping the shell for.
    pub(crate) fn flush(&mut self) {
        if self.unsaved.is_empty() {
            return;
        }
        let unsaved: Vec<&'static str> = self.unsaved.drain().collect();
        let Some(dir) = self.dir.clone() else { return };
        for app in unsaved {
            let text = serde_json::to_string_pretty(&self.apps[app]).unwrap_or_default();
            let path = dir.join(file_name(AppId(app)));
            // Written beside the file and moved over it, so a crash mid-write leaves the old one whole.
            let beside = path.with_extension("json.new");
            let written = std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::write(&beside, text))
                .and_then(|()| std::fs::rename(&beside, &path));
            if let Err(e) = written {
                warn!(app, error = %e, "couldn't save what the app keeps on this machine");
            }
        }
    }
}

/// Saves what changed this frame.
pub(crate) fn save(mut store: ResMut<LocalStore>) {
    if !store.unsaved.is_empty() {
        store.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES: AppId = AppId("notes");

    fn saved(store: LocalStore) -> LocalStore {
        let mut app = App::new();
        app.insert_resource(store).add_systems(Update, save);
        app.update();
        app.world_mut().remove_resource::<LocalStore>().unwrap()
    }

    #[test]
    fn what_an_app_keeps_comes_back_when_the_store_is_opened_again() {
        let dir = tempfile::tempdir().unwrap();
        let at = || Some(dir.path().join("kept"));
        let mut store = LocalStore::open(at(), &[NOTES]);
        assert_eq!(store.get::<String>(NOTES, "draft"), None);
        store.set(NOTES, "draft", "buy rope");
        store.set(NOTES, "page", 3);
        saved(store);

        let mut again = LocalStore::open(at(), &[NOTES]);
        assert_eq!(again.get::<String>(NOTES, "draft").as_deref(), Some("buy rope"));
        assert_eq!(again.get::<u32>(NOTES, "page"), Some(3));
        assert_eq!(again.get::<u32>(NOTES, "draft"), None, "a value of another shape reads as nothing kept");
        again.remove(NOTES, "draft");
        saved(again);
        assert_eq!(LocalStore::open(at(), &[NOTES]).get::<String>(NOTES, "draft"), None);
    }

    #[test]
    fn setting_what_is_already_kept_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = LocalStore::open(Some(dir.path().to_path_buf()), &[NOTES]);
        store.set(NOTES, "page", 3);
        let mut store = saved(store);
        let file = dir.path().join("notes.json");
        std::fs::remove_file(&file).unwrap();
        store.set(NOTES, "page", 3);
        saved(store);
        assert!(!file.exists());
    }

    #[test]
    fn a_file_that_is_not_json_is_an_app_with_nothing_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.json"), "not json").unwrap();
        let store = LocalStore::open(Some(dir.path().to_path_buf()), &[NOTES]);
        assert_eq!(store.get::<u32>(NOTES, "page"), None);
    }

    #[test]
    fn without_a_directory_it_keeps_things_only_while_it_runs() {
        let mut store = LocalStore::open(None, &[NOTES]);
        store.set(NOTES, "page", 3);
        let store = saved(store);
        assert_eq!(store.get::<u32>(NOTES, "page"), Some(3));
    }
}
