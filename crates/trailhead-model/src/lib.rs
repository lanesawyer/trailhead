//! Trailhead's domain types and the rules about them that need no database: what a task, comment or claim is, the
//! statuses and how they may change. `trailhead-core` re-exports all of it; clients that talk to the API
//! (Lookout, the factory, the daemon) use it without the store.

use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// A value that isn't a valid status, kind or other model value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

macro_rules! string_enum {
    ($name:ident $label:literal { $($variant:ident => $s:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
            type Err = ParseError;
            fn from_str(s: &str) -> Result<Self, ParseError> {
                match s {
                    $($s => Ok($name::$variant),)+
                    _ => Err(ParseError(format!(
                        "unknown {}: {s} (expected one of: {})",
                        $label,
                        [$($s),+].join(", ")
                    ))),
                }
            }
        }
    };
}

string_enum!(Status "status" {
    Open => "open",
    InProgress => "in_progress",
    NeedsInput => "needs_input",
    InReview => "in_review",
    Approved => "approved",
    Done => "done",
    Cancelled => "cancelled",
});

impl Status {
    /// Every status, in lifecycle order.
    pub const ALL: [Status; 7] = [
        Status::Open,
        Status::InProgress,
        Status::NeedsInput,
        Status::InReview,
        Status::Approved,
        Status::Done,
        Status::Cancelled,
    ];

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
            NeedsInput => matches!(to, Open | InProgress | InReview | Cancelled),
            InReview => matches!(to, Open | InProgress | NeedsInput | Approved | Done | Cancelled),
            Approved => matches!(to, Open | InReview | Done | Cancelled),
            Done | Cancelled => matches!(to, Open),
        }
    }
}

string_enum!(CommentKind "comment kind" {
    Handoff => "handoff",
    Progress => "progress",
    Question => "question",
    Decision => "decision",
    Note => "note",
    Review => "review",
});

string_enum!(Verdict "verdict" {
    Approve => "approve",
    RequestChanges => "request_changes",
});

string_enum!(ProjectStatus "project status" {
    Active => "active",
    Archived => "archived",
});

/// A task's key, `TRAIL-42`: its project's key, a dash and its number.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaskKey(pub String);

impl TaskKey {
    /// The project's key: everything before the last dash, or the whole key if it has none.
    pub fn project(&self) -> &str {
        self.0.rsplit_once('-').map_or(self.0.as_str(), |(p, _)| p)
    }
}

