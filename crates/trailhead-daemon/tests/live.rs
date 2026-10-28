//! The daemon in a real Sprite, reached over iroh through n0's relay under a deny-by-default policy, reaching a
//! Trailhead server here through the API tunnel. Creates and destroys one Sprite. Needs `SPRITES_TOKEN` and a release build of the daemon in `TRAILHEAD_DAEMON_BIN`:
//!
//! `mise run test-live-daemon`

use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, SecretKey};
use trailhead_compute_sprites::SpritesCompute;
use trailhead_control::CoreTracker;
use trailhead_core::{Ctx, EventFilter, NewProject, NewTask, Store};
use trailhead_daemon::net::{API_ALPN, ApiTunnel, Launch, RemoteDaemon};
use trailhead_factory::{
    Check, Compute, Daemon, Egress, Endpoint as ModelEndpoint, IROH_DOMAINS, Labels, ModelId, PrRef, PrepareRequest,
    Repo, Role, RunRequest, RunResult, SandboxSpec, TaskKey, TaskSpec, Tracker,
};

/// A Trailhead server on a local port with a `LIVE` project and task, and a tracker for the control plane.
async fn trailhead(dir: &std::path::Path) -> (std::net::SocketAddr, CoreTracker, Arc<Store>, Ctx, TaskKey) {
    let db = dir.join("t.db");
    let db = db.to_str().unwrap();
    let server_store = Store::open_local(db).await.unwrap();
    let (ws, user) = server_store.bootstrap_local("Lane").await.unwrap();
    let human = Ctx::user(ws.id, user.id);
    server_store
        .create_project(&human, NewProject { key: "LIVE".into(), name: "Live".into(), ..Default::default() })
        .await
        .unwrap();
    let task = NewTask { project: "LIVE".into(), title: "Live run".into(), ..Default::default() };
    let key = TaskKey(server_store.create_task(&human, task).await.unwrap().key);
    let agent = server_store.create_agent(&human, "factory").await.unwrap();
    let ctx = server_store.agent_ctx(ws.id, agent.id).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = trailhead_server::router(Arc::new(trailhead_server::AppState::new(server_store)));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = Arc::new(Store::open_local(db).await.unwrap());
    (addr, CoreTracker::new(store.clone(), ctx, "factory"), store, human, key)
}

/// A gateway path with no connector behind it, so Claude Code fails at once instead of retrying.
const DEAD_MODEL: &str = "https://api.sprites.dev/v1/gateway/anthropic/none";

/// Where the worker run sends model calls; `LIVE_MODEL_URL=http://127.0.0.1:9` makes Claude Code retry for minutes.
fn worker_model() -> String {
    std::env::var("LIVE_MODEL_URL").unwrap_or_else(|_| DEAD_MODEL.into())
}

/// With `LIVE_SUBSCRIPTION` set, the worker runs Claude Code for real on `CLAUDE_CODE_OAUTH_TOKEN` (a few cents of
/// Lane's subscription), with a task that only asks a question, so nothing reaches GitHub.
fn subscription() -> Option<String> {
    std::env::var("LIVE_SUBSCRIPTION").ok().and_then(|_| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok())
}

fn worker_task() -> String {
    if subscription().is_some() {
        "This is a connectivity test. Don't read or change any files. Finish straight away with outcome \
         needs_input and the question \"Is the factory connected?\"."
            .into()
    } else {
        "Say hi.".into()
    }
}

fn domains(extra: &[&str]) -> Egress {
    Egress { domains: IROH_DOMAINS.iter().chain(extra).map(|d| d.to_string()).collect() }
}

