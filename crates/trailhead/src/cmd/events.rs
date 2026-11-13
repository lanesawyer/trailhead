use anyhow::{Context, Result, bail};
use trailhead_client::http::{EventQuery, Revert, Undo, UndoBatch};

use super::{Ctx, count, params, print_json, to_json};
use crate::client::api_err;
use crate::dates::parse_when;
use crate::output::{event_line, text};

pub(crate) async fn log(cx: &Ctx, task: Option<String>, limit: i64) -> Result<()> {
    let q = EventQuery { task, newest_first: true, limit: count(Some(limit))?, ..EventQuery::default() };
    let events = cx.c.api().list_events(&q).await.map_err(api_err)?;
    if cx.json {
        print_json(&to_json(&events));
    } else {
        events.iter().for_each(|e| println!("{}", text().event_line(e)));
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
    let api = cx.c.api();
    let report = match (event_id, batch) {
        (Some(event_id), None) => api.undo(&Undo { event_id }).await,
        (None, Some(b)) => {
            api.undo_batch(&UndoBatch { batch_id: b.parse().with_context(|| format!("{b:?} isn't a batch id"))? }).await
        }
        (None, None) => {
            let me = api.me().await.map_err(api_err)?;
            let actor = me.agent.map_or(me.user.id, |a| a.id);
            let q = EventQuery {
                actor: Some(actor),
                newest_first: true,
                changes_only: true,
                limit: Some(1),
                ..EventQuery::default()
            };
            let last = api.list_events(&q).await.map_err(api_err)?;
            let Some(e) = last.first() else { bail!("nothing to undo") };
            api.undo_batch(&UndoBatch { batch_id: e.batch_id }).await
        }
        _ => bail!("give an event id or --batch, not both"),
    }
    .map_err(api_err)?;
    cx.emit(&report, |r| format!("reverted {} change(s)", r.reverted.len()));
    Ok(())
}

pub(crate) async fn revert(cx: &Ctx, actor_id: &str, since: &str) -> Result<()> {
    let revert = Revert {
        actor_id: actor_id.parse().with_context(|| format!("{actor_id:?} isn't a user or agent id"))?,
        since: parse_when(since)?,
    };
    let report = cx.c.api().revert(&revert).await.map_err(api_err)?;
    if cx.json {
        print_json(&to_json(&report));
        return Ok(());
    }
    println!("reverted {} change(s)", report.reverted.len());
    for sk in &report.skipped {
        println!("skipped {} {}: {}", sk.entity.as_str(), sk.entity_id, sk.reason);
    }
    Ok(())
}
