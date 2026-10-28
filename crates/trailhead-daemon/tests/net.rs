//! The control plane and sandbox daemons over real iroh connections on localhost: the MVP lifecycle with
//! `RemoteDaemon` in place of the in-process daemon, plus cancel and the peer check.

use std::sync::Arc;
use std::time::Duration;

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, SecretKey};
use trailhead_control::{Factory, Outcome};
use trailhead_daemon::net::{ALPN, DaemonProtocol, Launch, RemoteDaemon, sandbox_key};
use trailhead_factory::{
    Commit, Compute, Daemon, DaemonSecrets, Error, FactoryConfig, FileChange, Labels, Role, RunOutcome, SandboxId,
    SandboxSpec, Status, Tracker, Usd, VerdictKind,
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
        let remote =
            Arc::new(RemoteDaemon::new(control, MASTER, Arc::new(compute.clone()), Launch::new(b"daemon".to_vec())));
        Rig { tracker, compute, codehost, harness, lookup, remote, local, routers: Vec::new() }
    }

    /// Stands up the daemon a sandbox would run, from the secrets the control plane would hand it.
    async fn serve(&mut self, sandbox: &str) {
        let program = self.remote.program(&SandboxId(sandbox.into()));
        let secrets: DaemonSecrets = serde_json::from_slice(&program.files[1].contents).unwrap();
        let key = trailhead_daemon::net::parse_secret_key(&secrets.secret_key).unwrap();
        let control_plane = trailhead_daemon::net::parse_endpoint_id(&secrets.control_plane).unwrap();
        let ep = endpoint(key, None).await;
        self.lookup.add_endpoint_info(ep.addr());
        let protocol = DaemonProtocol::new(self.local.runner(), control_plane);
        self.routers.push(Router::builder(ep).accept(ALPN, protocol).spawn());
    }

    fn factory(&self) -> Factory {
        Factory {
            config: FactoryConfig::parse(CONFIG).unwrap(),
            compute: Arc::new(self.compute.clone()),
            tracker: Arc::new(self.tracker.clone()),
            provider: Arc::new(FakeProvider::new(1)),
            daemon: self.remote.clone(),
        }
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
        Script::new(RunOutcome::Verdict { kind: VerdictKind::Approve, body: "Looks right.".into() }).steps(1, 50_000),
    );

    let ran = r.factory().tick().await.unwrap();
    let Outcome::Reviewed { verdict, .. } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.kind, VerdictKind::Approve);
    assert_eq!(verdict.reviewer, "factory/trail-1-a1-r1");

    let task = r.tracker.task(&key);
    assert_eq!(task.status, Status::InReview);
    // Usage streamed over the wire as events is what the control plane priced.
    assert_eq!(task.run.spend, Some(Usd::dollars(0.35)));
    assert_eq!(r.codehost.prs()[0].comments, ["Looks right."]);

    let prepared: Vec<_> = r.local.prepared().into_iter().map(|(s, _)| s).collect();
    assert!(prepared.is_empty(), "prepare went over the wire, not through LocalDaemon::prepare");
}

#[tokio::test]
async fn cancel_reaches_the_daemon() {
    let mut r = Rig::new().await;
    r.serve("slow").await;
    let sandbox = SandboxId("slow".into());
    r.compute
        .create(&SandboxSpec { name: "slow".into(), labels: Labels::new(), egress: Default::default() })
        .await
        .unwrap();
    let key = r.tracker.add_task("TRAIL", "Slow", "");
    r.harness.push(Role::Worker, Script::new(changes()).hang());
    let cred = r.tracker.create_sub_agent("slow", "TRAIL", Duration::from_secs(600)).await.unwrap();
    let req = trailhead_factory::RunRequest {
        role: Role::Worker,
        task: trailhead_factory::TaskSpec { key, title: "Slow".into(), description: String::new() },
        repo: trailhead_factory::Repo { owner: "o".into(), name: "r".into() },
        base_branch: "main".into(),
        branch: "factory/slow".into(),
        pr: None,
        model: trailhead_factory::ModelId("fake-sonnet".into()),
        endpoint: trailhead_factory::Endpoint { base_url: "http://gateway.invalid".into() },
        max_steps: 5,
        required_checks: Vec::new(),
        blocked_hosts: Vec::new(),
        allowed_hosts: Vec::new(),
        credential: cred,
    };
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