#[tokio::test]
#[ignore = "needs SPRITES_TOKEN and TRAILHEAD_DAEMON_BIN, and creates a real Sprite"]
async fn daemon_in_a_sprite() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info,iroh=info,trailhead_daemon=debug".into());
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_test_writer().try_init();
    let token = std::env::var("SPRITES_TOKEN").expect("SPRITES_TOKEN");
    let binary = std::fs::read(std::env::var("TRAILHEAD_DAEMON_BIN").expect("TRAILHEAD_DAEMON_BIN")).unwrap();
    let compute = Arc::new(SpritesCompute::new(token).unwrap().with_prefix("th-live-"));
    let name = format!("th-live-m3-{}", std::process::id());
    let spec = SandboxSpec {
        name: name.clone(),
        labels: Labels::from([("factory".into(), "trailhead-live".into())]),
        egress: domains(&["github.com", "codeload.github.com"]),
    };
    let sandbox = compute.create(&spec).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let (server, tracker, store, human, key) = trailhead(dir.path()).await;
    let control = Endpoint::builder(presets::N0).bind().await.unwrap();
    let master = SecretKey::generate().to_bytes();
    let remote = Arc::new(RemoteDaemon::new(
        control.clone(),
        master,
        compute.clone(),
        Launch {
            log: std::env::var("LIVE_DAEMON_LOG").ok(),
            claude_oauth_token: subscription(),
            ..Launch::new(binary)
        },
    ));
    let _tunnel = Router::builder(control).accept(API_ALPN, ApiTunnel::new(server, remote.peers())).spawn();

    let result = tokio::spawn({
        let (remote, compute, sandbox) = (remote.clone(), compute.clone(), sandbox.clone());
        async move {
            let prepare = PrepareRequest {
                repo: Repo { owner: "lanesawyer".into(), name: "trailhead".into() },
                base_branch: "main".into(),
                setup: Some(Check { name: "setup".into(), command: "test -f Cargo.toml".into() }),
            };
            let t = Instant::now();
            remote.prepare(&sandbox.id, prepare.clone()).await.expect("first prepare");
            eprintln!("start + connect + clone + setup: {:.1}s", t.elapsed().as_secs_f64());

            // Agent-work egress: iroh only. The daemon stays reachable, but GitHub stops being, after a delay:
            // the policy call returns before enforcement catches up, so measure how long that takes.
            let agent_hosts: &[&str] = if subscription().is_some() { &["api.anthropic.com"] } else { &[] };
            compute.set_egress(&sandbox.id, &domains(agent_hosts)).await.unwrap();
            let t = Instant::now();
            let mut attempts = 0;
            let err = loop {
                attempts += 1;
                match remote.prepare(&sandbox.id, prepare.clone()).await {
                    Err(e) => break e.to_string(),
                    Ok(()) if attempts < 30 => tokio::time::sleep(Duration::from_millis(500)).await,
                    Ok(()) => panic!("GitHub still reachable {:.1}s after narrowing", t.elapsed().as_secs_f64()),
                }
            };
            eprintln!("GitHub blocked after {:.1}s ({attempts} prepares): {err}", t.elapsed().as_secs_f64());
            assert!(err.contains("git fetch"), "{err}");

            // A worker run: the daemon claims the task in Trailhead through the API tunnel as its sub-agent, Claude
            // Code can't reach a model (no Anthropic connector here), and the daemon releases the task again.
            let cred = tracker.create_sub_agent("live-a1", "LIVE", Duration::from_secs(900)).await.unwrap();
            let req = RunRequest {
                run: "live-a1".into(),
                role: Role::Worker,
                task: TaskSpec { key: key.clone(), title: "Live run".into(), description: worker_task() },
                repo: Repo { owner: "lanesawyer".into(), name: "trailhead".into() },
                base_branch: "main".into(),
                branch: "factory/live-1".into(),
                pr: None,
                model: ModelId("claude-sonnet-4-5".into()),
                endpoint: ModelEndpoint { base_url: worker_model() },
                max_steps: 1,
                required_checks: Vec::new(),
                blocked_hosts: vec!["github.com".into()],
                allowed_hosts: vec!["api.anthropic.com".into()],
                credential: cred,
            };
            let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
            let t = Instant::now();
            let report = remote.run(&sandbox.id, req, events).await.expect("the run reports");
            eprintln!("run finished in {:.1}s: {:?}", t.elapsed().as_secs_f64(), report.result);
            if subscription().is_some() {
                // A real model call on Lane's subscription, through the locked-down Sprite.
                assert!(matches!(report.result, RunResult::NeedsInput(_)), "{:?}", report.result);
                eprintln!("subscription run usage: {:?}", report.usage);
            } else {
                assert!(matches!(report.result, RunResult::Failed(_)), "{:?}", report.result);
            }

            let filter = EventFilter { task: Some(key.0.clone()), ..Default::default() };
            let kinds: Vec<String> =
                store.list_events(&human, filter).await.unwrap().into_iter().map(|e| e.kind).collect();
            eprintln!("task events: {kinds:?}");
            assert!(kinds.iter().any(|k| k == "claim.acquired"), "{kinds:?}");
            // A failed run releases the task; a question parks it with a question comment.
            let after = if subscription().is_some() { "comment.created" } else { "claim.released" };
            assert!(kinds.iter().any(|k| k == after), "{kinds:?}");

            // Subscription mode ignores the dead model endpoint, so a reviewer there would really review the PR and
            // post its verdict to GitHub. It stops here.
            if subscription().is_some() {
                return;
            }

            // A reviewer run reads the PR's diff through the Sprite's GitHub connector before the agent starts, so
            // failing in Claude Code rather than in GitHub means the connector worked. Read-only: the closed M0 PR.
            let cred = tracker.create_sub_agent("live-r1", "LIVE", Duration::from_secs(900)).await.unwrap();
            let pr = PrRef {
                repo: Repo { owner: "lanesawyer".into(), name: "trailhead".into() },
                number: 3,
                url: "https://github.com/lanesawyer/trailhead/pull/3".into(),
            };
            let review = RunRequest {
                run: "live-r1".into(),
                role: Role::Reviewer,
                task: TaskSpec { key: key.clone(), title: "Live run".into(), description: "Say hi.".into() },
                repo: pr.repo.clone(),
                base_branch: "main".into(),
                branch: "spike/m0-github-connector".into(),
                pr: Some(pr),
                model: ModelId("claude-opus-4-5".into()),
                endpoint: ModelEndpoint { base_url: DEAD_MODEL.into() },
                max_steps: 1,
                required_checks: Vec::new(),
                blocked_hosts: Vec::new(),
                allowed_hosts: Vec::new(),
                credential: cred,
            };
            let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
            let t = Instant::now();
            let again = review.clone();
            let report = remote.run(&sandbox.id, review, events).await.expect("the review reports");
            eprintln!("review finished in {:.1}s: {:?}", t.elapsed().as_secs_f64(), report.result);
            assert!(matches!(&report.result, RunResult::Failed(r) if r.starts_with("claude")), "{:?}", report.result);

            // With LIVE_PAUSE set: leave the Sprite idle until it pauses, then review again. The control plane has
            // to wake it, since an iroh dial alone doesn't.
            if std::env::var("LIVE_PAUSE").is_ok() {
                eprintln!("idling 150s so the Sprite pauses");
                tokio::time::sleep(Duration::from_secs(150)).await;
                let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
                let t = Instant::now();
                let report = remote.run(&sandbox.id, again, events).await.expect("the review after a pause reports");
                eprintln!("review after pause finished in {:.1}s: {:?}", t.elapsed().as_secs_f64(), report.result);
            }
        }
    });
    let outcome = tokio::time::timeout(Duration::from_secs(300), result).await;
    if !matches!(outcome, Ok(Ok(()))) {
        match compute.logs(&sandbox.id, "trailhead-daemon").await {
            Ok(logs) => eprintln!("--- daemon logs ---\n{logs}--- end ---"),
            Err(e) => eprintln!("couldn't read the daemon's logs: {e}"),
        }
    }
    compute.destroy(&sandbox.id).await.unwrap();
    outcome.expect("finished within 5 minutes").unwrap();
}
