//! The daemon in a real Sprite, reached over iroh through n0's relay under a deny-by-default policy. Creates and
//! destroys one Sprite. Needs `SPRITES_TOKEN` and a release build of the daemon in `TRAILHEAD_DAEMON_BIN`:
//!
//! `mise run test-live-daemon`

use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::endpoint::presets;
use iroh::{Endpoint, SecretKey};
use trailhead_compute_sprites::SpritesCompute;
use trailhead_daemon::net::{ALPN, Launch, RemoteDaemon};
use trailhead_factory::{
    Check, Compute, Credential, Daemon, Egress, Endpoint as ModelEndpoint, IROH_DOMAINS, Labels, ModelId,
    PrepareRequest, Repo, Role, RunRequest, SandboxSpec, TaskKey, TaskSpec,
};

fn domains(extra: &[&str]) -> Egress {
    Egress { domains: IROH_DOMAINS.iter().chain(extra).map(|d| d.to_string()).collect() }
}

#[tokio::test]
#[ignore = "needs SPRITES_TOKEN and TRAILHEAD_DAEMON_BIN, and creates a real Sprite"]
async fn daemon_in_a_sprite() {
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

    let control = Endpoint::builder(presets::N0).alpns(vec![ALPN.to_vec()]).bind().await.unwrap();
    let master = SecretKey::generate().to_bytes();
    let remote = Arc::new(RemoteDaemon::new(control, master, compute.clone(), Launch::new(binary)));

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
            compute.set_egress(&sandbox.id, &domains(&[])).await.unwrap();
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

            // A run crosses the wire and comes back as the daemon's typed error until Trailhead access lands (M4).
            let req = RunRequest {
                role: Role::Worker,
                task: TaskSpec { key: TaskKey("LIVE-1".into()), title: "Live".into(), description: String::new() },
                repo: Repo { owner: "lanesawyer".into(), name: "trailhead".into() },
                base_branch: "main".into(),
                branch: "factory/live-1".into(),
                pr: None,
                model: ModelId("claude-sonnet-4-5".into()),
                endpoint: ModelEndpoint { base_url: "https://api.sprites.dev/v1/gateway/anthropic/none".into() },
                max_steps: 1,
                required_checks: Vec::new(),
                blocked_hosts: vec!["github.com".into()],
                credential: Credential { agent: "live".into(), token: "none".into() },
            };
            let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
            let err = remote.run(&sandbox.id, req, events).await.expect_err("no tracker yet").to_string();
            assert!(err.contains("M4"), "{err}");
        }
    });
    let outcome = tokio::time::timeout(Duration::from_secs(300), result).await;
    compute.destroy(&sandbox.id).await.unwrap();
    outcome.expect("finished within 5 minutes").unwrap();
}
