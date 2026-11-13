//! Where a factory task is in its lifecycle, read off its record (`docs/factory/lifecycle.md`, "Explicit phases").
//!
//! A task's state is its status, its holder and the factory's run fields. [`phase`] is the one place that says what
//! they add up to; nothing is stored. Two things the record can't say are left to reconcile: whether the run's
//! sandbox is still busy (an input here) and what the code host says of the PR (asked by the phases that need it).

use trailhead_factory::{ChangesRequested, ReviewBreach, Status, Task, WorkerBreach};

use crate::lifecycle::worker_name;

#[derive(Debug, PartialEq)]
pub(crate) enum Phase<'a> {
    /// The factory hasn't run it: the queue's business.
    Untouched,
    /// A reviewer overran its budget and the park for it was cut short. `claim` when the task has to be claimed
    /// first (it is open), not when the factory already holds it.
    ReviewBreachUnparked {
        breach: &'a ReviewBreach,
        claim: bool,
    },
    /// A worker overran its budget and the park for it was cut short.
    WorkerBreachUnparked(&'a WorkerBreach),
    /// The current run holds the task, its report isn't handled and its sandbox is still busy with it.
    Running,
    /// In progress with no run going: a run that was cut off, or a claim nobody is using.
    Interrupted,
    /// Open again after a run: waiting in the queue for its next attempt or rework.
    Requeued,
    /// Waiting for a person.
    Parked(Parked<'a>),
    /// Its PR is open and being looked at.
    InReview(Review<'a>),
    /// The gates passed; waiting to merge.
    Approved,
    Closed,
}

/// What a parked task still has on it for reconcile to finish.
#[derive(Debug, PartialEq)]
pub(crate) struct Parked<'a> {
    /// A person merged the PR unapproved and was asked to mark the task done: its PR isn't looked at again.
    pub awaiting_done: bool,
    /// A changes request no one has settled, which the park may have been for.
    pub unsettled: Option<&'a ChangesRequested>,
}

/// What an in-review task is waiting on, as far as its record says. The PR's head can overrule the first two: a
/// request made on a head that has since moved was settled by whoever pushed.
#[derive(Debug, PartialEq)]
pub(crate) enum Review<'a> {
    /// Its newest review asks for changes and no worker has answered: it is listed as ready for a rework. `request`
    /// is the factory's own record of that request, when it has one for this attempt that still stands.
    ReworkWaiting { request: Option<&'a ChangesRequested> },
    /// The factory recorded a request on this attempt, but the review that makes the task ready was never written.
    RequestCutShort(&'a ChangesRequested),
    /// The attempt's PR hasn't been reviewed.
    Unreviewed,
    /// Reviewed, as far as the record says: waiting on CI, a person or the merger.
    AwaitingGates,
}

/// The phase `task` is in. `me` is the control plane's own agent name; `sandbox_busy` is whether the task's pooled
/// sandbox is still busy with it, which only matters to a run that [`may_be_running`].
pub(crate) fn phase<'a>(task: &'a Task, me: &str, sandbox_busy: bool) -> Phase<'a> {
    let run = &task.run;
    let Some(name) = &run.sandbox else { return Phase::Untouched };
    let held_by = |agent: &str| task.holder.as_deref() == Some(agent);
    let review_breach = run.review_breach.as_ref().filter(|b| !b.parked && b.attempt == run.attempt.unwrap_or(1));
    match task.status {
        Status::Open => match review_breach {
            Some(breach) => Phase::ReviewBreachUnparked { breach, claim: true },
            None => Phase::Requeued,
        },
        Status::InProgress => {
            if let Some(breach) = review_breach.filter(|_| held_by(me)) {
                return Phase::ReviewBreachUnparked { breach, claim: false };
            }
            if sandbox_busy && may_be_running(task, me) {
                return Phase::Running;
            }
            match run.worker_breach.as_ref().filter(|b| !b.parked && *name == b.run) {
                Some(breach) if held_by(&sub_agent(me, &breach.run)) => Phase::WorkerBreachUnparked(breach),
                _ => Phase::Interrupted,
            }
        }
        Status::NeedsInput => Phase::Parked(Parked {
            awaiting_done: run.unapproved_merge == Some(true),
            unsettled: run.changes_requested.as_ref().filter(|c| !c.settled),
        }),
        Status::InReview => Phase::InReview(review(task)),
        Status::Approved => Phase::Approved,
        Status::Done | Status::Cancelled => Phase::Closed,
    }
}

