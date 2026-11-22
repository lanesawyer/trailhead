//! The planner's run (`docs/app-platform.md`, "Planning an ask"): what a turn is told, what it hands back, and how
//! the control plane makes the engine's plan match it. The agent only proposes; [`changes`] decides what is written.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use trailhead_model::{ProposedGrade, Spec, TASK_PLAN_KEY, TaskPlan};

use crate::{Grade, Status, TaskKey};

/// One task of a plan as the engine holds it: a direct child of the spec task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTask {
    pub key: TaskKey,
    pub title: String,
    pub description: String,
    pub status: Status,
    /// A person's grade; a planner's task has none until the plan is approved.
    pub grade: Option<Grade>,
    pub proposed: Option<ProposedGrade>,
    /// The unfinished tasks it waits on.
    #[serde(default)]
    pub blocked_by: Vec<TaskKey>,
}

impl PlanTask {
    /// Whether the planner may still change it: open and ungraded. A graded or started one is a person's now.
    pub fn is_draft(&self) -> bool {
        self.grade.is_none() && self.status == Status::Open
    }
}

/// What a planner's turn is told beyond the task: the ask as first written, and the plan as the engine holds it now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Planning {
    /// The person's ask. The spec task's description becomes the spec after the first plan, so the ask is kept apart.
    pub ask: String,
    pub plan: Vec<PlanTask>,
}

/// Where a planner's turn is written and its session kept, on the sandbox's disk outside the checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFiles {
    /// The JSON file the agent ends each turn by writing ([`PlanTurn`]).
    pub turn: PathBuf,
    /// The harness's own note of the agent's session, so the next turn can resume it.
    pub session: PathBuf,
}

impl PlanFiles {
    /// The files for one task, in `dir`.
    pub fn in_dir(dir: &Path) -> PlanFiles {
        PlanFiles { turn: dir.join("turn.json"), session: dir.join("session") }
    }
}

/// [`Planning`] with the files, as the harness gets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanContext {
    pub planning: Planning,
    pub files: PlanFiles,
}

/// One turn of a planner, as the agent writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTurn {
    /// What the person reads: questions, or a summary of the plan.
    pub message: String,
    /// The spec, which replaces the spec task's description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// The whole plan; absent leaves it as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<Vec<ProposedTask>>,
    /// The plan is ready to approve.
    #[serde(default)]
    pub ready: bool,
}

/// A task in a planner's plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedTask {
    /// The planner's handle for a task it adds.
    #[serde(default)]
    pub id: Option<String>,
    /// The key of a task that already exists.
    #[serde(default)]
    pub key: Option<TaskKey>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Handles or keys of what it waits on.
    #[serde(default)]
    pub blocked_by: Vec<String>,
    pub grade: Grade,
    #[serde(default)]
    pub why: String,
}

impl PlanTurn {
    /// A turn read from what the agent wrote, or why it can't be one.
    pub fn parse(text: &str) -> std::result::Result<PlanTurn, String> {
        let turn: PlanTurn = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if turn.message.trim().is_empty() {
            return Err("`message` is empty".into());
        }
        if let Some(t) = turn.tasks.iter().flatten().find(|t| t.title.trim().is_empty()) {
            return Err(format!("a task ({}) has no title", t.id.as_deref().unwrap_or("without an id")));
        }
        Ok(turn)
    }
}

/// A task the factory adds to a plan: a child of the spec task, ungraded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPlanTask {
    pub title: String,
    pub description: String,
    pub proposed: ProposedGrade,
    pub blocked_by: Vec<TaskKey>,
}

/// A change to a task of the plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanTaskUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub proposed: Option<ProposedGrade>,
    pub add_blockers: Vec<TaskKey>,
    pub remove_blockers: Vec<TaskKey>,
}

impl PlanTaskUpdate {
    pub fn is_empty(&self) -> bool {
        *self == PlanTaskUpdate::default()
    }
}

/// A task to add, by the planner's handle, with the handles of the other new tasks it waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Creation {
    pub handle: String,
    pub task: NewPlanTask,
    /// New tasks it waits on, created before it.
    pub after: Vec<String>,
}

