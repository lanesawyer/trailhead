//! What an infrastructure failure does to its task: the one policy a worker's run and a reviewer's share.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

use trailhead_factory::{Error, RunPatch, TaskKey, Tracker, Usd};

use crate::Factory;

/// How many times in a row a task may wait for a host's resource (its disk) before it's parked for a person: about
/// two and a half hours at the capped backoff.
pub(crate) const MAX_RESOURCE_WAITS: u32 = 12;

/// Why a failure parks its task for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Park {
    /// The repo's `setup` fails on the base branch itself, so no retry can pass.
    BrokenBase,
    /// The remote refused the request for good, and would again.
    Rejected,
    /// The host has been short of a resource for `MAX_RESOURCE_WAITS` waits in a row.
    Waited,
    /// `max_infra_failures` failures in a row.
    Limit,
}

/// What to do about one infrastructure failure, and the counts that go with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Setback {
    /// Infrastructure failures in a row, this one included.
    pub failures: u32,
    /// Resource waits in a row, this one included.
    pub waits: u32,
    /// Whether the host is short of a resource, which is waited out and never counted as a failure.
    pub waiting: bool,
    /// Why the task is parked, if it is.
    pub park: Option<Park>,
    prior: u32,
}

impl Setback {
    /// Decides on `error`. `prior` is the count the tracker holds and `held` the (failures, waits) kept in memory,
    /// which cover a tracker that didn't take the write. `broken_base` is the caller's finding that `setup` has
    /// failed often enough to blame the base branch.
    pub(crate) fn decide(error: &Error, prior: Option<u32>, held: (u32, u32), max: u32, broken_base: bool) -> Setback {
        let prior = prior.unwrap_or(0);
        let failures = prior.max(held.0) + 1;
        let waits = held.1 + 1;
        let waiting = matches!(error, Error::Unavailable(_));
        let park = if broken_base {
            Some(Park::BrokenBase)
        } else if matches!(error, Error::Rejected(_)) {
            Some(Park::Rejected)
        } else if waiting {
            (waits >= MAX_RESOURCE_WAITS).then_some(Park::Waited)
        } else {
            (failures >= max.max(1)).then_some(Park::Limit)
        };
        Setback { failures, waits, waiting, park, prior }
    }

    /// The count to record with the attempt given back: none for a parked task, whose count starts again when a
    /// person reopens it, and unchanged by a wait.
    pub(crate) fn recorded(&self) -> u32 {
        if self.park.is_some() { 0 } else { self.unparked() }
    }

    /// The count to keep for a task that isn't parked, or whose park failed: the next failure must try to park again.
    pub(crate) fn unparked(&self) -> u32 {
        if self.waiting { self.prior } else { self.failures }
    }
}

/// A task held back after failing on the infrastructure.
#[derive(Debug, Clone, Copy)]
struct Held {
    /// Failures in a row: the count that parks.
    failures: u32,
    /// Waits in a row for a resource its host lacks.
    waits: u32,
    next_try: Instant,
}

/// What the factory remembers of tasks that failed on the infrastructure. In memory: a restart starts the waits and
/// counts again.
#[derive(Default)]
pub(crate) struct Backoff {
    /// Tasks whose last runs failed on the infrastructure: how many in a row, and when to try again.
    setbacks: Mutex<HashMap<TaskKey, Held>>,
    /// Tasks whose last runs all failed in `setup`: how many in a row. Any other outcome clears it, so a broken base
    /// is only declared when `setup` itself failed twice running (TRAIL-876).
    setup_failures: Mutex<HashMap<TaskKey, u32>>,
}

impl Backoff {
    /// The (failures, waits) in a row held for a task.
    pub(crate) fn held(&self, key: &TaskKey) -> (u32, u32) {
        self.setbacks.lock().unwrap().get(key).map_or((0, 0), |h| (h.failures, h.waits))
    }

    /// When a held task may be tried again; `None` for one that isn't held.
    pub(crate) fn next_try(&self, key: &TaskKey) -> Option<Instant> {
        self.setbacks.lock().unwrap().get(key).map(|h| h.next_try)
    }

    /// Holds a task for `wait` after `failures` in a row, with no waits for a resource behind it.
    pub(crate) fn hold(&self, key: &TaskKey, failures: u32, wait: Duration) {
        let held = Held { failures, waits: 0, next_try: Instant::now() + wait };
        self.setbacks.lock().unwrap().insert(key.clone(), held);
    }

