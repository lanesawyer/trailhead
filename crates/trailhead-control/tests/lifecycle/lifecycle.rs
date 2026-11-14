use std::sync::Arc;
use std::time::Duration;

use trailhead_control::Outcome;
use trailhead_factory::{
    CodeHost, Commit, Compute, FileChange, MergeOutcome, Role, RunOutcome, Status, TaskKey, Tracker, Usd, VerdictKind,
    label,
};
use trailhead_testkit::{FAKE_BASE, Script, TrackerOp};

use crate::common::*;

#[tokio::test]
async fn a_profile_selects_the_scripted_harness_by_adapter_name() {
    let config = CONFIG.replace("claude-code", trailhead_testkit::SCRIPTED);
    let r = rig_selecting(&config);
    r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));

    let ran = r.factory.tick().await.unwrap();
    let Outcome::Reviewed { verdict, .. } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.kind, VerdictKind::Approve);
    assert_eq!(r.codehost.prs().len(), 1);
}

#[tokio::test]
async fn spec_to_reviewed_pr_then_human_merge() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()).steps(3, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));

    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1);
    let Outcome::Reviewed { pr, verdict } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.kind, VerdictKind::Approve);
    assert_eq!(verdict.reviewer, "factory/trail-1-a1-r1");

    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview);
    assert_eq!(task.run.pr.as_ref(), Some(pr));
    assert_eq!(task.run.spend, Some(Usd::dollars(0.35)));
    assert_eq!(task.run.reviewed, Some(1));
    assert_eq!(task.run.sandbox.as_deref(), Some("trail-1-a1"));

    let opened = &r.codehost.prs()[0];
    assert_eq!(opened.spec.branch, "factory/trail-1");
    assert_eq!(
        opened.spec.title, "feat(core): greet [TRAIL-1]",
        "the PR is titled with the commit subject the repo squashes to"
    );
    let start = r.codehost.start(&opened.pr.repo, "factory/trail-1");
    assert_eq!(start.as_deref(), Some(FAKE_BASE), "published on the commit the checkout started from");
    assert_eq!(
        opened.comments,
        ["🔍 **Review by `factory/trail-1-a1-r1` (`fake-opus`) · Verdict: Approve**\n\nLooks right; tests cover it."]
    );
    assert!(
        opened
            .spec
            .body
            .ends_with("🤖 Opened by the Trailhead factory · worker `factory/trail-1-a1` · `fake-sonnet` · TRAIL-1"),
        "{}",
        opened.spec.body
    );
    let review = r.harness.contexts().into_iter().find(|c| c.role == Role::Reviewer).unwrap();
    assert!(review.diff.unwrap().contains("src/greet.rs"), "reviewer sees the diff");
    assert_eq!(review.model.0, "fake-opus");
    let reviewer_prep = r.daemon.prepared().into_iter().find(|(id, _)| id.0 == "trail-1-a1-r1").unwrap().1;
    assert_eq!(reviewer_prep.branch.as_deref(), Some("factory/trail-1"), "the reviewer's checkout is the PR head");
    let names: Vec<_> = review.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["fmt", "lint", "test"], "the reviewer is told which checks the worker ran");
    assert!(review.checks.iter().all(|c| c.passed));

    assert_eq!(busy(&r.compute), Vec::<String>::new(), "no sandbox stays busy after the run");
    let pooled = r.compute.get("trailhead-w1").expect("the worker's sandbox stays in the pool");
    assert_eq!(pooled.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));
    assert!(!pooled.labels.contains_key(label::ROLE), "an idle sandbox has no connector access");
    assert!(r.compute.egress("trailhead-w1").unwrap().domains.is_empty(), "and no egress");
    assert!(r.compute.get("trail-1-a1-r1").is_none(), "reviewer sandboxes aren't pooled");
    assert!(r.tracker.is_revoked("factory/trail-1-a1"));
    assert!(r.tracker.is_revoked("factory/trail-1-a1-r1"));
    assert!(r.tracker.comments(&key).iter().any(|c| c.kind == "review" && c.body.starts_with("approve")));

    // The human merges and finishes.
    let head = CodeHost::head(&r.codehost, pr).await.unwrap();
    assert_eq!(r.codehost.merge(pr, &head).await.unwrap(), MergeOutcome::Merged);
    r.tracker.finish(&key, "merged").await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::Done);
    assert!(r.factory.tick().await.unwrap().is_empty());
}

#[tokio::test]
async fn budget_breach_parks_the_task_and_suspends_its_sandbox() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    // $2 a step against a $5 budget: the third step breaches, the rest never run.
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));

    let ran = r.factory.tick().await.unwrap();
    let Outcome::Parked { reason } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert!(reason.contains("budget"), "{reason}");

    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.holder.as_deref(), Some("factory/trail-1-a1"));
    assert_eq!(task.run.spend, Some(Usd::dollars(6.0)));
    // Concluded with the spend, so a restart before the park lands can't have reconcile count the run twice.
    assert_eq!(task.run.concluded.as_deref(), Some("trail-1-a1"));
    assert!(r.tracker.comments(&key).iter().any(|c| c.kind == "question" && c.body.contains("is suspended")));

    let sandbox = r.compute.get("trailhead-w1").expect("sandbox kept for resuming");
    assert!(!sandbox.labels.contains_key(label::ROLE), "connector access cut");
    assert!(r.compute.egress("trailhead-w1").unwrap().domains.is_empty());
    assert_eq!(r.compute.checkpoints("trailhead-w1").last().unwrap(), &format!("parked: {reason}"));
    assert!(r.codehost.prs().is_empty());
}

#[tokio::test]
async fn a_breach_is_parked_and_recorded_even_when_the_checkpoint_fails() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    r.compute.fail("trailhead-w1", "checkpoint");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));

    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { .. }), "{:?}", ran[0].1);
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.spend, Some(Usd::dollars(6.0)));
    let sandbox = r.compute.get("trailhead-w1").unwrap();
    assert!(!sandbox.labels.contains_key(label::ROLE), "still cut off");
}

#[tokio::test]
async fn a_breached_sandbox_that_cant_be_relabelled_is_still_cut_off_and_destroyed() {
    for destroy_fails in [false, true] {
        let r = rig();
        let key = r.tracker.add_task("TRAIL", "Expensive", "");
        // The labels only start failing once the sandbox is acquired, as when it goes bad mid-run.
        r.compute.fail_after("trailhead-w1", "set_labels", 2);
        if destroy_fails {
            r.compute.fail("trailhead-w1", "destroy");
        }
        r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));

        let ran = r.factory.tick().await.unwrap();
        assert!(matches!(&ran[0].1, Outcome::Parked { .. }), "{:?}", ran[0].1);
        let task = r.tracker.task(&key);
        assert_eq!(task.status, Status::NeedsInput);
        assert_eq!(task.run.spend, Some(Usd::dollars(6.0)));
        let question = r.tracker.comments(&key).into_iter().find(|c| c.kind == "question").unwrap().body;
        assert!(question.contains("fresh checkout"), "{question}");
        assert!(!question.contains("is suspended"), "{question}");
        // Only claim the sandbox was destroyed when it was.
        assert_eq!(question.contains("was destroyed"), !destroy_fails, "{question}");
        assert_eq!(question.contains("being torn down"), destroy_fails, "{question}");
        if destroy_fails {
            assert_eq!(
                r.compute.egress("trailhead-w1"),
                Some(trailhead_factory::Egress::none()),
                "egress is cut before the labels"
            );
        } else {
            assert!(r.compute.get("trailhead-w1").is_none(), "a sandbox that can't be cut off is destroyed");
            assert_eq!(r.factory.sandbox_entries(), 0, "a destroyed sandbox's per-sandbox entries are forgotten");
        }
    }
}

#[tokio::test]
async fn a_task_that_fails_its_last_attempt_is_parked_even_when_the_sandbox_wont_release() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_attempts = 1"));
    let key = r.tracker.add_task("TRAIL", "Fails", "");
    r.compute.fail("trailhead-w1", "restore");
    r.compute.fail("trailhead-w1", "destroy");
    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "no luck".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await;
    assert!(matches!(ran, Ok(Outcome::Parked { .. })), "{ran:?}");
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
}

#[tokio::test]
async fn a_failed_park_after_giving_up_does_not_leave_the_gave_up_marker() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_attempts = 1"));
    let key = r.tracker.add_task("TRAIL", "Fails", "");
    r.tracker.fail_next("factory/trail-1-a1", TrackerOp::Ask, 1);
    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "no luck".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await;
    assert!(ran.is_err(), "{ran:?}");
    let task = r.tracker.task(&key);
    assert_ne!(task.status, Status::NeedsInput);
    assert_eq!(task.run.gave_up.unwrap_or(0), 0, "a task that wasn't parked doesn't get a fresh count");
    assert!(r.tracker.is_revoked("factory/trail-1-a1"), "the sub-agent is revoked, not left to expire");
}

