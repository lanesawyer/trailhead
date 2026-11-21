//! Projects whose metadata points at the repo: their graded tasks are the factory's alongside the file's own project.

use std::sync::Arc;

use trailhead_control::{Factory, Host};
use trailhead_factory::{FactoryConfig, HostConfig, Role, Status};
use trailhead_testkit::{FakeProvider, Script};

use crate::common::*;

const SLUG: &str = "lanesawyer/trailhead";

#[tokio::test]
async fn a_pointed_projects_graded_task_is_worked_and_an_ungraded_one_left_alone() {
    let (r, _queue) = queued_rig();
    r.tracker.point("TICK", Some((SLUG, Some("apps/ticklist"))));
    let graded = r.tracker.add_task("TICK", "Add a due date", "");
    r.tracker.set_grade(&graded, 2);
    let ungraded = r.tracker.add_task("TICK", "Buy milk", "");
    r.harness.push(Role::Worker, Script::new(changes_to("apps/ticklist/src/lib.rs")));
    r.harness.push(Role::Reviewer, Script::new(approve()));

    let ran = r.factory.tick().await.unwrap();
    assert_eq!(ran.iter().map(|(t, _)| &t.key).collect::<Vec<_>>(), [&graded]);
    assert_eq!(r.tracker.task(&graded).status, Status::InReview);
    assert_eq!(r.tracker.task(&graded).grade, Some(gr(2)), "a change inside the path keeps its grade");
    assert_eq!(r.tracker.task(&ungraded).status, Status::Open);
    assert!(notes(&r, &ungraded).is_empty(), "a person's own task gets no ungraded note");
    assert_ne!(r.tracker.task(&ungraded).run.ungraded, Some(true));

    let told: Vec<_> = r.harness.contexts().into_iter().map(|c| c.task.path).collect();
    assert_eq!(told, [Some("apps/ticklist".to_string()), Some("apps/ticklist".to_string())]);
}

#[tokio::test]
async fn a_project_pointed_while_the_factory_runs_is_worked_on_the_next_poll() {
    let (r, _queue) = queued_rig();
    let key = r.tracker.add_task("TICK", "Add a due date", "");
    r.tracker.set_grade(&key, 2);
    assert!(r.factory.tick().await.unwrap().is_empty(), "not pointed yet");

    r.tracker.point("TICK", Some(("someone/else", None)));
    assert!(r.factory.tick().await.unwrap().is_empty(), "pointed at another repo");

    r.tracker.point("TICK", Some(("LaneSawyer/Trailhead", None)));
    r.harness.push(Role::Worker, Script::new(changes_to("anywhere.rs")));
    r.harness.push(Role::Reviewer, Script::new(approve()));
    assert_eq!(r.factory.tick().await.unwrap().len(), 1);
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
    assert_eq!(r.harness.contexts()[0].task.path, None, "without a path the whole repo is the project's");
}

#[tokio::test]
async fn a_pointed_project_that_is_another_repos_own_is_skipped() {
    let host = Host::new(&HostConfig::default());
    let r = rig_on(CONFIG, host.clone());
    // A second repo on the host whose file names TICK as its project.
    let other = CONFIG.replace("name = \"trailhead\"", "name = \"apps\"");
    let _apps = Factory::new(
        FactoryConfig::parse_table(&other, "TICK", None).unwrap(),
        Arc::new(r.compute.clone()),
        Arc::new(r.tracker.clone()),
        Arc::new(FakeProvider::new(1)),
        r.daemon.clone(),
        host,
    );
    r.tracker.point("TICK", Some((SLUG, None)));
    let key = r.tracker.add_task("TICK", "Add a due date", "");
    r.tracker.set_grade(&key, 2);
    r.harness.push(Role::Worker, Script::new(changes()));
    assert!(r.factory.tick().await.unwrap().is_empty());
    assert!(r.factory.tick().await.unwrap().is_empty(), "skipped on every poll, without stopping the loop");
    assert_eq!(r.tracker.task(&key).status, Status::Open);
}

#[tokio::test]
async fn a_pointed_projects_task_is_reconciled_and_its_sandbox_let_go_once_it_stops_pointing() {
    let (r, _queue) = queued_rig();
    r.tracker.point("TICK", Some((SLUG, None)));
    let key = r.tracker.add_task("TICK", "Ask first", "");
    r.tracker.set_grade(&key, 2);
    let question = trailhead_factory::RunOutcome::NeedsInput { question: "Which list?".into() };
    r.harness.push(Role::Worker, Script::new(question));
    r.factory.tick().await.unwrap();
    assert_eq!(r.tracker.task(&key).status, Status::NeedsInput);
    assert_eq!(busy(&r.compute).len(), 1, "the parked task keeps its sandbox");
    r.factory.reconcile().await.unwrap();
    assert_eq!(busy(&r.compute).len(), 1, "reconcile sees the pointed project's parked task");

    r.tracker.point("TICK", None);
    r.factory.tick().await.unwrap();
    assert!(busy(&r.compute).is_empty(), "no longer the factory's, so its sandbox is released");
}