/// A task to change once the new tasks exist: blockers among the new ones are named by handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub key: TaskKey,
    pub update: PlanTaskUpdate,
    pub add_new: Vec<String>,
}

/// What a turn's plan does to the engine's, in the order it is written: the new tasks (each after the new ones it
/// waits on), then the changes to the existing ones, then the cancels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanChanges {
    pub create: Vec<Creation>,
    pub revise: Vec<Revision>,
    /// New tasks that wait on a task outside the plan, added once they exist.
    pub external: Vec<(String, Vec<TaskKey>)>,
    pub cancel: Vec<TaskKey>,
    /// What the turn asked for that the factory didn't do, and why.
    pub warnings: Vec<String>,
}

/// Where an edge's ends are: a task the planner may change, by key, or one it adds, by handle.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Node {
    Old(TaskKey),
    New(String),
}

/// What a planner's `tasks` do to the plan in the engine (`current`: the spec task's direct children). An open,
/// ungraded child is the planner's: listed with its key it is changed to match, left out it is cancelled. Any other
/// child is a person's and is never touched; a key naming one is ignored with a warning. A listed task without a key
/// is added. Dependencies between the planner's tasks are set to what is listed; one naming nothing it knows, or that
/// would close a cycle, is dropped with a warning. A dependency on a task outside the plan is added, never removed.
pub fn changes(current: &[PlanTask], tasks: &[ProposedTask]) -> PlanChanges {
    let mut out = PlanChanges::default();
    let drafts: BTreeMap<&TaskKey, &PlanTask> = current.iter().filter(|t| t.is_draft()).map(|t| (&t.key, t)).collect();
    let mut listed: Vec<(Node, &ProposedTask)> = Vec::new();
    let mut seen: BTreeSet<Node> = BTreeSet::new();
    for (i, t) in tasks.iter().enumerate() {
        let node = match &t.key {
            Some(key) if drafts.contains_key(key) => Node::Old(key.clone()),
            Some(key) => {
                out.warnings.push(format!("{key} isn't an open, ungraded task of this plan, so it was left as it is"));
                continue;
            }
            None => Node::New(t.id.clone().filter(|id| !id.trim().is_empty()).unwrap_or_else(|| format!("#{}", i + 1))),
        };
        if !seen.insert(node.clone()) {
            out.warnings.push(format!("{} is listed twice; the first is kept", name(&node)));
            continue;
        }
        listed.push((node, t));
    }
    let handles: HashMap<&str, Node> = listed
        .iter()
        .flat_map(|(node, t)| {
            let by_id = t.id.as_deref().map(|id| (id, node.clone()));
            let by_key = t.key.as_ref().map(|k| (k.as_str(), node.clone()));
            by_id.into_iter().chain(by_key)
        })
        .collect();
    // Edges among the plan's own tasks, kept in listed order while they don't close a cycle.
    let mut inner: BTreeMap<Node, Vec<Node>> = BTreeMap::new();
    let mut outer: BTreeMap<Node, Vec<TaskKey>> = BTreeMap::new();
    for (node, t) in &listed {
        for b in &t.blocked_by {
            let b = b.trim();
            match handles.get(b) {
                Some(blocker) if blocker == node => {
                    out.warnings.push(format!("{} can't wait on itself", name(node)));
                }
                Some(blocker) if reaches(&inner, blocker, node) => out.warnings.push(format!(
                    "{} waiting on {} would close a cycle, so that dependency was dropped",
                    name(node),
                    name(blocker)
                )),
                Some(blocker) => {
                    let edges = inner.entry(node.clone()).or_default();
                    if !edges.contains(blocker) {
                        edges.push(blocker.clone());
                    }
                }
                None if looks_like_key(b) && !drafts.contains_key(&TaskKey::from(b)) => {
                    outer.entry(node.clone()).or_default().push(TaskKey::from(b));
                }
                None => {
                    out.warnings.push(format!("{} waits on `{b}`, which names no task, so it was dropped", name(node)))
                }
            }
        }
    }
    for (node, t) in &listed {
        let proposed = ProposedGrade { grade: t.grade, why: t.why.trim().to_string() };
        let blockers = inner.get(node).cloned().unwrap_or_default();
        let external = outer.get(node).cloned().unwrap_or_default();
        match node {
            Node::New(handle) => {
                let blocked_by = blockers.iter().filter_map(old_key).cloned().collect();
                let after = blockers.iter().filter_map(new_handle).map(str::to_string).collect();
                let task = NewPlanTask {
                    title: t.title.trim().to_string(),
                    description: t.description.clone(),
                    proposed,
                    blocked_by,
                };
                out.create.push(Creation { handle: handle.clone(), task, after });
                if !external.is_empty() {
                    out.external.push((handle.clone(), external));
                }
            }
            Node::Old(key) => out.revise.push(revision(drafts[key], t, proposed, &blockers, external, &drafts)),
        }
    }
    out.create = in_dependency_order(std::mem::take(&mut out.create));
    out.revise.retain(|r| !r.update.is_empty() || !r.add_new.is_empty());
    let kept: BTreeSet<&TaskKey> = listed.iter().filter_map(|(n, _)| old_key(n)).collect();
    out.cancel = drafts.keys().filter(|k| !kept.contains(*k)).map(|k| (*k).clone()).collect();
    out
}

