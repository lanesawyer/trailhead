//! Every status move a person can make from Lookout, against the real engine behind the shell's in-process server
//! (TRAIL-1618). For a task in each status, held by nobody, an agent or the factory, the inspector's view says what it
//! offers; each offered move is sent as Lookout sends it and must be taken, and each move it leaves out is sent anyway
//! and must be refused, unless it is left out on purpose. The table this pins is `docs/factory/ui.md`'s "Status moves
//! a person can make".

use std::collections::HashMap;

use serde_json::json;
use trailhead_client::http::{Call, Client, NewComment, NewProject, NewTask, TaskUpdate};
use trailhead_lookout::board::Input;
use trailhead_lookout::inspector::{Action, Fetched, InspectorView, Subject, build, plan};
use trailhead_model::{CommentKind, Status, TaskKey};
use trailhead_shell::commands::{Outcome, Request, execute};
use trailhead_shell::testing::server::Server;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Run {
    /// No factory metadata.
    None,
    /// The factory ran it and holds a sandbox for it: running, or parked.
    Sandbox,
    /// The factory ran it and opened a PR that is still open.
    OpenPr,
}

struct Case {
    name: &'static str,
    status: Status,
    run: Run,
}

const CASES: [Case; 12] = [
    Case { name: "open", status: Status::Open, run: Run::None },
    Case { name: "open, the factory's (requeued or set aside)", status: Status::Open, run: Run::Sandbox },
    Case { name: "in progress, an agent holds it", status: Status::InProgress, run: Run::None },
    Case { name: "in progress, the factory's run holds it", status: Status::InProgress, run: Run::Sandbox },
    Case { name: "needs input, an agent holds it", status: Status::NeedsInput, run: Run::None },
    Case { name: "needs input, the factory parked it", status: Status::NeedsInput, run: Run::Sandbox },
    Case { name: "in review, no factory run", status: Status::InReview, run: Run::None },
    Case { name: "in review, the factory's PR open", status: Status::InReview, run: Run::OpenPr },
    Case { name: "approved", status: Status::Approved, run: Run::None },
    Case { name: "approved, the factory's PR open", status: Status::Approved, run: Run::OpenPr },
    Case { name: "done", status: Status::Done, run: Run::None },
    Case { name: "cancelled", status: Status::Cancelled, run: Run::None },
];

struct Rig {
    _server: Server,
    human: Client,
    agent: Client,
}

impl Rig {
    async fn new() -> Rig {
        let server = Server::new().await;
        let base = format!("http://{}", server.server);
        let human = Client::new(&base, &server.token);
        let agent = Client::new(&base, &server.agent_token);
        let review = NewProject {
            key: "REV".into(),
            name: "Reviewed".into(),
            brief: String::new(),
            require_review: true,
            claim_ttl: None,
        };
        human.create_project(&review).await.unwrap();
        let reviewed = human.show_project("REV").await.unwrap();
        assert!(reviewed.require_review);
        Rig { _server: server, human, agent }
    }

    /// A new task in `project` brought to `case`'s status by the writes that bring a real one there.
    async fn task(&self, project: &str, case: &Case) -> TaskKey {
        let new = NewTask { project: project.into(), title: case.name.into(), ..Default::default() };
        let key = self.human.create_task(&new).await.unwrap().task.key;
        let factory = match case.run {
            Run::None => None,
            Run::Sandbox => Some(json!({"attempt": 1, "sandbox": "trail-1-a1"})),
            Run::OpenPr => Some(json!({"attempt": 1, "sandbox": "trail-1-a1",
                "pr": {"repo": {"owner": "o", "name": "r"}, "number": 3, "url": "https://example.com/o/r/pull/3"}})),
        };
        if let Some(factory) = factory {
            let update = TaskUpdate { metadata: Some(json!({ "factory": factory })), ..Default::default() };
            self.human.update_task(&key, &update).await.unwrap();
        }
        let to = |status| TaskUpdate { status: Some(status), ..Default::default() };
        match case.status {
            Status::Open => {}
            Status::InProgress => {
                self.agent.claim(&key, None).await.unwrap();
            }
            Status::NeedsInput => {
                self.agent.claim(&key, None).await.unwrap();
                self.agent.add_comment(&key, &NewComment::new(CommentKind::Question, "which one?")).await.unwrap();
            }
            Status::InReview | Status::Approved => {
                self.agent.claim(&key, None).await.unwrap();
                self.agent.update_task(&key, &to(Status::InReview)).await.unwrap();
                if case.status == Status::Approved {
                    self.human.update_task(&key, &to(Status::Approved)).await.unwrap();
                }
            }
            Status::Done | Status::Cancelled => {
                self.human.update_task(&key, &to(case.status)).await.unwrap();
            }
        }
        let detail = self.human.show_task(&key).await.unwrap();
        assert_eq!(detail.task.status, case.status, "{}", case.name);
        let held = matches!(case.status, Status::InProgress | Status::NeedsInput);
        assert_eq!(detail.claim.is_some(), held, "{}", case.name);
        key
    }

