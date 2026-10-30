use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{Ctx, names, params, print_json};
use crate::cli::{AddArgs, EditArgs};
use crate::dates::{Field, local_today, parse_day_time};
use crate::output::{self, comment_line, s, short_time, task_line, task_list};
use trailhead_factory::RunUpdate;

pub(crate) async fn ready(cx: &Ctx, project: Option<String>, grade: Option<String>, limit: Option<i64>) -> Result<()> {
    let q = params([
        ("project", project.or(cx.project.clone())),
        ("grade", grade),
        ("limit", limit.map(|n| n.to_string())),
    ]);
    let v = cx.c.get("ready", &q).await?;
    if cx.json {
        print_json(&v)
    } else {
        task_list(&v, "nothing ready");
        // People's claims don't expire, so a forgotten one would hide work for good.
        let held = cx.c.get("claims", &[]).await?;
        for h in held.as_array().into_iter().flatten() {
            println!("you hold {} ({}), claimed {}", s(h, "key"), s(h, "status"), output::lease(&h["claim"]));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn ls(
    cx: &Ctx,
    project: Option<String>,
    status: Option<String>,
    parent: Option<String>,
    all: bool,
    today: bool,
    grade: Option<String>,
    limit: Option<i64>,
) -> Result<()> {
    let status = status.or(all.then(|| "open,in_progress,needs_input,in_review,approved,done,cancelled".into()));
    let q = params([
        ("project", project.or(cx.project.clone())),
        ("status", status),
        ("parent", parent),
        ("scheduled_through", today.then(|| local_today().to_string())),
        ("grade", grade),
        ("sort", today.then(|| "scheduled".to_string())),
        ("limit", limit.map(|n| n.to_string())),
    ]);
    let v = cx.c.get("tasks", &q).await?;
    if cx.json {
        print_json(&v)
    } else {
        task_list(&v, "no tasks")
    }
    Ok(())
}

pub(crate) async fn show(cx: &Ctx, task: &str) -> Result<()> {
    let v = cx.c.get(&format!("tasks/{task}"), &[]).await?;
    if cx.json {
        print_json(&v);
        return Ok(());
    }
    let names = names(&cx.c).await?;
    let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.chars().take(8).collect());
    let t = &v["task"];
    println!("{}  {}", s(t, "key"), s(t, "title"));
    let mut meta = vec![s(t, "status").to_string(), format!("p{}", t["priority"]), format!("v{}", t["version"])];
    if let Some(g) = t["grade"].as_i64() {
        meta.insert(2, format!("grade {g}"));
    }
    if let Some(w) = output::when(t, "scheduled", short_time) {
        meta.push(format!("scheduled {w}"));
    }
    if let Some(w) = output::when(t, "due", short_time) {
        meta.push(format!("due {w}"));
    }
    println!("{}", meta.join(" · "));
    if v["claim"].is_object() {
        println!("claimed by {} {}", name(s(&v["claim"], "holder_id")), output::lease(&v["claim"]));
    }
    let keys = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "));
    if let Some(b) = keys("blocked_by").filter(|b| !b.is_empty()) {
        println!("blocked by {b}");
    }
    if let Some(b) = keys("blocks").filter(|b| !b.is_empty()) {
        println!("blocks {b}");
    }
    if let Some(d) = v["duplicate_of"].as_str() {
        println!("duplicate of {d}");
    }
    if let Some(d) = keys("duplicates").filter(|d| !d.is_empty()) {
        println!("duplicates {d}");
    }
    if t["metadata"].as_object().is_some_and(|m| !m.is_empty()) {
        println!("metadata {}", t["metadata"]);
    }
    if !s(t, "description").is_empty() {
        println!("\n{}", s(t, "description"));
    }
    let comments = v["comments"].as_array().cloned().unwrap_or_default();
    if !comments.is_empty() {
        println!();
        for cm in &comments {
            println!("{}", comment_line(cm, &name));
        }
    }
    Ok(())
}

pub(crate) async fn add(cx: &Ctx, a: AddArgs) -> Result<()> {
    let project =
        a.project.or(cx.project.clone()).context("--project is required (or set project in .trailhead.toml)")?;
    let mut body = json!({
        "project": project,
        "title": a.title,
        "description": a.description.unwrap_or_default(),
        "priority": a.priority,
        "grade": a.grade.as_deref().map(parse_grade).transpose()?.flatten(),
        "parent": a.parent,
        "blocked_by": a.after,
        "blocks": a.blocks,
    });
    if let Some(due) = a.due {
        merge(&mut body, parse_day_time(Field::Due, &due)?);
    }
    if let Some(on) = a.on {
        merge(&mut body, parse_day_time(Field::Scheduled, &on)?);
    }
    let created = cx.c.post("tasks", body).await?;
    cx.show(&created, task_line);
    if !cx.json
        && let Some(keys) = created["possible_duplicates"].as_array()
    {
        let keys: Vec<_> = keys.iter().filter_map(Value::as_str).collect();
        println!("possible duplicates: {}", keys.join(", "));
    }
    Ok(())
}

pub(crate) async fn edit(cx: &Ctx, a: EditArgs) -> Result<()> {
    let mut body = json!({});
    if let Some(t) = a.title {
        body["title"] = json!(t);
    }
    if let Some(d) = a.description {
        body["description"] = json!(d);
    }
    if let Some(p) = a.priority {
        body["priority"] = json!(p);
    }
    if let Some(g) = a.grade {
        body["grade"] = json!(parse_grade(&g)?);
    }
    if let Some(d) = a.due {
        if d == ["none"] {
            body["due_on"] = Value::Null;
        } else {
            merge(&mut body, parse_day_time(Field::Due, &d)?);
        }
    }
    if let Some(d) = a.on {
        if d == ["none"] {
            body["scheduled_on"] = Value::Null;
        } else {
            merge(&mut body, parse_day_time(Field::Scheduled, &d)?);
        }
    }
    if let Some(st) = a.status {
        body["status"] = json!(st);
    }
    if let Some(p) = a.parent {
        body["parent"] = if p == "none" { Value::Null } else { json!(p) };
    }
    if !a.meta.is_empty() {
        body["metadata"] = meta_patch(&a.meta)?;
    }
    if let Some(v) = a.version {
        body["version"] = json!(v);
    }
    cx.show(&cx.c.patch(&format!("tasks/{}", a.task), body).await?, task_line);
    Ok(())
}

/// Builds a JSON merge patch from `a.b=value` pairs; a value that isn't JSON is a string.
fn meta_patch(pairs: &[String]) -> Result<Value> {
    let mut patch = json!({});
    for pair in pairs {
        let (path, raw) = pair
            .split_once('=')
            .filter(|(p, _)| !p.is_empty() && p.split('.').all(|k| !k.is_empty()))
            .ok_or_else(|| anyhow::anyhow!("--meta takes key=value (dots nest), not {pair:?}"))?;
        let value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
        check_factory_field(path, &value)?;
        let mut at = &mut patch;
        for key in path.split('.') {
            if !at.is_object() {
                *at = json!({});
            }
            at = at.as_object_mut().unwrap().entry(key).or_insert(Value::Null);
        }
        *at = value;
    }
    Ok(patch)
}

/// Rejects a value for a `factory.<field>` key the factory couldn't read back as a `RunUpdate` field, such as
/// `factory.attempt_base=3x`. `null` removes a field, so it always passes; deeper paths and unknown keys are left alone.
fn check_factory_field(path: &str, value: &Value) -> Result<()> {
    let Some(("factory", field)) = path.split_once('.') else {
        return Ok(());
    };
    if field.contains('.') || value.is_null() {
        return Ok(());
    }
    serde_json::from_value::<RunUpdate>(json!({ field: value }))
        .map(drop)
        .map_err(|e| anyhow::anyhow!("--meta {path}: {e}"))
}

/// A grade is 1 to 4; "none" clears it. The rules live in core.
fn parse_grade(g: &str) -> Result<Option<i64>> {
    trailhead_core::parse_grade(g).context("--grade")
}

fn merge(body: &mut Value, fields: Value) {
    if let (Some(body), Value::Object(fields)) = (body.as_object_mut(), fields) {
        body.extend(fields);
    }
}

/// Moves a task to `status`, leaving a handoff first if there is one.
pub(crate) async fn finish(cx: &Ctx, task: &str, status: &str, handoff: Option<String>) -> Result<()> {
    if let Some(m) = handoff {
        cx.c.post(&format!("tasks/{task}/comments"), json!({ "kind": "handoff", "body": m })).await?;
    }
    cx.show(&cx.c.patch(&format!("tasks/{task}"), json!({ "status": status })).await?, task_line);
    Ok(())
}

pub(crate) async fn cancel(cx: &Ctx, task: &str, duplicate_of: Option<String>) -> Result<()> {
    let mut body = json!({ "status": "cancelled" });
    if let Some(d) = duplicate_of {
        body["duplicate_of"] = json!(d);
    }
    cx.show(&cx.c.patch(&format!("tasks/{task}"), body).await?, task_line);
    Ok(())
}

pub(crate) async fn rm(cx: &Ctx, task: &str) -> Result<()> {
    cx.show(&cx.c.delete(&format!("tasks/{task}")).await?, |t| format!("deleted {}", s(t, "key")));
    Ok(())
}

pub(crate) async fn block(cx: &Ctx, blocker: &str, blocked: &str) -> Result<()> {
    let v = cx.c.post(&format!("tasks/{blocked}/blockers"), json!({ "blocker": blocker })).await?;
    cx.show(&v, |_| format!("{blocker} now blocks {blocked}"));
    Ok(())
}

pub(crate) async fn unblock(cx: &Ctx, blocker: &str, blocked: &str) -> Result<()> {
    cx.c.delete(&format!("tasks/{blocked}/blockers/{blocker}")).await?;
    if !cx.json {
        println!("{blocker} no longer blocks {blocked}");
    }
    Ok(())
}

pub(crate) async fn search(cx: &Ctx, query: &[String], limit: Option<i64>) -> Result<()> {
    let q = params([("q", Some(query.join(" "))), ("limit", limit.map(|n| n.to_string()))]);
    let v = cx.c.get("search", &q).await?;
    if cx.json {
        print_json(&v)
    } else {
        task_list(&v, "no matches")
    }
    Ok(())
}
