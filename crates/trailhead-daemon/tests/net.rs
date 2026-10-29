//! The control plane and sandbox daemons over real iroh connections on localhost: the MVP lifecycle with
//! `RemoteDaemon` in place of the in-process daemon, plus cancel, reattaching after a dropped connection and the
//! peer check.

use std::sync::Arc;
use std::time::Duration;

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, SecretKey};
use tokio::sync::Notify;
use trailhead_control::{Factory, Outcome};
use trailhead_daemon::net::{ALPN, DaemonProtocol, Launch, RemoteDaemon, read_frame, sandbox_key, write_frame};
use trailhead_factory::{
    Commit, Compute, Credential, Daemon, DaemonSecrets, Error, FactoryConfig, FileChange, FromDaemon, Labels, Role,
    RunEvent, RunOutcome, RunRequest, RunResult, SandboxId, SandboxSpec, Status, TaskKey, ToDaemon, Tracker, Usd,
    VerdictKind,
};
use trailhead_testkit::{FakeCodeHost, FakeCompute, FakeProvider, FakeTracker, LocalDaemon, Script, ScriptedHarness};

const CONFIG: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"
project = "TRAIL"

[checks]
fmt = "cargo fmt --check"
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
"#;

const MASTER: [u8; 32] = [7; 32];

async fn endpoint(key: SecretKey, lookup: Option<Arc<MemoryLookup>>) -> Endpoint {
    let mut builder =
        Endpoint::builder(presets::Minimal).secret_key(key).relay_mode(RelayMode::Disabled).alpns(vec![ALPN.to_vec()]);
    if let Some(lookup) = lookup {
        builder = builder.address_lookup(lookup);
    }
    builder.bind().await.unwrap()
}

struct Rig {
    tracker: FakeTracker,
    compute: FakeCompute,
    codehost: FakeCodeHost,
    harness: Arc<ScriptedHarness>,
    lookup: Arc<MemoryLookup>,
    control: Endpoint,
    remote: Arc<RemoteDaemon>,
    local: Arc<LocalDaemon>,
    routers: Vec<Router>,
}

impl Rig {
    async fn new() -> Rig {
        let tracker = FakeTracker::new("factory");
        let compute = FakeCompute::new();
        let codehost = FakeCodeHost::new();
        let harness = Arc::new(ScriptedHarness::new());
        let local = Arc::new(LocalDaemon::new(harness.clone(), Arc::new(codehost.clone()), tracker.clone()));
        let lookup = Arc::new(MemoryLookup::new());
        let control = endpoint(SecretKey::generate(), Some(lookup.clone())).await;
        let remote = Arc::new(RemoteDaemon::new(
            control.clone(),
            MASTER,
            Arc::new(compute.clone()),
            Launch::new(b"daemon".to_vec()),
        ));
        Rig { tracker, compute, codehost, harness, lookup, control, remote, local, routers: Vec::new() }
    }

    /// Stands up the daemon a sandbox would run, from the secrets the control plane would hand it.
    async fn serve(&mut self, sandbox: &str) {
        self.serve_with(sandbox, trailhead_daemon::net::ATTACH_PATIENCE).await;
    }

    async fn serve_with(&mut self, sandbox: &str, attach_patience: Duration) {
        let program = self.remote.program(&SandboxId(sandbox.into()));
        let secrets: DaemonSecrets = serde_json::from_slice(&program.files[1].contents).unwrap();
        let key = trailhead_daemon::net::parse_secret_key(&secrets.secret_key).unwrap();
        let control_plane = trailhead_daemon::net::parse_endpoint_id(&secrets.control_plane).unwrap();
        let ep = endpoint(key, None).await;
        self.lookup.add_endpoint_info(ep.addr());
        let protocol = DaemonProtocol::new(self.local.runner(), control_plane).attach_patience(attach_patience);
        self.routers.push(Router::builder(ep).accept(ALPN, protocol).spawn());
    }

    /// A sandbox the fake compute knows about, so the control plane can wake it before dialing.
    async fn sandbox(&self, name: &str) -> SandboxId {
        let spec = SandboxSpec { name: name.into(), labels: Labels::new(), egress: Default::default() };
        self.compute.create(&spec).await.unwrap().id
    }

