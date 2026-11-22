//! The planner: a spec task talked through with a person by turns, its plan written into the engine by the control
//! plane (`docs/app-platform.md`, "Planning an ask").

use std::time::Duration;

use trailhead_control::Outcome;
use trailhead_factory::messages;
use trailhead_factory::{PlanTurn, ProposedTask, Role, RunOutcome, Spec, Status, TaskKey, TaskPlan, label};
use trailhead_testkit::Script;

use crate::common::*;

const PLANNER: &str = r#"
[profiles.planner]
role = "planner"
adapter = "claude-code"
provider = "anthropic"
model = "opus"
budget = { usd = 5, minutes = 30, steps = 20 }
"#;

/// A rig that works the whole TRAIL project, with a planner.
fn planned() -> Rig {
    rig_with(&format!("{CONFIG}\n{PLANNER}"))
}

/// A rig that works only TRAIL-1's subtasks, with a planner, and a spec task under it: a plan's tasks are the spec
/// task's subtasks, so nothing of the plan is worked even once graded.
fn queued_spec() -> (Rig, TaskKey) {
    let (r, queue) = queued_rig_with(PLANNER);
    let spec = r.tracker.add_subtask(&queue, "A todo app", "Lists I can tick off.");
    r.tracker.set_plan(&spec, TaskPlan { spec: Some(Spec::default()), proposed: None });
    (r, spec)
}

fn question(message: &str) -> RunOutcome {
    RunOutcome::PlanTurn(PlanTurn { message: message.into(), spec: None, tasks: None, ready: false })
}

fn task(id: Option<&str>, key: Option<&TaskKey>, title: &str, blocked_by: &[&str], grade: u8) -> ProposedTask {
    ProposedTask {
        id: id.map(str::to_string),
        key: key.cloned(),
        title: title.into(),
        description: format!("{title}. Done when it works."),
        blocked_by: blocked_by.iter().map(|b| b.to_string()).collect(),
        grade: gr(grade),
        why: format!("why {title}"),
    }
}

fn plan_turn(message: &str, spec: &str, tasks: Vec<ProposedTask>, ready: bool) -> RunOutcome {
    RunOutcome::PlanTurn(PlanTurn { message: message.into(), spec: Some(spec.into()), tasks: Some(tasks), ready })
}

fn questions(r: &Rig, key: &TaskKey) -> Vec<String> {
    r.tracker.comments(key).into_iter().filter(|c| c.kind == "question").map(|c| c.body).collect()
}

#[tokio::test]
async fn a_first_turn_that_asks_parks_the_spec_task_with_no_plan() {
    let r = planned();
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists I can tick off.");
    r.harness.push(Role::Planner, Script::new(question("Which colours, and does it sync?")).steps(2, 1000));

    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.len(), 1);
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.starts_with("Which colours")), "{ran:?}");
    let t = r.tracker.task(&spec);
    assert_eq!(t.status, Status::NeedsInput);
    assert_eq!(t.grade, None, "a spec task is planned ungraded");
    assert_eq!(questions(&r, &spec), ["Which colours, and does it sync?"]);
    assert!(r.tracker.children(&spec).is_empty());
    assert_eq!(t.description, "Lists I can tick off.", "no spec yet");
    assert!(notes(&r, &spec).is_empty(), "a spec task gets no ungraded note");
    assert!(r.codehost.prs().is_empty());

    let ctx = r.harness.contexts().pop().unwrap();
    assert_eq!(ctx.role, Role::Planner);
    assert_eq!(ctx.model.0, "fake-opus");
    let plan = ctx.plan.expect("the planner is told the plan");
    assert_eq!(plan.planning.ask, "Lists I can tick off.");
    assert!(plan.planning.plan.is_empty());
    assert!(plan.files.turn.ends_with(format!("{}/turn.json", spec.0)), "{:?}", plan.files.turn);
    let host = t.run.host.expect("the turn ran in a sandbox");
    assert!(busy(&r.compute).contains(&host), "the parked planner keeps its sandbox, with its session");
}

