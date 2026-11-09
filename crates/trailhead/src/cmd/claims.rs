use anyhow::Result;
use jiff::Timestamp;
use trailhead_core::TaskKey;

use super::{Ctx, print_json, to_json};
use crate::client::api_err;
use crate::output::{short_time, text};

pub(crate) async fn claim(cx: &Ctx, task: &str, ttl: Option<i64>) -> Result<()> {
    let r = cx.c.api().claim(&TaskKey(task.into()), ttl).await.map_err(api_err)?;
    if cx.json {
        print_json(&to_json(&r));
        return Ok(());
    }
    println!("{}", text().task_line(&r.task));
    println!("claimed {}", text().lease(&r.claim, Timestamp::now()));
    if let Some(h) = &r.handoff {
        let at = to_json(&h.created_at);
        println!("\nlast handoff ({}):\n{}", short_time(at.as_str().unwrap_or_default()), h.body);
    }
    Ok(())
}

pub(crate) async fn mine(cx: &Ctx) -> Result<()> {
    let held = cx.c.api().my_claims().await.map_err(api_err)?;
    if cx.json {
        print_json(&to_json(&held));
        return Ok(());
    }
    if held.is_empty() {
        println!("you hold no claims");
    }
    for h in &held {
        let lease = text().lease(&h.claim, Timestamp::now());
        println!("{:<10} {:<11} {}  {}", h.key, h.status.as_str(), lease, h.title);
    }
    Ok(())
}

pub(crate) async fn renew(cx: &Ctx, task: &str, ttl: Option<i64>) -> Result<()> {
    let claim = cx.c.api().renew(&TaskKey(task.into()), ttl).await.map_err(api_err)?;
    cx.emit(&claim, |c| format!("claimed {}", text().lease(c, Timestamp::now())));
    Ok(())
}

pub(crate) async fn release(cx: &Ctx, task: &str) -> Result<()> {
    let task = cx.c.api().release(&TaskKey(task.into())).await.map_err(api_err)?;
    cx.emit(&task, |t| text().task_line(t));
    Ok(())
}
