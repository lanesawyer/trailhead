use anyhow::Result;
use serde_json::{Value, json};

use super::{Ctx, print_json};
use crate::cli::WebhookCmd;
use crate::output::{s, short_time};

pub(crate) async fn webhook(cx: &Ctx, cmd: WebhookCmd) -> Result<()> {
    let c = &cx.c;
    match cmd {
        WebhookCmd::Ls => {
            let v = c.get("webhooks", &[]).await?;
            if cx.json {
                print_json(&v);
                return Ok(());
            }
            for w in v.as_array().into_iter().flatten() {
                let kinds = w["event_kinds"]
                    .as_array()
                    .map(|k| k.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","));
                let state = match (w["disabled_at"].as_str(), w["failure_count"].as_i64()) {
                    (Some(_), _) => "disabled".to_string(),
                    (None, Some(n)) if n > 0 => format!("failing ({n})"),
                    _ => "ok".to_string(),
                };
                let kinds = kinds.filter(|k| !k.is_empty()).unwrap_or_else(|| "all events".into());
                println!("{}  {}  [{kinds}]  {state}", s(w, "id"), s(w, "url"));
            }
        }
        WebhookCmd::Add { target, kinds } => {
            let v = c.post("webhooks", json!({ "url": target, "event_kinds": kinds })).await?;
            if cx.json {
                print_json(&v);
            } else {
                println!("webhook {}", s(&v["webhook"], "id"));
                println!("signing secret (shown once): {}", s(&v, "secret"));
            }
        }
        WebhookCmd::Rm { id } => {
            c.delete(&format!("webhooks/{id}")).await?;
            if !cx.json {
                println!("deleted");
            }
        }
        WebhookCmd::Log { id, limit } => {
            let v = c.get(&format!("webhooks/{id}/deliveries"), &[("limit", limit.to_string())]).await?;
            if cx.json {
                print_json(&v);
                return Ok(());
            }
            for d in v.as_array().into_iter().flatten() {
                let outcome = match (d["status_code"].as_i64(), d["error"].as_str()) {
                    (Some(code), None) => code.to_string(),
                    (code, Some(err)) => format!("failed {} {err}", code.map(|c| c.to_string()).unwrap_or_default()),
                    _ => "?".into(),
                };
                println!("{}  event #{}  {outcome}", short_time(s(d, "at")), d["event_id"]);
            }
        }
    }
    Ok(())
}
