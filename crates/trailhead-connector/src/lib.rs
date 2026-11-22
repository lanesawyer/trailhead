//! The kit a Trailhead connector is written against.
//!
//! A connector is a small program under `connectors/<id>/` in someone's apps repo that fetches from one outside
//! service and hands the server feeds of records (`docs/app-platform.md`, "The app data store and connectors"; the
//! guide is `docs/connectors.md`). The server builds it, runs it on its schedule as the agent `connector/<id>`, and
//! writes each feed it hands over into every app collection filled from it. This crate is what it is built from, so a
//! connector is only "fetch and map":
//!
//! - [`run`], the whole of a connector's `main`: it reads what the server gave the run, calls the connector's sync
//!   with a [`Cx`], saves the state if the sync succeeded, and exits, saying why on stderr when it failed.
//! - [`Cx`]: [`Cx::settings`], [`Cx::secrets`], [`Cx::http`], [`Cx::state`], and [`Cx::hand_over`], which checks a
//!   feed's set against the manifest and sends it to the server, the only place it may send anything.
//! - [`Manifest`], the parsed `connector.toml`: id, schedule, the hosts it may reach, its settings and secrets, and
//!   each feed's fields ([`FieldType`], the same type an app's `collections.toml` uses). It lives in `trailhead-model`
//!   so the server reads it the same way.
//! - [`Http`], the only way to the network: https only, declared hosts only (redirects included), a user agent,
//!   retries with backoff, `ETag` revalidation and paging. [`Http::record_to`] captures real answers as fixtures.
//! - [`testing::Test`], which runs a sync against recorded answers ([`Recorded`], [`Http::replay_dir`]) with no
//!   server, network or token, and gives back the feeds it handed over.
//! - [`State`] (a cursor, an ETag between runs), [`Secrets`] and [`Settings`].

mod http;
mod replay;
mod run;
mod secrets;
mod settings;
mod state;
pub mod testing;

pub use http::{Http, HttpError, Request, Response, RetryPolicy};
pub use replay::Recorded;
pub use run::{Cx, Error, run, run_with_env};
pub use secrets::{SecretError, Secrets};
pub use settings::Settings;
pub use state::{State, StateError};
pub use trailhead_model::app_data::{FieldKind, FieldType};
pub use trailhead_model::connector::{
    FeedError, FeedRecord, FeedSpec, HandedOver, Manifest, ManifestError, SettingKind, SettingSpec, SettingsError,
    parse_duration, secret_var as env_var,
};

#[cfg(test)]
mod http_tests;

#[cfg(test)]
pub(crate) mod fixtures {
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
}
