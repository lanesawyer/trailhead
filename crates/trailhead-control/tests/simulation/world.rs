//! The simulated world: the real task engine, the fake sandboxes, daemon and code host, and agents that do whatever
//! the current step's seed says.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use trailhead_control::{CoreTracker, Factory, Host};
use trailhead_core::{CommentKind, Ctx, Grade, NewComment, NewProject, NewTask, ProjectPatch, Store, TaskPatch};
use trailhead_factory::{
    CiState, CodeHost, Commit, EventSink, FactoryConfig, Feedback, FileChange, Harness, HostConfig, PrRef, Result,
    Role, RunContext, RunEvent, RunOutcome, Status, TaskKey, Tracker, Usage, VerdictKind,
};
use trailhead_testkit::{Connect, FakeCodeHost, FakeCompute, FakeProvider, LocalDaemon};

use crate::rng::Rng;

/// Grades 1 and 2 merge on the agents' say; grade 3 waits for a person. The merger is on, so a task that passed its
/// gates has no reason to stay open. There is room for every task at once: who gets a sandbox when they run short
/// is the pool's own tests' business.
const CONFIG: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"
max_sandboxes = 4

[checks]
test = "cargo test"

[profiles.worker]
role = "worker"
adapter = "claude-code"
provider = "anthropic"
model = "sonnet"
budget = { usd = 5, minutes = 60, steps = 40 }

[profiles.reviewer]
role = "reviewer"
adapter = "claude-code"
provider = "anthropic"
model = "opus"
budget = { usd = 2, minutes = 20, steps = 20 }

[gates.1]
require = ["checks", "ci", "agent_review"]
auto_merge = true

[gates.2]
require = ["checks", "ci", "agent_review"]
auto_merge = true

[gates.3]
require = ["checks", "ci", "agent_review", "human"]
auto_merge = false

[merge]
mode = "on"
"#;

/// One thing that happens. A task is named by its position among the world's tasks; an event that doesn't apply to
/// the state it meets (a merge with no PR, an approval of a task that isn't in review) does nothing, so any list of
/// events is a valid history and removing one never invalidates the rest.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The factory starts what is ready; the agents it runs behave as the seed says.
    Tick(u64),
    /// The factory reconciles (and runs the reviews that finds due).
    Reconcile(u64),
    /// The control plane restarts: everything it kept in memory is gone.
    Restart,
    /// Time passes.
    Wait(u64),
    Ci(usize, Ci),
    Mergeable(usize, Option<bool>),
    Behind(usize),
    Push(usize),
    PersonMerges(usize),
    PersonCloses(usize),
    PersonApproves(usize),
    PersonRequestsChanges(usize),
    /// A person answers a parked task's questions and reopens it.
    PersonAnswers(usize),
    /// A person sends a task in review back to the queue.
    PersonReopens(usize),
    PersonFinishes(usize),
    PersonCancels(usize),
    Fault(Fault),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ci {
    Pending,
    Passed,
    Failed,
}

/// Infrastructure trouble, armed for whatever the factory does next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fault {
    RunFails,
    RunFailsMidway,
    RunUnavailable,
    RunCancelled,
    PrepareFails,
    PublishFails,
    PublishFailsAfterApplying,
    OpenPrFails,
    CommentFails,
    MergeRefused,
    ComputeDown,
    ComputeUp,
}

/// What the agents do in the step under way: `None` is the calm world of the settling phase, where a worker makes
/// its changes and a reviewer approves.
#[derive(Default)]
struct Mood {
    seed: Option<u64>,
    runs: u64,
}

struct Agents {
    mood: Arc<Mutex<Mood>>,
}