    /// Holds a task that waits on a resource once more: `wait` is given the waits in a row, this one included, and
    /// says how long this one lasts.
    pub(crate) fn hold_waiting(&self, key: &TaskKey, failures: u32, wait: impl FnOnce(u32) -> Duration) {
        let mut setbacks = self.setbacks.lock().unwrap();
        let waits = setbacks.get(key).map_or(0, |h| h.waits) + 1;
        setbacks.insert(key.clone(), Held { failures, waits, next_try: Instant::now() + wait(waits) });
    }

    /// Lets a task go: it is no longer held back, whatever `setup` did on its last runs.
    pub(crate) fn release(&self, key: &TaskKey) {
        self.setbacks.lock().unwrap().remove(key);
    }

    /// Counts a run that failed in `setup`, and says how many have in a row.
    pub(crate) fn setup_failed(&self, key: &TaskKey) -> u32 {
        let mut counts = self.setup_failures.lock().unwrap();
        let n = counts.entry(key.clone()).or_insert(0);
        *n += 1;
        *n
    }

    /// A run got past `setup`, or failed on something else: its `setup` failures in a row are over.
    pub(crate) fn setup_passed(&self, key: &TaskKey) {
        self.setup_failures.lock().unwrap().remove(key);
    }
}

impl Factory {
    /// The (failures, waits) in a row held in memory for a task.
    pub(crate) fn held_setbacks(&self, key: &TaskKey) -> (u32, u32) {
        self.backoff.held(key)
    }

    /// A task that should have been parked for its failures and couldn't be asked the question: it keeps its backoff
    /// and its count, or reconcile would take it up again at once. `by` is who holds the task and so may report.
    pub(crate) async fn park_failed(&self, by: &Arc<dyn Tracker>, key: &TaskKey, setback: &Setback) {
        self.back_off(key, setback);
        let keep = RunPatch::infra_failures(setback.unparked());
        if let Err(e) = by.report(key, keep).await {
            tracing::warn!(task = %key, error = %e, "couldn't keep the infrastructure failure count");
        }
    }

    /// Records what a run cost before the infrastructure lost it, as the run's sub-agent: it counts toward the
    /// budget across retries (TRAIL-447). A record that fails is logged, and the failure is handled all the same.
    pub(crate) async fn record_lost_spend(&self, sub: &Arc<dyn Tracker>, key: &TaskKey, spend: Usd) {
        if let Err(e) = sub.report(key, RunPatch::spend(spend)).await {
            tracing::warn!(task = %key, error = %e, "couldn't record the lost run's spend");
        }
    }

    /// Holds a task back for the backoff its failure calls for: one that isn't parked, or whose park failed.
    pub(crate) fn back_off(&self, key: &TaskKey, setback: &Setback) {
        if setback.waiting {
            self.set_back_waiting(key, setback.failures - 1);
        } else {
            self.set_back(key, setback.failures);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn other() -> Error {
        Error::Other("boom".into())
    }

    #[test]
    fn failures_count_up_to_the_limit_then_park_and_the_count_starts_again() {
        let first = Setback::decide(&other(), None, (0, 0), 3, false);
        assert_eq!((first.failures, first.park, first.recorded()), (1, None, 1));
        let third = Setback::decide(&other(), Some(2), (0, 0), 3, false);
        assert_eq!((third.failures, third.park, third.recorded(), third.unparked()), (3, Some(Park::Limit), 0, 3));
    }

    #[test]
    fn the_count_in_memory_stands_in_for_one_the_tracker_didnt_take() {
        let setback = Setback::decide(&other(), Some(1), (2, 0), 3, false);
        assert_eq!((setback.failures, setback.park), (3, Some(Park::Limit)));
    }

    #[test]
    fn a_wait_for_a_resource_is_not_counted_and_parks_only_after_many_in_a_row() {
        let full = Error::Unavailable("disk".into());
        let waiting = Setback::decide(&full, Some(2), (2, 3), 3, false);
        assert_eq!((waiting.park, waiting.waits, waiting.recorded()), (None, 4, 2), "past the limit, but only waiting");
        let last = Setback::decide(&full, Some(2), (2, MAX_RESOURCE_WAITS - 1), 3, false);
        assert_eq!((last.park, last.recorded(), last.unparked()), (Some(Park::Waited), 0, 2));
    }

    #[test]
    fn a_refusal_for_good_and_a_broken_base_park_at_once() {
        let refused = Setback::decide(&Error::Rejected("422".into()), None, (0, 0), 3, false);
        assert_eq!(refused.park, Some(Park::Rejected));
        let broken = Setback::decide(&Error::SetupFailed("cargo".into()), None, (0, 0), 3, true);
        assert_eq!(broken.park, Some(Park::BrokenBase));
    }

    #[test]
    fn a_limit_of_zero_parks_on_the_first_failure() {
        assert_eq!(Setback::decide(&other(), None, (0, 0), 0, false).park, Some(Park::Limit));
    }
}