#[tokio::test(start_paused = true)]
async fn a_park_that_cannot_claim_after_giving_up_revokes_the_sub_agent() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_attempts = 1"));
    let key = r.tracker.add_task("TRAIL", "Fails", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "no luck".into() }));
    // The run's own claim goes through; the park's claim is the next one.
    r.tracker.fail_after("factory/trail-1-a1", TrackerOp::Claim, 1, 1);
    let ran = r.factory.run_task(&r.tracker.task(&key)).await;
    assert!(ran.is_err(), "{ran:?}");
    assert!(r.tracker.is_revoked("factory/trail-1-a1"), "the sub-agent is revoked, not left to expire");
    assert_eq!(r.tracker.task(&key).run.gave_up.unwrap_or(0), 0);
    // The task is set back too: it isn't picked up again until the backoff is over.
    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "no luck".into() }));
    assert!(r.factory.tick().await.unwrap().is_empty(), "still backing off");
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    assert_eq!(r.factory.tick().await.unwrap().len(), 1, "retried once the backoff is over");
}

#[tokio::test]
async fn too_many_steps_is_a_breach() {
    let r = rig();
    r.tracker.add_task("TRAIL", "Chatty", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(50, 1));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("steps")));
}

#[tokio::test]
async fn failed_check_requeues_and_next_attempt_gets_a_fresh_sandbox() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Lint me", "");
    r.daemon.fail_check("lint", "warning: unused variable");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));

    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { reason } if reason.contains("lint")));
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open);
    assert_eq!(task.holder, None);
    assert_eq!(busy(&r.compute), Vec::<String>::new());
    assert!(r.codehost.prs().is_empty());
    let notes: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "note").collect();
    assert_eq!(notes.len(), 1, "the failure is noted on the task");
    assert!(
        notes[0].body.contains("`lint` failed") && notes[0].body.contains("warning: unused variable"),
        "{}",
        notes[0].body
    );
    assert_eq!(notes[0].author, "factory/trail-1-a1");

    // Fixed; the next tick is attempt 2 in a new sandbox under a new sub-agent.
    r.daemon.pass_check("lint");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Reviewed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).run.attempt, Some(2));
    assert!(r.tracker.agents().contains(&"factory/trail-1-a2".to_string()));
}

#[tokio::test]
async fn check_that_passes_on_rerun_still_opens_the_pr_with_a_note() {
    let r = rig();
    r.tracker.add_task("TRAIL", "Flaky", "");
    r.daemon.fail_check_times("lint", "flaked", 1);
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Reviewed { .. }), "{:?}", ran[0].1);
    let prs = r.codehost.prs();
    assert_eq!(prs.len(), 1);
    let flaky = trailhead_factory::messages::flaky_checks(&["lint".into()]);
    assert!(prs[0].spec.body.contains(flaky.trim_start()), "{}", prs[0].spec.body);
}

#[tokio::test]
async fn check_that_fails_twice_fails_the_attempt() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Broken", "");
    r.daemon.fail_check_times("lint", "still broken", 2);
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { reason } if reason.contains("lint")), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::Open);
    assert!(r.codehost.prs().is_empty());
}

#[tokio::test]
async fn agent_question_parks_the_task() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Ambiguous", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason == "Which crate?"));
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    assert_eq!(busy(&r.compute), ["trailhead-w1"], "kept for resuming");
}

#[tokio::test]
async fn the_next_worker_sees_the_answer_to_its_question() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Ambiguous", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    r.factory.tick().await.unwrap();
    r.tracker.answer(&key, "The core crate.");
    r.factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();

    let ctxs = r.harness.contexts();
    assert!(ctxs[0].task.context.is_empty());
    let answers = &ctxs[1].task.context.answers;
    assert_eq!(answers.len(), 1);
    assert_eq!((answers[0].question.as_str(), answers[0].answer.as_str()), ("Which crate?", "The core crate."));
    assert!(ctxs[1].task.context.notes.is_empty(), "the factory's own comments aren't passed on");
}

#[tokio::test]
async fn a_decision_on_a_queued_task_reaches_the_first_run() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Lock", "");
    r.tracker.decide(&key, "Use a per-machine flock.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();
    let notes = &r.harness.contexts()[0].task.context.notes;
    assert_eq!(notes.len(), 1);
    assert_eq!((notes[0].author.as_str(), notes[0].body.as_str()), ("lane", "Use a per-machine flock."));
}

#[tokio::test(start_paused = true)]
async fn a_daemon_that_ignores_cancel_is_cut_off_and_parked() {
    let r = rig_with(&no_stall());
    let key = r.tracker.add_task("TRAIL", "Wedged", "");
    r.daemon.ignore_cancel();
    r.harness.push(Role::Worker, Script::new(changes()).steps(3, 2_000_000).hang());

    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("budget")), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    let sandbox = r.compute.get("trailhead-w1").unwrap();
    assert!(!sandbox.labels.contains_key(label::ROLE));
    assert!(r.compute.egress("trailhead-w1").unwrap().domains.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_hung_agent_that_keeps_working_hits_the_wall_clock() {
    let r = rig_with(&no_stall());
    let key = r.tracker.add_task("TRAIL", "Stuck", "");
    r.harness.push(Role::Worker, Script::new(changes()).hang());
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("60 minutes")), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
}

#[tokio::test]
async fn a_refused_publish_parks_the_task_instead_of_redoing_the_work() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Fix CI", "");
    r.codehost.refuse_publish("github 404 Not Found on POST /repos/lanesawyer/trailhead/git/refs: Not Found");
    r.harness.push(Role::Worker, Script::new(changes()));

    let ran = r.factory.tick().await.unwrap();
    assert!(
        matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("publishing to GitHub failed")),
        "{:?}",
        ran[0].1
    );
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "parked for Lane, not requeued");
    assert_eq!(task.run.attempt, Some(1));
    let question = r.tracker.comments(&key).into_iter().find(|c| c.kind == "question").unwrap();
    assert!(question.body.contains("POST /repos/lanesawyer/trailhead/git/refs"), "{}", question.body);
    assert_eq!(busy(&r.compute), ["trailhead-w1"], "the sandbox keeps the finished checkout");
}

#[tokio::test(start_paused = true)]
async fn a_transient_publish_failure_is_retried_without_redoing_the_work() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Fix CI", "");
    r.codehost.flaky_publish(3);
    r.harness.push(Role::Worker, Script::new(changes()));

    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Reviewed { .. } | Outcome::AwaitingReview { .. }), "{:?}", ran[0].1);
    assert_eq!(r.codehost.publish_attempts(), 4);
    assert_ne!(r.tracker.task(&key).status, Status::NeedsInput);
}

#[tokio::test(start_paused = true)]
async fn a_transient_failure_opening_the_pr_is_retried() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Fix CI", "");
    r.codehost.flaky_open_pr(2);
    r.harness.push(Role::Worker, Script::new(changes()));

    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Reviewed { .. } | Outcome::AwaitingReview { .. }), "{:?}", ran[0].1);
    assert_eq!(r.codehost.calls("open_pr"), 3);
    assert_ne!(r.tracker.task(&key).status, Status::NeedsInput);
}

#[tokio::test(start_paused = true)]
async fn a_transient_failure_commenting_on_an_existing_pr_is_retried() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Tidy up.")));
    r.factory.tick().await.unwrap();
    assert_eq!(r.codehost.prs().len(), 1, "the PR is already open");

    // The rework's run retries a check, so its note goes on the PR as a comment, which fails twice first.
    r.daemon.fail_check_times("test", "flaked", 1);
    r.codehost.flaky_comment(2);
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Reviewed { .. } | Outcome::AwaitingReview { .. }), "{:?}", ran[0].1);

    let pr = &r.codehost.prs()[0];
    let flaky = trailhead_factory::messages::flaky_checks(&["test".into()]);
    assert_eq!(pr.comments.iter().filter(|c| c.contains(flaky.trim_start())).count(), 1, "{:?}", pr.comments);
    assert_eq!(r.codehost.calls("comment"), pr.comments.len() + 2, "two failed posts, each retried");
    assert_ne!(r.tracker.task(&key).status, Status::NeedsInput);
}

