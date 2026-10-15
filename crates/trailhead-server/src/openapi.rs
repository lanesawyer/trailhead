//! The OpenAPI document for `/v1`, served at `GET /v1/openapi.json`.

use std::sync::LazyLock;

use axum::body::Bytes;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::openapi::path::{Operation, ParameterBuilder, ParameterIn};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{
    ContentBuilder, HeaderBuilder, KnownFormat, ObjectBuilder, Ref, RefOr, ResponseBuilder, SchemaFormat, Type,
};
use utoipa::{Modify, OpenApi, ToSchema};
use uuid::Uuid;

use crate::routes;

/// Every stable error code, as returned by `trailhead_core::Error::code()`.
pub const ERROR_CODES: &[&str] = &[
    "not_found",
    "claim_held",
    "not_claim_holder",
    "claim_lost",
    "version_conflict",
    "blocked_by",
    "invalid_transition",
    "review_required",
    "dependency_cycle",
    "undo_conflict",
    "undo_window_expired",
    "already_exists",
    "unauthorized",
    "forbidden",
    "idempotency_key_reused",
    "idempotency_key_in_flight",
    "rate_limited",
    "invalid",
    "internal",
];

/// The error envelope every failed request returns.
#[derive(Serialize, ToSchema)]
#[allow(dead_code, reason = "schema only; handlers build these bodies with json!")]
pub struct ErrorBody {
    error: ErrorDetail,
}

/// `code` is stable; `message` is for people. Some codes carry extra fields.
#[derive(Serialize, ToSchema)]
#[allow(dead_code, reason = "schema only; handlers build these bodies with json!")]
pub struct ErrorDetail {
    #[schema(schema_with = error_code)]
    code: String,
    message: String,
    /// `blocked_by`: keys of the unfinished blockers.
    blocked_by: Option<Vec<String>>,
    /// `version_conflict`: the task's current version.
    current_version: Option<i64>,
    /// `claim_held`: who holds the claim.
    holder: Option<String>,
    /// `claim_held`: the holder's user or agent id.
    holder_id: Option<String>,
    /// `claim_held`: when the claim expires.
    expires_at: Option<String>,
    /// `rate_limited`: seconds until a request will be accepted (also in `Retry-After`).
    retry_after: Option<u64>,
    /// `undo_conflict`: the later event that would be overwritten.
    later_event: Option<i64>,
    /// `invalid_transition`: the current status.
    from: Option<trailhead_core::Status>,
    /// `invalid_transition`: the requested status.
    to: Option<trailhead_core::Status>,
}

fn error_code() -> utoipa::openapi::Object {
    ObjectBuilder::new().schema_type(Type::String).enum_values(Some(ERROR_CODES.iter().copied())).build()
}

/// `GET /v1/health`.
#[derive(Serialize, ToSchema)]
#[allow(dead_code, reason = "schema only; handlers build these bodies with json!")]
pub struct Health {
    ok: bool,
    version: String,
    pid: u32,
    /// The binary this server was started from.
    exe: Option<String>,
    /// That binary's modification time when the server started, in Unix milliseconds.
    exe_modified_ms: Option<u64>,
    db: Option<String>,
    started_at: time::OffsetDateTime,
}

/// `GET /v1/me`: the caller.
#[derive(Serialize, ToSchema)]
#[allow(dead_code, reason = "schema only; handlers build these bodies with json!")]
pub struct Me {
    workspace_id: Uuid,
    user: trailhead_core::User,
    /// Set when the token belongs to an agent.
    agent: Option<trailhead_core::Agent>,
}

/// `POST /v1/agents`. The secret is shown only here.
#[derive(Serialize, ToSchema)]
#[allow(dead_code, reason = "schema only; handlers build these bodies with json!")]
pub struct CreatedAgent {
    agent: trailhead_core::Agent,
    token: trailhead_core::Token,
    secret: String,
}