/// Whether the record says the task's current run may still be going: the task is in progress under that run's
/// sub-agent, on a pooled sandbox, and the run's report hasn't been handled. A run that gave its attempt back (a stop
/// on purpose, a lost daemon) is over, whatever the daemon still has.
pub(crate) fn may_be_running(task: &Task, me: &str) -> bool {
    let run = &task.run;
    let (Some(name), Some(_), Some(attempt)) = (&run.sandbox, &run.host, run.attempt) else { return false };
    *name == worker_name(&task.key, attempt, run.retries.unwrap_or(0))
        && run.concluded.as_ref() != Some(name)
        && task.status == Status::InProgress
        && task.holder.as_deref() == Some(sub_agent(me, name).as_str())
}

/// The request on this attempt that still stands, by the record: pending, and not one a park was settled on. A park
/// resets the counts to zero, so a request still recorded beside a zero count was parked on and then settled by a
/// person (TRAIL-304).
pub(crate) fn standing_request(task: &Task) -> Option<&ChangesRequested> {
    let run = &task.run;
    let attempt = run.attempt.unwrap_or(1);
    let parked_on = |c: &ChangesRequested| {
        let count = if c.kind.is_review() { run.review_rounds } else { run.conflict_reworks };
        count == Some(0)
    };
    run.pending_changes().filter(|c| c.attempt == attempt && !parked_on(c))
}

/// Whether the record alone says the attempt's PR is owed a review. One it says is reviewed may still be owed one,
/// if the PR was pushed to since.
pub(crate) fn unreviewed(task: &Task) -> bool {
    task.run.pr.is_some() && task.run.reviewed != Some(task.run.attempt.unwrap_or(1))
}

fn review(task: &Task) -> Review<'_> {
    let request = standing_request(task);
    if task.changes_requested {
        return Review::ReworkWaiting { request };
    }
    match request {
        Some(request) => Review::RequestCutShort(request),
        None if unreviewed(task) => Review::Unreviewed,
        None => Review::AwaitingGates,
    }
}

fn sub_agent(me: &str, run: &str) -> String {
    format!("{me}/{run}")
}

#[cfg(test)]
mod tests {
    use trailhead_factory::{PrRef, Priority, Repo, RunUpdate, TaskKey};

    use super::*;
    use trailhead_factory::RequestKind;

    const ME: &str = "factory";

    /// A task on attempt 1, run `trail-7-a1`, on a pooled sandbox.
    fn task(status: Status, holder: Option<&str>, run: RunUpdate) -> Task {
        let base = RunUpdate {
            attempt: Some(1),
            sandbox: Some("trail-7-a1".into()),
            host: Some("trailhead-w1".into()),
            ..RunUpdate::default()
        };
        let mut merged = base;
        merged.merge(run);
        Task {
            key: TaskKey("TRAIL-7".into()),
            project: "TRAIL".into(),
            title: String::new(),
            description: String::new(),
            status,
            grade: None,
            priority: Priority::default(),
            holder: holder.map(str::to_string),
            changes_requested: false,
            run: merged,
        }
    }

    fn pr() -> PrRef {
        PrRef { repo: Repo { owner: "o".into(), name: "r".into() }, number: 4, url: "https://host/o/r/4".into() }
    }

    fn request(kind: RequestKind) -> ChangesRequested {
        ChangesRequested {
            reviewer: "factory/trail-7-r1".into(),
            body: "Fix it.".into(),
            attempt: 1,
            kind,
            head: None,
            settled: false,
        }
    }