#[tokio::test(start_paused = true)]
async fn a_publish_that_keeps_failing_transiently_parks_the_task_after_the_retries() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Fix CI", "");
    r.codehost.flaky_publish(usize::MAX);
    r.harness.push(Role::Worker, Script::new(changes()));

    let ran = r.factory.tick().await.unwrap();
    assert!(
        matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("publishing to GitHub failed")),
        "{:?}",
        ran[0].1
    );
    assert!(r.codehost.publish_attempts() > 4);
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
}

#[tokio::test]
async fn a_task_that_keeps_failing_is_parked_after_max_attempts_and_retries_see_the_last_failure() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Lint me", "");
    r.daemon.fail_check("lint", "warning: unused variable");
    for attempt in 1..=3 {
        r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
        let ran = r.factory.tick().await.unwrap();
        assert_eq!(ran.len(), 1, "attempt {attempt} runs");
        if attempt < 3 {
            assert!(matches!(&ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
            assert_eq!(r.tracker.task(&key).status, Status::Open);
        } else {
            assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("lint")), "{:?}", ran[0].1);
        }
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.attempt, Some(3));
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("3 failed attempts") && questions[0].body.contains("unused variable"));
    assert!(r.factory.tick().await.unwrap().is_empty(), "a parked task isn't retried");

    let ctxs = r.harness.contexts();
    assert_eq!(ctxs.len(), 3);
    assert_eq!(ctxs[0].task.previous_failure, None);
    for ctx in &ctxs[1..] {
        let note = ctx.task.previous_failure.as_deref().expect("the last failure is passed on");
        assert!(note.contains("`lint` failed") && note.contains("warning: unused variable"), "{note}");
    }
}

#[tokio::test]
async fn failures_before_the_agent_starts_dont_use_up_attempts() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Broken host", "");
    for n in 1..=3 {
        r.compute.fail(&format!("trailhead-w{n}"), "set_labels");
    }
    for _ in 0..3 {
        let task = r.tracker.task(&key);
        assert!(r.factory.run_task(&task).await.is_err());
        let task = r.tracker.task(&key);
        assert_eq!(task.status, Status::Open, "not parked");
        assert_eq!(task.holder, None);
        assert_eq!(task.run.attempt.unwrap_or(0), 0);
    }
    assert!(r.tracker.comments(&key).iter().all(|c| c.kind != "question"));

    assert!(r.factory.tick().await.unwrap().is_empty(), "not retried before its backoff is over");
    assert!(r.harness.contexts().is_empty());

    // Fixed: the next attempt is attempt 1, under a name none of the failed runs used.
    for n in 1..=3 {
        r.compute.heal(&format!("trailhead-w{n}"));
    }
    r.factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Reviewed { .. }), "{ran:?}");
    assert_eq!(r.tracker.task(&key).run.attempt, Some(1));
    assert!(r.tracker.agents().contains(&"factory/trail-1-a1-3".to_string()), "{:?}", r.tracker.agents());
}

#[tokio::test(start_paused = true)]
async fn a_failure_before_the_agent_starts_is_retried_after_a_backoff() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Flaky host", "");
    for n in 1..=3 {
        r.compute.fail(&format!("trailhead-w{n}"), "set_labels");
    }
    assert!(r.factory.run_task(&r.tracker.task(&key)).await.is_err());
    for n in 1..=3 {
        r.compute.heal(&format!("trailhead-w{n}"));
    }
    r.factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert!(r.factory.tick().await.unwrap().is_empty(), "still backing off");
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1, "retried once the backoff is over");
    assert_eq!(r.tracker.task(&key).run.attempt, Some(1));
}

#[tokio::test(start_paused = true)]
async fn a_deliberate_stop_gives_the_attempt_back() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Hangs", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100).hang());
    let factory = Arc::new(r.factory);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(factory.clone().run(
        Arc::new(tokio::sync::Notify::new()),
        std::time::Duration::from_secs(60),
        async {
            let _ = stopped.await;
        },
    ));
    let tracker = r.tracker.clone();
    until("the task to start", || tracker.task(&key).status == Status::InProgress && !r.daemon.requests().is_empty())
        .await;
    stop.send(()).unwrap();
    handle.await.unwrap().unwrap();
    assert_eq!(r.tracker.task(&key).run.attempt, Some(0), "the stopped run doesn't count");

    // The next boot requeues it, and its retry runs as attempt 1 under a fresh name.
    factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Reviewed { .. }), "{ran:?}");
    assert_eq!(r.tracker.task(&key).run.attempt, Some(1));
    assert!(r.tracker.agents().contains(&"factory/trail-1-a1-1".to_string()), "{:?}", r.tracker.agents());
}

fn api_timeout() -> RunOutcome {
    RunOutcome::Failed { reason: "claude: API Error: No response from API (waited 3m)".into() }
}

#[tokio::test(start_paused = true)]
async fn a_silent_agent_is_failed_as_a_stall_without_using_an_attempt() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Silent", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000).hang());
    let started = tokio::time::Instant::now();
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(err.to_string().contains("stalled"), "{err}");
    let waited = started.elapsed();
    assert!(waited >= Duration::from_secs(5 * 60) && waited < Duration::from_secs(6 * 60), "{waited:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open, "not parked");
    assert_eq!(task.run.attempt, Some(0));
    assert!(task.run.spend.is_some_and(|s| s > Usd::ZERO), "the stalled run's spend is recorded: {:?}", task.run.spend);
    assert_eq!(task.run.infra_failures, Some(1));
}

#[tokio::test(start_paused = true)]
async fn a_slow_tool_isnt_a_stall() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Long build", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000).slow_tool(Duration::from_secs(20 * 60)));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Reviewed { .. }), "{ran:?}");
    assert_eq!(r.tracker.task(&key).run.infra_failures.unwrap_or(0), 0);
}

#[tokio::test]
async fn the_agents_api_timeouts_dont_use_up_attempts() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Slow API", "");
    for _ in 0..2 {
        r.harness.push(Role::Worker, Script::new(api_timeout()));
        let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
        assert!(matches!(ran, Outcome::Failed { .. }), "{ran:?}");
        let task = r.tracker.task(&key);
        assert_eq!(task.status, Status::Open, "not parked");
        assert_eq!(task.holder, None);
        assert_eq!(task.run.attempt, Some(0));
    }
    assert_eq!(r.tracker.task(&key).run.infra_failures, Some(2));
    assert!(r.tracker.comments(&key).iter().all(|c| c.kind != "question"));
    assert!(r.factory.tick().await.unwrap().is_empty(), "backing off");

    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Reviewed { .. }), "{ran:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.run.attempt, Some(1));
    assert_eq!(task.run.infra_failures, Some(0), "an agent that ends the run resets the count");
    let agents = r.tracker.agents();
    for name in ["factory/trail-1-a1-1", "factory/trail-1-a1-2"] {
        assert!(agents.contains(&name.to_string()), "{name} in {agents:?}");
    }
}

#[tokio::test]
async fn infrastructure_failures_past_the_cap_park_the_task_as_the_infrastructures_fault() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_infra_failures = 3"));
    let key = r.tracker.add_task("TRAIL", "Lost daemon", "");
    r.daemon.fail_runs(3, "not found: run trail-1 on this daemon");
    for n in 1..=3 {
        let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
        assert!(err.to_string().contains("not found: run"), "{err}");
        let task = r.tracker.task(&key);
        if n < 3 {
            assert_eq!(task.status, Status::Open, "failure {n} is retried");
        }
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.attempt, Some(0), "no attempt was used");
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    let body = &questions[0].body;
    assert!(body.contains("it's not the task") && body.contains("not found: run trail-1"), "{body}");
    assert!(!body.contains("failed attempts"), "{body}");
    assert!(r.factory.tick().await.unwrap().is_empty(), "a parked task isn't retried");
}

#[tokio::test]
async fn worker_infrastructure_failures_keep_counting_across_a_control_plane_restart() {
    let config = CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_infra_failures = 3");
    let mut r = rig_with(&config);
    let key = r.tracker.add_task("TRAIL", "Lost daemon", "");
    r.daemon.fail_runs(3, "not found: run trail-1 on this daemon");
    for n in 1..=3u32 {
        // Each failure is the first its factory has seen: only the tracker remembers the earlier ones.
        restart(&mut r, &config);
        let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
        assert!(err.to_string().contains("not found: run"), "{err}");
        let task = r.tracker.task(&key);
        if n < 3 {
            assert_eq!(task.status, Status::Open, "failure {n} is retried");
            assert_eq!(task.run.infra_failures, Some(n), "the count keeps growing");
        }
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "parked at max_infra_failures");
    assert_eq!(task.run.attempt, Some(0), "no attempt was used");
    assert_eq!(task.run.infra_failures.unwrap_or(0), 0, "parking resets the count, so a reopened task starts again");
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("it's not the task"), "{}", questions[0].body);

    // Lane answers, and a new factory sees one more failure: the streak starts again at 1, so it's retried.
    r.tracker.answer(&key, "the daemon is back");
    restart(&mut r, &config);
    r.daemon.fail_runs(1, "not found: run trail-1 on this daemon");
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(err.to_string().contains("not found: run"), "{err}");
    let task = r.tracker.task(&key);
    assert_eq!(task.run.infra_failures, Some(1), "the reopened task's streak starts over");
    assert_eq!(task.status, Status::Open, "retried, not parked");
}

