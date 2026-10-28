//! Agents and tokens.

use axum::extract::State;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use trailhead_core::{Agent, Error, Principal, Token, TokenOptions, TokenPatch};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path};
use crate::openapi::{CreatedAgent, CreatedToken, ErrorBody};

use super::{S, created, nullable};

#[utoipa::path(
    get, path = "/v1/agents", tag = "agents",
    responses((status = 200, description = "Agents", body = Vec<Agent>))
)]
pub(crate) async fn list_agents(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_agents(&ctx).await?)))
}

/// A token's optional project scope and rate limit.
#[derive(Deserialize, Default, ToSchema)]
pub(crate) struct TokenLimits {
    /// Project keys or ids. Omitted or empty means every project.
    projects: Option<Vec<String>>,
    rate_limit_per_minute: Option<i64>,
    /// Seconds until the token stops working. Omitted means never, unless your own token expires sooner.
    expires_in_seconds: Option<i64>,
}

impl From<TokenLimits> for TokenOptions {
    fn from(l: TokenLimits) -> Self {
        TokenOptions {
            projects: l.projects,
            rate_limit_per_minute: l.rate_limit_per_minute,
            expires_in: l.expires_in_seconds.map(jiff::SignedDuration::from_secs),
        }
    }
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateAgent {
    name: String,
    #[serde(flatten)]
    limits: TokenLimits,
}

/// Creates an agent and its first token. The secret is only shown here.
#[utoipa::path(
    post, path = "/v1/agents", tag = "agents", request_body = CreateAgent,
    responses(
        (status = 201, description = "The agent, its token and the token's secret", body = CreatedAgent),
        (status = 403, description = "`forbidden`: humans only", body = ErrorBody),
        (status = 409, description = "`already_exists`", body = ErrorBody),
    )
)]
pub(crate) async fn create_agent(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateAgent>) -> ApiResult<Response> {
    let agent = s.store.create_agent(&ctx, &b.name).await?;
    let (token, secret) =
        s.store.create_token_with(&ctx, Principal::Agent(agent.id), &agent.name, b.limits.into()).await?;
    Ok(created(json!({ "agent": agent, "token": token, "secret": secret })))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateToken {
    name: String,
    /// Agent name or id. Omitted means a token for the caller.
    agent: Option<String>,
    #[serde(flatten)]
    limits: TokenLimits,
}

/// Issues a token for the caller or one of their agents. The secret is only shown here.
#[utoipa::path(
    post, path = "/v1/tokens", tag = "tokens", request_body = CreateToken,
    responses(
        (status = 201, description = "The token and its secret, shown once", body = CreatedToken),
        (status = 403, description = "forbidden: agents and scoped tokens can't issue tokens", body = ErrorBody),
        (status = 404, description = "not_found: no such agent", body = ErrorBody),
    )
)]
pub(crate) async fn create_token(State(s): S, Auth(ctx): Auth, Json(b): Json<CreateToken>) -> ApiResult<Response> {
    let principal = match &b.agent {
        None => Principal::User(ctx.user_id),
        Some(a) => {
            let agents = s.store.list_agents(&ctx).await?;
            let agent = agents
                .iter()
                .find(|x| x.owner_user_id == ctx.user_id && (x.name == *a || x.id.to_string() == *a))
                .ok_or_else(|| ApiError(Error::NotFound(format!("agent {a}"))))?;
            Principal::Agent(agent.id)
        }
    };
    let (token, secret) = s.store.create_token_with(&ctx, principal, &b.name, b.limits.into()).await?;
    Ok(created(json!({ "token": token, "secret": secret })))
}

#[utoipa::path(
    get, path = "/v1/tokens", tag = "tokens",
    responses((status = 200, description = "Tokens, without secrets", body = Vec<Token>))
)]
pub(crate) async fn list_tokens(State(s): S, Auth(ctx): Auth) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.list_tokens(&ctx).await?)))
}

#[utoipa::path(
    delete, path = "/v1/tokens/{id}", tag = "tokens", params(("id" = Uuid, Path, description = "Token id")),
    responses(
        (status = 200, description = "The revoked token", body = Token),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn revoke_token(State(s): S, Auth(ctx): Auth, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.revoke_token(&ctx, id).await?)))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct UpdateToken {
    /// Project keys or ids. `null` or `[]` opens the token to every project.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Vec<String>>)]
    projects: Option<Option<Vec<String>>>,
    /// Requests per minute. `null` removes the limit.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    rate_limit_per_minute: Option<Option<i64>>,
}

/// Changes a token's project scope or rate limit without reissuing its secret.
/// Omitted fields stay as they are. Humans only, for their own tokens and their agents'.
#[utoipa::path(
    patch, path = "/v1/tokens/{id}", tag = "tokens", params(("id" = Uuid, Path, description = "Token id")),
    request_body = UpdateToken,
    responses(
        (status = 200, description = "The updated token", body = Token),
        (status = 403, description = "forbidden: agents and scoped tokens can't change tokens", body = ErrorBody),
        (status = 404, description = "`not_found`: no such live token of yours", body = ErrorBody),
    )
)]
pub(crate) async fn update_token(
    State(s): S,
    Auth(ctx): Auth,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateToken>,
) -> ApiResult<Json<Value>> {
    let patch = TokenPatch { projects: b.projects, rate_limit_per_minute: b.rate_limit_per_minute };
    Ok(Json(json!(s.store.update_token(&ctx, id, patch).await?)))
}

/// Revokes an agent and every sub-agent under it; their tokens stop working at
/// once. The owning human or any agent above it may do this.
#[utoipa::path(
    post, path = "/v1/agents/{agent}/revoke", tag = "agents",
    params(("agent" = String, Path, description = "Agent name (e.g. `openclaw/researcher`) or id")),
    responses(
        (status = 200, description = "The revoked agent", body = Agent),
        (status = 403, description = "`forbidden`: not your agent or sub-agent", body = ErrorBody),
        (status = 404, description = "`not_found`", body = ErrorBody),
    )
)]
pub(crate) async fn revoke_agent(State(s): S, Auth(ctx): Auth, Path(agent): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(s.store.revoke_agent(&ctx, &agent).await?)))
}