impl Agents {
    fn outcome(&self, role: Role) -> (RunOutcome, u32) {
        let mut mood = self.mood.lock().unwrap();
        mood.runs += 1;
        let n = mood.runs;
        let roll = mood.seed.map(|seed| Rng::new(seed ^ n.wrapping_mul(0x9E37_79B9)).below(100));
        let changes = RunOutcome::Changes {
            commits: vec![Commit {
                message: format!("feat: change {n}"),
                changes: vec![FileChange::new("src/greet.rs", Some(format!("pub fn hi{n}() {{}}").into_bytes()))],
                merges: vec![],
            }],
            summary: format!("Change {n}."),
        };
        let verdict = |kind, body: &str| RunOutcome::Verdict {
            kind,
            body: format!("{body} ({n})"),
            follow_ups: vec![],
            raise_grade: None,
        };
        match (role, roll) {
            (Role::Worker, None | Some(0..60)) => (changes, 2),
            (Role::Worker, Some(60..72)) => (RunOutcome::NeedsInput { question: format!("Which way? ({n})") }, 2),
            (Role::Worker, Some(72..88)) => (RunOutcome::Failed { reason: format!("it broke ({n})") }, 2),
            // More steps than the budget allows.
            (Role::Worker, Some(_)) => (changes, 60),
            (_, None | Some(0..55)) => (verdict(VerdictKind::Approve, "Looks right."), 1),
            (_, Some(55..85)) => (verdict(VerdictKind::RequestChanges, "Add a test."), 1),
            (_, Some(85..93)) => (RunOutcome::Failed { reason: format!("the review broke ({n})") }, 1),
            (_, Some(_)) => (verdict(VerdictKind::Approve, "Looks right."), 30),
        }
    }
}

#[async_trait]
impl Harness for Agents {
    async fn run(&self, ctx: RunContext, events: EventSink) -> Result<RunOutcome> {
        let (outcome, steps) = self.outcome(ctx.role);
        for step in 1..=steps {
            let _ = events.send(RunEvent::StepStarted { step });
            let usage = Usage { output_tokens: 1_000, ..Usage::default() };
            let _ = events.send(RunEvent::Usage { model: ctx.model.clone(), usage });
            tokio::task::yield_now().await;
        }
        Ok(outcome)
    }

    async fn resume(&self, ctx: RunContext, _feedback: Feedback, events: EventSink) -> Result<RunOutcome> {
        self.run(ctx, events).await
    }
}

pub struct World {
    pub store: Arc<Store>,
    pub human: Ctx,
    pub factory: Factory,
    pub tracker: CoreTracker,
    pub compute: FakeCompute,
    pub codehost: FakeCodeHost,
    pub daemon: Arc<LocalDaemon>,
    pub tasks: Vec<TaskKey>,
    mood: Arc<Mutex<Mood>>,
    /// What each step did, for the report of a failed run.
    pub log: Vec<String>,
}

impl World {
    /// A project the factory may review in, with one task per grade given.
    pub async fn new(grades: &[u8]) -> World {
        let store = Arc::new(Store::open_local(":memory:").await.unwrap());
        let (ws, user) = store.bootstrap_local("Lane").await.unwrap();
        let human = Ctx::user(ws.id, user.id);
        let project = NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() };
        store.create_project(&human, project).await.unwrap();
        let agent = store.create_agent(&human, "factory").await.unwrap();
        let reviewers = ProjectPatch { reviewers: Some(vec!["factory".into()]), ..Default::default() };
        store.update_project(&human, "TRAIL", reviewers, None).await.unwrap();
        let ctx = store.agent_ctx(ws.id, agent.id).await.unwrap();
        let tracker = CoreTracker::new(store.clone(), ctx, "factory");

