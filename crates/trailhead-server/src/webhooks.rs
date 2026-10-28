//! Delivers events to registered webhooks. State (cursors, backoff, the
//! delivery log) lives in core; this is the HTTP side.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use trailhead_core::{Event, Webhook, webhook_signature};

use crate::AppState;

/// How often to look for work when nothing wakes the loop sooner.
const POLL: Duration = Duration::from_secs(2);
const BATCH: i64 = 50;
const TIMEOUT: Duration = Duration::from_secs(10);

/// Where webhooks may post. A hosted server must not let a workspace point a
/// webhook at its own network (cloud metadata, internal services).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Targets {
    Any,
    PublicOnly,
}

/// The HTTP client for deliveries. With `PublicOnly`, names resolve only to public
/// addresses (checked at connect time, so DNS rebinding can't slip past), redirects
/// aren't followed and proxies from the environment are ignored.
pub fn client(targets: Targets) -> reqwest::Client {
    let builder = reqwest::Client::builder().timeout(TIMEOUT);
    let builder = match targets {
        Targets::Any => builder,
        Targets::PublicOnly => {
            builder.dns_resolver(PublicResolver).redirect(reqwest::redirect::Policy::none()).no_proxy()
        }
    };
    builder.build().expect("HTTP client")
}

struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.filter(|a| is_public(a.ip())).collect();
            if addrs.is_empty() {
                return Err(format!("{host} doesn't resolve to a public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// False for loopback, private, link-local (cloud metadata), shared, reserved and
/// other non-global ranges.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

/// Only global unicast (2000::/3) can be public. Anything else, including reserved ranges such as the `0fdf::1`
/// a Sprite maps `localhost` to, is not.
fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    (s[0] & 0xe000) == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8)
}

pub fn spawn(state: Arc<AppState>) {
    let http = client(state.webhook_targets);
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
            match post(http, state.webhook_targets, &hook, secret, e).await {
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

async fn post(
    http: &reqwest::Client,
    targets: Targets,
    hook: &Webhook,
    secret: &str,
    e: &Event,
) -> Result<u16, (Option<u16>, String)> {
    // IP literals never reach the resolver, so check them here.
    if targets == Targets::PublicOnly {
        let url = reqwest::Url::parse(&hook.url).map_err(|err| (None, err.to_string()))?;
        let host = url.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = host.parse::<IpAddr>()
            && !is_public(ip)
        {
            return Err((None, format!("{ip} is not a public address")));
        }
    }
    let body = serde_json::to_vec(e).map_err(|err| (None, err.to_string()))?;
    let signature = webhook_signature(secret, jiff::Timestamp::now().as_second(), &body);
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
        .map_err(|err| (None, with_causes(&err)))?;
    let status = res.status().as_u16();
    if res.status().is_success() { Ok(status) } else { Err((Some(status), format!("HTTP {status}"))) }
}

/// reqwest's own message is just "error sending request"; the reason is in its sources.
fn with_causes(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut cause = err.source();
    while let Some(c) = cause {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cause = c.source();
    }
    out
}
