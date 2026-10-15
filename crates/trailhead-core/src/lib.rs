//! Trailhead's domain core: schema, rules, claims, the ready queue and the event
//! log. Every interface (REST, MCP, CLI) goes through [`Store`].

mod claims;
mod comments;
mod db;
mod deps;
mod error;
mod events;
mod identity;
mod metadata;
mod model;
mod projects;
mod store;
mod tasks;
mod undo;

pub use claims::ClaimResult;
pub use comments::NewComment;
pub use error::{Error, Result};
pub use events::EventFilter;
pub use model::*;
pub use projects::{NewProject, ProjectPatch};
pub use store::{Clock, Ctx, Store};
pub use tasks::{NewTask, TaskDetail, TaskFilter, TaskPatch};
pub use undo::{Reverted, Skipped, UndoReport};