    /// The inspector's view of `key` as Lookout builds it from what it mirrors, and the request `action` sends.
    async fn view(&self, key: &TaskKey, action: Option<&Action>) -> (InspectorView, Option<Request>) {
        let detail = self.human.show_task(key).await.unwrap();
        let projects = self.human.list_projects(false).await.unwrap();
        let agents = self.human.list_agents().await.unwrap();
        let tasks = [detail.task.clone()];
        let claims: Vec<_> = detail.claim.iter().cloned().collect();
        let input = || Input {
            projects: &projects,
            tasks: &tasks,
            claims: &claims,
            dependencies: &[],
            comments: &detail.comments,
            agents: &agents,
            users: &[],
            queues: &[],
            deep: &[],
            now: jiff::Timestamp::now(),
        };
        let subject = Subject::Task(detail.task.id.to_string());
        let latest = HashMap::new();
        let view = build(input(), &latest, &subject, Fetched::default()).unwrap();
        let request = action.and_then(|a| plan(a, input(), &latest, &subject, jiff::Timestamp::now().as_second()));
        (view, request)
    }

    async fn send(&self, request: &Request) -> Outcome {
        execute(&self.human, request).await.unwrap()
    }
}

fn offers(view: &InspectorView, to: Status) -> Option<Action> {
    let action = if to == Status::Cancelled { Action::Cancel } else { Action::Status(to) };
    view.actions.contains(&action).then_some(action)
}

