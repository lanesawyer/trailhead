use std::sync::Arc;
use std::time::Duration;

use trailhead_control::Outcome;
use trailhead_factory::{
    CodeHost, Commit, FileChange, MergeOutcome, Role, RunOutcome, Status, Tracker, Usd, VerdictKind, label,
};
use trailhead_testkit::{FAKE_BASE, Script};

use crate::common::*;

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
        if destroy_fails {
            assert_eq!(
                r.compute.egress("trailhead-w1"),
                Some(trailhead_factory::Egress::none()),
                "egress is cut before the labels"
            );
        } else {
            assert!(r.compute.get("trailhead-w1").is_none(), "a sandbox that can't be cut off is destroyed");
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

#[tokio::test(start_paused = true)]
async fn infrastructure_retries_wait_longer_each_time_up_to_a_cap() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Down for a while", "");
    r.daemon.fail_runs(7, "connection lost");
    let secs = std::time::Duration::from_secs;
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

#[tokio::test]
async fn failures_that_are_the_agents_or_the_tasks_still_use_up_attempts() {
    let r = rig();
    let key = r.tracker.add_task("TRAIL", "Gives up", "");
    // Not API trouble, though it mentions one: a 400 about max_tokens: 5000.
    for reason in ["I couldn't make the build pass", "API Error: 400 invalid request, max_tokens: 5000"] {
        r.harness.push(Role::Worker, Script::new(RunOutcome::Failed { reason: reason.into() }));
    }
    for _ in 0..2 {
        let ran = r.factory.run_task(&r.tracker.task(&key)).await.unwrap();
        assert!(matches!(ran, Outcome::Failed { .. }), "{ran:?}");
    }
    let task = r.tracker.task(&key);
    assert_eq!(task.run.attempt, Some(2));
    assert_eq!(task.run.infra_failures.unwrap_or(0), 0);

    r.daemon.fail_check("lint", "warning: unused variable");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
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
    for op in ["note", "release"] {
        let r = rig();
        let key = r.tracker.add_task("TRAIL", "Lint me", "");
        r.daemon.fail_check("lint", "warning: unused variable");
        r.harness.push(Role::Worker, Script::new(changes()).steps(1, 10));
        // Only the worker's own call fails; the control plane's go through.
        r.tracker.fail_next("factory/trail-1-a1", op, 1);

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
            changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
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
async fn a_warm_sandbox_that_fails_prepare_is_destroyed_and_the_next_task_gets_a_fresh_one() {
    let r = rig();
    let first = r.tracker.add_task("TRAIL", "Warms the pool", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));
    r.factory.run_task(&r.tracker.task(&first)).await.unwrap();
    let idle = r.compute.get("trailhead-w1").expect("the first task leaves a warm sandbox");
    assert_eq!(idle.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));
    assert!(idle.labels.contains_key(label::WARM));

    let second = r.tracker.add_task("TRAIL", "Hits the broken checkout", "");
    r.daemon.fail_prepares(1, "git clone failed");
    let err = r.factory.run_task(&r.tracker.task(&second)).await.unwrap_err();
    assert!(err.to_string().contains("git clone failed"), "{err}");
    assert!(r.compute.get("trailhead-w1").is_none(), "the sandbox is destroyed, not relabelled idle");
    assert_eq!(r.tracker.task(&second).run.infra_failures, Some(1), "an infrastructure failure");
    assert_eq!(r.tracker.task(&second).run.attempt.unwrap_or(0), 0, "no attempt was used");

    let third = r.tracker.add_task("TRAIL", "Gets a fresh sandbox", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100_000));
    r.harness.push(Role::Reviewer, Script::new(approve()).steps(1, 50_000));
    r.factory.run_task(&r.tracker.task(&third)).await.unwrap();
    let fresh = r.compute.get("trailhead-w1").expect("a new sandbox took the freed name");
    assert_eq!(r.compute.checkpoints("trailhead-w1").len(), 1, "built from scratch, not restored: {fresh:?}");
}
