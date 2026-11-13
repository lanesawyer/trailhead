//! Trailhead's domain core: schema, rules, claims, the ready queue and the event
//! log. Every interface (REST, MCP, CLI) goes through [`Store`].

mod claims;
mod comments;
mod db;
mod deps;
mod error;
mod events;
mod idempotency;
mod identity;
mod metadata;
mod projects;
mod reminders;
mod store;
mod tasks;
mod tokens;
mod undo;
mod webhooks;
mod when;

pub use claims::NamedClaim;
pub use comments::NewComment;
pub use deps::BlockersOf;
pub use error::{Error, Result};
pub use events::{EventFilter, UNDO_WINDOW};
pub use idempotency::{StoredResponse, idempotency_fingerprint};
pub use projects::{NewProject, ProjectPatch};
pub use reminders::{DEFAULT_REMINDER_WINDOW, DUE_SOON, OVERDUE};
pub use store::{Clock, Ctx, Store};
pub use tasks::{GradeFilter, NewTask, SearchFilter, TaskFilter, TaskPatch, TaskSort, parse_grade};
pub use tokens::{TokenOptions, TokenPatch};
pub use trailhead_model::*;
pub use webhooks::{MAX_FAILURES as WEBHOOK_MAX_FAILURES, webhook_signature};
pub use when::parse_at;