#[tokio::test(start_paused = true)]
async fn infrastructure_retries_wait_longer_each_time_up_to_a_cap() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Down for a while", "");
    r.daemon.fail_runs(7, "connection lost");
    let secs = Duration::from_secs;
    // 1, 2, 4, 8, then 15 minutes at most.
    for (n, wait) in [60, 120, 240, 480, 900, 900, 900].into_iter().enumerate() {
        assert!(r.factory.tick().await.is_err(), "failure {}", n + 1);
        assert_eq!(r.tracker.task(&key).run.infra_failures, Some(n as u32 + 1));
        // Reconcile frees the failed run's sandbox, as the run loop does; the task itself still waits.
        r.factory.reconcile().await.unwrap();
        assert!(r.factory.tick().await.unwrap().is_empty(), "backing off for {wait}s");
        tokio::time::advance(secs(wait - 1)).await;
        assert!(r.factory.tick().await.unwrap().is_empty(), "still a second short of {wait}s");
        tokio::time::advance(secs(2)).await;
    }
    assert_eq!(r.tracker.task(&key).status, Status::Open, "under the cap of 10");
}

#[tokio::test(start_paused = true)]
async fn a_task_backing_off_at_the_head_of_the_queue_doesnt_hold_up_the_rest() {
    let r = rig();
    let first = r.tracker.add_task("TRAIL", "Down for a while", "");
    r.daemon.fail_runs(1, "connection lost");
    assert!(r.factory.tick().await.is_err());
    r.factory.reconcile().await.unwrap();
    let second = r.tracker.add_task("TRAIL", "Behind it", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    // One place, and the first task in the queue is still backing off: the one behind it takes the place (TRAIL-756).
    let started: Vec<_> = r.factory.tick().await.unwrap().into_iter().map(|(t, _)| t.key).collect();
    assert_eq!(started, [second]);
    assert_eq!(r.tracker.task(&first).status, Status::Open, "still waiting out its backoff");
}

/// A rework that can't start because the tracker failed before the run's sub-agent existed backs off without the
/// factory claiming the task. A claim and release of a task in review reads to core as the factory sending the work
/// to review, after which it could neither approve nor finish it (TRAIL-1462).
#[tokio::test]
async fn a_rework_that_fails_before_its_sub_agent_exists_doesnt_make_the_factory_the_worker() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test.")));
    r.factory.tick().await.unwrap();

    r.tracker.fail_sub_agents(true);
    assert!(r.factory.tick().await.is_err(), "the rework couldn't start");
    let task = r.tracker.task(&key);
    assert_eq!((task.status, task.holder), (Status::InReview, None));
    assert_eq!(task.run.infra_failures, Some(1), "counted without the claim");
    Tracker::approve(&r.tracker, &key).await.expect("the factory didn't do the work, so it may still approve it");
}

/// Reports and sub-agent creation both fail, so `infra_failed` runs every time with no persisted count.
#[tokio::test(start_paused = true)]
async fn infrastructure_retries_back_off_longer_when_the_tracker_is_down_entirely() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Tracker down", "");
    r.tracker.fail_reports(true);
    r.tracker.fail_sub_agents(true);
    backs_off_1_2_4_minutes(&r, &key).await;
}

/// Only reports fail: failure 1 comes from the lost `started` report, and 2 and 3 are skips past the revoked run name,
/// which grow the wait too.
#[tokio::test(start_paused = true)]
async fn infrastructure_retries_back_off_longer_when_the_tracker_wont_take_reports() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Reports down", "");
    r.tracker.fail_reports(true);
    backs_off_1_2_4_minutes(&r, &key).await;
}

async fn backs_off_1_2_4_minutes(r: &Rig, key: &TaskKey) {
    let secs = Duration::from_secs;
    // 1, 2, 4 minutes, though no failure count ever reaches the tracker.
    for (n, wait) in [60, 120, 240].into_iter().enumerate() {
        assert!(r.factory.tick().await.is_err(), "failure {}", n + 1);
        assert_eq!(r.tracker.task(key).run.infra_failures.unwrap_or(0), 0, "the write was lost");
        r.factory.reconcile().await.unwrap();
        assert!(r.factory.tick().await.unwrap().is_empty(), "backing off for {wait}s");
        tokio::time::advance(secs(wait - 1)).await;
        assert!(r.factory.tick().await.unwrap().is_empty(), "still a second short of {wait}s");
        tokio::time::advance(secs(2)).await;
    }
}

#[tokio::test]
async fn failures_that_are_the_agents_or_the_tasks_still_use_up_attempts() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Gives up", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "I couldn't make the build pass".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Failed { .. }), "{ran:?}");

    // A failed check changed the checkout, so it doesn't count toward the two give-ups in a row.
    r.daemon.fail_check("lint", "warning: unused variable");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Failed { .. }), "{ran:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.run.attempt, Some(2));
    assert_eq!(task.run.infra_failures.unwrap_or(0), 0);

    // Not API trouble, though it mentions one: a 400 about max_tokens: 5000.
    r.harness.push(
        Role::Worker,
        Script::new(RunOutcome::Failed { reason: "API Error: 400 invalid request, max_tokens: 5000".into() }),
    );
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(&ran, Outcome::Parked { .. }), "{ran:?}");
    let body = r.tracker.comments(&key).into_iter().find(|c| c.kind == "question").unwrap().body;
    assert!(body.contains("Gave up after 3 failed attempts"), "{body}");
    assert!(!body.contains("attempt_base"), "reopening is all it asks: {body}");
    assert_eq!(task.run.gave_up, None);
    assert_eq!(r.tracker.task(&key).run.gave_up, Some(3), "the marker records where it gave up");

    // Reopened by a human: the count starts again, so one more failure doesn't park it.
    r.tracker.set_status(&key, Status::Open);
    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "still broken".into() }));
    let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    assert!(matches!(ran, Outcome::Failed { .. }), "{ran:?}");
    let task = r.tracker.task(&key);
    assert_eq!((task.run.attempt, task.run.attempt_base), (Some(4), Some(3)));
}

#[tokio::test]
async fn a_sandbox_that_wont_release_after_the_agent_ends_the_run_doesnt_give_the_attempt_back() {
    for (outcome, status) in [
        (changes(), Status::InReview),
        (RunOutcome::Failed { reason: "I couldn't make the build pass".into() }, Status::Open),
    ] {
        let r = rig();
        let key = r.tracker.add_task("TRAIL", "Ends the run", "");
        r.harness.push(Role::Worker, Script::new(outcome));
        // Restoring and destroying are all releasing does after the run; preparing a new sandbox does neither.
        r.compute.fail("trailhead-w1", "restore");
        r.compute.fail("trailhead-w1", "destroy");
        r.harness.push(Role::Reviewer, Script::new(approve()));
        let ran = r.factory.run_task(&r.tracker.task(&key)).await;
        if status == Status::InReview {
            // A release that fails is logged and left to reconcile; the PR still goes to review.
            assert!(matches!(ran, Ok(Outcome::Reviewed { .. })), "{ran:?}");
        } else {
            assert!(matches!(ran, Ok(Outcome::Failed { .. })), "{ran:?}");
        }
        let task = r.tracker.task(&key);
        assert_eq!(task.status, status);
        assert_eq!(task.run.attempt, Some(1), "the agent ended the run, so the attempt counts");
        assert_eq!(task.run.infra_failures.unwrap_or(0), 0);
        assert_eq!(task.run.retries.unwrap_or(0), 0);
    }
}

