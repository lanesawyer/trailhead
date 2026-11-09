use anyhow::{Context, Result};
use serde_json::{Value, json};
use trailhead_core::{CommentKind, Status};

use super::{Ctx, names, params, print_json};
use crate::cli::{AddArgs, EditArgs};
use crate::dates::{Field, local_today, parse_day_time};
use crate::output::{self, s, task_line, task_list, task_list_flagged};

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
        let waiting = if v.as_array().into_iter().flatten().any(|t| t["status"] == "in_review") {
            let mut q = q;
            q.retain(|(k, _)| ["project", "parent"].contains(k));
            q.push(("status", "in_review".into()));
            q.push(("changes_requested", "true".into()));
            let asked = cx.c.get("tasks", &q).await?;
            asked.as_array().into_iter().flatten().map(|t| s(t, "key").to_string()).collect()
        } else {
            Default::default()
        };
        task_list_flagged(&v, "no tasks", &waiting)
    }
    Ok(())
}

pub(crate) async fn show(cx: &Ctx, task: &str) -> Result<()> {
    let v = cx.c.get(&format!("tasks/{task}"), &[]).await?;
    if cx.json {
        print_json(&v);
        return Ok(());
    }
    println!("{}", output::detail(&v, &names(&cx.c).await?));
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

/// The JSON kind a scalar `factory.<field>` takes. The CLI can't depend on `trailhead-factory` (it isn't published),
/// so a test checks this table against `RunUpdate`.
const FACTORY_FIELDS: &[(&str, Kind)] = &[
    ("attempt", Kind::Unsigned),
    ("attempt_base", Kind::Unsigned),
    ("gave_up", Kind::Unsigned),
    ("review_rounds", Kind::Unsigned),
    ("reviewed", Kind::Unsigned),
    ("retries", Kind::Unsigned),
    ("infra_failures", Kind::Unsigned),
    ("conflict_reworks", Kind::Unsigned),
    ("started", Kind::Integer),
    ("released", Kind::Integer),
    ("sandbox", Kind::String),
    ("host", Kind::String),
    ("target", Kind::String),
    ("profile", Kind::String),
    ("failure", Kind::String),
    ("reviewed_head", Kind::String),
    ("merging", Kind::String),
    ("stale_head", Kind::String),
    ("concluded", Kind::String),
    ("ungraded", Kind::Bool),
];

#[derive(Clone, Copy)]
enum Kind {
    Unsigned,
    Unsigned64,
    Integer,
    String,
    Bool,
    Object,
}

impl Kind {
    fn accepts(self, v: &Value) -> bool {
        match self {
            Kind::Unsigned => v.as_u64().is_some_and(|n| u32::try_from(n).is_ok()),
            Kind::Unsigned64 => v.as_u64().is_some(),
            Kind::Integer => v.as_i64().is_some(),
            Kind::String => v.is_string(),
            Kind::Bool => v.is_boolean(),
            Kind::Object => v.is_object(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Unsigned | Kind::Unsigned64 => "a non-negative integer",
            Kind::Integer => "an integer",
            Kind::String => "a string",
            Kind::Bool => "true or false",
            Kind::Object => "an object",
        }
    }
}

/// `pr`, the one structured field whose leaves have a fixed kind, and those leaves, by dotted path.
const FACTORY_NESTED: &[(&str, Kind)] = &[
    ("pr", Kind::Object),
    ("pr.repo", Kind::Object),
    ("pr.number", Kind::Unsigned64),
    ("pr.url", Kind::String),
    ("pr.repo.owner", Kind::String),
    ("pr.repo.name", Kind::String),
];

/// Rejects a value for a known `factory.<field>` key (or nested path, like `factory.pr.number`) of the wrong kind,
/// such as `factory.attempt_base=3x` or `factory.attempt_base={"a":1}`, which the factory couldn't read back. A
/// whole-object `factory={...}` value is checked field by field, and anything but an object or `null` is rejected.
/// `null` removes a field, so it always passes; other structured fields and unknown keys are left alone. The check
/// is client-side and can't see the stored task, so `factory={"pr":{"number":7}}` passes even where there is no `pr`
/// yet to merge into, and would store a partial one.
fn check_factory_field(path: &str, value: &Value) -> Result<()> {
    match path.split_once('.') {
        Some(("factory", field)) => {
            check_factory_prefixes(field)?;
            check_factory_leaves(path, field, value)
        }
        None if path == "factory" => match value {
            Value::Null => Ok(()),
            Value::Object(map) => map.iter().try_for_each(|(k, v)| check_factory_leaves(&format!("{path}.{k}"), k, v)),
            _ => anyhow::bail!("--meta factory must be an object, not {value}"),
        },
        _ => Ok(()),
    }
}

/// Rejects a dotted `field` that goes under a known non-object field, like `attempt_base.a` or `pr.number.x`, which
/// as a merge patch would replace the scalar with an object.
fn check_factory_prefixes(field: &str) -> Result<()> {
    for (i, _) in field.match_indices('.') {
        let prefix = &field[..i];
        if let Some((_, kind)) = FACTORY_FIELDS.iter().chain(FACTORY_NESTED).find(|(f, _)| *f == prefix)
            && !matches!(kind, Kind::Object)
        {
            anyhow::bail!("--meta factory.{prefix} is {}, so factory.{field} can't be set", kind.name());
        }
    }
    Ok(())
}

/// Checks `value` against the kind of `field` if it's a known path, then descends into an object, so an unknown
/// field holding an object still has its known leaves checked.
fn check_factory_leaves(path: &str, field: &str, value: &Value) -> Result<()> {
    if let Some((_, kind)) = FACTORY_FIELDS.iter().chain(FACTORY_NESTED).find(|(f, _)| *f == field)
        && !value.is_null()
        && !kind.accepts(value)
    {
        anyhow::bail!("--meta {path} must be {}, not {value}", kind.name());
    }
    match value {
        Value::Object(map) => {
            map.iter().try_for_each(|(k, v)| check_factory_leaves(&format!("{path}.{k}"), &format!("{field}.{k}"), v))
        }
        _ => Ok(()),
    }
}

/// A grade is 1 to 4; "none" clears it. The rules live in core.
fn parse_grade(g: &str) -> Result<Option<trailhead_core::Grade>> {
    trailhead_core::parse_grade(g).context("--grade")
}

fn merge(body: &mut Value, fields: Value) {
    if let (Some(body), Value::Object(fields)) = (body.as_object_mut(), fields) {
        body.extend(fields);
    }
}

/// Moves a task to `status`, leaving a handoff first if there is one.
pub(crate) async fn finish(cx: &Ctx, task: &str, status: Status, handoff: Option<String>) -> Result<()> {
    if let Some(m) = handoff {
        cx.c.post(&format!("tasks/{task}/comments"), json!({ "kind": CommentKind::Handoff, "body": m })).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps `FACTORY_FIELDS` in step with `RunUpdate`: a sample of each kind must set that field.
    #[test]
    fn factory_fields_match_run_update() {
        for (field, kind) in FACTORY_FIELDS {
            let sample = match kind {
                Kind::Unsigned | Kind::Unsigned64 | Kind::Integer => json!(3),
                Kind::String => json!("x"),
                Kind::Bool => json!(true),
                Kind::Object => unreachable!(),
            };
            assert!(kind.accepts(&sample), "{field}");
            let run: trailhead_factory::RunUpdate = serde_json::from_value(json!({ *field: sample.clone() })).unwrap();
            let back = serde_json::to_value(&run).unwrap();
            assert_eq!(back, json!({ *field: sample }), "{field} isn't a {} on RunUpdate", kind.name());
        }
    }

    /// Fields of `RunUpdate` that hold objects, which `FACTORY_FIELDS` leaves to the factory to read.
    const STRUCTURED: &[&str] =
        &["pr", "spend", "changes_requested", "merged", "grade", "merge", "review_breach", "worker_breach"];

    /// A new scalar field on `RunUpdate` must be added to `FACTORY_FIELDS`, or to `STRUCTURED` if it isn't a scalar.
    /// The struct literal has no `..`, so a new field also fails to compile here until it is populated.
    #[test]
    fn every_scalar_run_update_field_is_in_factory_fields() {
        let pr = json!({ "repo": { "owner": "o", "name": "n" }, "number": 1, "url": "u" });
        let full = trailhead_factory::RunUpdate {
            attempt: Some(1),
            sandbox: Some("x".into()),
            host: Some("x".into()),
            target: Some("x".into()),
            started: Some(1),
            profile: Some("x".into()),
            pr: Some(serde_json::from_value(pr.clone()).unwrap()),
            review_rounds: Some(1),
            reviewed: Some(1),
            spend: Some(trailhead_factory::Usd(1)),
            concluded: Some("x".into()),
            failure: Some("x".into()),
            retries: Some(1),
            infra_failures: Some(1),
            changes_requested: Some(
                serde_json::from_value(json!({ "reviewer": "r", "body": "b", "attempt": 1 })).unwrap(),
            ),
            review_breach: Some(trailhead_factory::ReviewBreach {
                round: 1,
                attempt: 1,
                reason: "x".into(),
                parked: false,
            }),
            worker_breach: Some(trailhead_factory::WorkerBreach {
                run: "x".into(),
                reason: "x".into(),
                parked: false,
                stuck: false,
            }),
            conflict_reworks: Some(1),
            attempt_base: Some(1),
            gave_up: Some(1),
            merged: Some(serde_json::from_value(json!({ "by": "human", "pr": pr })).unwrap()),
            grade: Some(serde_json::from_value(json!({ "grade": 2 })).unwrap()),
            ungraded: Some(true),
            reviewed_head: Some("x".into()),
            merge: Some(
                serde_json::from_value(json!({
                    "grade": 2, "head": "h", "passed": [], "waiting": [], "would_merge": false,
                    "approved_head": null, "human_head": null, "problem": null, "message": "m"
                }))
                .unwrap(),
            ),
            merging: Some("x".into()),
            stale_head: Some("x".into()),
            released: Some(1),
        };
        let Value::Object(map) = serde_json::to_value(&full).unwrap() else { panic!("RunUpdate isn't an object") };
        for key in map.keys() {
            let known = FACTORY_FIELDS.iter().any(|(f, _)| f == key) || STRUCTURED.contains(&key.as_str());
            assert!(known, "RunUpdate.{key} is in neither FACTORY_FIELDS nor STRUCTURED");
        }
        for key in STRUCTURED {
            assert!(map.contains_key(*key), "STRUCTURED lists {key}, which RunUpdate doesn't serialize");
        }
    }

    /// `FACTORY_NESTED` matches the leaves of `PrRef`.
    #[test]
    fn factory_nested_match_pr_ref() {
        let pr = json!({ "repo": { "owner": "o", "name": "n" }, "number": 3, "url": "u" });
        let parsed: trailhead_factory::PrRef = serde_json::from_value(pr.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), pr);
        for (path, kind) in FACTORY_NESTED {
            let root = json!({ "pr": pr });
            let leaf = path.split('.').fold(&root, |v, k| &v[k]);
            assert!(kind.accepts(leaf), "{path}");
        }
    }

    #[test]
    fn meta_patch_checks_whole_object_and_nested_factory_paths() {
        let bad = |p: &str| meta_patch(&[p.to_string()]).unwrap_err().to_string();
        assert!(bad(r#"factory={"attempt_base":"3x"}"#).contains("factory.attempt_base"));
        assert!(bad("factory.pr.number=x").contains("factory.pr.number"));
        assert!(bad(r#"factory={"pr":{"number":"x"}}"#).contains("factory.pr.number"));
        assert!(bad(r#"factory.attempt_base={"a":1}"#).contains("factory.attempt_base"));
        assert!(bad(r#"factory={"attempt_base":{"a":1}}"#).contains("factory.attempt_base"));
        assert!(bad(r#"factory={"pr":{"number":{}}}"#).contains("factory.pr.number"));
        assert!(bad("factory.attempt_base.a=1").contains("factory.attempt_base"));
        assert!(bad("factory.pr.number.x=1").contains("factory.pr.number"));
        assert!(bad("factory.pr.url.x=1").contains("factory.pr.url"));
        assert!(bad("factory.pr=5").contains("factory.pr"));
        assert!(bad(r#"factory.pr.repo="x""#).contains("factory.pr.repo"));
        assert!(bad(r#"factory={"pr":5}"#).contains("factory.pr"));
        assert!(bad("factory=3").contains("factory"));
        assert!(meta_patch(&[r#"factory={"attempt_base":3,"pr":{"number":7},"other":"x"}"#.into()]).is_ok());
        assert!(meta_patch(&["factory.pr=null".into(), "factory=null".into()]).is_ok());
    }
}