#[tokio::test]
async fn a_turn_with_a_plan_files_its_tasks_ungraded_and_parks_with_its_message() {
    let r = planned();
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists I can tick off.");
    let tasks = vec![
        task(Some("store"), None, "Store the lists", &[], 3),
        task(Some("view"), None, "Draw the lists", &["store"], 2),
    ];
    r.harness.push(Role::Planner, Script::new(plan_turn("Here's the plan.", "# Todo\nLists.", tasks, true)));

    r.factory.tick().await.unwrap();
    let t = r.tracker.task(&spec);
    assert_eq!(t.status, Status::NeedsInput);
    assert_eq!(questions(&r, &spec), ["Here's the plan."]);
    assert_eq!(t.description, "# Todo\nLists.", "the spec replaces the description");
    let kept = t.plan.spec.clone().unwrap();
    assert_eq!(kept.ask.as_deref(), Some("Lists I can tick off."), "the ask is kept");
    assert!(kept.ready);

    let children = r.tracker.children(&spec);
    let titles: Vec<_> = children.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Store the lists", "Draw the lists"]);
    for c in &children {
        assert_eq!((c.grade, c.status), (None, Status::Open), "{}", c.key);
    }
    assert_eq!(
        children[0].plan.proposed.as_ref().map(|p| (p.grade, p.why.as_str())),
        Some((gr(3), "why Store the lists"))
    );
    assert_eq!(r.tracker.blockers(&children[1].key), [children[0].key.clone()]);

    assert!(r.factory.tick().await.unwrap().is_empty(), "the plan's tasks wait for a person to grade them");
    for c in &children {
        assert!(notes(&r, &c.key).is_empty(), "and get no ungraded note");
    }
}

#[tokio::test]
async fn a_revised_plan_updates_adds_and_cancels_and_leaves_a_persons_tasks_alone() {
    let (r, spec) = queued_spec();
    let first = vec![
        task(Some("a"), None, "A", &[], 2),
        task(Some("b"), None, "B", &["a"], 2),
        task(Some("c"), None, "C", &[], 2),
    ];
    r.harness.push(Role::Planner, Script::new(plan_turn("Plan v1.", "Spec v1.", first, false)));
    r.factory.tick().await.unwrap();
    let keys: Vec<TaskKey> = r.tracker.children(&spec).into_iter().map(|c| c.key).collect();
    let (a, b, c) = (&keys[0], &keys[1], &keys[2]);
    let host = r.tracker.task(&spec).run.host.unwrap();
    let prepares = r.daemon.prepared().len();

    // A person grades C and makes A wait on it, then replies.
    r.tracker.set_grade(c, 3);
    r.tracker.add_blocker(a, c);
    r.tracker.answer(&spec, "Drop B; add a D after A.");
    let second = vec![task(None, Some(a), "A, renamed", &[], 2), task(Some("d"), None, "D", &[a.as_str()], 1)];
    r.harness.push(Role::Planner, Script::new(plan_turn("Plan v2.", "Spec v2.", second, true)));
    r.factory.tick().await.unwrap();

    let ctx = r.harness.contexts().pop().unwrap();
    let told = ctx.plan.unwrap().planning;
    assert_eq!(told.ask, "Lists I can tick off.", "the ask, not the spec that replaced it");
    assert!(told.plan.iter().any(|t| t.key == *c && t.grade == Some(gr(3))), "the turn sees the person's grade");
    let answers: Vec<_> = ctx.task.context.answers.iter().map(|x| (x.question.as_str(), x.answer.as_str())).collect();
    assert_eq!(answers, [("Plan v1.", "Drop B; add a D after A.")], "the conversation so far");

    let children = r.tracker.children(&spec);
    let by = |k: &TaskKey| children.iter().find(|t| t.key == *k).unwrap().clone();
    assert_eq!(by(a).title, "A, renamed");
    assert_eq!(r.tracker.blockers(a), std::slice::from_ref(c), "the hand-made dependency on a person's task stays");
    assert_eq!(by(b).status, Status::Cancelled, "a task the plan leaves out is cancelled");
    assert_eq!((by(c).status, by(c).grade), (Status::Open, Some(gr(3))), "a graded task is never touched");
    let d = children.iter().find(|t| t.title == "D").expect("D is added");
    assert_eq!((d.grade, r.tracker.blockers(&d.key)), (None, vec![a.clone()]));
    assert_eq!(r.tracker.task(&spec).description, "Spec v2.");
    assert_eq!(r.tracker.task(&spec).status, Status::NeedsInput);

    assert_eq!(r.tracker.task(&spec).run.host.as_deref(), Some(host.as_str()), "the same sandbox, with the session");
    assert_eq!(r.daemon.prepared().len(), prepares, "neither prepared again nor reset");
    assert!(r.compute.restores(&host).is_empty(), "nor restored to a checkpoint");
}

#[tokio::test]
async fn a_turn_without_a_turn_file_fails_like_a_worker_that_gave_up() {
    let r = planned();
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists.");
    let reason = messages::no_plan_turn("turn.json: No such file or directory");
    r.harness.push(Role::Planner, Script::new(RunOutcome::Failed { reason: reason.clone() }));

    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Failed { reason: why } if *why == reason), "{ran:?}");
    let t = r.tracker.task(&spec);
    assert_eq!((t.status, t.holder.as_deref()), (Status::Open, None), "back in the queue");
    assert_eq!(t.run.failure, Some(messages::agent_gave_up(&reason)));
    assert!(questions(&r, &spec).is_empty());
}

