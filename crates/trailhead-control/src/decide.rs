use trailhead_factory::{CiState, GateKind, MergeMode, Status};

/// What the merger knows about a PR's head when it decides, all gathered before anything is decided or done.
pub(crate) struct Facts<'a> {
    /// Whether the merger may approve and merge (`evaluate_gates`), or only say where things stand.
    pub act: bool,
    pub merge_mode: MergeMode,
    pub require: &'a [GateKind],
    pub auto_merge: bool,
    pub ci: &'a CiState,
    /// The agent review's latest verdict approves, and is on this head or one carried to it.
    pub verdict_approves: bool,
    pub status: Status,
    pub head: &'a str,
    /// The head the control plane approved on, and the head Lane did.
    pub approved_head: Option<&'a str>,
    pub human_head: Option<&'a str>,
}

/// What the merger does about a PR, for the caller to carry out in order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Decision {
    pub passed: Vec<GateKind>,
    pub waiting: Vec<GateKind>,
    pub failing: Vec<String>,
    pub gates_pass: bool,
    /// Every gate passed and the grade auto-merges, so a merger that was on would merge now.
    pub would_merge: bool,
    /// The control plane's own approval no longer holds, so the task goes back to review.
    pub withdraw: bool,
    /// The gates pass without Lane, so the control plane moves the task to approved.
    pub approve: bool,
    /// Merge now, provided approving (when `approve`) worked.
    pub merge: bool,
}

