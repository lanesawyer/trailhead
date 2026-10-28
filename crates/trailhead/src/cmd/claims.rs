use anyhow::Result;
use serde_json::json;

use super::{Ctx, print_json};
use crate::output::{lease, s, short_time, task_line};

pub(crate) async fn claim(cx: &Ctx, task: &str, ttl: Option<i64>) -> Result<()> {
    let v = cx.c.post(&format!("tasks/{task}/claim"), json!({ "ttl": ttl })).await?;
    if cx.json {
        print_json(&v);
        return Ok(());
    }
    println!("{}", task_line(&v["task"]));
    println!("claimed {}", lease(&v["claim"]));
    if v["handoff"].is_object() {
        println!("\nlast handoff ({}):\n{}", short_time(s(&v["handoff"], "created_at")), s(&v["handoff"], "body"));
    }
    Ok(())
}

pub(crate) async fn mine(cx: &Ctx) -> Result<()> {
    let v = cx.c.get("claims", &[]).await?;
    if cx.json {
        print_json(&v);
        return Ok(());
    }
    match v.as_array() {
        Some(held) if !held.is_empty() => {
            for h in held {
                println!("{:<10} {:<11} {}  {}", s(h, "key"), s(h, "status"), lease(&h["claim"]), s(h, "title"));
            }
        }
        _ => println!("you hold no claims"),
    }
    Ok(())
}

pub(crate) async fn renew(cx: &Ctx, task: &str, ttl: Option<i64>) -> Result<()> {
    let v = cx.c.post(&format!("tasks/{task}/renew"), json!({ "ttl": ttl })).await?;
    cx.show(&v, |v| format!("claimed {}", lease(v)));
    Ok(())
}

pub(crate) async fn release(cx: &Ctx, task: &str) -> Result<()> {
    cx.show(&cx.c.post(&format!("tasks/{task}/release"), json!({})).await?, task_line);
    Ok(())
}
