//! Delivers events to registered webhooks. State (cursors, backoff, the
//! delivery log) lives in core; this is the HTTP side.

use std::sync::Arc;
use std::time::Duration;

use trailhead_core::{Event, Webhook, webhook_signature};

use crate::AppState;

/// How often to look for work when nothing wakes the loop sooner.
const POLL: Duration = Duration::from_secs(2);
const BATCH: i64 = 50;
const TIMEOUT: Duration = Duration::from_secs(10);

pub fn spawn(state: Arc<AppState>) {
    let http = reqwest::Client::builder().timeout(TIMEOUT).build().expect("HTTP client");
    let mut changes = state.store.changes();
    tokio::spawn(async move {
        loop {
            deliver_due(&state, &http).await;
            tokio::select! {
                _ = changes.changed() => {}
                _ = tokio::time::sleep(POLL) => {}
            }
        }
    });
}

/// One pass over every webhook that's due.
pub async fn deliver_due(state: &AppState, http: &reqwest::Client) {
    let hooks = match state.store.webhooks_due().await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "listing webhooks failed");
            return;
        }
    };
    for (hook, secret) in hooks {
        if let Err(e) = deliver(state, http, hook, &secret).await {
            tracing::warn!(error = %e, "webhook delivery bookkeeping failed");
        }
    }
}

/// Sends a webhook's backlog in order, stopping at the first failure so events
/// are never delivered out of order.
async fn deliver(
    state: &AppState,
    http: &reqwest::Client,
    mut hook: Webhook,
    secret: &str,
) -> trailhead_core::Result<()> {
    let store = &state.store;
    loop {
        let backlog = store.webhook_backlog(&hook, BATCH).await?;
        let Some(last) = backlog.last().map(|e| e.id) else { return Ok(()) };
        let mut skipped = None;
        for e in &backlog {
            if !hook.wants(&e.kind) {
                skipped = Some(e.id);
                continue;
            }
            if let Some(id) = skipped.take() {
                store.webhook_advance(&hook, id, None).await?;
            }
            match post(http, &hook, secret, e).await {
                Ok(status) => {
                    store.webhook_advance(&hook, e.id, Some(status)).await?;
                    hook.failure_count = 0;
                }
                Err((status, error)) => {
                    tracing::info!(webhook = %hook.id, event = e.id, %error, "webhook delivery failed");
                    return store.webhook_failed(&hook, e.id, status, &error).await;
                }
            }
            hook.cursor = e.id;
        }
        if hook.cursor < last {
            store.webhook_advance(&hook, last, None).await?;
            hook.cursor = last;
        }
    }
}

async fn post(http: &reqwest::Client, hook: &Webhook, secret: &str, e: &Event) -> Result<u16, (Option<u16>, String)> {
    let body = serde_json::to_vec(e).map_err(|err| (None, err.to_string()))?;
    let signature = webhook_signature(secret, time::OffsetDateTime::now_utc().unix_timestamp(), &body);
    let res = http
        .post(&hook.url)
        .header("content-type", "application/json")
        .header("user-agent", concat!("trailhead/", env!("CARGO_PKG_VERSION")))
        .header("trailhead-webhook-id", hook.id.to_string())
        .header("trailhead-event-id", e.id.to_string())
        .header("trailhead-event-kind", &e.kind)
        .header("trailhead-signature", signature)
        .body(body)
        .send()
        .await
        .map_err(|err| (None, err.to_string()))?;
    let status = res.status().as_u16();
    if res.status().is_success() { Ok(status) } else { Err((Some(status), format!("HTTP {status}"))) }
}