        let compute = FakeCompute::new();
        let codehost = FakeCodeHost::new();
        let mood = Arc::new(Mutex::new(Mood::default()));
        let s = store.clone();
        let connect: Connect = Arc::new(move |cred| {
            let s = s.clone();
            Box::pin(async move { Ok(Arc::new(CoreTracker::authenticate(s, &cred).await?) as Arc<dyn Tracker>) })
        });
        let heads = codehost.clone();
        let agents = Arc::new(Agents { mood: mood.clone() });
        let daemon = Arc::new(
            LocalDaemon::connecting(agents, Arc::new(codehost.clone()), connect)
                .with_heads(move |repo, branch| heads.head(repo, branch)),
        );
        let factory = Self::boot(&compute, &tracker, &daemon, &codehost);
        let mut tasks = Vec::new();
        for (i, grade) in grades.iter().enumerate() {
            let new = NewTask {
                project: "TRAIL".into(),
                title: format!("Task {i}"),
                grade: Some(Grade::new(*grade).unwrap()),
                ..Default::default()
            };
            tasks.push(TaskKey(store.create_task(&human, new).await.unwrap().key.0));
        }
        World { store, human, factory, tracker, compute, codehost, daemon, tasks, mood, log: Vec::new() }
    }

    fn boot(
        compute: &FakeCompute,
        tracker: &CoreTracker,
        daemon: &Arc<LocalDaemon>,
        codehost: &FakeCodeHost,
    ) -> Factory {
        Factory::new(
            FactoryConfig::parse_table(CONFIG, "TRAIL", None).unwrap(),
            Arc::new(compute.clone()),
            Arc::new(tracker.clone()),
            Arc::new(FakeProvider::new(1)),
            daemon.clone(),
            Host::new(&HostConfig::default()),
        )
        .with_codehost(Arc::new(codehost.clone()))
    }

    pub async fn show(&self, key: &TaskKey) -> trailhead_factory::Task {
        self.tracker.show(key).await.unwrap()
    }

    /// The task's PR, if it has one that is still open.
    async fn pr(&self, task: usize) -> Option<PrRef> {
        let pr = self.show(&self.tasks[task % self.tasks.len()]).await.run.pr?;
        let open = self.codehost.prs().iter().any(|p| p.pr == pr && !p.merged && !p.closed);
        open.then_some(pr)
    }

    async fn set_status(&self, key: &TaskKey, status: trailhead_core::Status) -> std::result::Result<(), String> {
        let patch = TaskPatch { status: Some(status), ..Default::default() };
        self.store.update_task(&self.human, &key.0, patch, None).await.map(|_| ()).map_err(|e| e.to_string())
    }

    /// The factory's own steps, with the agents in the mood `seed` gives. An error is part of the history, not a
    /// failure of the simulation: the next steps have to cope with whatever it left.
    async fn step(&mut self, seed: Option<u64>, reconcile: bool) -> String {
        self.mood.lock().unwrap().seed = seed;
        if reconcile {
            match self.factory.reconcile().await {
                Ok(done) => format!("{done:?}"),
                Err(e) => format!("error: {e}"),
            }
        } else {
            match self.factory.tick().await {
                Ok(ran) => ran.iter().map(|(t, o)| format!("{}: {o:?}", t.key)).collect::<Vec<_>>().join("; "),
                Err(e) => format!("error: {e}"),
            }
        }
    }

    pub async fn apply(&mut self, event: &Event) {
        let did = match event {
            Event::Tick(seed) => self.step(Some(*seed), false).await,
            Event::Reconcile(seed) => self.step(Some(*seed), true).await,
            Event::Restart => {
                self.factory = Self::boot(&self.compute, &self.tracker, &self.daemon, &self.codehost);
                String::new()
            }
            Event::Wait(secs) => {
                tokio::time::advance(Duration::from_secs(*secs)).await;
                String::new()
            }
            Event::Fault(fault) => {
                self.arm(*fault);
                String::new()
            }
            Event::Ci(i, _)
            | Event::Mergeable(i, _)
            | Event::Behind(i)
            | Event::Push(i)
            | Event::PersonMerges(i)
            | Event::PersonCloses(i) => match self.pr(*i).await {
                Some(pr) => self.on_pr(event, &pr).await,
                None => "nothing to do".into(),
            },
            Event::PersonApproves(i)
            | Event::PersonRequestsChanges(i)
            | Event::PersonAnswers(i)
            | Event::PersonReopens(i)
            | Event::PersonFinishes(i)
            | Event::PersonCancels(i) => {
                let key = self.tasks[*i % self.tasks.len()].clone();
                self.on_task(event, &key).await
            }
        };
        let now = self.states().await;
        self.log.push(format!("{event:?} -> {did}\n      {now}"));
    }

    /// Each task's status and the run fields the lifecycle turns on, in one line.
    async fn states(&self) -> String {
        let mut out = Vec::new();
        for key in &self.tasks {
            let t = self.show(key).await;
            let request = t
                .run
                .changes_requested
                .as_ref()
                .map(|c| format!("{:?}@a{}{}", c.kind, c.attempt, if c.settled { " settled" } else { "" }));
            out.push(format!(
                "{} {:?}{} attempt {:?} reviewed {:?} rounds {:?} conflicts {:?} request {request:?} holder {:?}",
                key.0,
                t.status,
                if t.changes_requested { " (changes requested)" } else { "" },
                t.run.attempt,
                t.run.reviewed,
                t.run.review_rounds,
                t.run.conflict_reworks,
                t.holder,
            ));
        }
        out.join("\n      ")
    }

    /// What the code host or a person does to an open PR.
    async fn on_pr(&self, event: &Event, pr: &PrRef) -> String {
        match event {
            Event::Ci(_, ci) => {
                let ci = match ci {
                    Ci::Pending => CiState::Pending,
                    Ci::Passed => CiState::Passed,
                    Ci::Failed => CiState::Failed { checks: vec!["test".into()] },
                };
                self.codehost.set_ci(pr, ci);
            }
            Event::Mergeable(_, mergeable) => self.codehost.set_mergeable(pr, *mergeable),
            Event::Behind(_) => self.codehost.set_behind(pr, true),
            Event::Push(_) => self.codehost.push_to(pr),
            Event::PersonCloses(_) => self.codehost.close(pr),
            Event::PersonMerges(_) => {
                let head = CodeHost::head(&self.codehost, pr).await.unwrap();
                return format!("{:?}", self.codehost.merge(pr, &head).await);
            }
            _ => unreachable!("{event:?} isn't about a PR"),
        }
        String::new()
    }

    /// What a person does to a task, when its status makes that a sensible thing to do.
    async fn on_task(&self, event: &Event, key: &TaskKey) -> String {
        use trailhead_core::Status as To;
        let status = self.show(key).await.status;
        let open = !matches!(status, Status::Done | Status::Cancelled);
        let done = match event {
            Event::PersonApproves(_) if status == Status::InReview => self.set_status(key, To::Approved).await,
            Event::PersonRequestsChanges(_) if status == Status::InReview => {
                let review = NewComment::review(trailhead_core::Verdict::RequestChanges, "A person wants more.");
                self.store.add_comment(&self.human, &key.0, review).await.map(|_| ()).map_err(|e| e.to_string())
            }
            Event::PersonAnswers(_) if status == Status::NeedsInput => {
                self.answer(key).await;
                self.set_status(key, To::Open).await
            }
            Event::PersonReopens(_) if status == Status::InReview => self.set_status(key, To::Open).await,
            Event::PersonFinishes(_) if open => self.set_status(key, To::Done).await,
            Event::PersonCancels(_) if open => self.set_status(key, To::Cancelled).await,
            _ => return "nothing to do".into(),
        };
        done.err().unwrap_or_default()
    }

    async fn answer(&self, key: &TaskKey) {
        let task = self.store.get_task(&self.human, &key.0).await.unwrap();
        let open = self.store.open_questions(&self.human, &[task.id]).await.unwrap();
        for question in open.into_values().flatten() {
            self.store.resolve_comment(&self.human, question.id, Some("Go ahead.".into())).await.unwrap();
        }
    }

    fn arm(&self, fault: Fault) {
        match fault {
            Fault::RunFails => self.daemon.fail_runs(1, "the daemon fell over"),
            Fault::RunFailsMidway => self.daemon.fail_runs_midway(1, "the connection dropped"),
            Fault::RunUnavailable => self.daemon.unavailable_runs(1, "no capacity"),
            Fault::RunCancelled => self.daemon.cancel_runs(1),
            Fault::PrepareFails => self.daemon.fail_prepares(1, "the clone failed"),
            Fault::PublishFails => self.codehost.flaky_publish(1),
            Fault::PublishFailsAfterApplying => self.codehost.flaky_publish_after_applying(1),
            Fault::OpenPrFails => self.codehost.flaky_open_pr(1),
            Fault::CommentFails => self.codehost.flaky_comment(1),
            Fault::MergeRefused => self.codehost.refuse_merge(Some("a required check is expected")),
            Fault::ComputeDown => self.compute.set_unreachable(true),
            Fault::ComputeUp => self.compute.set_unreachable(false),
        }
    }

    /// Everything a pass could have written: each task's version and comments, each PR's head, comments and state,
    /// and the sandboxes. Two passes with the same print wrote nothing between them.
    pub async fn print(&self) -> String {
        let mut out = String::new();
        for key in &self.tasks {
            let task = self.store.get_task(&self.human, &key.0).await.unwrap();
            let comments = self.store.list_comments(&self.human, &key.0).await.unwrap().len();
            out.push_str(&format!("{} v{} {:?} c{comments}\n", key.0, task.version, task.status));
        }
        for pr in self.codehost.prs() {
            let head = self.codehost.head(&pr.pr.repo, &pr.spec.branch);
            out.push_str(&format!(
                "#{} {head:?} c{} merged={} closed={}\n",
                pr.pr.number,
                pr.comments.len(),
                pr.merged,
                pr.closed
            ));
        }
        let mut sandboxes: Vec<String> =
            self.compute.sandboxes().iter().map(|s| format!("{} {:?}", s.name, s.labels)).collect();
        sandboxes.sort();
        out.push_str(&sandboxes.join("\n"));
        out
    }

    /// The trouble stops and the world turns fair: the infrastructure works, CI passes, PRs merge cleanly, workers
    /// make their changes and reviewers approve. The factory then gets pass after pass until one writes nothing.
    /// `Err` with what kept changing if it never does.
    pub async fn settle(&mut self) -> std::result::Result<(), String> {
        self.compute.set_unreachable(false);
        self.codehost.refuse_merge(None);
        let mut before = self.print().await;
        let mut still = 0;
        for round in 0..SETTLE_ROUNDS {
            for pr in self.codehost.prs().iter().filter(|p| !p.merged && !p.closed) {
                self.codehost.set_ci(&pr.pr, CiState::Passed);
                self.codehost.set_mergeable(&pr.pr, Some(true));
                self.codehost.set_behind(&pr.pr, false);
            }
            tokio::time::advance(Duration::from_secs(3600)).await;
            let reconciled = self.step(None, true).await;
            let ticked = self.step(None, false).await;
            let now = self.states().await;
            self.log.push(format!("settle {round}: reconcile -> {reconciled}; tick -> {ticked}\n      {now}"));
            let after = self.print().await;
            still = if after == before { still + 1 } else { 0 };
            if still == 2 {
                return Ok(());
            }
            before = after;
        }
        Err(format!("still writing after {SETTLE_ROUNDS} fair passes; the last state:\n{before}"))
    }

    /// Whether a person has an unanswered question on the task.
    pub async fn asked(&self, key: &TaskKey) -> bool {
        let task = self.store.get_task(&self.human, &key.0).await.unwrap();
        let open = self.store.open_questions(&self.human, &[task.id]).await.unwrap();
        open.values().flatten().any(|c| c.kind == CommentKind::Question)
    }
}

const SETTLE_ROUNDS: usize = 40;
