//! A repo whose `[stack]` asks for node 22 gets node 22: the daemon installs mise and the tool in a real Sprite,
//! and the `setup` check runs under mise's environment. Creates and destroys one Sprite. Needs `SPRITES_TOKEN` and a
//! release build of the daemon in `TRAILHEAD_DAEMON_BIN`:
//!
//! `mise run test-live-mise`

use std::sync::Arc;

use iroh::Endpoint;
use iroh::endpoint::presets;
use trailhead_compute_sprites::SpritesCompute;
use trailhead_control::MISE_DOMAINS;
use trailhead_daemon::net::{Launch, RemoteDaemon};
use trailhead_factory::{Check, Compute, Daemon, Egress, IROH_DOMAINS, Labels, PrepareRequest, Repo, SandboxSpec};

#[tokio::test]
#[ignore = "needs SPRITES_TOKEN and TRAILHEAD_DAEMON_BIN, and creates a real Sprite"]
async fn a_stack_asking_for_node_22_gets_node_22() {
    let token = std::env::var("SPRITES_TOKEN").expect("SPRITES_TOKEN");
    let binary = std::fs::read(std::env::var("TRAILHEAD_DAEMON_BIN").expect("TRAILHEAD_DAEMON_BIN")).unwrap();
    let compute = Arc::new(SpritesCompute::new(token).unwrap().with_prefix("th-live-"));
    let domains = IROH_DOMAINS.iter().chain(MISE_DOMAINS).chain(&["github.com", "codeload.github.com"]);
    let spec = SandboxSpec {
        name: format!("th-live-mise-{}", std::process::id()),
        labels: Labels::from([("factory".into(), "trailhead-live".into())]),
        egress: Egress { domains: domains.map(|d| d.to_string()).collect() },
    };
    let sandbox = compute.create(&spec).await.unwrap();

    let control = Endpoint::builder(presets::N0).bind().await.unwrap();
    let remote =
        RemoteDaemon::new(control, iroh::SecretKey::generate().to_bytes(), compute.clone(), Launch::new(binary));
    let prepare = PrepareRequest {
        repo: Repo { owner: "lanesawyer".into(), name: "trailhead".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        setup: Some(Check {
            name: "setup".into(),
            command: "node --version | tee /dev/stderr | grep -q '^v22\\.'".into(),
        }),
        tools: [("node".to_string(), "22".to_string())].into(),
    };
    let result = remote.prepare(&sandbox.id, prepare).await;
    let _ = compute.destroy(&sandbox.id).await;
    result.expect("prepare installs node 22 and setup sees it");
}