/// One case's row: what Lookout offers, what the engine refuses that Lookout leaves out, and what the engine would
/// take that Lookout leaves out on purpose.
async fn row(rig: &Rig, project: &str, case: &Case) -> String {
    let (mut offered, mut refused, mut hidden) = (Vec::new(), Vec::new(), Vec::new());
    for to in Status::ALL.into_iter().filter(|s| *s != case.status) {
        let key = rig.task(project, case).await;
        let (view, _) = rig.view(&key, None).await;
        let label = to.as_str();
        match offers(&view, to) {
            Some(action) => {
                let (_, request) = rig.view(&key, Some(&action)).await;
                let outcome = rig.send(&request.unwrap()).await;
                assert!(matches!(outcome, Outcome::Ok { .. }), "{}: {label} was offered: {outcome:?}", case.name);
                assert_eq!(rig.human.show_task(&key).await.unwrap().task.status, to, "{}", case.name);
                offered.push(label);
            }
            None => {
                let update = TaskUpdate { status: Some(to), ..Default::default() };
                let request =
                    Request::new(Call::UpdateTask { key: key.clone(), update }, Some(view.task.unwrap().version));
                match rig.send(&request).await {
                    Outcome::Failed(why) => {
                        assert_eq!(why, format!("cannot move from {} to {label}", case.status), "{}", case.name);
                        refused.push(label);
                    }
                    Outcome::Ok { .. } => hidden.push(label),
                    other => panic!("{}: {label}: {other:?}", case.name),
                }
            }
        }
    }
    format!("{} | {} | {} | {}", case.name, offered.join(", "), refused.join(", "), hidden.join(", "))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_move_lookout_offers_a_person_the_engine_takes_and_every_one_it_leaves_out_is_refused() {
    let rig = Rig::new().await;
    let mut rows = Vec::new();
    for case in &CASES {
        let plain = row(&rig, "TRAIL", case).await;
        // A project that requires review asks nothing more of a person.
        assert_eq!(row(&rig, "REV", case).await, plain, "require_review changed a person's moves");
        rows.push(plain);
    }
    let want = [
        "open | in_progress, needs_input, in_review, done, cancelled | approved | ",
        "open, the factory's (requeued or set aside) | in_progress, needs_input, in_review, done, cancelled | approved | ",
        "in progress, an agent holds it | open, needs_input, in_review, done, cancelled | approved | ",
        "in progress, the factory's run holds it | open, needs_input, in_review, cancelled | approved | done",
        "needs input, an agent holds it | open, in_progress, in_review, cancelled | approved, done | ",
        "needs input, the factory parked it | open, in_progress, in_review, cancelled | approved, done | ",
        "in review, no factory run | open, in_progress, needs_input, approved, done, cancelled |  | ",
        "in review, the factory's PR open | open, in_progress, needs_input, approved, done, cancelled |  | ",
        "approved | open, in_review, done, cancelled | in_progress, needs_input | ",
        "approved, the factory's PR open | open, in_review, done, cancelled | in_progress, needs_input | ",
        "done | open | in_progress, needs_input, in_review, approved, cancelled | ",
        "cancelled | open | in_progress, needs_input, in_review, approved, done | ",
    ];
    assert_eq!(rows, want, "\n{}", rows.join("\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parked_factory_task_is_finished_by_setting_it_aside_then_marking_it_done() {
    let rig = Rig::new().await;
    let parked = &CASES[5];
    assert_eq!(parked.status, Status::NeedsInput);
    let key = rig.task("TRAIL", parked).await;
    let (view, request) = rig.view(&key, Some(&Action::SetAside)).await;
    assert!(view.actions.contains(&Action::SetAside));
    assert!(view.done_hint.unwrap().contains("set it aside"));
    assert!(matches!(rig.send(&request.unwrap()).await, Outcome::Ok { .. }));
    let detail = rig.human.show_task(&key).await.unwrap();
    assert_eq!((detail.task.status, detail.claim.is_none()), (Status::Open, true));

    let done = Action::Status(Status::Done);
    let (view, request) = rig.view(&key, Some(&done)).await;
    assert!(view.actions.contains(&done), "{:?}", view.actions);
    assert_eq!(view.done_hint, None);
    assert!(matches!(rig.send(&request.unwrap()).await, Outcome::Ok { .. }));
    assert_eq!(rig.human.show_task(&key).await.unwrap().task.status, Status::Done);
}

#[tokio::test(flavor = "multi_thread")]
async fn answering_a_question_sends_the_task_back_to_whoever_holds_it() {
    let rig = Rig::new().await;
    let key = rig.task("TRAIL", &CASES[4]).await;
    let question = rig.human.show_task(&key).await.unwrap().comments[0].id;
    let answer = Call::ResolveComment { id: question.to_string(), answer: Some("this one".into()) };
    assert!(matches!(rig.send(&Request::new(answer, None)).await, Outcome::Ok { .. }));
    let detail = rig.human.show_task(&key).await.unwrap();
    assert_eq!((detail.task.status, detail.claim.is_some()), (Status::InProgress, true), "the agent still holds it");

    let key = rig.task("TRAIL", &CASES[5]).await;
    rig.human.release(&key).await.unwrap();
    let detail = rig.human.show_task(&key).await.unwrap();
    assert_eq!(detail.task.status, Status::NeedsInput, "releasing keeps it waiting");
    let answer = Call::ResolveComment { id: detail.comments[0].id.to_string(), answer: Some("this one".into()) };
    assert!(matches!(rig.send(&Request::new(answer, None)).await, Outcome::Ok { .. }));
    assert_eq!(rig.human.show_task(&key).await.unwrap().task.status, Status::Open, "nobody holds it");
}
