//! Commands (`docs/factory/ui.md#architecture`, `#connection-gating`): one REST request per action under `/v1`,
//! with `If-Match` and an `Idempotency-Key`.
//!
//! The answer is only an [`Outcome`]: `Ok` with the new version, or an error routed back to the control that sent
//! it. A successful write is not applied locally; the event stream brings it to the mirror.
//!
//! A control sends [`Submit`] and reads [`CommandDone`]. The [`Outbox`] keeps every command until it has an answer.
//! Commands go out only while the connection is `Live`. One in flight when the link drops goes back in the queue and
//! is sent again, with the same key, once `Live` returns, so a retry the server already applied gets that answer
//! back instead of running twice. A `RateLimited` answer is retried the same way, with the same key, once its
//! `Retry-After` has passed (capped at [`MAX_RETRY_AFTER`]). After [`MAX_ATTEMPTS`] it is reported failed (a rate
//! limit is handed back as `RateLimited`) rather than tried forever.

use std::time::{Duration, Instant};

use bevy::prelude::*;
use serde_json::Value;
use trailhead_client::http::{Call, Client};

use crate::connection::{Connection, SendNet};
use crate::net::NetCommand;

/// How many times a command is sent before it is reported failed.
pub const MAX_ATTEMPTS: u32 = 5;

