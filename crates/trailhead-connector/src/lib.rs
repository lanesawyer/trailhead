//! The kit a Trailhead connector is written against.
//!
//! A connector is a small program in someone's apps repo that fetches from one outside service and hands the
//! server feeds of records (`docs/app-platform.md`, "The app data store and connectors"). This crate is what it is
//! built from, so a connector is only "fetch and map":
//!
//! - [`Manifest`], the parsed `connector.toml`: id, schedule, the hosts it may reach, its settings and secrets, and
//!   each feed's fields ([`FieldType`], the same type an app's `collections.toml` uses, from `trailhead-model`).
//! - [`FeedRecord`] and the check of a set against a feed's fields ([`Manifest::check_feed`]).
//! - [`Http`], the only way to the network: https only, declared hosts only (redirects included), a user agent,
//!   retries with backoff, `ETag` revalidation and paging.
//! - [`Http::replay`] and [`Http::replay_dir`] answer from recorded responses, with the same host rules, so a
//!   connector is tested with no network and no token; [`Http::record_to`] captures them.
//! - [`State`] (a cursor, an ETag between runs), [`Secrets`] and [`Settings`].
//!
//! Still to come, once the server's API exists: handing a feed to the server, and the run loop that gives a
//! connector its settings, secrets and state and runs it under the server's schedule and time limit.

mod feed;
mod http;
mod manifest;
mod replay;
mod secrets;
mod settings;
mod state;

pub use feed::{FeedError, FeedRecord};
pub use http::{Http, HttpError, Request, Response, RetryPolicy};
pub use manifest::{FeedSpec, Manifest, ManifestError, SettingKind, SettingSpec, parse_duration};
pub use replay::Recorded;
pub use secrets::{SecretError, Secrets, env_var};
pub use settings::{Settings, SettingsError};
pub use state::{State, StateError};
pub use trailhead_model::app_data::{FieldKind, FieldType};

#[cfg(test)]
mod http_tests;
