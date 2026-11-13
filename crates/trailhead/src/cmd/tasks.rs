use std::collections::HashSet;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use trailhead_client::http::{NewTask, ReadyQuery, TaskQuery, TaskUpdate};
use trailhead_core::{Priority, Status, Task, TaskKey};

use super::{Ctx, count, names, print_json, to_json};
use crate::cli::{AddArgs, EditArgs};
use crate::client::api_err;
use crate::dates::{Field, local_today, parse_day_time};
use crate::output::{self, task_list, task_list_flagged, text};

pub(crate) async fn ready(cx: &Ctx, project: Option<String>, grade: Option<String>, limit: Option<i64>) -> Result<()> {
    let q = ReadyQuery { project: project.or(cx.project.clone()), grade, limit: count(limit)? };
    let tasks = cx.c.api().ready(&q).await.map_err(api_err)?;
    if cx.json {
        print_json(&to_json(&tasks))
    } else {
        task_list(&tasks, "nothing ready");
        // People's claims don't expire, so a forgotten one would hide work for good.
        let held = cx.c.api().my_claims().await.map_err(api_err)?;
        for h in &held {
            println!("you hold {} ({}), claimed {}", h.key, h.status.as_str(), output::lease(&h.claim));
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
    title: Option<String>,
    limit: Option<i64>,
) -> Result<()> {
    let status = match status {
        Some(s) => statuses(&s)?,
        None if all => Status::ALL.to_vec(),
        None => Vec::new(),
    };
    let q = TaskQuery {
        project: project.or(cx.project.clone()),
        status,
        parent: parent.map(TaskKey),
        title,
        scheduled_through: today.then(local_today),
        grade,
        sort: today.then(|| "scheduled".to_string()),
        limit: count(limit)?,
        ..TaskQuery::default()
    };
    let tasks: Vec<Task> = cx.c.api().list_tasks(&q).await.map_err(api_err)?.into_iter().map(|t| t.task).collect();
    if cx.json {
        print_json(&to_json(&tasks))
    } else {
        let waiting = if tasks.iter().any(|t| t.status == Status::InReview) {
            // The server returns at most 100 a page by default, so page until a short one.
            const PAGE: usize = 100;
            let mut q = TaskQuery {
                project: q.project,
                parent: q.parent,
                status: vec![Status::InReview],
                changes_requested: Some(true),
                limit: Some(PAGE),
                ..TaskQuery::default()
            };
            let mut keys = HashSet::new();
            let mut offset = 0;
            loop {
                q.offset = Some(offset);
                let page = cx.c.api().list_tasks(&q).await.map_err(api_err)?;
                keys.extend(page.iter().map(|t| t.task.key.0.clone()));
                if page.len() < PAGE {
                    break;
                }
                offset += PAGE;
            }
            keys
        } else {
            Default::default()
        };
        task_list_flagged(&tasks, "no tasks", &waiting)
    }
    Ok(())
}

/// `--status`, comma-separated, blanks dropped.
fn statuses(s: &str) -> Result<Vec<Status>> {
    s.split(',').map(str::trim).filter(|p| !p.is_empty()).map(|p| p.parse().context("--status")).collect()
}

pub(crate) async fn show(cx: &Ctx, task: &str) -> Result<()> {
    let detail = cx.c.api().show_task(&TaskKey(task.into())).await.map_err(api_err)?;
    if cx.json {
        print_json(&to_json(&detail));
        return Ok(());
    }
    println!("{}", output::detail(&detail, &names(&cx.c).await?));
    Ok(())
}

pub(crate) async fn add(cx: &Ctx, a: AddArgs) -> Result<()> {
    let project =
        a.project.or(cx.project.clone()).context("--project is required (or set project in .trailhead.toml)")?;
    let mut new = NewTask {
        project,
        title: a.title,
        description: a.description.unwrap_or_default(),
        priority: a.priority.map(priority).transpose()?,
        grade: a.grade.as_deref().map(parse_grade).transpose()?.flatten(),
        parent: a.parent,
        blocked_by: a.after,
        blocks: a.blocks,
        ..NewTask::default()
    };
    if let Some(due) = a.due {
        let v = parse_day_time(Field::Due, &due)?;
        (new.due_on, new.due_at, new.due_tz) =
            (set(&v, "due_on").flatten(), set(&v, "due_at").flatten(), set(&v, "due_tz").flatten());
    }
    if let Some(on) = a.on {
        let v = parse_day_time(Field::Scheduled, &on)?;
        (new.scheduled_on, new.scheduled_at, new.scheduled_tz) =
            (set(&v, "scheduled_on").flatten(), set(&v, "scheduled_at").flatten(), set(&v, "scheduled_tz").flatten());
    }
    let created = cx.c.api().create_task(&new).await.map_err(api_err)?;
    cx.emit(&created, |c| text().task_line(&c.task));
    if !cx.json
        && let Some(keys) = &created.possible_duplicates
    {
        println!("possible duplicates: {}", keys.join(", "));
    }
    Ok(())
}

pub(crate) async fn edit(cx: &Ctx, a: EditArgs) -> Result<()> {
    let mut update = TaskUpdate {
        title: a.title,
        description: a.description,
        priority: a.priority.map(priority).transpose()?,
        grade: a.grade.as_deref().map(parse_grade).transpose()?,
        status: a.status.as_deref().map(|s| s.parse().context("--status")).transpose()?,
        parent: a.parent.map(|p| (p != "none").then_some(p)),
        version: a.version,
        ..TaskUpdate::default()
    };
    if let Some(d) = a.due {
        if d == ["none"] {
            update.due_on = Some(None);
        } else {
            let v = parse_day_time(Field::Due, &d)?;
            (update.due_on, update.due_at, update.due_tz) = (set(&v, "due_on"), set(&v, "due_at"), set(&v, "due_tz"));
        }
    }
    if let Some(d) = a.on {
        if d == ["none"] {
            update.scheduled_on = Some(None);
        } else {
            let v = parse_day_time(Field::Scheduled, &d)?;
            (update.scheduled_on, update.scheduled_at, update.scheduled_tz) =
                (set(&v, "scheduled_on"), set(&v, "scheduled_at"), set(&v, "scheduled_tz"));
        }
    }
    if !a.meta.is_empty() {
        update.metadata = Some(meta_patch(&a.meta)?);
    }
    let task = cx.c.api().update_task(&TaskKey(a.task), &update).await.map_err(api_err)?;
    cx.emit(&task, |t| text().task_line(t));
    Ok(())
}

/// A field [`parse_day_time`] set: unset if it left it out, `Some(None)` if it set it to `null`.
fn set(fields: &Value, key: &str) -> Option<Option<String>> {
    fields.get(key).map(|v| v.as_str().map(String::from))
}

fn priority(n: i64) -> Result<Priority> {
    Priority::try_from(n).context("--priority")
}

/// Builds a JSON merge patch from `a.b=value` pairs; a value that isn't JSON is a string,
/// and so is any value for a string `factory.*` field (`factory.sandbox=123`) except `null`, which removes it.
fn meta_patch(pairs: &[String]) -> Result<Value> {
    let mut patch = json!({});
    for pair in pairs {
        let (path, raw) = pair
            .split_once('=')
            .filter(|(p, _)| !p.is_empty() && p.split('.').all(|k| !k.is_empty()))
            .ok_or_else(|| anyhow::anyhow!("--meta takes key=value (dots nest), not {pair:?}"))?;
        let mut value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
        if !value.is_null() && !value.is_string() && factory_kind(path).is_some_and(|k| matches!(k, Kind::String)) {
            value = json!(raw);
        }
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
    ("asked", Kind::Unsigned),
    ("review_rounds", Kind::Unsigned),
    ("reviewed", Kind::Unsigned),
    ("retries", Kind::Unsigned),
    ("infra_failures", Kind::Unsigned),
    ("conflict_reworks", Kind::Unsigned),
    ("started", Kind::Integer),
    ("released", Kind::Integer),
    ("unapproved_merge", Kind::Bool),
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

/// The kind of a known `factory.<field>` key (or nested path, like `factory.pr.url`), if `path` is one.
fn factory_kind(path: &str) -> Option<Kind> {
    let (_, field) = path.split_once('.').filter(|(ns, _)| *ns == "factory")?;
    FACTORY_FIELDS.iter().chain(FACTORY_NESTED).find(|(f, _)| *f == field).map(|(_, k)| *k)
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

/// Moves a task to `status`, with a handoff if there is one: both or neither.
pub(crate) async fn finish(cx: &Ctx, task: &str, status: Status, handoff: Option<String>) -> Result<()> {
    let update = TaskUpdate { status: Some(status), handoff, ..TaskUpdate::default() };
    let task = cx.c.api().update_task(&TaskKey(task.into()), &update).await.map_err(api_err)?;
    cx.emit(&task, |t| text().task_line(t));
    Ok(())
}

pub(crate) async fn cancel(cx: &Ctx, task: &str, duplicate_of: Option<String>) -> Result<()> {
    let update =
        TaskUpdate { status: Some(Status::Cancelled), duplicate_of: duplicate_of.map(Some), ..TaskUpdate::default() };
    let task = cx.c.api().update_task(&TaskKey(task.into()), &update).await.map_err(api_err)?;
    cx.emit(&task, |t| text().task_line(t));
    Ok(())
}

pub(crate) async fn rm(cx: &Ctx, task: &str) -> Result<()> {
    let task = cx.c.api().delete_task(&TaskKey(task.into())).await.map_err(api_err)?;
    cx.emit(&task, |t| format!("deleted {}", t.key));
    Ok(())
}

pub(crate) async fn block(cx: &Ctx, blocker: &str, blocked: &str) -> Result<()> {
    let dep = cx.c.api().add_blocker(&TaskKey(blocked.into()), &TaskKey(blocker.into())).await.map_err(api_err)?;
    cx.emit(&dep, |_| format!("{blocker} now blocks {blocked}"));
    Ok(())
}

pub(crate) async fn unblock(cx: &Ctx, blocker: &str, blocked: &str) -> Result<()> {
    cx.c.api().remove_blocker(&TaskKey(blocked.into()), &TaskKey(blocker.into())).await.map_err(api_err)?;
    if !cx.json {
        println!("{blocker} no longer blocks {blocked}");
    }
    Ok(())
}

pub(crate) async fn search(cx: &Ctx, query: &[String], limit: Option<i64>) -> Result<()> {
    let hits = cx.c.api().search(&query.join(" "), count(limit)?).await.map_err(api_err)?;
    if cx.json {
        print_json(&to_json(&hits))
    } else {
        let tasks: Vec<Task> = hits.into_iter().map(|h| h.task).collect();
        task_list(&tasks, "no matches")
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

    #[test]
    fn string_factory_fields_keep_raw_text() {
        let p = meta_patch(&["factory.sandbox=123".into(), "factory.failure=true".into(), "factory.host=null".into()])
            .unwrap();
        assert_eq!(p, json!({ "factory": { "sandbox": "123", "failure": "true", "host": null } }));
        assert!(meta_patch(&["factory.attempt=true".into()]).is_err());
        assert_eq!(meta_patch(&["other.x=123".into()]).unwrap(), json!({ "other": { "x": 123 } }));
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
            asked: Some(1),
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
            unapproved_merge: Some(true),
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