/// The longest the outbox waits on a `Retry-After` before sending again.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Why an attempt got no usable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// The request reached the net thread before it was `Live`. Nothing was sent, so it isn't a failed attempt and
    /// doesn't count toward [`MAX_ATTEMPTS`].
    NotConnected,
    /// The request was tried and got no usable answer (the link dropped, timed out, a server error).
    Failed(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => f.write_str("not connected"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

impl From<String> for SendError {
    fn from(why: String) -> Self {
        Self::Failed(why)
    }
}

impl From<&str> for SendError {
    fn from(why: &str) -> Self {
        Self::Failed(why.to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandId(pub u64);

/// One request to the API. Its `key` is fixed for the life of the command, so every retry carries the same one.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// What to send: the endpoint and its typed body.
    pub call: Call,
    /// The version the control saw, sent as `If-Match`.
    pub if_match: Option<i64>,
    pub key: String,
}

impl Request {
    /// A request with a fresh idempotency key.
    pub fn new(call: Call, if_match: Option<i64>) -> Request {
        Request { call, if_match, key: new_key() }
    }
}

fn new_key() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("lookout-{now:x}-{:x}-{:x}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// What the server said, for the control that sent the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Applied, at this version when the answer had one. The stream brings the change itself.
    Ok {
        version: Option<i64>,
    },
    /// The version the control saw is stale (`412 version_conflict`, or a `409`).
    Conflict {
        current_version: Option<i64>,
        message: String,
    },
    Forbidden(String),
    RateLimited {
        retry_after: Option<Duration>,
        message: String,
    },
    /// Any other refusal, or no answer after [`MAX_ATTEMPTS`].
    Failed(String),
}

/// A control asks for a request to be made.
#[derive(Message, Debug, Clone)]
pub struct Submit {
    /// The control (entity) the answer goes back to.
    pub source: Entity,
    pub request: Request,
}

/// The answer to a [`Submit`], for the control in `source`.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct CommandDone {
    pub source: Entity,
    pub id: CommandId,
    pub outcome: Outcome,
}

/// What the net thread learned of one attempt.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub id: CommandId,
    /// Which send this answers: a stale "no answer" doesn't undo a later send.
    pub attempt: u32,
    /// `Err` when the request got no usable answer (the link dropped, timed out): retry.
    pub result: Result<Outcome, SendError>,
}

#[derive(Debug, PartialEq, Eq)]
enum Stage {
    Queued,
    Sent,
}

#[derive(Debug)]
struct Pending {
    id: CommandId,
    source: Entity,
    request: Request,
    stage: Stage,
    /// Sends so far; tags each one so a stale answer can be told from the latest.
    attempts: u32,
    /// Attempts that really failed. A resend after the link dropped, or a send the net thread refused for want of a
    /// connection, isn't one.
    failures: u32,
    not_before: Option<Instant>,
}

/// Every command without an answer yet.
#[derive(Resource, Debug)]
pub struct Outbox {
    pending: Vec<Pending>,
    next: u64,
    /// Wait before sending again after a failed attempt, times the failures so far.
    pub backoff: Duration,
}

impl Default for Outbox {
    fn default() -> Self {
        Outbox { pending: Vec::new(), next: 1, backoff: Duration::from_millis(500) }
    }
}

impl Outbox {
    /// Commands still waiting for an answer, sent or not.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

pub struct CommandsPlugin;

impl Plugin for CommandsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Outbox>()
            .add_message::<Submit>()
            .add_message::<CommandDone>()
            .add_message::<Reply>()
            .add_message::<SendNet>()
            .add_systems(Update, (accept, apply_replies, dispatch.run_if(in_state(Connection::Live))).chain())
            .add_systems(OnExit(Connection::Live), requeue_sent);
    }
}

/// Takes new commands. Outside `Live` there is nothing to send them over, so they are refused at once rather than
/// left to go stale behind a greyed-out control.
fn accept(
    mut submits: MessageReader<Submit>,
    connection: Res<State<Connection>>,
    mut outbox: ResMut<Outbox>,
    mut done: MessageWriter<CommandDone>,
) {
    for Submit { source, request } in submits.read() {
        let id = CommandId(outbox.next);
        outbox.next += 1;
        if *connection.get() != Connection::Live {
            done.write(CommandDone {
                source: *source,
                id,
                outcome: Outcome::Failed("not connected to the server".into()),
            });
            continue;
        }
        outbox.pending.push(Pending {
            id,
            source: *source,
            request: request.clone(),
            stage: Stage::Queued,
            attempts: 0,
            failures: 0,
            not_before: None,
        });
    }
}

fn apply_replies(mut replies: MessageReader<Reply>, mut outbox: ResMut<Outbox>, mut done: MessageWriter<CommandDone>) {
    let backoff = outbox.backoff;
    for reply in replies.read() {
        let Some(at) = outbox.pending.iter().position(|p| p.id == reply.id) else { continue };
        match &reply.result {
            // Rate limited: wait out `Retry-After`, then send again with the same key, until attempts run out.
            Ok(Outcome::RateLimited { retry_after, .. }) if outbox.pending[at].attempts < MAX_ATTEMPTS => {
                let p = &mut outbox.pending[at];
                let wait = retry_after.map_or(backoff * p.attempts, |after| after.min(MAX_RETRY_AFTER));
                p.stage = Stage::Queued;
                p.not_before = Some(Instant::now() + wait);
            }
            // An answer to any attempt settles it: the key means they are all the same request.
            Ok(outcome) => {
                let p = outbox.pending.remove(at);
                done.write(CommandDone { source: p.source, id: p.id, outcome: outcome.clone() });
            }
            Err(_) if outbox.pending[at].attempts != reply.attempt => {}
            Err(why) => {
                let p = &mut outbox.pending[at];
                if *why != SendError::NotConnected {
                    p.failures += 1;
                }
                if p.failures >= MAX_ATTEMPTS {
                    let p = outbox.pending.remove(at);
                    let message = format!("no answer after {MAX_ATTEMPTS} tries: {why}");
                    done.write(CommandDone { source: p.source, id: p.id, outcome: Outcome::Failed(message) });
                } else {
                    p.stage = Stage::Queued;
                    p.not_before = Some(Instant::now() + backoff * p.failures.max(1));
                }
            }
        }
    }
}

fn dispatch(mut outbox: ResMut<Outbox>, mut net: MessageWriter<SendNet>) {
    let now = Instant::now();
    for p in outbox.pending.iter_mut().filter(|p| p.stage == Stage::Queued) {
        if p.not_before.is_some_and(|t| t > now) {
            continue;
        }
        p.attempts += 1;
        p.stage = Stage::Sent;
        net.write(SendNet(NetCommand::Request { id: p.id, attempt: p.attempts, request: Box::new(p.request.clone()) }));
    }
}

/// The link dropped: whatever was out may or may not have landed. Send it again when `Live`, same key.
fn requeue_sent(mut outbox: ResMut<Outbox>) {
    for p in outbox.pending.iter_mut().filter(|p| p.stage == Stage::Sent) {
        p.stage = Stage::Queued;
        p.not_before = None;
    }
}

/// Makes the request and reads the answer. `Err` is a failure to get one (connection, timeout, or the server still
/// working on an earlier try with this key), which is worth sending again.
pub async fn execute(api: &Client, request: &Request) -> Result<Outcome, String> {
    let mut req = api.request_for(&request.call).header("Idempotency-Key", &request.key);
    if let Some(version) = request.if_match {
        req = req.header("If-Match", format!("\"{version}\""));
    }
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status();
    let etag = res.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
    let retry_header = res.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()?.parse().ok());
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        let version = etag
            .and_then(|e| e.trim().trim_start_matches("W/").trim_matches('"').parse().ok())
            .or_else(|| body["version"].as_i64());
        return Ok(Outcome::Ok { version });
    }
    let error = &body["error"];
    let code = error["code"].as_str().unwrap_or("");
    let message = error["message"].as_str().map_or_else(|| format!("the server answered {status}"), str::to_string);
    Ok(match (status.as_u16(), code) {
        (_, "idempotency_key_in_flight") => return Err(message),
        (500..=599, _) => return Err(message),
        (_, "version_conflict") | (409 | 412, _) => {
            Outcome::Conflict { current_version: error["current_version"].as_i64(), message }
        }
        (_, "forbidden") | (403, _) => Outcome::Forbidden(message),
        (_, "rate_limited") | (429, _) => Outcome::RateLimited {
            retry_after: error["retry_after"].as_u64().or(retry_header).map(Duration::from_secs),
            message,
        },
        _ => Outcome::Failed(message),
    })
}