    fn factory(&self) -> Factory {
        Factory::new(
            FactoryConfig::parse(CONFIG).unwrap(),
            Arc::new(self.compute.clone()),
            Arc::new(self.tracker.clone()),
            Arc::new(FakeProvider::new(1)),
            self.remote.clone(),
        )
    }
}

fn request(run: &str, key: TaskKey, credential: Credential) -> RunRequest {
    RunRequest {
        run: run.into(),
        role: Role::Worker,
        task: trailhead_factory::TaskSpec {
            key,
            title: "Slow".into(),
            description: String::new(),
            previous_failure: None,
            context: Default::default(),
        },
        repo: trailhead_factory::Repo { owner: "o".into(), name: "r".into() },
        base_branch: "main".into(),
        branch: format!("factory/{run}"),
        pr: None,
        feedback: None,
        model: trailhead_factory::ModelId("fake-sonnet".into()),
        endpoint: trailhead_factory::Endpoint { base_url: "http://gateway.invalid".into() },
        max_steps: 5,
        required_checks: Vec::new(),
        blocked_hosts: Vec::new(),
        allowed_hosts: Vec::new(),
        credential,
    }
}

fn changes() -> RunOutcome {
    RunOutcome::Changes {
        commits: vec![Commit {
            message: "feat: greet".into(),
            changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
        }],
        summary: "Adds a greeting.".into(),
    }
}

#[tokio::test]
async fn lifecycle_over_iroh() {
    let mut r = Rig::new().await;
    r.serve("trailhead-w1").await;
    r.serve("trail-1-a1-r1").await;
    let key = r.tracker.add_task("TRAIL", "Add a greeting", "Acceptance: `hi()` exists.");
    r.harness.push(Role::Worker, Script::new(changes()).steps(3, 100_000));
    r.harness.push(
        Role::Reviewer,
        Script::new(RunOutcome::Verdict {
            kind: VerdictKind::Approve,
            body: "Looks right.".into(),
            follow_ups: vec![],
        })
        .steps(1, 50_000),
    );

    let ran = r.factory().tick().await.unwrap();
    let Outcome::Reviewed { verdict, .. } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.kind, VerdictKind::Approve);
    assert_eq!(verdict.reviewer, "factory/trail-1-a1-r1");

    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview);
    // Usage streamed over the wire as events is what the control plane priced.
    assert_eq!(task.run.spend, Some(Usd::dollars(0.35)));
    let comment = &r.codehost.prs()[0].comments[0];
    assert!(
        comment.starts_with("🔍 **Review by `factory/trail-1-a1-r1`") && comment.ends_with("Looks right."),
        "{comment}"
    );

    let prepared: Vec<_> = r.local.prepared().into_iter().map(|(s, _)| s).collect();
    assert!(prepared.is_empty(), "prepare went over the wire, not through LocalDaemon::prepare");
}

