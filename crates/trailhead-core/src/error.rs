use crate::model::Status;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} not found")]
    NotFound(String),
    #[error("task is claimed by {holder} until {expires_at}")]
    ClaimHeld { holder: String, holder_id: String, expires_at: String },
    #[error("you do not hold a claim on this task")]
    NotClaimHolder,
    #[error("version conflict: expected {expected}, current is {current}")]
    VersionConflict { expected: i64, current: i64 },
    #[error("blocked by {}", .0.join(", "))]
    BlockedBy(Vec<String>),
    #[error("cannot move from {from} to {to}")]
    InvalidTransition { from: Status, to: Status },
    #[error("this needs human review before it can be done")]
    ReviewRequired,
    #[error("dependency would create a cycle")]
    DependencyCycle,
    #[error("cannot undo: {entity} was changed later by event {later_event}")]
    UndoConflict { entity: String, later_event: i64 },
    #[error("cannot undo {0}: it is older than the 90-day undo window")]
    UndoWindowExpired(String),
    #[error("{0} already exists")]
    AlreadyExists(String),
    #[error("missing or invalid token")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("this idempotency key was already used for a different request")]
    IdempotencyKeyReused,
    #[error("a request with this idempotency key is still in progress")]
    IdempotencyKeyInFlight,
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("rate limit exceeded; retry in {retry_after}s")]
    RateLimited { retry_after: u64 },
    #[error("database error: {0}")]
    Db(#[from] libsql::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("corrupt data: {0}")]
    Corrupt(String),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::ClaimHeld { .. } => "claim_held",
            Error::NotClaimHolder => "not_claim_holder",
            Error::VersionConflict { .. } => "version_conflict",
            Error::BlockedBy(_) => "blocked_by",
            Error::InvalidTransition { .. } => "invalid_transition",
            Error::ReviewRequired => "review_required",
            Error::DependencyCycle => "dependency_cycle",
            Error::UndoConflict { .. } => "undo_conflict",
            Error::UndoWindowExpired(_) => "undo_window_expired",
            Error::AlreadyExists(_) => "already_exists",
            Error::Unauthorized => "unauthorized",
            Error::Forbidden(_) => "forbidden",
            Error::IdempotencyKeyReused => "idempotency_key_reused",
            Error::IdempotencyKeyInFlight => "idempotency_key_in_flight",
            Error::Invalid(_) => "invalid",
            Error::RateLimited { .. } => "rate_limited",
            Error::Db(_) | Error::Json(_) | Error::Corrupt(_) => "internal",
        }
    }
}
