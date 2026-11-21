//! The server's settings in the per-machine user config (`<config dir>/config.toml`), as `GET /v1/config` shows
//! them and `PATCH /v1/config` changes them: `[factory]` and `[shell]`. The server reads that file when it starts, so
//! a change waits for a restart.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

/// `GET /v1/config`: the settings in the user config, read with the parsers the server starts with, and how they
/// differ from what the server is running. Only the typed settings, never the file's text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ServerConfig {
    /// The user config file.
    pub path: String,
    /// The file's content as it was read, hashed (empty text for a file that isn't there): a change names it, and is
    /// refused when the file has changed on disk since.
    pub version: String,
    /// `[factory]`, its defaults filled in. `None` when the table doesn't load, or in a server built without the
    /// factory (`factory_built`).
    pub factory: Option<FactorySettings>,
    /// Whether this server was built with the factory. Without it, `[factory]` is neither shown nor changed.
    pub factory_built: bool,
    /// `[shell]`, its defaults filled in; `repo: None` means there is no builder. `None` when the table doesn't load.
    pub shell: Option<ShellSettings>,
    /// What the server would say about the file as it stands if it started now: why the factory wouldn't start (the
    /// server wouldn't either), or why the builder would be off.
    pub problem: Option<String>,
    /// What the server started with, of the sections that differ from the file now.
    pub running: RunningSettings,
    /// Whether the file says something the running server doesn't do: `waiting` names what.
    pub restart_needed: bool,
    /// The settings that wait for a restart, as `section.key` (`shell.jobs`).
    #[serde(default)]
    pub waiting: Vec<String>,
    /// Whether the server runs at a terminal (`trailhead serve`, `trailhead factory run`), where it is restarted by
    /// stopping it there and running it again; otherwise `trailhead server restart` restarts it.
    pub foreground: bool,
}

/// `[factory]`: the repos the factory runs and how it shares the machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FactorySettings {
    /// Directories holding a `.trailhead.toml` with a `[factory]` table. A leading `~` is the home directory.
    pub repos: Vec<String>,
    /// The factory runs nothing, whatever `repos` lists.
    pub disabled: bool,
    /// Usage windows (`five_hour`, `seven_day`) and the percent of each at which the factory starts no new runs.
    pub pause_at: BTreeMap<String, u32>,
    /// Sandboxes at once across all repos; `None` is no cap of the host's own.
    pub max_sandboxes: Option<usize>,
    /// The keys the file doesn't set, which are their defaults.
    #[serde(default)]
    pub defaults: Vec<String>,
}

/// `[shell]`: the apps repo the server builds the window from, and how.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ShellSettings {
    /// The apps repo's checkout; `None` is no builder. A leading `~` is the home directory.
    pub repo: Option<String>,
    /// The cargo package to build; `None` is the package with a binary named `trailhead-shell`.
    pub package: Option<String>,
    /// The cargo profile.
    pub profile: String,
    /// Cargo's `-j`.
    pub jobs: u32,
    /// How often, in seconds, the builder looks for a new commit.
    pub poll_secs: u64,
    /// The keys the file doesn't set, which are their defaults.
    #[serde(default)]
    pub defaults: Vec<String>,
}

/// What the running server started with, of the sections that differ from the file now; a section that is the same
/// is left out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunningSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factory: Option<FactorySettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellSettings>,
}

/// `PATCH /v1/config`: a change to some of the settings. A key left out is left as it is; for the keys that take one,
/// `null` takes the key out of the file, back to its default. Nothing else in the file is touched: its comments, its
/// order and the keys this doesn't name stay as they were.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConfigChange {
    /// [`ServerConfig::version`] as last read: the change is refused (`412`) when the file has changed since.
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub factory: Option<FactoryChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellChange>,
}

/// A change to `[factory]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FactoryChange {
    /// The whole list, in its order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// Every window's percent: a window left out has no pause, and an empty map takes `pause_at` out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_at: Option<BTreeMap<String, u32>>,
}

/// A change to `[shell]`; `null` takes a key out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ShellChange {
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub repo: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub package: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub profile: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<u32>))]
    pub jobs: Option<Option<u32>>,
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<u64>))]
    pub poll_secs: Option<Option<u64>>,
}

/// Tells an absent key (`None`) from `null` (`Some(None)`).
fn nullable<'de, T: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_takes_a_key_out_and_an_absent_one_is_left() {
        let change: ShellChange = serde_json::from_str(r#"{"repo": null, "jobs": 2}"#).unwrap();
        assert_eq!((change.repo, change.jobs, change.profile), (Some(None), Some(Some(2)), None));
        let back = serde_json::to_value(ShellChange { repo: Some(None), ..Default::default() }).unwrap();
        assert_eq!(back, serde_json::json!({"repo": null}));
    }
}
