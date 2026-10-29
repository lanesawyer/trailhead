//! M4: work flows from a real Trailhead server. The control plane uses the in-process tracker; each daemon reaches
//! the server through the control plane's iroh API tunnel with its sandbox's own sub-agent token.

use std::sync::Arc;
use std::time::Duration;

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use trailhead_control::{CoreTracker, Factory, Outcome};
use trailhead_core::{CommentKind, Ctx, NewProject, NewTask, Store};
use trailhead_daemon::net::{
    ALPN, API_ALPN, ApiTunnel, DaemonProtocol, Launch, RemoteDaemon, build_id, open_api_tunnel, parse_endpoint_id,
    parse_secret_key,
};
use trailhead_daemon::{Connect, RENEW_EVERY, RestTracker, Runner};
use trailhead_factory::{
    Commit, Credential, DaemonSecrets, Error, FactoryConfig, FileChange, FollowUp, Role, RunOutcome, SandboxId, Status,
    TaskKey, Tracker, VerdictKind,
};
use trailhead_testkit::{FakeCodeHost, FakeCompute, FakeProvider, FakeWorkspace, Script, ScriptedHarness};

const CONFIG: &str = r#"
[repo]
owner = "lanesawyer"
name = "trailhead"
project = "TRAIL"

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
"#;

const MASTER: [u8; 32] = [9; 32];

async fn endpoint(key: SecretKey, lookup: Arc<MemoryLookup>, alpns: Vec<Vec<u8>>) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(RelayMode::Disabled)
        .address_lookup(lookup)
        .alpns(alpns)
        .bind()
        .await
        .unwrap()
}

struct Rig {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    human: Ctx,
    tracker: CoreTracker,
    compute: FakeCompute,
    codehost: FakeCodeHost,
    harness: Arc<ScriptedHarness>,
    lookup: Arc<MemoryLookup>,
    control: EndpointAddr,
    remote: Arc<RemoteDaemon>,
    routers: Vec<Router>,
}

impl Rig {
    async fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("trailhead.db");
        let db = db.to_str().unwrap();

        // The server's store and the control plane's are two handles on one database, as when the factory is
        // hosted in the server process.
        let server_store = Store::open_local(db).await.unwrap();
        let (ws, user) = server_store.bootstrap_local("Lane").await.unwrap();
        let human = Ctx::user(ws.id, user.id);
        let project = NewProject { key: "TRAIL".into(), name: "Trailhead".into(), ..Default::default() };
        server_store.create_project(&human, project).await.unwrap();
        let agent = server_store.create_agent(&human, "factory").await.unwrap();
        let factory_ctx = server_store.agent_ctx(ws.id, agent.id).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = listener.local_addr().unwrap();
        let app = trailhead_server::router(Arc::new(trailhead_server::AppState::new(server_store)));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let store = Arc::new(Store::open_local(db).await.unwrap());
        let tracker = CoreTracker::new(store.clone(), factory_ctx, "factory");

        let lookup = Arc::new(MemoryLookup::new());
        let control_ep = endpoint(SecretKey::generate(), lookup.clone(), vec![API_ALPN.to_vec()]).await;
        let control = control_ep.addr();
        let compute = FakeCompute::new();
        let remote = Arc::new(RemoteDaemon::new(
            control_ep.clone(),
            MASTER,
            Arc::new(compute.clone()),
            Launch::new(b"daemon".to_vec()),
        ));
        let tunnel = Router::builder(control_ep).accept(API_ALPN, ApiTunnel::new(server, remote.peers())).spawn();

        Rig {
            _dir: dir,
            store,
            human,
            tracker,
            compute,
            codehost: FakeCodeHost::new(),
            harness: Arc::new(ScriptedHarness::new()),
            lookup,
            control,
            remote,
            routers: vec![tunnel],
        }
    }

    /// Stands up the daemon a sandbox would run, from its secrets, with its tracker on the API tunnel.
    async fn serve(&mut self, sandbox: &str) {
        let program = self.remote.program(&SandboxId(sandbox.into()));
        let secrets: DaemonSecrets = serde_json::from_slice(&program.files[1].contents).unwrap();
        let key = parse_secret_key(&secrets.secret_key).unwrap();
        let control_plane = parse_endpoint_id(&secrets.control_plane).unwrap();
        let lookup = Arc::new(MemoryLookup::new());
        lookup.add_endpoint_info(self.control.clone());
        let ep = endpoint(key, lookup, vec![ALPN.to_vec()]).await;
        self.lookup.add_endpoint_info(ep.addr());
        let api = open_api_tunnel(ep.clone(), control_plane).await.unwrap();
        let connect: Connect = Arc::new(move |cred: Credential| {
            Box::pin(async move { Ok(Arc::new(RestTracker::new(format!("http://{api}"), cred)) as Arc<dyn Tracker>) })
        });
        let runner = Runner {
            harness: self.harness.clone(),
            codehost: Arc::new(self.codehost.clone()),
            workspace: Arc::new(FakeWorkspace::default()),
            connect,
            renew_every: RENEW_EVERY,
        };
        let protocol = DaemonProtocol::new(Arc::new(runner), control_plane).build(build_id(b"daemon"));
        self.routers.push(Router::builder(ep).accept(ALPN, protocol).spawn());
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

    async fn task(&self, title: &str) -> TaskKey {
        let new = NewTask { project: "TRAIL".into(), title: title.into(), ..Default::default() };
        TaskKey(self.store.create_task(&self.human, new).await.unwrap().key)
    }
}