    #[test]
    fn a_task_the_factory_never_ran_is_untouched_whatever_its_status() {
        for status in [Status::Open, Status::InProgress, Status::InReview, Status::NeedsInput] {
            let mut t = task(status, None, RunUpdate::default());
            t.run.sandbox = None;
            assert_eq!(phase(&t, ME, true), Phase::Untouched);
        }
    }

    #[test]
    fn an_in_progress_task_is_running_only_under_its_current_runs_sub_agent_on_a_busy_sandbox() {
        let running = task(Status::InProgress, Some("factory/trail-7-a1"), RunUpdate::default());
        assert_eq!(phase(&running, ME, true), Phase::Running);
        assert_eq!(phase(&running, ME, false), Phase::Interrupted, "the sandbox has moved on");

        let concluded = RunUpdate { concluded: Some("trail-7-a1".into()), ..RunUpdate::default() };
        assert_eq!(
            phase(&task(Status::InProgress, Some("factory/trail-7-a1"), concluded), ME, true),
            Phase::Interrupted
        );
        let retried = RunUpdate { retries: Some(1), ..RunUpdate::default() };
        let gave_back = task(Status::InProgress, Some("factory/trail-7-a1"), retried);
        assert_eq!(phase(&gave_back, ME, true), Phase::Interrupted, "the run gave its attempt back");
        assert_eq!(
            phase(&task(Status::InProgress, Some("factory"), RunUpdate::default()), ME, true),
            Phase::Interrupted
        );
        assert_eq!(phase(&task(Status::InProgress, None, RunUpdate::default()), ME, true), Phase::Interrupted);
    }

    #[test]
    fn a_workers_breach_not_yet_parked_is_finished_unless_the_run_is_still_going() {
        let breach = WorkerBreach { run: "trail-7-a1".into(), reason: "tokens".into(), parked: false, stuck: false };
        let run = RunUpdate { worker_breach: Some(breach.clone()), ..RunUpdate::default() };
        let t = task(Status::InProgress, Some("factory/trail-7-a1"), run.clone());
        assert_eq!(phase(&t, ME, false), Phase::WorkerBreachUnparked(&breach));
        assert_eq!(phase(&t, ME, true), Phase::Running, "a busy sandbox is followed first");

        let parked = WorkerBreach { parked: true, ..breach.clone() };
        let run = RunUpdate { worker_breach: Some(parked), ..RunUpdate::default() };
        assert_eq!(phase(&task(Status::InProgress, Some("factory/trail-7-a1"), run), ME, false), Phase::Interrupted);
        let earlier = WorkerBreach { run: "trail-7-a0".into(), ..breach };
        let run = RunUpdate { worker_breach: Some(earlier), ..RunUpdate::default() };
        let t = task(Status::InProgress, Some("factory/trail-7-a1"), run);
        assert_eq!(phase(&t, ME, false), Phase::Interrupted, "an earlier run's breach is left behind");
    }

    #[test]
    fn a_reviewers_breach_not_yet_parked_is_finished_from_open_or_from_the_factorys_own_claim() {
        let breach = ReviewBreach { round: 1, attempt: 1, reason: "minutes".into(), parked: false };
        let run = RunUpdate { review_breach: Some(breach.clone()), ..RunUpdate::default() };
        let open = task(Status::Open, None, run.clone());
        assert_eq!(phase(&open, ME, false), Phase::ReviewBreachUnparked { breach: &breach, claim: true });
        let mine = task(Status::InProgress, Some("factory"), run.clone());
        assert_eq!(phase(&mine, ME, false), Phase::ReviewBreachUnparked { breach: &breach, claim: false });
        let workers = task(Status::InProgress, Some("factory/trail-7-a1"), run.clone());
        assert_eq!(phase(&workers, ME, true), Phase::Running, "a worker has started since");
        assert!(matches!(phase(&task(Status::InReview, None, run), ME, false), Phase::InReview(_)));

        let next = RunUpdate {
            attempt: Some(2),
            sandbox: Some("trail-7-a2".into()),
            review_breach: Some(breach),
            ..RunUpdate::default()
        };
        assert_eq!(phase(&task(Status::Open, None, next), ME, false), Phase::Requeued, "left behind by attempt 2");
    }