#[tokio::test]
async fn a_worker_that_cant_record_its_failed_check_still_counts_the_attempt_and_releases_the_task() {
    for (op, skip) in [(TrackerOp::Note, 0), (TrackerOp::Report, 2), (TrackerOp::Release, 0)] {
        let r = rig();
        let key = r.tracker.add_task("TRAIL", "Lint me", "");
        r.daemon.fail_check("lint", "warning: unused variable");
        r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
        // Only the daemon's call fails, not the control plane's own `report`s as the same sub-agent before it.
        r.tracker.fail_after("factory/trail-1-a1", op, skip, 1);

        let ran = r.factory.run_task(&r.tracker.task(&key)).await;
        let Ok(Outcome::Failed { reason }) = ran else { panic!("{op}: {ran:?}") };
        assert!(reason.contains("finishing the run failed"), "{op}: {reason}");
        let task = r.tracker.task(&key);
        assert_eq!(task.status, Status::Open, "{op}: the daemon released the task");
        assert_eq!(task.holder, None);
        assert_eq!(task.run.attempt, Some(1), "{op}: the agent ended the run, so the attempt counts");
        assert_eq!(task.run.infra_failures.unwrap_or(0), 0);
        assert_eq!(task.run.retries.unwrap_or(0), 0);
        assert!(r.codehost.prs().is_empty());
    }
}

/// The daemon's fallback release fails too, so the task is still held by the sub-agent the control plane revoked.
/// The attempt counts, and the task waits out its lease (TRAIL-614).
#[tokio::test]
async fn a_worker_whose_fallback_release_fails_too_leaves_the_task_for_reconcile() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Lint me", "");
    r.daemon.fail_check("lint", "warning: unused variable");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.tracker.fail_next("factory/trail-1-a1", TrackerOp::Release, 2);

    let ran = r.factory.run_task(&r.tracker.task(&key)).await;
    let Ok(Outcome::Failed { reason }) = ran else { panic!("{ran:?}") };
    assert!(reason.contains("finishing the run failed"), "{reason}");
    let task = r.tracker.task(&key);
    assert_eq!(task.run.attempt, Some(1));
    assert_eq!(task.run.infra_failures.unwrap_or(0), 0);
    assert_eq!(task.status, Status::InProgress, "{task:?}");
    assert_eq!(task.holder.as_deref(), Some("factory/trail-1-a1"));
    assert!(r.codehost.prs().is_empty());

    // Reconcile can't release it: the revoked sub-agent is the holder and is refused. The task waits for its lease
    // to lapse, which returns it to the queue, and the next run is the attempt after.
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.requeued.is_empty(), "{done:?}");
    let task = r.tracker.task(&key);
    assert_eq!((task.status, task.holder.as_deref()), (Status::InProgress, Some("factory/trail-1-a1")));
    assert!(r.factory.tick().await.unwrap().is_empty(), "a held task is not picked up");

    r.tracker.expire_claim(&key);
    assert_eq!(r.tracker.task(&key).status, Status::Open);
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1);
    assert_eq!(r.tracker.task(&key).run.attempt, Some(2));
}

/// The PR is open and the checks passed, so a worker that can't ask for review doesn't make a failed attempt (TRAIL-441).
#[tokio::test]
async fn a_worker_that_cant_ask_for_review_after_opening_the_pr_still_gets_it_reviewed() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    // The daemon tries three times; the control plane's own request goes through.
    r.tracker.fail_next("factory/trail-1-a1", TrackerOp::RequestReview, 3);

    let ran = r.factory.run_task(&r.tracker.task(&key)).await;
    assert!(matches!(ran, Ok(Outcome::Reviewed { .. })), "{ran:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview);
    assert_eq!(r.codehost.prs().len(), 1);
    assert!(task.run.pr.is_some());
}

#[tokio::test]
async fn a_task_parked_for_infrastructure_failures_starts_without_a_wait_when_reopened() {
    let r = rig_with(
        &CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_infra_failures = 2\nmax_sandboxes = 2"),
    );
    let key = r.tracker.add_task("TRAIL", "Lost daemon", "");
    r.daemon.fail_runs(2, "connection lost");
    for _ in 0..2 {
        r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
        r.factory.reconcile().await.unwrap();
    }
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);

    r.tracker.set_status(&key, Status::Open);
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1, "no backoff left over from before the park");
}

#[tokio::test]
async fn a_harness_that_publishes_an_unconventional_subject_still_gets_a_conventional_pr_title() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    let outcome = RunOutcome::Changes {
        commits: vec![Commit {
            message: "added a greeting".into(),
            changes: vec![FileChange::new("src/greet.rs", Some(b"pub fn hi() {}".to_vec()))],
            merges: vec![],
        }],
        summary: "Adds a greeting.".into(),
    };
    r.harness.push(Role::Worker, Script::new(outcome));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    let opened = r.codehost.prs().pop().expect("a PR");
    assert_eq!(opened.spec.title, "chore: Add a greeting [TRAIL-1]");
}

#[tokio::test]
async fn a_warm_sandbox_that_fails_prepare_once_is_released_and_keeps_its_cache() {
    let r = rig();
    let first = r.tracker.add_task("TRAIL", "Warms the pool", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));
    r.factory.run_task(&r.tracker.task(&first)).await.unwrap();

    let second = r.tracker.add_task("TRAIL", "Hits a transient failure", "");
    r.daemon.fail_prepares(1, "git fetch failed");
    let err = r.factory.run_task(&r.tracker.task(&second)).await.unwrap_err();
    assert!(err.to_string().contains("git fetch failed"), "{err}");
    let kept = r.compute.get("trailhead-w1").expect("the sandbox survives one failed prepare");
    assert_eq!(kept.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));
    assert!(kept.labels.contains_key(label::WARM));
    assert_eq!(r.tracker.task(&second).run.infra_failures, Some(1), "an infrastructure failure");
}

#[tokio::test]
async fn a_good_prepare_between_two_failed_ones_resets_the_failure_count() {
    let r = rig();
    let first = r.tracker.add_task("TRAIL", "Warms the pool", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));
    r.factory.run_task(&r.tracker.task(&first)).await.unwrap();

    let second = r.tracker.add_task("TRAIL", "Fails, then works", "");
    r.daemon.fail_prepares(1, "git fetch failed");
    r.factory.run_task(&r.tracker.task(&second)).await.unwrap_err();
    assert!(r.compute.get("trailhead-w1").is_some(), "kept after the first failure");

    // A good prepare on the same sandbox clears its count.
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));
    r.factory.run_task(&r.tracker.task(&second)).await.unwrap();

    let third = r.tracker.add_task("TRAIL", "Fails again", "");
    r.daemon.fail_prepares(1, "git fetch failed");
    r.factory.run_task(&r.tracker.task(&third)).await.unwrap_err();
    let kept = r.compute.get("trailhead-w1").expect("one failure after a good prepare isn't two in a row");
    assert_eq!(kept.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));
}

#[tokio::test]
async fn a_warm_sandbox_that_fails_prepare_repeatedly_is_destroyed_and_the_next_task_gets_a_fresh_one() {
    let r = rig();
    let first = r.tracker.add_task("TRAIL", "Warms the pool", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));
    r.factory.run_task(&r.tracker.task(&first)).await.unwrap();
    let idle = r.compute.get("trailhead-w1").expect("the first task leaves a warm sandbox");
    assert_eq!(idle.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));
    assert!(idle.labels.contains_key(label::WARM));

    let second = r.tracker.add_task("TRAIL", "Hits the broken checkout", "");
    r.daemon.fail_prepares(2, "git clone failed");
    r.factory.run_task(&r.tracker.task(&second)).await.unwrap_err();
    assert!(r.compute.get("trailhead-w1").is_some(), "kept after the first failure");
    let err = r.factory.run_task(&r.tracker.task(&second)).await.unwrap_err();
    assert!(err.to_string().contains("git clone failed"), "{err}");
    assert!(r.compute.get("trailhead-w1").is_none(), "destroyed after the second, not relabelled idle");
    assert_eq!(r.tracker.task(&second).run.infra_failures, Some(2), "an infrastructure failure");
    assert_eq!(r.tracker.task(&second).run.attempt.unwrap_or(0), 0, "no attempt was used");

    let third = r.tracker.add_task("TRAIL", "Gets a fresh sandbox", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));
    r.factory.run_task(&r.tracker.task(&third)).await.unwrap();
    let fresh = r.compute.get("trailhead-w1").expect("a new sandbox took the freed name");
    // Release restores the warm checkpoint of the sandbox it returns to the pool; a restore at acquire would be a second.
    assert_eq!(r.compute.restores("trailhead-w1").len(), 1, "only release restored it, acquire did not: {fresh:?}");
    assert_eq!(r.compute.checkpoints("trailhead-w1").len(), 1, "built from scratch, not restored: {fresh:?}");
}

