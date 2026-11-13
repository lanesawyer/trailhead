//! Histories the simulation once failed on, shrunk, and kept so they never fail again. Each is named for the state
//! it used to end in. The numbers in `Tick` and `Reconcile` seed what the agents do during that step (`Agents` in
//! `world.rs`), so a change to that table makes these tell different stories: they must still pass, but re-shrink
//! the ones that matter from the fix's own test if they stop reaching the state they are named for.

use crate::replay;
use crate::world::Event::*;
use crate::world::Fault::*;

/// TRAIL-1468: the merger waited on agent_review, reading the factory's own conflict request as the newest review.
#[test]
fn a_conflict_that_clears_doesnt_leave_the_task_waiting_on_a_review() {
    replay(&[1, 1], &[Tick(3560847788132740062), Mergeable(0, Some(false)), Reconcile(8920950728352337595)]);
}

/// TRAIL-1468: settling the cleared conflict sent the task to review as the factory, which then couldn't approve it.
#[test]
fn a_conflict_that_clears_on_a_reopened_task_doesnt_make_the_factory_its_worker() {
    replay(
        &[1],
        &[Tick(16181071696641644463), Mergeable(0, Some(false)), Reconcile(5383943800667341387), PersonReopens(0)],
    );
}

/// TRAIL-1468: every tick settled the same cleared conflict again, "a later request stands", and nothing ran.
#[test]
fn a_cleared_conflict_under_a_persons_request_on_a_reopened_task_still_lets_it_run() {
    replay(
        &[1],
        &[
            Tick(1801572595553233354),
            Mergeable(0, Some(false)),
            Reconcile(7238607812543816968),
            Fault(RunFails),
            PersonRequestsChanges(0),
            PersonReopens(0),
        ],
    );
}

/// TRAIL-1469: approved forever, the rework never starting because the task wasn't in review.
#[test]
fn an_approval_over_a_request_for_changes_doesnt_strand_the_rework() {
    replay(&[1, 2], &[Tick(14038607207048404726), PersonApproves(0)]);
}

/// TRAIL-1469: approved forever over the person's own request for changes, which the factory hadn't recorded.
#[test]
fn an_approval_over_the_persons_own_request_doesnt_strand_the_rework() {
    replay(&[3, 1], &[Tick(3660349945580343941), PersonRequestsChanges(1), PersonApproves(1)]);
}

/// TRAIL-1469: approved forever, the rework's review never starting because the task wasn't in review.
#[test]
fn an_approval_before_the_reworks_review_doesnt_strand_the_review() {
    replay(&[2, 3], &[Tick(578614372503024644), Tick(1475254212943182506), PersonApproves(1)]);
}

/// TRAIL-1469: approved forever on a head the person pushed, which no review had read.
#[test]
fn an_approval_after_a_persons_push_doesnt_strand_the_review() {
    replay(&[3, 1], &[Tick(10340341780540827069), Push(0), PersonApproves(0)]);
}

/// TRAIL-1469 and TRAIL-1468 together: the conflict request replaced the reviewer's, and clearing it answered both.
#[test]
fn a_conflict_on_a_task_approved_over_a_request_doesnt_lose_the_request() {
    replay(
        &[1, 3],
        &[
            Tick(469685433637206678),
            Tick(4006939365228298787),
            Push(0),
            Reconcile(11384181573462101558),
            PersonApproves(0),
            Mergeable(0, Some(false)),
            Reconcile(7130047409875532943),
        ],
    );
}

/// TRAIL-1470: the request reconcile took up had no reviewed head, so the push after it was never reviewed.
#[test]
fn a_push_after_a_request_taken_up_from_its_verdict_is_reviewed() {
    replay(&[3], &[Fault(CommentFails), Tick(15740515805878934363), Reconcile(13473975886714444618), Push(1)]);
}

/// TRAIL-1470: the conflict request marked the attempt reviewed, and clearing it left the owed review forgotten.
#[test]
fn a_conflict_that_clears_on_an_unreviewed_pr_leaves_the_review_owed() {
    replay(
        &[1],
        &[
            Tick(9083807209801353033),
            PersonAnswers(0),
            Fault(CommentFails),
            Tick(17196730076952014513),
            Mergeable(1, Some(false)),
            Reconcile(14150357104499192775),
        ],
    );
}

/// TRAIL-1471: the first request for changes after a park on review rounds was born settled, and never reworked.
#[test]
fn a_request_for_changes_after_a_park_on_review_rounds_is_reworked() {
    replay(
        &[2, 1],
        &[
            Tick(2421939630983332174),
            Tick(13749176605930509684),
            Tick(11646840527822202892),
            PersonAnswers(1),
            Tick(10290872230746660934),
        ],
    );
}

/// TRAIL-1462: the tracker failed before the rework's sub-agent existed, the factory claimed and released the task
/// to back it off, and core then took the factory for the worker: the merge it made later could never be finished.
#[test]
fn a_rework_that_cant_start_doesnt_make_the_factory_its_worker() {
    replay(
        &[3, 1],
        &[
            Tick(5688206883176109799),
            Tick(1712250019273695481),
            PersonMerges(0),
            Tick(10545829268931444722),
            Fault(TrackerFailsWrite(1)),
            Tick(388732358854232492),
            PersonApproves(1),
        ],
    );
}

/// TRAIL-1462: the same, found through a conflict rework, where it left the factory unable to approve.
#[test]
fn a_conflict_rework_that_cant_start_doesnt_make_the_factory_its_worker() {
    replay(
        &[1, 1],
        &[
            Tick(2328591859132753582),
            Mergeable(0, Some(false)),
            Reconcile(8920950728352337595),
            Push(0),
            Fault(TrackerFailsWrite(5)),
            Reconcile(14972276120280943070),
            Tick(17905383653971260893),
        ],
    );
}