fn revision(
    was: &PlanTask,
    t: &ProposedTask,
    proposed: ProposedGrade,
    blockers: &[Node],
    external: Vec<TaskKey>,
    drafts: &BTreeMap<&TaskKey, &PlanTask>,
) -> Revision {
    let title = t.title.trim();
    let wanted: Vec<&TaskKey> = blockers.iter().filter_map(old_key).collect();
    let mut add_blockers: Vec<TaskKey> =
        wanted.iter().filter(|k| !was.blocked_by.contains(k)).map(|k| (*k).clone()).collect();
    add_blockers.extend(external.into_iter().filter(|k| !was.blocked_by.contains(k)));
    // Only edges between the planner's own tasks are its to take away.
    let remove_blockers =
        was.blocked_by.iter().filter(|b| drafts.contains_key(b) && !wanted.contains(b)).cloned().collect();
    let update = PlanTaskUpdate {
        title: (title != was.title).then(|| title.to_string()),
        description: (t.description != was.description).then(|| t.description.clone()),
        proposed: (was.proposed.as_ref() != Some(&proposed)).then_some(proposed),
        add_blockers,
        remove_blockers,
    };
    let add_new = blockers.iter().filter_map(new_handle).map(str::to_string).collect();
    Revision { key: was.key.clone(), update, add_new }
}

fn old_key(node: &Node) -> Option<&TaskKey> {
    match node {
        Node::Old(key) => Some(key),
        Node::New(_) => None,
    }
}

fn new_handle(node: &Node) -> Option<&str> {
    match node {
        Node::New(handle) => Some(handle),
        Node::Old(_) => None,
    }
}

fn name(node: &Node) -> String {
    match node {
        Node::Old(key) => key.0.clone(),
        Node::New(handle) => format!("`{handle}`"),
    }
}

/// Whether `from` waits on `to`, directly or through others.
fn reaches(edges: &BTreeMap<Node, Vec<Node>>, from: &Node, to: &Node) -> bool {
    let mut stack = vec![from];
    let mut seen = BTreeSet::new();
    while let Some(at) = stack.pop() {
        if at == to {
            return true;
        }
        if seen.insert(at) {
            stack.extend(edges.get(at).into_iter().flatten());
        }
    }
    false
}