#[tokio::test]
async fn a_reviewer_sandbox_that_fails_prepare_is_destroyed_and_its_daemon_forgotten() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
    let forgotten_before = r.daemon.forgotten().len();

    // The worker's prepare worked; the next review's sandbox is the one that fails.
    r.daemon.fail_prepares(1, "git clone failed");
    let shown = r.tracker.task(&key);
    let pr = shown.run.pr.clone().expect("a PR");
    let reviewer = trailhead_factory::review_name(&key, shown.run.attempt.unwrap_or(1), 2);
    let err = r.factory.review(&shown, &pr, 1, &[]).await.unwrap_err();
    assert!(err.to_string().contains("git clone failed"), "{err}");
    assert!(r.compute.get(&reviewer).is_none(), "the reviewer's sandbox is destroyed");
    let forgotten = r.daemon.forgotten();
    assert!(
        forgotten[forgotten_before..].iter().any(|id| id.0 == reviewer),
        "the reviewer's daemon connection is forgotten with its sandbox: {forgotten:?}"
    );
}

fn with_setup() -> Rig {
    rig_with(&CONFIG.replace("[checks]\n", "[checks]\nsetup = \"cargo build --all-targets\"\n"))
}

#[tokio::test]
async fn a_rework_whose_setup_fails_starts_the_agent_and_keeps_the_sandbox() {
    let mut r = with_setup();
    r.factory = r.factory.with_codehost(Arc::new(r.codehost.clone()));
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(request_changes("Add a test for `hi()`.")));
    r.factory.tick().await.unwrap();

    r.daemon.fail_setups(1, "error[E0063]: missing field `spend` in initializer of `RunRecord`");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Reviewed { .. }), "{:?}", ran[0].1);

    let task = r.tracker.task(&key);
    assert_eq!(task.run.infra_failures, None, "not an infrastructure failure");
    assert_eq!(task.run.attempt, Some(2));
    let reqs = r.daemon.requests();
    let rework = reqs.iter().rfind(|q| q.role == Role::Worker).unwrap();
    let body = &rework.feedback.as_ref().expect("the rework is told").body;
    assert!(body.contains("Add a test for `hi()`.") && body.contains("E0063"), "{body}");
    let sandbox = r.compute.get("trailhead-w1").expect("the sandbox is kept");
    assert_eq!(sandbox.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));
    let prepared = r.daemon.prepared();
    assert!(prepared.last().is_some_and(|(_, p)| p.setup.is_none()), "finished without the failing setup");
}

#[tokio::test]
async fn a_fresh_run_whose_setup_fails_is_retried_once_then_parked_with_the_output() {
    let r = with_setup();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.daemon.fail_setups(1, "error[E0063]: broken base");
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(err.to_string().contains("setup failed") && err.to_string().contains("E0063"), "{err}");
    let task = r.tracker.task(&key);
    assert_eq!(task.run.infra_failures, Some(1));
    assert_eq!(task.run.attempt.unwrap_or(0), 0, "no attempt was used");
    assert!(r.daemon.requests().is_empty(), "no agent started");
    assert_eq!(task.status, Status::Open, "the first failure is retried");

    r.daemon.fail_setups(1, "error[E0063]: broken base");
    r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "the same setup error twice parks the task");
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("base branch") && questions[0].body.contains("E0063"), "{}", questions[0].body);
    assert!(r.compute.get("trailhead-w1").is_none(), "a new sandbox has no warm checkpoint, so release destroys it");
}

#[tokio::test(start_paused = true)]
async fn a_setup_failure_after_an_unrelated_infra_failure_is_retried_not_parked() {
    let r = with_setup();
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000).hang());
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(err.to_string().contains("stalled"), "{err}");

    r.daemon.fail_setups(1, "error[E0063]: broken base");
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(err.to_string().contains("setup failed"), "{err}");
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open, "one setup failure after a stall isn't a broken base");
    assert_eq!(task.run.infra_failures, Some(2));

    r.daemon.fail_setups(1, "error[E0063]: broken base");
    r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput, "the second setup failure in a row parks it");
}

#[tokio::test]
async fn a_fresh_run_whose_setup_fails_returns_a_warm_idle_sandbox_to_the_pool() {
    let r = with_setup();
    let first = r.tracker.add_task("TRAIL", "Warms the pool", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));
    r.factory.run_task(&r.tracker.task(&first)).await.unwrap();
    let warm = r.compute.get("trailhead-w1").unwrap().labels.get(label::WARM).cloned().expect("warmed");

    let second = r.tracker.add_task("TRAIL", "Hits a broken base", "");
    r.daemon.fail_setups(1, "error[E0063]: broken base");
    let err = r.factory.run_task(&r.tracker.task(&second)).await.unwrap_err();
    assert!(err.to_string().contains("setup failed"), "{err}");
    assert_eq!(r.tracker.task(&second).run.infra_failures, Some(1));
    let sandbox = r.compute.get("trailhead-w1").expect("the warm sandbox is kept");
    assert_eq!(sandbox.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));
    assert_eq!(sandbox.labels.get(label::WARM), Some(&warm), "still on its warm checkpoint");
}

#[tokio::test]
async fn a_breach_park_cut_short_is_finished_by_reconcile_without_a_rerun_or_a_second_count() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    // The control plane goes down after the breach was reported and the sandbox cut off, before the question lands.
    r.tracker.fail_next("factory/trail-1-a1", TrackerOp::Ask, 1);
    assert!(r.factory.run_task(&r.tracker.task(&key)).await.is_err());
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InProgress);
    assert_eq!(task.run.spend, Some(Usd::dollars(6.0)));
    assert_eq!(task.run.concluded.as_deref(), Some("trail-1-a1"));
    assert!(task.run.worker_breach.is_some(), "recorded with the spend");

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.breached, std::slice::from_ref(&key.0));
    assert!(done.requeued.is_empty() && done.reattached.is_empty() && done.released.is_empty(), "{done:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.holder.as_deref(), Some("factory/trail-1-a1"));
    assert_eq!(task.run.spend, Some(Usd::dollars(6.0)), "counted once");
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.starts_with("Budget breached") && questions[0].body.contains("is suspended"));

    // The suspended sandbox stays as it was, not released to idle, and nothing is asked again.
    let sandbox = r.compute.get("trailhead-w1").expect("the suspended sandbox is kept");
    assert_eq!(sandbox.labels.get(label::STATE).map(String::as_str), Some(label::BUSY));
    assert!(!sandbox.labels.contains_key(label::ROLE));
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.breached.is_empty() && done.released.is_empty(), "{done:?}");
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
}

#[tokio::test]
async fn a_breach_park_finished_after_the_sandbox_was_destroyed_says_it_isnt_suspended() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    r.tracker.fail_next("factory/trail-1-a1", TrackerOp::Ask, 1);
    assert!(r.factory.run_task(&r.tracker.task(&key)).await.is_err());
    // The sandbox is gone by the time the control plane is back.
    r.compute.destroy(&r.compute.get("trailhead-w1").unwrap().id).await.unwrap();

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.breached, std::slice::from_ref(&key.0));
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.spend, Some(Usd::dollars(6.0)), "counted once");
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.starts_with("Budget breached"), "{}", questions[0].body);
    assert!(!questions[0].body.contains("is suspended"), "{}", questions[0].body);
    assert!(questions[0].body.contains("was destroyed"), "{}", questions[0].body);
}

#[tokio::test]
async fn a_breach_park_finished_when_the_sandbox_cant_be_cut_off_destroys_it_and_still_parks() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    r.tracker.fail_next("factory/trail-1-a1", TrackerOp::Ask, 1);
    assert!(r.factory.run_task(&r.tracker.task(&key)).await.is_err());
    // The cut-off that reconcile repeats fails this time.
    r.compute.fail("trailhead-w1", "set_labels");

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.breached, std::slice::from_ref(&key.0));
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.spend, Some(Usd::dollars(6.0)), "counted once");
    assert!(task.run.worker_breach.as_ref().is_some_and(|b| b.parked));
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(!questions[0].body.contains("is suspended"), "{}", questions[0].body);
    assert!(questions[0].body.contains("was destroyed"), "{}", questions[0].body);
    assert!(r.compute.get("trailhead-w1").is_none(), "a sandbox that can't be cut off is destroyed");
}