#[tokio::test]
async fn cancel_reaches_the_daemon() {
    let mut r = Rig::new().await;
    r.serve("slow").await;
    let sandbox = r.sandbox("slow").await;
    let key = r.tracker.add_task("TRAIL", "Slow", "");
    r.harness.push(Role::Worker, Script::new(changes()).hang());
    let cred = r.tracker.create_sub_agent("slow", "TRAIL", Duration::from_secs(600)).await.unwrap();
    let req = request("slow", key, cred);
    let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
    let remote = r.remote.clone();
    let run = tokio::spawn({
        let sandbox = sandbox.clone();
        async move { remote.run(&sandbox, req, events).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.remote.cancel(&sandbox).await.unwrap();
    let out = tokio::time::timeout(Duration::from_secs(10), run).await.unwrap().unwrap();
    assert_eq!(out.unwrap_err(), Error::Cancelled);
    assert_eq!(r.compute.wakes("slow"), 1, "woken once, before the first dial");
}

#[tokio::test]
async fn a_run_survives_a_dropped_connection() {
    let mut r = Rig::new().await;
    r.serve("flaky").await;
    let sandbox = r.sandbox("flaky").await;
    let key = r.tracker.add_task("TRAIL", "Flaky", "");
    let gate = Arc::new(Notify::new());
    r.harness.push(Role::Worker, Script::new(changes()).steps(2, 100).wait_for(gate.clone()));
    let cred = r.tracker.create_sub_agent("flaky-a1", "TRAIL", Duration::from_secs(600)).await.unwrap();
    let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let remote = r.remote.clone();
    let run = tokio::spawn({
        let sandbox = sandbox.clone();
        let req = request("flaky-a1", key.clone(), cred);
        async move { remote.run(&sandbox, req, events).await }
    });
    let mut seen = Vec::new();
    while seen.len() < 4 {
        seen.push(tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap().unwrap());
    }

    // The connection drops while the agent is still working.
    r.remote.forget(&sandbox).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    gate.notify_one();

    let report = tokio::time::timeout(Duration::from_secs(20), run).await.unwrap().unwrap().unwrap();
    assert!(matches!(report.result, RunResult::PrOpened(_)), "{:?}", report.result);
    while let Ok(ev) = rx.try_recv() {
        seen.push(ev);
    }
    let steps = seen.iter().filter(|e| matches!(e, RunEvent::StepStarted { .. })).count();
    assert_eq!((seen.len(), steps), (4, 2), "every event exactly once: {seen:?}");
    assert_eq!(r.harness.contexts().len(), 1, "the run went on rather than starting again");
    assert_eq!(r.compute.wakes("flaky"), 2, "redialed once after the drop");
    assert_eq!(r.tracker.task(&key).status, Status::InReview);
}

#[tokio::test]
async fn a_run_survives_the_compute_api_failing_while_reattaching() {
    let mut r = Rig::new().await;
    r.serve("blip").await;
    let sandbox = r.sandbox("blip").await;
    let key = r.tracker.add_task("TRAIL", "Blip", "");
    let gate = Arc::new(Notify::new());
    r.harness.push(Role::Worker, Script::new(changes()).steps(2, 100).wait_for(gate.clone()));
    let cred = r.tracker.create_sub_agent("blip-a1", "TRAIL", Duration::from_secs(600)).await.unwrap();
    let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let remote = r.remote.clone();
    let run = tokio::spawn({
        let sandbox = sandbox.clone();
        let req = request("blip-a1", key.clone(), cred);
        async move { remote.run(&sandbox, req, events).await }
    });
    for _ in 0..4 {
        tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
    }

    r.compute.fail_wakes("blip", 2);
    r.remote.forget(&sandbox).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    gate.notify_one();

    let report = tokio::time::timeout(Duration::from_secs(20), run).await.unwrap().unwrap().unwrap();
    assert!(matches!(report.result, RunResult::PrOpened(_)), "{:?}", report.result);
    assert_eq!(r.harness.contexts().len(), 1, "the run went on rather than starting again");
}

#[tokio::test]
async fn a_prepare_survives_a_dropped_connection() {
    let mut r = Rig::new().await;
    r.serve("cold").await;
    let sandbox = r.sandbox("cold").await;
    let gate = Arc::new(Notify::new());
    r.local.gate_prepare(gate.clone());
    let req = r.factory().config.prepare_request(Role::Worker, None);
    let remote = r.remote.clone();
    let prepare = tokio::spawn({
        let sandbox = sandbox.clone();
        async move { remote.prepare(&sandbox, req).await }
    });
    until(|| r.local.prepares_started() == 1).await;

    // The connection drops partway through a long build.
    r.remote.forget(&sandbox).await;
    until(|| r.compute.wakes("cold") == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    gate.notify_one();

    tokio::time::timeout(Duration::from_secs(20), prepare).await.unwrap().unwrap().unwrap();
    assert_eq!(r.local.prepares_started(), 1, "the retry joined the prepare in flight");
}

async fn until(mut cond: impl FnMut() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting");
}

#[tokio::test]
async fn an_abandoned_run_is_cancelled() {
    let mut r = Rig::new().await;
    r.serve_with("lonely", Duration::from_millis(200)).await;
    let key = r.tracker.add_task("TRAIL", "Lonely", "");
    r.harness.push(Role::Worker, Script::new(changes()).steps(1, 100).hang());
    let cred = r.tracker.create_sub_agent("lonely-a1", "TRAIL", Duration::from_secs(600)).await.unwrap();
    let daemon = sandbox_key(&MASTER, &SandboxId("lonely".into())).public();

    let conn = r.control.connect(daemon, ALPN).await.unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &ToDaemon::Run(Box::new(request("lonely-a1", key, cred)))).await.unwrap();
    let first = read_frame::<FromDaemon>(&mut recv).await.unwrap();
    assert!(matches!(first, Some(FromDaemon::Event(_))), "{first:?}");
    conn.close(0u32.into(), b"gone");
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Reattaching later finds the run cancelled, with its events replayed from the start.
    let conn = r.control.connect(daemon, ALPN).await.unwrap();
    let attach = |run: &str| ToDaemon::Attach { run: run.into(), seen: 0 };
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &attach("lonely-a1")).await.unwrap();
    let mut replayed = 0;
    let last = loop {
        match read_frame::<FromDaemon>(&mut recv).await.unwrap() {
            Some(FromDaemon::Event(_)) => replayed += 1,
            other => break other,
        }
    };
    assert!(matches!(last, Some(FromDaemon::Failed(Error::Cancelled))), "{last:?}");
    assert_eq!(replayed, 2);

    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &attach("nobody")).await.unwrap();
    let reply = read_frame::<FromDaemon>(&mut recv).await.unwrap();
    assert!(matches!(reply, Some(FromDaemon::Failed(Error::NotFound(_)))), "{reply:?}");
}

#[tokio::test]
async fn daemons_refuse_other_peers() {
    let mut r = Rig::new().await;
    r.serve("locked").await;
    let stranger = endpoint(SecretKey::generate(), Some(r.lookup.clone())).await;
    let id = sandbox_key(&MASTER, &SandboxId("locked".into())).public();
    let conn = stranger.connect(id, ALPN).await.unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(10), conn.closed()).await.unwrap();
    assert!(reason.to_string().contains("unknown peer"), "{reason}");
}

