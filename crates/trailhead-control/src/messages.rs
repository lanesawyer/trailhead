use trailhead_factory::{MergeDecision, PrGrade};

use crate::merger::Rework;

/// What the merger says on the PR about its decision, e.g. `Grade 1: checks, CI and review passed; would merge
/// (shadow).` or `Grade 3 (Cargo.lock changed): waiting for a person.` `merging` is whether the merger is on and will
/// try to merge now; the message then says the merge happened, so it is only posted once it has.
pub(crate) fn decision_message(
    grade: &PrGrade,
    d: &MergeDecision,
    auto_merge: bool,
    merging: bool,
    behind: bool,
    changes: Option<Rework>,
) -> String {
    let message = gate_message(grade, d, auto_merge, merging, behind);
    match changes {
        Some(Rework::Running) => format!("{message} The latest review asked for changes; a rework is running."),
        Some(Rework::Pushed) => format!("{message} The latest review asked for changes; the rework has pushed."),
        None => message,
    }
}

fn gate_message(grade: &PrGrade, d: &MergeDecision, auto_merge: bool, merging: bool, behind: bool) -> String {
    fn join(items: Vec<String>) -> String {
        match items.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    }
    let head = grade.describe();
    if !d.waiting.is_empty() {
        let mut parts = Vec::new();
        let mut waiting = Vec::new();
        for gate in &d.waiting {
            match gate.as_str() {
                "ci" if !d.failing.is_empty() => parts.push(format!("CI failed ({})", d.failing.join(", "))),
                "ci" => waiting.push("CI".to_string()),
                "agent_review" => waiting.push("review".into()),
                "human" => waiting.push("a person".into()),
                other => waiting.push(other.to_string()),
            }
        }
        if !waiting.is_empty() {
            parts.push(format!("waiting for {}", join(waiting)));
        }
        return format!("{head}: {}.", parts.join("; "));
    }
    let passed = d.passed.iter().map(|g| match g.as_str() {
        "ci" => "CI".to_string(),
        "agent_review" => "review".into(),
        "human" => "a person's approval".into(),
        other => other.to_string(),
    });
    let passed = join(passed.collect());
    match (&d.problem, d.would_merge, auto_merge) {
        (Some(problem), ..) => format!("{head}: {passed} passed, but {problem}."),
        _ if merging => format!("{head}: {passed} passed; merged."),
        (None, true, _) if behind => {
            format!("{head}: {passed} passed; would bring the branch up to date, then merge (shadow).")
        }
        (None, true, _) => format!("{head}: {passed} passed; would merge (shadow)."),
        (None, false, true) => format!("{head}: {passed} passed; a person merges."),
        (None, false, false) => format!("{head}: {passed} passed; not auto-merged at this grade, so a person merges."),
    }
}