#[tokio::test]
async fn a_breach_park_finished_when_the_sandbox_cant_be_cut_off_or_destroyed_parks_and_a_later_reconcile_destroys_it()
{
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    r.tracker.fail_next("factory/trail-1-a1", TrackerOp::Ask, 1);
    assert!(r.factory.run_task(&r.tracker.task(&key)).await.is_err());
    // Both the cut-off that reconcile repeats and the destroy that follows it fail.
    r.compute.fail("trailhead-w1", "set_labels");
    r.compute.fail("trailhead-w1", "destroy");

    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.breached, std::slice::from_ref(&key.0));
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "still parked");
    assert_eq!(task.run.spend, Some(Usd::dollars(6.0)), "counted once");
    assert!(task.run.worker_breach.as_ref().is_some_and(|b| b.parked && b.stuck));
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("being torn down"), "{}", questions[0].body);
    assert!(!questions[0].body.contains("was destroyed"), "{}", questions[0].body);
    assert!(r.compute.get("trailhead-w1").is_some(), "the destroy failed, so the sandbox is still live");

    // Once the sandbox answers again, the next reconcile destroys it and leaves the park alone.
    r.compute.heal("trailhead-w1");
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.destroyed.contains(&"trailhead-w1".to_string()), "{done:?}");
    assert!(done.breached.is_empty(), "{done:?}");
    assert!(r.compute.get("trailhead-w1").is_none());
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput, "still parked");
}

#[tokio::test]
async fn a_breached_sandbox_that_couldnt_be_destroyed_is_destroyed_by_a_later_reconcile_while_parked() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    // The cut-off fails, and so does the destroy that follows it, once.
    r.compute.fail_after("trailhead-w1", "set_labels", 2);
    r.compute.fail_once_after("trailhead-w1", "destroy", 0);
    assert!(matches!(r.factory.run_task(&r.tracker.task(&key)).await.unwrap(), Outcome::Parked { .. }));
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert!(task.run.worker_breach.as_ref().is_some_and(|b| b.parked && b.stuck));
    assert!(r.compute.get("trailhead-w1").is_some(), "the destroy failed, so the sandbox is still live");

    let done = r.factory.reconcile().await.unwrap();
    assert!(done.destroyed.contains(&"trailhead-w1".to_string()), "{done:?}");
    assert!(r.compute.get("trailhead-w1").is_none());
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput, "still parked");
}

#[tokio::test]
async fn a_stale_stuck_breach_doesnt_get_a_later_runs_parked_sandbox_destroyed() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    r.compute.fail_after("trailhead-w1", "set_labels", 2);
    r.compute.fail_once_after("trailhead-w1", "destroy", 0);
    assert!(matches!(r.factory.run_task(&r.tracker.task(&key)).await.unwrap(), Outcome::Parked { .. }));
    assert!(r.tracker.task(&key).run.worker_breach.as_ref().is_some_and(|b| b.parked && b.stuck));

    // Lane answers and the task is requeued; its next run parks with an ordinary question.
    r.tracker.answer(&key, "Raised the budget; carry on.");
    r.factory.reconcile().await.unwrap();
    r.compute.heal("trailhead-w1");
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which crate?".into() }));
    r.factory.tick().await.unwrap();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    let host = task.run.host.clone().expect("the parked run has a host");
    assert!(r.compute.get(&host).is_some(), "the parked run's sandbox is suspended");

    let done = r.factory.reconcile().await.unwrap();
    assert!(!done.destroyed.contains(&host), "{done:?}");
    assert!(r.compute.get(&host).is_some(), "the later run's parked sandbox is kept");
}
#[tokio::test]
async fn an_answer_to_a_breach_within_the_lease_requeues_the_task_without_asking_again() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Expensive", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(10, 2_000_000));
    assert!(matches!(r.factory.run_task(&r.tracker.task(&key)).await.unwrap(), Outcome::Parked { .. }));
    assert!(r.tracker.task(&key).run.worker_breach.as_ref().is_some_and(|b| b.parked));

    r.tracker.answer_within_lease(&key, "Raised the budget; carry on.");
    let task = r.tracker.task(&key);
    assert_eq!((task.status, task.holder.as_deref()), (Status::InProgress, Some("factory/trail-1-a1")));
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.breached.is_empty(), "the answered park isn't finished again: {done:?}");
    assert_eq!(done.requeued, std::slice::from_ref(&key.0));
    assert_eq!(r.tracker.task(&key).status, Status::Open);
    let questions = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").count();
    assert_eq!(questions, 1, "asked once");
}

#[tokio::test]
async fn a_failed_revoke_doesnt_undo_a_park_or_a_failure() {
    for (max, parked) in [(1, true), (3, false)] {
        let r =
            rig_with(&CONFIG.replace("name = \"trailhead\"", &format!("name = \"trailhead\"\nmax_attempts = {max}")));
        let key = r.tracker.add_task("TRAIL", "Fails", "");
        r.tracker.fail_next("factory", TrackerOp::RevokeSubAgent, 1);
        r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "no luck".into() }));
        let ran = r.factory.run_task(&r.tracker.task(&key)).await;
        if parked {
            assert!(matches!(ran, Ok(Outcome::Parked { .. })), "{ran:?}");
            assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
        } else {
            assert!(matches!(ran, Ok(Outcome::Failed { .. })), "{ran:?}");
            assert_eq!(r.tracker.task(&key).status, Status::Open);
        }
        assert!(!r.tracker.is_revoked("factory/trail-1-a1"), "the injected revoke failure fired on the worker");
    }
}

#[tokio::test]
async fn the_status_board_follows_a_run_from_a_cold_sandbox_through_review() {
    use trailhead_factory::{ControlState, FactoryEvent, RunPhase, SandboxState};

    let r = rig();
    r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()).steps(3, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));
    let (before, mut rx) = r.factory.status().subscribe();
    assert_eq!((before.seq, before.runs.len()), (0, 0));

    r.factory.tick().await.unwrap();

    let events: Vec<FactoryEvent> = std::iter::from_fn(|| rx.try_recv().ok()).map(|e| e.event).collect();
    let phases: Vec<RunPhase> = events
        .iter()
        .filter_map(|e| match e {
            FactoryEvent::RunPhase { phase, .. } => Some(*phase),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases.first(),
        Some(&RunPhase::Preparing { cold: false }),
        "a run starts when its sandbox is being got: {phases:?}"
    );
    let agent = phases.iter().position(|p| *p == RunPhase::Agent).expect("the agent phase");
    // The reviewer's `begin` also emits a cold Preparing, so only one before the agent phase is the worker's.
    assert!(
        phases[..agent].contains(&RunPhase::Preparing { cold: true }),
        "no warm checkpoint the first time, so the worker's sandbox is cold: {phases:?}"
    );
    let reviewing = phases.iter().position(|p| *p == RunPhase::Reviewing).expect("the reviewing phase");
    assert!(agent < reviewing, "{phases:?}");
    let steps = events.iter().filter(|e| matches!(e, FactoryEvent::Step { .. })).count();
    assert!(steps >= 3, "the worker's steps are in the stream: {events:?}");
    assert!(matches!(events.last(), Some(FactoryEvent::RunEnded { .. })), "{events:?}");

    let after = r.factory.status().snapshot();
    assert!(after.runs.is_empty(), "a finished run leaves the board");
    assert_ne!(after.control, ControlState::Stopped);
    assert!(after.seq > 0);
    // The pooled worker went back to idle; the reviewer's own sandbox is gone.
    assert_eq!(after.sandboxes.len(), 1, "{after:?}");
    assert_eq!(after.sandboxes[0].state, SandboxState::Idle);
}

const SCOPED: &str = "\n[checks.ui-lint]\nrun = \"cargo clippy -p ui\"\npaths = [\"clients/**\"]\n";

#[tokio::test]
async fn a_path_scoped_check_is_skipped_and_reported_when_the_diff_misses_its_paths() {
    let r = rig_with(&CONFIG.replace("[checks]", &format!("{SCOPED}\n[checks]")));
    r.tracker.add_task("TRAIL", "Backend only", "");
    r.daemon.change_files(&["crates/a/src/lib.rs"]);
    // Would fail the PR if it ran.
    r.daemon.fail_check("ui-lint", "broken");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Reviewed { .. }), "{:?}", ran[0].1);

    let review = r.harness.contexts().into_iter().find(|c| c.role == Role::Reviewer).unwrap();
    let ui = review.checks.iter().find(|c| c.name == "ui-lint").expect("the scoped check is reported");
    assert!(ui.skipped && ui.passed);
    assert!(review.checks.iter().filter(|c| c.name != "ui-lint").all(|c| !c.skipped));
    let body = &r.codehost.prs()[0].spec.body;
    assert!(body.contains("Skipped, not run: `ui-lint`"), "{body}");
}

#[tokio::test]
async fn a_path_scoped_check_runs_when_the_diff_touches_its_paths() {
    let r = rig_with(&CONFIG.replace("[checks]", &format!("{SCOPED}\n[checks]")));
    r.tracker.add_task("TRAIL", "Touch the UI", "");
    r.daemon.change_files(&["crates/a/src/lib.rs", "clients/lookout/src/main.rs"]);
    r.daemon.fail_check("ui-lint", "broken");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
    assert!(r.codehost.prs().is_empty(), "the failing UI check stopped the PR");
}

