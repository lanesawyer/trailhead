use anyhow::{Result, bail};
use serde_json::json;

use super::{Ctx, params, print_json};
use crate::dates::parse_when;
use crate::output::{event_line, s};

pub(crate) async fn log(cx: &Ctx, task: Option<String>, limit: i64) -> Result<()> {
    let q = params([("newest_first", Some("true".into())), ("limit", Some(limit.to_string())), ("task", task)]);
    let v = cx.c.get("events", &q).await?;
    if cx.json {
        print_json(&v);
    } else {
        v.as_array().into_iter().flatten().for_each(|e| println!("{}", event_line(e)));
    }
    Ok(())
}

pub(crate) async fn watch(cx: &Ctx, task: Option<String>, kinds: Option<String>, after: Option<i64>) -> Result<()> {
    let q = params([("task", task), ("kinds", kinds), ("after", after.map(|a| a.to_string()))]);
    cx.c.watch(&q, |e| {
        if cx.json {
            println!("{e}");
        } else {
            println!("{}", event_line(&e));
        }
    })
    .await
}

/// Undoes an event or a batch, or with neither, the caller's last batch.
pub(crate) async fn undo(cx: &Ctx, event_id: Option<i64>, batch: Option<String>) -> Result<()> {
    let body = match (event_id, batch) {
        (Some(e), None) => json!({ "event_id": e }),
        (None, Some(b)) => json!({ "batch_id": b }),
        (None, None) => {
            let me = cx.c.get("me", &[]).await?;
            let actor = me["agent"]["id"].as_str().or(me["user"]["id"].as_str()).unwrap_or("").to_string();
            let q = [
                ("actor", actor),
                ("newest_first", "true".into()),
                ("changes_only", "true".into()),
                ("limit", "1".into()),
            ];
            let last = cx.c.get("events", &q).await?;
            let Some(b) = last[0]["batch_id"].as_str() else { bail!("nothing to undo") };
            json!({ "batch_id": b })
        }
        _ => bail!("give an event id or --batch, not both"),
    };
    let v = cx.c.post("undo", body).await?;
    cx.show(&v, |r| format!("reverted {} change(s)", r["reverted"].as_array().map_or(0, Vec::len)));
    Ok(())
}

pub(crate) async fn revert(cx: &Ctx, actor_id: &str, since: &str) -> Result<()> {
    let v = cx.c.post("revert", json!({ "actor_id": actor_id, "since": parse_when(since)? })).await?;
    if cx.json {
        print_json(&v);
        return Ok(());
    }
    println!("reverted {} change(s)", v["reverted"].as_array().map_or(0, Vec::len));
    for sk in v["skipped"].as_array().into_iter().flatten() {
        println!("skipped {} {}: changed later by event #{}", s(sk, "entity"), s(sk, "entity_id"), sk["later_event"]);
    }
    Ok(())
}