#[tokio::test]
async fn an_approved_spec_task_is_never_planned_and_lets_its_sandbox_go() {
    let (r, spec) = queued_spec();
    r.harness
        .push(Role::Planner, Script::new(plan_turn("Plan.", "Spec.", vec![task(Some("a"), None, "A", &[], 2)], true)));
    r.factory.tick().await.unwrap();
    let host = r.tracker.task(&spec).run.host.unwrap();
    assert!(busy(&r.compute).contains(&host));

    r.tracker.approve_plan(&spec);
    assert!(r.factory.tick().await.unwrap().is_empty());
    assert!(!busy(&r.compute).contains(&host), "the approved planner's sandbox goes back to the pool");
    r.tracker.answer(&spec, "Thanks.");
    assert!(r.factory.tick().await.unwrap().is_empty(), "approved, it is never planned again");
    r.factory.reconcile().await.unwrap();
    assert!(notes(&r, &spec).is_empty(), "and sits quietly");
    assert_eq!(r.harness.contexts().len(), 1);
}

#[tokio::test]
async fn a_spec_task_is_closed_and_the_person_told_once_every_task_of_its_plan_is_finished() {
    let (r, spec) = queued_spec();
    let tasks = vec![task(Some("a"), None, "A", &[], 2), task(Some("b"), None, "B", &[], 2)];
    r.harness.push(Role::Planner, Script::new(plan_turn("Plan.", "Spec.", tasks, true)));
    r.factory.tick().await.unwrap();
    let keys: Vec<TaskKey> = r.tracker.children(&spec).into_iter().map(|t| t.key).collect();
    r.factory.reconcile().await.unwrap();
    assert_eq!(r.tracker.task(&spec).status, Status::NeedsInput, "not approved, it is the planner's still");

    r.tracker.approve_plan(&spec);
    r.tracker.set_status(&keys[0], Status::Done);
    let done = r.factory.reconcile().await.unwrap();
    assert!(done.finished.is_empty());
    assert_eq!(r.tracker.task(&spec).status, Status::Open, "one task is still to do");

    r.tracker.set_status(&keys[1], Status::Cancelled);
    r.tracker.fail_next("factory", trailhead_testkit::TrackerOp::Finish, 1);
    assert!(r.factory.reconcile().await.unwrap().finished.is_empty(), "a close that fails is tried again");
    let done = r.factory.reconcile().await.unwrap();
    assert_eq!(done.finished, std::slice::from_ref(&spec.0));
    assert_eq!(r.tracker.task(&spec).status, Status::Done);
    let notices = r.tracker.open_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].kind, trailhead_factory::NOTICE_PLAN_DONE);
    assert!(notices[0].title.starts_with(&format!("{} is finished", spec.0)), "{}", notices[0].title);
    assert_eq!(notices[0].task.as_ref(), Some(&spec));
    assert!(r.factory.reconcile().await.unwrap().finished.is_empty());
    assert_eq!(r.tracker.open_notices().len(), 1, "told once");
}

#[tokio::test]
async fn a_plan_whose_tasks_were_all_cancelled_is_left_for_the_person() {
    let (r, spec) = queued_spec();
    r.harness
        .push(Role::Planner, Script::new(plan_turn("Plan.", "Spec.", vec![task(Some("a"), None, "A", &[], 2)], true)));
    r.factory.tick().await.unwrap();
    let a = r.tracker.children(&spec)[0].key.clone();
    r.tracker.approve_plan(&spec);
    r.tracker.set_status(&a, Status::Cancelled);
    assert!(r.factory.reconcile().await.unwrap().finished.is_empty());
    assert_eq!(r.tracker.task(&spec).status, Status::Open);
    assert!(r.tracker.open_notices().is_empty());
}

#[tokio::test]
async fn a_plans_tasks_are_left_alone_until_graded_then_worked() {
    let r = planned();
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists.");
    r.harness
        .push(Role::Planner, Script::new(plan_turn("Plan.", "Spec.", vec![task(Some("a"), None, "A", &[], 2)], true)));
    r.factory.tick().await.unwrap();
    let a = r.tracker.children(&spec)[0].key.clone();
    assert!(r.factory.tick().await.unwrap().is_empty(), "ungraded, in a project worked whole too");

    r.tracker.approve_plan(&spec);
    r.harness.push(Role::Worker, Script::new(changes()));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.iter().map(|(t, _)| &t.key).collect::<Vec<_>>(), [&a], "graded, it is ordinary work");
    assert_eq!(r.tracker.task(&a).status, Status::InReview);
}