pub(crate) fn decide(f: &Facts) -> Decision {
    let (mut passed, mut waiting, mut failing) = (Vec::new(), Vec::new(), Vec::new());
    for required in f.require {
        let (name, ok) = match required {
            GateKind::Ci => match f.ci {
                CiState::Passed => (GateKind::Ci, true),
                CiState::Pending => (GateKind::Ci, false),
                CiState::Failed { checks } => {
                    failing = checks.clone();
                    (GateKind::Ci, false)
                }
            },
            GateKind::AgentReview => (GateKind::AgentReview, f.verdict_approves),
            GateKind::Human => (GateKind::Human, f.status == Status::Approved && f.human_head == Some(f.head)),
            GateKind::Quorum => (GateKind::Quorum, false),
            // The PR exists because the worker ran every verifying check, and they passed, before opening it.
            GateKind::Checks | GateKind::Named(_) => (GateKind::Checks, true),
        };
        let list = if ok { &mut passed } else { &mut waiting };
        if !list.contains(&name) {
            list.push(name);
        }
    }
    let gates_pass = waiting.is_empty();
    // The control plane's own approval stands only while its gates pass: a grade raised to one that needs Lane, or a
    // verdict that no longer approves, takes it back. CI running again on the head is waited for (a failure reworks
    // the task). Lane's approval (no mark) is his to withdraw.
    let withdraw =
        f.status == Status::Approved && f.approved_head.is_some() && waiting.iter().any(|w| *w != GateKind::Ci);
    let status = if withdraw { Status::InReview } else { f.status };
    let human_head = if withdraw { None } else { f.human_head };
    let approve = f.act && gates_pass && status == Status::InReview;
    let approved = approve || status == Status::Approved;
    let merge = f.act
        && f.merge_mode == MergeMode::On
        && gates_pass
        && approved
        && (f.auto_merge || human_head == Some(f.head));
    Decision {
        would_merge: gates_pass && f.auto_merge && f.merge_mode == MergeMode::Shadow,
        passed,
        waiting,
        failing,
        gates_pass,
        withdraw,
        approve,
        merge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GateKind::{AgentReview, Checks, Ci, Human, Named, Quorum};

    const HEAD: &str = "abc";

    fn facts<'a>(require: &'a [GateKind], ci: &'a CiState) -> Facts<'a> {
        Facts {
            act: true,
            merge_mode: MergeMode::On,
            require,
            auto_merge: true,
            ci,
            verdict_approves: true,
            status: Status::InReview,
            head: HEAD,
            approved_head: None,
            human_head: None,
        }
    }

    #[test]
    fn grade_one_passes_approves_and_merges() {
        let d = decide(&facts(&[Checks, Ci], &CiState::Passed));
        assert_eq!(d.passed, [Checks, Ci]);
        assert!(d.waiting.is_empty() && d.gates_pass && d.approve && !d.withdraw);
        // On the first pass the task is still in review; the merge follows approving.
        assert!(d.merge && !d.would_merge);
    }

    #[test]
    fn pending_ci_waits() {
        let d = decide(&facts(&[Checks, Ci], &CiState::Pending));
        assert_eq!((d.passed, d.waiting), (vec![Checks], vec![Ci]));
        assert!(!d.approve && !d.merge && !d.would_merge);
    }

    #[test]
    fn failed_ci_names_the_failing_checks() {
        let ci = CiState::Failed { checks: vec!["test".into()] };
        let d = decide(&facts(&[Ci], &ci));
        assert_eq!((d.waiting, d.failing), (vec![Ci], vec!["test".to_string()]));
    }

    #[test]
    fn a_review_that_doesnt_approve_waits() {
        let mut f = facts(&[Checks, Ci, AgentReview], &CiState::Passed);
        f.verdict_approves = false;
        let d = decide(&f);
        assert_eq!(d.waiting, [AgentReview]);
        assert!(!d.approve);
    }

    #[test]
    fn named_checks_count_as_checks_once() {
        let d = decide(&facts(&[Checks, Named("lint".into()), Named("test".into())], &CiState::Passed));
        assert_eq!(d.passed, [Checks]);
    }

    #[test]
    fn quorum_never_passes_yet() {
        let d = decide(&facts(&[Quorum], &CiState::Passed));
        assert_eq!(d.waiting, [Quorum]);
    }

    #[test]
    fn human_gate_needs_lanes_approval_on_this_head() {
        let require = [Checks, Ci, Human];
        let d = decide(&facts(&require, &CiState::Passed));
        assert_eq!(d.waiting, [Human]);
        assert!(!d.approve && !d.merge);

        let mut f = facts(&require, &CiState::Passed);
        f.status = Status::Approved;
        f.human_head = Some(HEAD);
        f.auto_merge = false;
        let d = decide(&f);
        assert!(d.gates_pass && !d.approve && !d.withdraw && d.merge);

        f.human_head = Some("old");
        let d = decide(&f);
        assert_eq!(d.waiting, [Human]);
        assert!(!d.merge);
    }

    #[test]
    fn an_approved_task_whose_grade_doesnt_auto_merge_waits_for_lane() {
        let mut f = facts(&[Checks, Ci], &CiState::Passed);
        f.status = Status::Approved;
        f.approved_head = Some(HEAD);
        f.auto_merge = false;
        let d = decide(&f);
        assert!(d.gates_pass && !d.merge);
    }

    #[test]
    fn the_control_planes_approval_is_withdrawn_when_gates_stop_passing() {
        let mut f = facts(&[Checks, Ci, AgentReview], &CiState::Passed);
        f.status = Status::Approved;
        f.approved_head = Some(HEAD);
        f.verdict_approves = false;
        let d = decide(&f);
        assert!(d.withdraw && !d.approve && !d.merge);
    }

    #[test]
    fn ci_running_again_keeps_the_approval() {
        let mut f = facts(&[Checks, Ci], &CiState::Pending);
        f.status = Status::Approved;
        f.approved_head = Some(HEAD);
        let d = decide(&f);
        assert!(!d.withdraw && !d.merge);
    }

    #[test]
    fn lanes_approval_is_not_withdrawn() {
        let mut f = facts(&[Checks, Ci, AgentReview], &CiState::Passed);
        f.status = Status::Approved;
        f.human_head = Some(HEAD);
        f.verdict_approves = false;
        assert!(!decide(&f).withdraw);
    }

    #[test]
    fn shadow_mode_says_it_would_merge_and_does_nothing() {
        let mut f = facts(&[Checks, Ci], &CiState::Passed);
        f.merge_mode = MergeMode::Shadow;
        let d = decide(&f);
        assert!(d.would_merge && d.approve && !d.merge);
    }

    #[test]
    fn saying_gates_neither_approves_nor_merges() {
        let mut f = facts(&[Checks, Ci], &CiState::Passed);
        f.act = false;
        f.merge_mode = MergeMode::Shadow;
        let d = decide(&f);
        assert!(d.would_merge && !d.approve && !d.merge);
    }

    #[test]
    fn off_neither_says_nor_merges() {
        let mut f = facts(&[Checks, Ci], &CiState::Passed);
        f.merge_mode = MergeMode::Off;
        let d = decide(&f);
        assert!(!d.would_merge && !d.merge);
    }
}