#[tokio::test]
async fn subscription_token_goes_in_the_secrets_file_only() {
    let control = endpoint(SecretKey::generate(), None).await;
    let launch = Launch { claude_oauth_token: Some("sk-ant-oat-test".into()), ..Launch::new(b"daemon".to_vec()) };
    let remote = RemoteDaemon::new(control, MASTER, Arc::new(FakeCompute::new()), launch);
    let program = remote.program(&SandboxId("s".into()));
    let secrets: DaemonSecrets = serde_json::from_slice(&program.files[1].contents).unwrap();
    assert_eq!(secrets.claude_oauth_token.as_deref(), Some("sk-ant-oat-test"));
    assert_eq!(program.files[1].mode, 0o600);
    assert!(!format!("{secrets:?} {program:?}").contains("sk-ant-oat-test"), "never in debug output");
    assert!(!program.args.iter().any(|a| a.contains("sk-ant")), "never on the command line");
}

#[tokio::test]
async fn daemon_log_filter_goes_on_the_command_line_when_set() {
    let control = endpoint(SecretKey::generate(), None).await;
    let remote =
        RemoteDaemon::new(control.clone(), MASTER, Arc::new(FakeCompute::new()), Launch::new(b"daemon".to_vec()));
    let program = remote.program(&SandboxId("s".into()));
    assert!(!program.args.iter().any(|a| a == "--log"));

    let launch = Launch { log: Some("info,iroh=debug".into()), ..Launch::new(b"daemon".to_vec()) };
    let remote = RemoteDaemon::new(control, MASTER, Arc::new(FakeCompute::new()), launch);
    let args = remote.program(&SandboxId("s".into())).args;
    let at = args.iter().position(|a| a == "--log").expect("--log is set");
    assert_eq!(args[at + 1], "info,iroh=debug");
}