#[tokio::test]
async fn the_model_the_person_chose_overrides_the_profiles() {
    let r = planned();
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists.");
    let chosen = Spec { model: Some("fable".into()), ..Spec::default() };
    r.tracker.set_plan(&spec, TaskPlan { spec: Some(chosen), proposed: None });
    r.harness.push(Role::Planner, Script::new(question("Colours?")));
    r.factory.tick().await.unwrap();
    assert_eq!(r.harness.contexts()[0].model.0, "fake-fable");
}

#[tokio::test]
async fn without_a_planner_profile_the_spec_task_is_parked_with_a_question() {
    let r = rig();
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists.");
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { .. }), "{ran:?}");
    assert_eq!(r.tracker.task(&spec).status, Status::NeedsInput);
    assert_eq!(questions(&r, &spec), [messages::no_planner_profile_question()]);
    assert!(r.harness.contexts().is_empty(), "never planned on the worker's profile");
    assert!(r.compute.sandboxes().is_empty());
}

/// TRAIL-1689: a planner profile written to the file after the server read it. The park says to restart, not to
/// add what is already there, and the status board says a restart is owed.
#[tokio::test]
async fn a_spec_task_parked_while_the_files_profiles_wait_for_a_restart_says_to_restart() {
    let mut r = rig();
    r.factory = r.factory.with_live_settings(Box::new(|| {
        Ok(trailhead_control::LiveSettings {
            merge_mode: trailhead_factory::MergeMode::Off,
            restart_for: vec!["profiles".into()],
        })
    }));
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists.");
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&spec).status, Status::NeedsInput);
    assert_eq!(questions(&r, &spec), [messages::planner_profile_needs_restart_question()]);
    assert_eq!(r.factory.status().snapshot().restart_for, ["profiles"]);
}

#[tokio::test]
async fn a_planner_that_breaches_its_budget_is_parked_like_a_worker() {
    let r = planned();
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists.");
    r.harness.push(Role::Planner, Script::new(question("Colours?")).steps(25, 10));
    let ran = r.factory.tick().await.unwrap();
    assert!(matches!(&ran[0].1, Outcome::Parked { reason } if reason.contains("20 steps")), "{ran:?}");
    let t = r.tracker.task(&spec);
    assert_eq!(t.status, Status::NeedsInput);
    assert!(t.run.worker_breach.is_some(), "recorded as the run's breach");
    assert!(questions(&r, &spec)[0].starts_with("Budget breached"), "{:?}", questions(&r, &spec));
}

#[tokio::test]
async fn a_planner_whose_daemon_is_lost_goes_back_without_using_its_attempt() {
    let r = planned();
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists.");
    r.daemon.fail_runs(1, "the daemon fell over");
    assert!(r.factory.tick().await.is_err(), "the run failed on the infrastructure");
    let t = r.tracker.task(&spec);
    assert_eq!((t.status, t.run.attempt, t.run.infra_failures), (Status::Open, Some(0), Some(1)));
}

#[tokio::test(start_paused = true)]
async fn a_planner_left_waiting_on_a_person_gives_up_its_sandbox_after_a_while() {
    let r = rig_with(
        &format!("{CONFIG}\n{PLANNER}")
            .replace("name = \"trailhead\"", "name = \"trailhead\"\nrelease_planner_after = 60"),
    );
    let spec = r.tracker.add_spec("TRAIL", "A todo app", "Lists.");
    r.harness.push(Role::Planner, Script::new(question("Colours?")));
    r.factory.tick().await.unwrap();
    let host = r.tracker.task(&spec).run.host.unwrap();
    r.factory.tick().await.unwrap();
    assert!(busy(&r.compute).contains(&host), "kept while the person may still reply");

    tokio::time::advance(Duration::from_secs(61 * 60)).await;
    r.factory.tick().await.unwrap();
    assert!(!busy(&r.compute).contains(&host), "given up after an hour");
    assert!(r.tracker.task(&spec).run.released.is_some(), "recorded, so the next turn starts cold");
    let idle = r.compute.sandboxes().into_iter().find(|s| s.name == host).unwrap();
    assert_eq!(idle.labels.get(label::STATE).map(String::as_str), Some(label::IDLE));

    r.tracker.answer(&spec, "Blue.");
    r.harness.push(Role::Planner, Script::new(question("Sync?")));
    r.factory.tick().await.unwrap();
    let t = r.tracker.task(&spec);
    assert_eq!((t.status, t.run.released), (Status::NeedsInput, None));
}