impl std::fmt::Display for TaskKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A 1-to-4 scale serialized as its number, for [`Grade`] and [`Priority`].
macro_rules! scale {
    ($(#[$doc:meta])* $name:ident, $what:literal, $range:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
        #[cfg_attr(feature = "openapi", schema(value_type = u8, minimum = 1, maximum = 4))]
        #[serde(try_from = "i64", into = "u8")]
        pub struct $name(u8);

        impl $name {
            /// Every value, 1 first.
            pub const ALL: [$name; 4] = [$name(1), $name(2), $name(3), $name(4)];

            /// `n` if it is 1 to 4.
            pub const fn new(n: u8) -> Option<$name> {
                if n >= 1 && n <= 4 { Some($name(n)) } else { None }
            }

            pub const fn get(self) -> u8 {
                self.0
            }
        }

        impl TryFrom<i64> for $name {
            type Error = ParseError;
            fn try_from(n: i64) -> Result<Self, ParseError> {
                u8::try_from(n).ok().and_then($name::new).ok_or_else(|| ParseError(concat!($what, " must be ", $range).into()))
            }
        }

        impl From<$name> for u8 {
            fn from(v: $name) -> u8 {
                v.0
            }
        }

        impl From<$name> for i64 {
            fn from(v: $name) -> i64 {
                v.0.into()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = ParseError;
            fn from_str(s: &str) -> Result<Self, ParseError> {
                s.trim()
                    .parse::<i64>()
                    .map_err(|_| ParseError(concat!($what, " must be ", $range).into()))
                    .and_then($name::try_from)
            }
        }
    };
}

scale!(
    /// A task's risk grade (`docs/factory/review.md#grades`), 1 (lowest risk) to 4. A human sets, lowers or clears
    /// it; an agent can only raise it.
    Grade,
    "grade",
    "1 to 4"
);

scale!(
    /// How urgent a task is, 1 (urgent) to 4 (low).
    Priority,
    "priority",
    "1 (urgent) to 4 (low)"
);

impl Default for Priority {
    fn default() -> Self {
        Priority(3)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub default_claim_ttl: i64,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct User {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub email: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Agent {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub owner_user_id: Uuid,
    /// Set for a sub-agent: the agent that created it. A sub-agent's name is
    /// `<parent name>/<own name>`.
    #[serde(default)]
    pub parent_agent_id: Option<Uuid>,
    pub name: String,
    pub created_at: Timestamp,
    /// Revoking an agent also revokes every sub-agent under it.
    #[serde(default)]
    pub revoked_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// Agents allowed to approve review work here (`in_review` → `approved` or `done`), on top of
    /// any human. Only humans can change this list.
    #[serde(default)]
    pub reviewers: Vec<Uuid>,
    pub version: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub deleted_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Task {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub key: String,
    pub title: String,
    pub description: String,
    pub status: Status,
    pub priority: Priority,
    #[serde(default)]
    pub grade: Option<Grade>,
    /// Set on a cancelled task that duplicates another: the one to look at instead.
    #[serde(default)]
    pub duplicate_of: Option<Uuid>,
    /// The day the task is due (`YYYY-MM-DD`). Without `due_at`, it's due by the end of
    /// that day in `due_tz`.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = Date))]
    pub due_on: Option<Date>,
    /// An optional time on `due_on` it's due by, e.g. 5pm.
    pub due_at: Option<Timestamp>,
    /// The IANA zone the deadline was set in; it decides the day and when the day ends.
    pub due_tz: Option<String>,
    /// The day the task is planned to be worked (`YYYY-MM-DD`), separate from its due date.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, format = Date))]
    pub scheduled_on: Option<Date>,
    /// An optional time on `scheduled_on`, e.g. 1pm that day.
    #[serde(default)]
    pub scheduled_at: Option<Timestamp>,
    /// The IANA zone `scheduled_at` was planned in; it decides which day it falls on.
    #[serde(default)]
    pub scheduled_tz: Option<String>,
    pub parent_id: Option<Uuid>,
    pub metadata: Value,
    pub version: i64,
    pub source: Option<String>,
    pub external_id: Option<String>,
    pub created_by: Uuid,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub deleted_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Claim {
    pub task_id: Uuid,
    pub workspace_id: Uuid,
    pub holder_id: Uuid,
    pub on_behalf_of: Uuid,
    pub claimed_at: Timestamp,
    /// `null` for a claim that never expires: people's claims, unless they asked for a TTL.
    pub expires_at: Option<Timestamp>,
}

impl Claim {
    pub fn is_live(&self, now: Timestamp) -> bool {
        self.expires_at.is_none_or(|e| e > now)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Comment {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub task_id: Uuid,
    pub author_id: Uuid,
    pub on_behalf_of: Uuid,
    pub kind: CommentKind,
    pub addressed_to: Option<Uuid>,
    pub body: String,
    /// Set on `review` comments only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// The model behind an agent's review, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub resolved_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub deleted_at: Option<Timestamp>,
}

/// The `request_changes` review comment a task is waiting on a rework for, if it is: the task is `in_review`, its
/// newest `review` comment with a verdict (newest by id, which is a UUIDv7 and so follows creation order) asks for
/// changes, and no `handoff` comment has a larger id. Computed from stored columns only, never from text. `in_review`
/// then means the work is in the review loop, and this says whose move it is.
pub fn changes_requested<'a>(status: Status, comments: impl IntoIterator<Item = &'a Comment>) -> Option<Uuid> {
    if status != Status::InReview {
        return None;
    }
    let comments: Vec<&Comment> = comments.into_iter().filter(|c| c.deleted_at.is_none()).collect();
    let live = || comments.iter().copied();
    let review = live().filter(|c| c.kind == CommentKind::Review && c.verdict.is_some()).max_by_key(|c| c.id)?;
    let answered = live().any(|c| c.kind == CommentKind::Handoff && c.id > review.id);
    (review.verdict == Some(Verdict::RequestChanges) && !answered).then_some(review.id)
}

/// A task with its claim, comments and links, as `GET /v1/tasks/{key}` returns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskDetail {
    pub task: Task,
    pub claim: Option<Claim>,
    /// Keys of unfinished tasks blocking this one.
    pub blocked_by: Vec<String>,
    /// The id of the `request_changes` review comment that makes this an `in_review` task with changes requested,
    /// computed like `blocked_by` and never stored (see [`changes_requested`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes_requested: Option<Uuid>,
    /// Keys of tasks this one blocks.
    pub blocks: Vec<String>,
    pub comments: Vec<Comment>,
    /// Key of the task this one was cancelled as a duplicate of.
    pub duplicate_of: Option<String>,
    /// Keys of tasks cancelled as duplicates of this one.
    pub duplicates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Dependency {
    pub workspace_id: Uuid,
    pub blocker_id: Uuid,
    pub blocked_id: Uuid,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    pub at: Timestamp,
}

string_enum!(Entity "entity" {
    Project => "project",
    Task => "task",
    Claim => "claim",
    Comment => "comment",
    Dependency => "dependency",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_round_trip_through_their_api_names() {
        for status in Status::ALL {
            assert_eq!(status.as_str().parse::<Status>().unwrap(), status);
            assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
        }
        assert_eq!(
            "shipped".parse::<Status>().unwrap_err().to_string(),
            "unknown status: shipped (expected one of: open, in_progress, needs_input, in_review, approved, done, cancelled)"
        );
    }

    #[test]
    fn parse_errors_name_the_type_and_list_the_values() {
        assert_eq!(
            "bogus".parse::<CommentKind>().unwrap_err().to_string(),
            "unknown comment kind: bogus (expected one of: handoff, progress, question, decision, note, review)"
        );
        assert_eq!(
            "x".parse::<ProjectStatus>().unwrap_err().to_string(),
            "unknown project status: x (expected one of: active, archived)"
        );
    }

    #[test]
    fn closed_tasks_can_only_reopen() {
        for to in Status::ALL {
            assert_eq!(Status::Done.can_transition_to(to), matches!(to, Status::Open | Status::Done));
        }
        assert!(!Status::NeedsInput.can_transition_to(Status::Done));
    }

    #[test]
    fn a_task_keys_project_is_before_its_last_dash() {
        assert_eq!(TaskKey("TRAIL-42".into()).project(), "TRAIL");
        assert_eq!(TaskKey("MY-APP-7".into()).project(), "MY-APP");
        assert_eq!(TaskKey("nodash".into()).project(), "nodash");
        assert_eq!(serde_json::to_value(TaskKey("TRAIL-1".into())).unwrap(), "TRAIL-1");
    }

    #[test]
    fn grades_and_priorities_are_1_to_4_and_serialize_as_numbers() {
        assert_eq!(Grade::ALL.map(Grade::get), [1, 2, 3, 4]);
        assert_eq!(serde_json::to_value(Grade::new(3).unwrap()).unwrap(), 3);
        assert_eq!(serde_json::from_value::<Priority>(serde_json::json!(2)).unwrap(), Priority::new(2).unwrap());
        for bad in [0, 5, -1, 300] {
            assert!(serde_json::from_value::<Grade>(serde_json::json!(bad)).is_err(), "{bad}");
        }
        assert_eq!(" 4 ".parse::<Grade>().unwrap(), Grade::ALL[3]);
        assert_eq!("9".parse::<Priority>().unwrap_err().to_string(), "priority must be 1 (urgent) to 4 (low)");
        assert_eq!(Priority::default().get(), 3);
        assert!(Grade::ALL[0] < Grade::ALL[1]);
    }
}