#[tokio::test]
async fn two_attempts_in_a_row_that_give_up_park_the_task_with_both_reasons() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Needs the network", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "crates.io unreachable".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::Open, "one give-up is retried");

    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "still cannot reach crates.io".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { .. }), "{:?}", ran[0].1);
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput);
    assert_eq!(task.run.gave_up, Some(2));
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(
        questions[0].body.contains("crates.io unreachable") && questions[0].body.contains("still cannot reach"),
        "{}",
        questions[0].body
    );
    assert!(r.factory.tick().await.unwrap().is_empty(), "a parked task isn't retried");
    assert_eq!(r.harness.contexts().len(), 2);
}

#[tokio::test]
async fn a_give_up_after_failed_checks_is_retried_because_that_attempt_changed_the_checkout() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_attempts = 4"));
    let key = r.tracker.add_task("TRAIL", "Lint then give up", "");
    r.daemon.fail_check("lint", "warning: unused variable");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);

    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "I couldn't make the build pass".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::Open, "only one attempt gave up");

    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "still can't".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { .. }), "{:?}", ran[0].1);
}

#[tokio::test]
async fn a_daemon_that_errors_after_the_workers_usage_still_records_the_spend() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Dropped mid-run", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000).hang());
    r.daemon.fail_runs_midway(1, "connection lost");
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(err.to_string().contains("connection lost"), "{err}");
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::Open, "not parked");
    assert_eq!(task.run.attempt, Some(0));
    assert!(task.run.spend.is_some_and(|s| s > Usd::ZERO), "the lost run's spend is recorded: {:?}", task.run.spend);
    assert_eq!(task.run.infra_failures, Some(1));
}

#[tokio::test]
async fn stack_env_reaches_the_agent_run_and_every_check() {
    let r = rig_with(&CONFIG.replace(
        "[checks]\n",
        "[stack]\nenv = { CARGO_BUILD_JOBS = \"2\" }\n\n[checks]\nsetup = \"cargo build\"\ntrim = \"true\"\n",
    ));
    r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    r.factory.tick().await.unwrap();

    let jobs = |env: &std::collections::BTreeMap<String, String>| env.get("CARGO_BUILD_JOBS").cloned();
    assert_eq!(r.daemon.prepared()[0].1.env["CARGO_BUILD_JOBS"], "2");
    let request = r.daemon.requests().into_iter().next().unwrap();
    assert_eq!(request.env["CARGO_BUILD_JOBS"], "2", "the agent's run request carries it");
    let checks = r.daemon.check_envs();
    let ran: Vec<_> = checks.iter().map(|(n, _)| n.as_str()).collect();
    for name in ["setup", "trim", "fmt", "lint", "test"] {
        assert!(ran.contains(&name), "{name} ran: {ran:?}");
    }
    for (name, env) in &checks {
        assert_eq!(jobs(env).as_deref(), Some("2"), "{name} sees the stack's env");
    }
}

#[test]
fn stack_env_cant_set_a_name_the_factory_does() {
    let src = CONFIG.replace("[checks]\n", "[stack]\nenv = { HTTPS_PROXY = \"http://elsewhere\" }\n\n[checks]\n");
    let err = trailhead_factory::FactoryConfig::parse_table(&src, "TRAIL", None).unwrap_err();
    assert!(err.to_string().contains("HTTPS_PROXY"), "{err}");
}

#[tokio::test]
async fn a_give_up_from_before_a_question_doesnt_count_toward_two_in_a_row() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_attempts = 5"));
    let key = r.tracker.add_task("TRAIL", "Needs the network", "");
    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "crates.io unreachable".into() }));
    r.factory.tick().await.unwrap();
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which registry?".into() }));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    assert_eq!(r.tracker.task(&key).run.asked, Some(2), "the question is marked");

    r.tracker.answer(&key, "The mirror.");
    r.factory.reconcile().await.unwrap();
    r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: "mirror unreachable too".into() }));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { .. }), "{:?}", ran[0].1);
    assert_eq!(r.tracker.task(&key).status, Status::Open, "one give-up since the question is retried");
    let ctxs = r.harness.contexts();
    let note = ctxs.last().unwrap().task.previous_failure.as_deref().expect("the earlier give-up is still passed on");
    assert!(note.contains("crates.io unreachable"), "{note}");
}

#[tokio::test]
async fn a_permanent_rejection_parks_the_task_at_once() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Refused", "");
    r.daemon.reject_runs("sprites api 400 Bad Request: bad size");
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(matches!(err, trailhead_factory::Error::Rejected(_)), "{err:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "parked on the first rejection");
    assert_eq!(task.run.attempt, Some(0), "no attempt was used");
    assert_eq!(task.run.infra_failures, Some(0));
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("400 Bad Request: bad size"), "{}", questions[0].body);
    assert!(r.factory.tick().await.unwrap().is_empty(), "a parked task isn't retried");
}

#[tokio::test]
async fn a_rejected_sandbox_create_parks_the_task_at_once() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Refused at create", "");
    r.compute.reject_creates("sprites api 400 Bad Request: bad size");
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(matches!(err, trailhead_factory::Error::Rejected(_)), "{err:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "parked on the first rejection, not requeued");
    assert_eq!(task.run.attempt, Some(0), "no attempt was used");
    assert_eq!(task.run.infra_failures, Some(0));
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert!(questions[0].body.contains("400 Bad Request: bad size"), "{}", questions[0].body);
    assert!(r.compute.sandboxes().is_empty(), "no sandbox was made");
    assert!(r.factory.tick().await.unwrap().is_empty(), "a parked task isn't retried");
}

/// No sub-agent exists to park through, so the factory's own identity claims the task and asks.
#[tokio::test]
async fn a_rejected_sub_agent_create_parks_the_task_as_the_factory() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Refused sub-agent", "");
    r.tracker.reject_sub_agents("tracker 400 Bad Request: bad agent name");
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(matches!(err, trailhead_factory::Error::Rejected(_)), "{err:?}");
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::NeedsInput, "parked on the first rejection, not requeued");
    assert_eq!(task.run.attempt, Some(0), "no attempt was used");
    let questions: Vec<_> = r.tracker.comments(&key).into_iter().filter(|c| c.kind == "question").collect();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].author, "factory", "asked by the factory's own identity, not a sub-agent");
    assert!(questions[0].body.contains("bad agent name"), "{}", questions[0].body);
    assert!(r.compute.sandboxes().is_empty(), "no sandbox was made");
    assert!(r.factory.tick().await.unwrap().is_empty(), "a parked task isn't retried");
}

#[tokio::test]
async fn a_park_that_fails_still_keeps_the_backoff() {
    let r = rig_with(&CONFIG.replace("name = \"trailhead\"", "name = \"trailhead\"\nmax_infra_failures = 1"));
    let key = r.tracker.add_task("TRAIL", "Lost daemon", "");
    r.daemon.fail_runs(1, "not found: run trail-1 on this daemon");
    r.tracker.fail_next("factory/trail-1-a1", TrackerOp::Ask, 1);
    r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InProgress, "the park failed, so the claim is left to reconcile");
    assert_eq!(task.run.infra_failures, Some(1), "the count is kept so the next failure parks");
    r.tracker.set_status(&key, Status::Open); // what reconcile does with it
    assert!(r.factory.tick().await.unwrap().is_empty(), "nothing is started during the backoff");
}

#[tokio::test]
async fn a_daemon_that_errors_after_a_run_that_finished_at_once_still_records_the_spend() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Finished before the drop", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.daemon.fail_runs_midway(1, "connection lost");
    let err = r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(err.to_string().contains("connection lost"), "{err}");
    let task = r.tracker.task(&key);
    assert!(task.run.spend.is_some_and(|s| s > Usd::ZERO), "the spend is recorded: {:?}", task.run.spend);
    assert_eq!(task.run.infra_failures, Some(1));
}

#[tokio::test]
async fn a_fresh_sandbox_destroyed_after_a_failed_prepare_leaves_nothing_in_the_per_sandbox_maps() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Fails on a new sandbox", "");
    r.daemon.fail_prepares(1, "git fetch failed");
    r.factory.run_task(&r.tracker.task(&key)).await.unwrap_err();
    assert!(r.compute.get("trailhead-w1").is_none(), "a new sandbox has no warm label, so it is destroyed");
    assert_eq!(r.factory.sandbox_entries(), 0);
}
