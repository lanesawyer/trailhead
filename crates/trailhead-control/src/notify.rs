//! The notifications the factory raises for what the task engine can't see on its own
//! (`docs/factory/operating.md#notifications`): a PR that waits on a person's review, and a usage pause. A park is a
//! question, which the engine already tells the person about. Raising or resolving one is best-effort: a failure is
//! logged and never holds up a run, a record or a park.

use trailhead_factory::{
    GateKind, MergeDecision, NOTICE_PAUSED, NOTICE_REVIEW, Notice, NotificationAction, PrGrade, PrRef, RunPatch,
    Status, Task, TaskKey,
};

use crate::Factory;
use crate::timing::unix_now;

/// The subject of the one notification a usage pause raises.
const PAUSE_SUBJECT: &str = "usage-pause";

/// How much of a task's title goes in a notification's title.
const TITLE_QUOTE: usize = 120;

/// What changed about a usage pause, for the next pass to tell the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PauseChange {
    /// The factory stopped starting runs: the window, its percent used and when it resets (unix seconds).
    Paused { window: String, used: u32, limit: Option<u32>, resets_at: i64 },
    /// It started them again, or has just started and isn't paused, so a notice from before a restart is stale.
    Resumed,
}

fn review_subject(key: &TaskKey) -> String {
    format!("review:{key}")
}

/// Whether the merger's decision leaves the PR waiting on a person alone: every other gate passed.
pub(crate) fn waits_on_person(act: bool, waiting: &[GateKind]) -> bool {
    act && waiting == [GateKind::Human]
}

fn review_notice(task: &Task, pr: &PrRef, grade: &PrGrade) -> Notice {
    let why = if grade.reasons.is_empty() { String::new() } else { format!(" ({})", grade.reasons.join("; ")) };
    let actions = if pr.url.starts_with("https://") || pr.url.starts_with("http://") {
        vec![NotificationAction { label: "Open the pull request".into(), url: pr.url.clone() }]
    } else {
        Vec::new()
    };
    Notice {
        kind: NOTICE_REVIEW.into(),
        title: format!("{} waits for your review: {}", task.key, trailhead_core::clip(&task.title, TITLE_QUOTE)),
        body: format!(
            "Its pull request is grade {}{why}, which needs a person's approval. Every other gate has passed: approve \
             the task once you have read the change, or ask for changes on it.",
            grade.grade
        ),
        subject: review_subject(&task.key),
        task: Some(task.key.clone()),
        actions,
    }
}

fn pause_notice(window: &str, used: u32, limit: Option<u32>, resets_at: i64) -> Notice {
    let secs = (resets_at - unix_now()).max(0);
    let (h, m) = (secs / 3600, (secs % 3600 + 59) / 60);
    let wait = if h > 0 { format!("{h} h {m} min") } else { format!("{m} min") };
    let limit = limit.map(|l| format!(", past the {l}% it pauses at")).unwrap_or_default();
    Notice {
        kind: NOTICE_PAUSED.into(),
        title: format!("The factory paused: the {window} usage window is {used}% used"),
        body: format!(
            "It starts no new runs until the window resets, in about {wait}{limit}. Runs already going carry on. \
             `pause_at` in the user config's `[factory]` sets how full a window may get."
        ),
        subject: PAUSE_SUBJECT.into(),
        task: None,
        actions: Vec::new(),
    }
}

impl Factory {
    /// Tells the person a PR waits on them, or that it no longer does, as the merger's decision changes; `notified` is
    /// the decision's mark from before. Returns the mark to record: a failure leaves it as it was, so the next pass
    /// tries again.
    pub(crate) async fn tell_review(
        &self,
        task: &Task,
        pr: &PrRef,
        grade: &PrGrade,
        waits: bool,
        notified: bool,
    ) -> bool {
        match (waits, notified) {
            (true, false) => match self.tracker.notify(&review_notice(task, pr, grade)).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(task = %task.key, error = %e, "couldn't tell the person the PR waits on their review");
                    false
                }
            },
            (false, true) => match self.tracker.resolve_notices(&review_subject(&task.key)).await {
                Ok(()) => false,
                Err(e) => {
                    tracing::warn!(task = %task.key, error = %e, "couldn't resolve the notification of a review that no longer waits on the person");
                    true
                }
            },
            _ => notified,
        }
    }

    /// Takes back the notice of a PR that waited on the person, for a task that left review (a rework, a park, a
    /// person's reopen), so that the PR's next wait on them is told afresh. The mark is cleared first, and the
    /// decision it is on is read again, since `task` may be older than the merger's last write. Best-effort.
    pub(crate) async fn forget_review_notice(&self, task: &Task) {
        if !task.run.merge.as_ref().is_some_and(|m| m.notified) {
            return;
        }
        let key = &task.key;
        let current = match self.tracker.show(key).await {
            Ok(current) => current,
            Err(e) => {
                tracing::warn!(task = %key, error = %e, "couldn't read the task to take back its review notification");
                return;
            }
        };
        let Some(decision) = current.run.merge.filter(|m| m.notified) else { return };
        let cleared = RunPatch::merge(MergeDecision { notified: false, ..decision });
        if let Err(e) = self.tracker.report(key, cleared).await {
            tracing::warn!(task = %key, error = %e, "couldn't clear the review notification's mark; trying again later");
            return;
        }
        if let Err(e) = self.tracker.resolve_notices(&review_subject(key)).await {
            tracing::warn!(task = %key, error = %e, "couldn't resolve the review notification of a task that left review");
        }
    }

    /// [`Factory::forget_review_notice`] for a task reconcile finds out of review: open or parked.
    pub(crate) async fn forget_review_notice_out_of_review(&self, task: &Task) {
        if matches!(task.status, Status::Open | Status::NeedsInput) {
            self.forget_review_notice(task).await;
        }
    }

    /// Tells the person about the usage pause starting or ending, if it did since the last pass. Once per pause on
    /// a host, whichever repo's pass gets to it.
    pub(crate) async fn tell_pause(&self) {
        let Some(change) = self.host.pause_change.lock().unwrap().take() else { return };
        // A pause from before a restart may still be open: one at a time.
        if let Err(e) = self.tracker.resolve_notices(PAUSE_SUBJECT).await {
            tracing::warn!(error = %e, "couldn't resolve the usage pause's notification");
        }
        if let PauseChange::Paused { window, used, limit, resets_at } = change
            && let Err(e) = self.tracker.notify(&pause_notice(&window, used, limit, resets_at)).await
        {
            tracing::warn!(error = %e, "couldn't tell the person the factory paused");
        }
    }
}