    #[test]
    fn a_parked_task_says_what_reconcile_still_has_to_finish() {
        let plain = task(Status::NeedsInput, Some("factory/trail-7-a1"), RunUpdate::default());
        assert_eq!(phase(&plain, ME, false), Phase::Parked(Parked { awaiting_done: false, unsettled: None }));

        let asked = request(RequestKind::Conflict);
        let run =
            RunUpdate { unapproved_merge: Some(true), changes_requested: Some(asked.clone()), ..RunUpdate::default() };
        let t = task(Status::NeedsInput, None, run);
        assert_eq!(phase(&t, ME, false), Phase::Parked(Parked { awaiting_done: true, unsettled: Some(&asked) }));

        let settled = ChangesRequested { settled: true, ..asked };
        let run = RunUpdate { changes_requested: Some(settled), ..RunUpdate::default() };
        let t = task(Status::NeedsInput, None, run);
        assert_eq!(phase(&t, ME, false), Phase::Parked(Parked { awaiting_done: false, unsettled: None }));
    }

    #[test]
    fn an_in_review_task_is_owed_a_review_until_its_attempt_has_one() {
        let unreviewed = task(Status::InReview, None, RunUpdate { pr: Some(pr()), ..RunUpdate::default() });
        assert_eq!(phase(&unreviewed, ME, false), Phase::InReview(Review::Unreviewed));
        let reviewed = RunUpdate { pr: Some(pr()), reviewed: Some(1), ..RunUpdate::default() };
        assert_eq!(phase(&task(Status::InReview, None, reviewed), ME, false), Phase::InReview(Review::AwaitingGates));
        let no_pr = task(Status::InReview, None, RunUpdate::default());
        assert_eq!(phase(&no_pr, ME, false), Phase::InReview(Review::AwaitingGates), "nothing to review");
    }

    #[test]
    fn a_changes_request_waits_for_a_worker_or_was_cut_short_before_the_review_was_written() {
        let asked = request(RequestKind::Review);
        let run = RunUpdate {
            pr: Some(pr()),
            review_rounds: Some(1),
            changes_requested: Some(asked.clone()),
            ..RunUpdate::default()
        };
        let cut_short = task(Status::InReview, None, run.clone());
        assert_eq!(phase(&cut_short, ME, false), Phase::InReview(Review::RequestCutShort(&asked)));
        let mut waiting = task(Status::InReview, None, run.clone());
        waiting.changes_requested = true;
        assert_eq!(phase(&waiting, ME, false), Phase::InReview(Review::ReworkWaiting { request: Some(&asked) }));

        // Parked on (the count was reset) and then settled by a person: the request no longer stands.
        let reset = RunUpdate { review_rounds: Some(0), ..run.clone() };
        let mut after_park = task(Status::InReview, None, reset);
        after_park.changes_requested = true;
        assert_eq!(phase(&after_park, ME, false), Phase::InReview(Review::ReworkWaiting { request: None }));

        // A request from an earlier attempt lingers until the next review.
        let later = RunUpdate { attempt: Some(2), sandbox: Some("trail-7-a2".into()), ..run };
        assert_eq!(phase(&task(Status::InReview, None, later), ME, false), Phase::InReview(Review::Unreviewed));
    }

    #[test]
    fn the_rest_follow_the_status() {
        assert_eq!(phase(&task(Status::Open, None, RunUpdate::default()), ME, false), Phase::Requeued);
        assert_eq!(phase(&task(Status::Approved, None, RunUpdate::default()), ME, false), Phase::Approved);
        assert_eq!(phase(&task(Status::Done, None, RunUpdate::default()), ME, false), Phase::Closed);
        assert_eq!(phase(&task(Status::Cancelled, None, RunUpdate::default()), ME, false), Phase::Closed);
    }
}