/// `PROJECT-12`.
fn looks_like_key(s: &str) -> bool {
    s.rsplit_once('-').is_some_and(|(p, n)| !p.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The new tasks with each after the new ones it waits on. The edges have no cycle, so every one is placed.
fn in_dependency_order(mut pending: Vec<Creation>) -> Vec<Creation> {
    let mut placed: Vec<Creation> = Vec::new();
    while !pending.is_empty() {
        let at =
            pending.iter().position(|c| c.after.iter().all(|h| placed.iter().any(|p| &p.handle == h))).unwrap_or(0);
        placed.push(pending.remove(at));
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(n: u8) -> Grade {
        Grade::new(n).unwrap()
    }

    fn draft(key: &str, blocked_by: &[&str]) -> PlanTask {
        PlanTask {
            key: key.into(),
            title: format!("{key} title"),
            description: String::new(),
            status: Status::Open,
            grade: None,
            proposed: Some(ProposedGrade { grade: g(2), why: "w".into() }),
            blocked_by: blocked_by.iter().map(|k| TaskKey::from(*k)).collect(),
        }
    }

    fn listed(id: Option<&str>, key: Option<&str>, title: &str, blocked_by: &[&str]) -> ProposedTask {
        ProposedTask {
            id: id.map(str::to_string),
            key: key.map(TaskKey::from),
            title: title.into(),
            description: String::new(),
            blocked_by: blocked_by.iter().map(|b| b.to_string()).collect(),
            grade: g(2),
            why: "w".into(),
        }
    }

    #[test]
    fn a_turn_parses_and_a_bad_one_says_why() {
        let turn = PlanTurn::parse(r#"{"message": "Which colours?", "ready": false}"#).unwrap();
        assert_eq!((turn.message.as_str(), turn.tasks, turn.spec), ("Which colours?", None, None));
        assert!(PlanTurn::parse("not json").is_err());
        assert!(PlanTurn::parse(r#"{"message": " "}"#).unwrap_err().contains("message"));
        let bad_grade = r#"{"message": "m", "tasks": [{"title": "t", "grade": 7}]}"#;
        assert!(PlanTurn::parse(bad_grade).is_err());
    }

    #[test]
    fn new_tasks_come_after_the_ones_they_wait_on() {
        let tasks = [listed(Some("b"), None, "B", &["a"]), listed(Some("a"), None, "A", &[])];
        let c = changes(&[], &tasks);
        let order: Vec<_> = c.create.iter().map(|c| c.handle.as_str()).collect();
        assert_eq!(order, ["a", "b"]);
        assert_eq!(c.create[1].after, ["a"]);
        assert!(c.warnings.is_empty() && c.cancel.is_empty());
    }

    #[test]
    fn a_revision_updates_adds_and_cancels_and_leaves_a_persons_tasks_alone() {
        let mut graded = draft("T-9", &[]);
        graded.grade = Some(g(3));
        let current = [draft("T-2", &["T-3"]), draft("T-3", &[]), draft("T-4", &["T-8"]), graded];
        let tasks = [
            listed(None, Some("T-2"), "Renamed", &["new", "T-9"]),
            listed(Some("new"), None, "New", &["T-4"]),
            listed(None, Some("T-4"), "T-4 title", &[]),
            listed(None, Some("T-9"), "Hijack", &[]),
        ];
        let c = changes(&current, &tasks);
        assert_eq!(c.cancel, [TaskKey::from("T-3")]);
        assert_eq!(c.create.len(), 1);
        assert_eq!(c.create[0].task.blocked_by, [TaskKey::from("T-4")]);
        let t2 = c.revise.iter().find(|r| r.key == "T-2").unwrap();
        assert_eq!(t2.update.title.as_deref(), Some("Renamed"));
        assert_eq!(t2.update.remove_blockers, [TaskKey::from("T-3")]);
        assert_eq!(t2.update.add_blockers, [TaskKey::from("T-9")], "a person's task may be waited on");
        assert_eq!(t2.add_new, ["new"]);
        assert!(c.revise.iter().all(|r| r.key != "T-4"), "unchanged, and a hand-made dependency outside it kept");
        assert!(c.warnings.iter().any(|w| w.contains("T-9")), "{:?}", c.warnings);
    }

    #[test]
    fn a_cycle_or_an_unknown_name_is_dropped_with_a_warning() {
        let tasks = [listed(Some("a"), None, "A", &["b"]), listed(Some("b"), None, "B", &["a", "nope", "a"])];
        let c = changes(&[], &tasks);
        assert_eq!(c.create.len(), 2);
        let waits: Vec<_> = c.create.iter().map(|c| (c.handle.as_str(), c.after.clone())).collect();
        assert!(waits.contains(&("a", vec!["b".to_string()])) && waits.contains(&("b", vec![])), "{waits:?}");
        assert_eq!(c.warnings.len(), 3, "{:?}", c.warnings);
    }
}