fn changes() -> RunOutcome {
    RunOutcome::Changes {
        commits: vec![Commit {
            message: "feat: greet".into(),
            changes: vec![FileChange { path: "src/greet.rs".into(), contents: Some(b"pub fn hi() {}".to_vec()) }],
            merges: vec![],
        }],
        summary: "Adds a greeting.".into(),
    }
}

#[tokio::test]
async fn work_flows_from_trailhead_over_the_tunnel() {
    let mut r = Rig::new().await;
    r.serve("trailhead-w1").await;
    r.serve("trail-1-a1-r1").await;
    let key = r.task("Add a greeting").await;
    r.harness.push(Role::Worker, Script::new(changes()).steps(2, 100_000));
    r.harness.push(
        Role::Reviewer,
        Script::new(RunOutcome::Verdict {
            kind: VerdictKind::Approve,
            body: "Looks right.".into(),
            follow_ups: vec![FollowUp {
                title: "Cover the edge".into(),
                description: "No test for empty input.".into(),
                priority: Some(2),
            }],
            raise_grade: None,
        })
        .steps(1, 10_000),
    );

    let ran = r.factory().tick().await.unwrap();
    let Outcome::Reviewed { pr, verdict } = &ran[0].1 else { panic!("{:?}", ran[0].1) };
    assert_eq!(verdict.reviewer, "factory/trail-1-a1-r1");

    // Everything the daemons wrote went through REST as their own sub-agents, and Trailhead's rules held.
    let task = r.tracker.show(&key).await.unwrap();
    assert_eq!(task.status, Status::InReview);
    assert_eq!(task.run.pr.as_ref(), Some(pr), "the worker daemon reported the PR");
    let detail = r.store.task_detail(&r.human, &key.0).await.unwrap();
    let review = detail.comments.iter().find(|c| c.kind == CommentKind::Review).expect("a review comment");
    assert_eq!(review.body, "Looks right.");
    assert_eq!(review.model.as_deref(), Some("fake-opus"));

    // The reviewer's project-scoped token created the follow-up through POST /tasks, and the lookup by title found it.
    let filed = r.tracker.find_open_by_title("TRAIL", "cover the edge ").await.unwrap().expect("a filed follow-up");
    assert_ne!(filed, key);
    let filed = r.store.task_detail(&r.human, &filed.0).await.unwrap().task;
    assert_eq!(filed.title, "Cover the edge");
    assert_eq!(filed.priority, 2);
    assert_eq!(filed.status, trailhead_core::Status::Open);
    assert!(filed.description.contains("No test for empty input."), "{}", filed.description);
    assert!(filed.description.contains(&key.0), "{}", filed.description);

    // Both sandboxes' sub-agents are revoked, so their tokens no longer work, even through the tunnel.
    let agents = r.store.list_agents(&r.human).await.unwrap();
    assert!(agents.iter().filter(|a| a.name.starts_with("factory/trail-1")).all(|a| a.revoked_at.is_some()));
}

#[tokio::test]
async fn parking_asks_through_the_tunnel() {
    let mut r = Rig::new().await;
    r.serve("trailhead-w1").await;
    let key = r.task("Add a greeting").await;
    r.harness.push(Role::Worker, Script::new(RunOutcome::NeedsInput { question: "Which greeting?".into() }));

    let ran = r.factory().tick().await.unwrap();
    assert!(matches!(ran[0].1, Outcome::Parked { .. }), "{:?}", ran[0].1);
    let detail = r.store.task_detail(&r.human, &key.0).await.unwrap();
    assert_eq!(detail.task.status, trailhead_core::Status::NeedsInput);
    let question = detail.comments.iter().find(|c| c.kind == CommentKind::Question).unwrap();
    assert_eq!(question.body, "Which greeting?");
}

#[tokio::test]
async fn the_tunnel_refuses_strangers_and_bad_tokens() {
    let mut r = Rig::new().await;
    r.serve("trailhead-w1").await;

    // A peer the control plane never started can't open the tunnel.
    let lookup = Arc::new(MemoryLookup::new());
    lookup.add_endpoint_info(r.control.clone());
    let stranger = endpoint(SecretKey::generate(), lookup, vec![]).await;
    let api = open_api_tunnel(stranger, r.control.id).await.unwrap();
    let cred = Credential { agent: "x".into(), token: "nope".into() };
    let err = RestTracker::new(format!("http://{api}"), cred).show(&TaskKey("TRAIL-1".into())).await.unwrap_err();
    assert!(matches!(err, Error::Other(_)), "no HTTP response at all: {err:?}");

    // A known daemon gets through, but the server still checks the token.
    let id = r.remote.daemon_id(&SandboxId("trail-1-a1".into()));
    r.remote.peers().lock().unwrap().insert(id);
    let program = r.remote.program(&SandboxId("trail-1-a1".into()));
    let secrets: DaemonSecrets = serde_json::from_slice(&program.files[1].contents).unwrap();
    let lookup = Arc::new(MemoryLookup::new());
    lookup.add_endpoint_info(r.control.clone());
    let daemon = endpoint(parse_secret_key(&secrets.secret_key).unwrap(), lookup, vec![]).await;
    let api = open_api_tunnel(daemon, r.control.id).await.unwrap();
    let cred = Credential { agent: "x".into(), token: "nope".into() };
    let err = tokio::time::timeout(
        Duration::from_secs(10),
        RestTracker::new(format!("http://{api}"), cred).show(&TaskKey("TRAIL-1".into())),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(err, Error::Forbidden(_)), "{err:?}");
}
