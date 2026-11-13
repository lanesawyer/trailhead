use anyhow::Result;
use trailhead_client::http::NewWebhook;

use super::{Ctx, count, print_json, to_json};
use crate::cli::WebhookCmd;
use crate::client::api_err;
use crate::output::short_time;

pub(crate) async fn webhook(cx: &Ctx, cmd: WebhookCmd) -> Result<()> {
    let api = cx.c.api();
    match cmd {
        WebhookCmd::Ls => {
            let hooks = api.list_webhooks().await.map_err(api_err)?;
            if cx.json {
                print_json(&to_json(&hooks));
                return Ok(());
            }
            for w in &hooks {
                let state = match (w.disabled_at, w.failure_count) {
                    (Some(_), _) => "disabled".to_string(),
                    (None, n) if n > 0 => format!("failing ({n})"),
                    _ => "ok".to_string(),
                };
                let kinds = if w.event_kinds.is_empty() { "all events".into() } else { w.event_kinds.join(",") };
                println!("{}  {}  [{kinds}]  {state}", w.id, w.url);
            }
        }
        WebhookCmd::Add { target, kinds } => {
            let created = api.create_webhook(&NewWebhook { url: target, event_kinds: kinds }).await.map_err(api_err)?;
            if cx.json {
                print_json(&to_json(&created));
            } else {
                println!("webhook {}", created.webhook.id);
                println!("signing secret (shown once): {}", created.secret);
            }
        }
        WebhookCmd::Rm { id } => {
            api.delete_webhook(&id).await.map_err(api_err)?;
            if !cx.json {
                println!("deleted");
            }
        }
        WebhookCmd::Log { id, limit } => {
            let limit = count(Some(limit))?.unwrap_or_default();
            let deliveries = api.webhook_deliveries(&id, limit).await.map_err(api_err)?;
            if cx.json {
                print_json(&to_json(&deliveries));
                return Ok(());
            }
            for d in &deliveries {
                let outcome = match (d.status_code, &d.error) {
                    (Some(code), None) => code.to_string(),
                    (code, Some(err)) => format!("failed {} {err}", code.map(|c| c.to_string()).unwrap_or_default()),
                    _ => "?".into(),
                };
                let event = d.event_id.map_or("null".to_string(), |e| e.to_string());
                println!("{}  event #{event}  {outcome}", short_time(&d.at.to_string()));
            }
        }
    }
    Ok(())
}
