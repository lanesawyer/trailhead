use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::Error;

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = Error;
            fn from_str(s: &str) -> Result<Self, Error> {
                match s {
                    $($s => Ok($name::$variant),)+
                    _ => Err(Error::Invalid(format!("unknown {}: {s}", stringify!($name).to_lowercase()))),
                }
            }
        }
    };
}

string_enum!(Status {
    Open => "open",
    InProgress => "in_progress",
    NeedsInput => "needs_input",
    InReview => "in_review",
    Done => "done",
    Cancelled => "cancelled",
});

impl Status {
    pub fn is_closed(self) -> bool {
        matches!(self, Status::Done | Status::Cancelled)
    }

    pub fn can_transition_to(self, to: Status) -> bool {
        use Status::*;
        if self == to {
            return true;
        }
        match self {
            Open => matches!(to, InProgress | NeedsInput | InReview | Done | Cancelled),
            InProgress => matches!(to, Open | NeedsInput | InReview | Done | Cancelled),
            NeedsInput => matches!(to, Open | InProgress | Cancelled),
            InReview => matches!(to, Open | InProgress | NeedsInput | Done | Cancelled),
            Done | Cancelled => matches!(to, Open),
        }
    }
}

string_enum!(CommentKind {
    Handoff => "handoff",
    Progress => "progress",
    Question => "question",
    Decision => "decision",
    Note => "note",
});

string_enum!(ProjectStatus {
    Active => "active",
    Archived => "archived",
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub default_claim_ttl: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub email: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub key: String,
    pub name: String,
    pub brief: String,
    pub status: ProjectStatus,
    pub metadata: Value,
    /// Seconds. Falls back to the workspace default when unset.
    pub claim_ttl: Option<i64>,
    pub require_review: bool,
    pub version: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub key: String,
    pub title: String,
    pub description: String,
    pub status: Status,
    /// 1 (urgent) to 4 (low).
    pub priority: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_at: Option<OffsetDateTime>,
    pub due_tz: Option<String>,
    pub parent_id: Option<Uuid>,
    pub metadata: Value,
    pub version: i64,
    pub source: Option<String>,
    pub external_id: Option<String>,
    pub created_by: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub task_id: Uuid,
    pub workspace_id: Uuid,
    pub holder_id: Uuid,
    pub on_behalf_of: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub claimed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub task_id: Uuid,
    pub author_id: Uuid,
    pub on_behalf_of: Uuid,
    pub kind: CommentKind,
    pub addressed_to: Option<Uuid>,
    pub body: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dependency {
    pub workspace_id: Uuid,
    pub blocker_id: Uuid,
    pub blocked_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub workspace_id: Uuid,
    pub batch_id: Uuid,
    pub actor_id: Uuid,
    pub on_behalf_of: Uuid,
    pub task_id: Option<Uuid>,
    /// Convenience for display; not stored on the event.
    #[serde(default)]
    pub task_key: Option<String>,
    pub entity: Entity,
    pub entity_id: String,
    pub kind: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub undoes: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

string_enum!(Entity {
    Project => "project",
    Task => "task",
    Claim => "claim",
    Comment => "comment",
    Dependency => "dependency",
});
