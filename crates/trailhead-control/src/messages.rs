use trailhead_factory::{GateKind, MergeDecision, PrGrade};

use crate::merger::Rework;

/// What the merger knows beyond the decision itself when it words the PR comment.
#[derive(Clone, Copy)]
pub(crate) struct Wording {
    pub auto_merge: bool,
    /// The merger is on and will try to merge now.
    pub merging: bool,
    /// The PR is behind its base branch.
    pub behind: bool,
    pub changes: Option<Rework>,
}

/// What the merger says on the PR about its decision, e.g. `Grade 1: checks, CI and review passed; would merge
/// (shadow).` or `Grade 3 (Cargo.lock changed): waiting for a person.` `merging` is whether the merger is on and will
/// try to merge now; the message then says the merge happened, so it is only posted once it has.
pub(crate) fn decision_message(grade: &PrGrade, d: &MergeDecision, said: Wording) -> String {
    let message = gate_message(grade, d, &said);
    match said.changes {
        Some(Rework::Running) => format!("{message} The latest review asked for changes; a rework is running."),
        Some(Rework::Pushed) => format!("{message} The latest review asked for changes; the rework has pushed."),
        None => message,
    }
}

fn gate_message(grade: &PrGrade, d: &MergeDecision, said: &Wording) -> String {
    let Wording { auto_merge, merging, behind, .. } = *said;
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
            match gate {
                GateKind::Ci if !d.failing.is_empty() => parts.push(format!("CI failed ({})", d.failing.join(", "))),
                GateKind::Ci => waiting.push("CI".to_string()),
                GateKind::AgentReview => waiting.push("review".into()),
                GateKind::Human => waiting.push("a person".into()),
                GateKind::Checks | GateKind::Quorum | GateKind::Named(_) => waiting.push(gate.to_string()),
            }
        }
        if !waiting.is_empty() {
            parts.push(format!("waiting for {}", join(waiting)));
        }
        return format!("{head}: {}.", parts.join("; "));
    }
    let passed = d.passed.iter().map(|g| match g {
        GateKind::Ci => "CI".to_string(),
        GateKind::AgentReview => "review".into(),
        GateKind::Human => "a person's approval".into(),
        GateKind::Checks | GateKind::Quorum | GateKind::Named(_) => g.to_string(),
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