/// `POST /v1/webhooks`. The signing secret is shown only here.
#[derive(Serialize, ToSchema)]
#[allow(dead_code, reason = "schema only; handlers build these bodies with json!")]
pub struct CreatedWebhook {
    webhook: trailhead_core::Webhook,
    secret: String,
}

/// `POST /v1/tokens`. The secret is shown only here.
#[derive(Serialize, ToSchema)]
#[allow(dead_code, reason = "schema only; handlers build these bodies with json!")]
pub struct CreatedToken {
    token: trailhead_core::Token,
    secret: String,
}

/// Bearer auth, the `trailhead-batch` header on every response, and the
/// error responses every authenticated route can return.
struct Common;

const PUBLIC: &[&str] = &["/v1/health", "/v1/openapi.json"];

impl Modify for Common {
    fn modify(&self, api: &mut utoipa::openapi::OpenApi) {
        let components = api.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some("A user or agent token, `Authorization: Bearer th_...`."))
                    .build(),
            ),
        );
        let batch = HeaderBuilder::new()
            .schema(Some(
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .format(Some(SchemaFormat::KnownFormat(KnownFormat::Uuid))),
            ))
            .description(Some(
                "The undo batch for this request. `POST /v1/undo {\"batch_id\": ...}` reverts everything it did.",
            ))
            .build();
        let error = |description: &str| {
            ResponseBuilder::new()
                .description(description)
                .content(
                    "application/json",
                    ContentBuilder::new().schema(Some(RefOr::Ref(Ref::from_schema_name("ErrorBody")))).build(),
                )
                .build()
        };
        let idempotency_key = ParameterBuilder::new()
            .name("Idempotency-Key")
            .parameter_in(ParameterIn::Header)
            .description(Some(
                "Optional, 1-255 characters. A retry with the same key and request replays the stored 2xx \
                 response (with `idempotent-replayed: true`) instead of running again. Keys belong to the token \
                 and last 24 hours.",
            ))
            .schema(Some(ObjectBuilder::new().schema_type(Type::String)))
            .build();
        for (path, item) in api.paths.paths.iter_mut() {
            let public = PUBLIC.contains(&path.as_str());
            if !public && path.starts_with("/v1/") {
                for op in [&mut item.post, &mut item.patch, &mut item.delete].into_iter().flatten() {
                    op.parameters.get_or_insert_with(Vec::new).push(idempotency_key.clone().into());
                    let r = &mut op.responses.responses;
                    r.entry("409".into())
                        .or_insert_with(|| error("`idempotency_key_in_flight`: the same key is still running").into());
                    r.entry("422".into()).or_insert_with(|| {
                        error("`idempotency_key_reused`: the key was used for a different request").into()
                    });
                }
            }
            let ops: [&mut Option<Operation>; 4] = [&mut item.get, &mut item.post, &mut item.patch, &mut item.delete];
            for op in ops.into_iter().flatten() {
                if !public {
                    let r = &mut op.responses.responses;
                    r.entry("400".into()).or_insert_with(|| error("`invalid`: bad input").into());
                    r.entry("401".into()).or_insert_with(|| error("`unauthorized`: missing or invalid token").into());
                    r.entry("429".into())
                        .or_insert_with(|| error("`rate_limited`: over the token's limit; see `Retry-After`").into());
                    r.entry("500".into()).or_insert_with(|| error("`internal`").into());
                }
                for r in op.responses.responses.values_mut() {
                    if let RefOr::T(r) = r {
                        r.headers.insert("trailhead-batch".into(), batch.clone().into());
                    }
                }
            }
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Trailhead",
        description = "Headless task engine for humans and agents. Errors are `{\"error\": {\"code\", \"message\", ...}}` with a stable `code`. Task and project reads return an `ETag` of the version; send it back as `If-Match` on writes, and a stale version gets `412 version_conflict`.",
    ),
    modifiers(&Common),
    security(("bearer" = [])),
    paths(
        routes::health,
        crate::openapi::serve,
        routes::me,
        routes::list_projects,
        routes::create_project,
        routes::get_project,
        routes::update_project,
        routes::list_tasks,
        routes::create_task,
        routes::get_task,
        routes::update_task,
        routes::delete_task,
        routes::claim,
        routes::renew,
        routes::release,
        routes::list_comments,
        routes::add_comment,
        routes::list_blockers,
        routes::add_blocker,
        routes::remove_blocker,
        routes::task_history,
        routes::resolve_comment,
        routes::ready,
        routes::search,
        routes::list_events,
        crate::stream::events,
        routes::undo,
        routes::revert,
        routes::list_agents,
        routes::create_agent,
        routes::list_tokens,
        routes::my_claims,
        routes::create_token,
        routes::create_webhook,
        routes::list_webhooks,
        routes::delete_webhook,
        routes::webhook_deliveries,
        routes::revoke_token,
    ),
    components(schemas(ErrorBody, ErrorDetail))
)]
pub struct ApiDoc;

