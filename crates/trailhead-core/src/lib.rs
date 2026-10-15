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
mod model;
mod projects;
mod reminders;
mod store;
mod tasks;
mod tokens;
mod undo;

pub use claims::{ClaimResult, HeldClaim};
pub use comments::NewComment;
pub use error::{Error, Result};
pub use events::{EventFilter, UNDO_WINDOW};
pub use idempotency::{StoredResponse, idempotency_fingerprint};
pub use model::*;
pub use projects::{NewProject, ProjectPatch};
pub use reminders::{DEFAULT_REMINDER_WINDOW, DUE_SOON, OVERDUE};
pub use store::{Clock, Ctx, Store};
pub use tasks::{NewTask, TaskDetail, TaskFilter, TaskPatch};
pub use tokens::{Principal, Token, TokenOptions};
pub use undo::{Reverted, Skipped, UndoReport};