static DOC: LazyLock<Bytes> =
    LazyLock::new(|| ApiDoc::openapi().to_json().expect("the OpenAPI document serializes").into());

/// This document.
#[utoipa::path(
    get,
    path = "/v1/openapi.json",
    tag = "meta",
    security(()),
    responses((status = 200, description = "The OpenAPI document", body = Object))
)]
pub async fn serve() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], DOC.clone()).into_response()
}

#[cfg(test)]
pub(crate) mod tests {
    use trailhead_core::{Error, Status};

    use super::ERROR_CODES;

    /// Adding an `Error` variant breaks this match until the list below and
    /// `ERROR_CODES` are updated too.
    #[allow(dead_code)]
    fn every_variant_is_listed(e: &Error) {
        match e {
            Error::NotFound(_)
            | Error::ClaimHeld { .. }
            | Error::NotClaimHolder
            | Error::ClaimLost(_)
            | Error::VersionConflict { .. }
            | Error::BlockedBy(_)
            | Error::InvalidTransition { .. }
            | Error::ReviewRequired
            | Error::DependencyCycle
            | Error::UndoConflict { .. }
            | Error::UndoWindowExpired(_)
            | Error::AlreadyExists(_)
            | Error::Unauthorized
            | Error::Forbidden(_)
            | Error::IdempotencyKeyReused
            | Error::IdempotencyKeyInFlight
            | Error::Invalid(_)
            | Error::RateLimited { .. }
            | Error::Db(_)
            | Error::Json(_)
            | Error::Corrupt(_) => {}
        }
    }

    /// One of each error, in `ERROR_CODES` order.
    pub(crate) fn sample_errors() -> Vec<Error> {
        vec![
            Error::NotFound(String::new()),
            Error::ClaimHeld { holder: String::new(), holder_id: String::new(), expires_at: String::new() },
            Error::NotClaimHolder,
            Error::ClaimLost(String::new()),
            Error::VersionConflict { expected: 0, current: 0 },
            Error::BlockedBy(vec![]),
            Error::InvalidTransition { from: Status::Open, to: Status::Done },
            Error::ReviewRequired,
            Error::DependencyCycle,
            Error::UndoConflict { entity: String::new(), later_event: 0 },
            Error::UndoWindowExpired(String::new()),
            Error::AlreadyExists(String::new()),
            Error::Unauthorized,
            Error::Forbidden(String::new()),
            Error::IdempotencyKeyReused,
            Error::IdempotencyKeyInFlight,
            Error::RateLimited { retry_after: 0 },
            Error::Invalid(String::new()),
            Error::Corrupt(String::new()),
        ]
    }

    #[test]
    fn error_codes_match_core() {
        let codes: Vec<_> = sample_errors().iter().map(Error::code).collect();
        assert_eq!(codes, ERROR_CODES);
    }
}
