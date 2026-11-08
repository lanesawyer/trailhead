//! The real daemon in a Docker sandbox over iroh, across a warm checkpoint's restore (TRAIL-666). Creates and destroys
//! one container and a gateway of its own, so it runs beside a factory.
//!
//! `DOCKER_HOST=... TRAILHEAD_DAEMON_BIN=... cargo test -p trailhead-compute-docker --test live_daemon -- --ignored`

use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::Endpoint;
use iroh::endpoint::presets;
use trailhead_compute_docker::{DockerCompute, DockerConfig};
use trailhead_daemon::plugins::CLAUDE_CODE;
use trailhead_factory::{Compute, Daemon, Egress, IROH_DOMAINS, Labels, PrepareRequest, Repo, SandboxId, SandboxSpec};
use trailhead_wire::daemon::{Launch, RemoteDaemon};

fn request() -> PrepareRequest {
    PrepareRequest {
        repo: Repo { owner: "lanesawyer".into(), name: "trailhead".into() },
        base_branch: "main".into(),
        branch: None,
        merge_base: false,
        setup: None,
        tools: Default::default(),
        tool_hosts: Default::default(),
        env: Default::default(),
        trim: None,
        disk_warn_bytes: None,
        disk_limit_bytes: None,
    }
}

#[tokio::test]
#[ignore = "needs a Docker engine, trailhead-sandbox:latest, TRAILHEAD_DAEMON_BIN and the internet"]
async fn the_daemon_answers_promptly_after_a_restore() {
    let path = std::env::var("TRAILHEAD_DAEMON_BIN").expect("TRAILHEAD_DAEMON_BIN");
    let binary = Arc::new(std::fs::read(&path).unwrap());
    let state = tempfile::tempdir().unwrap();
    let id = std::process::id();
    let mut config = DockerConfig::new(state.path(), binary.clone());
    config.gateway = format!("th-live-gw-daemon-{id}");
    config.network = format!("th-live-out-daemon-{id}");
    let (gateway, network) = (config.gateway.clone(), config.network.clone());
    let compute = Arc::new(DockerCompute::new(config).unwrap());
    let name = format!("th-live-daemon-{id}");
    let egress = Egress { domains: IROH_DOMAINS.iter().map(|d| d.to_string()).collect() };
    let spec = SandboxSpec { name: name.clone(), labels: Labels::new(), egress };
    let sandbox = compute.create(&spec).await.unwrap();

    let (c, sid) = (compute.clone(), sandbox.id.clone());
    let result = async move {
        let control = Endpoint::builder(presets::N0).bind().await.unwrap();
        let launch = Launch::new(binary.to_vec(), &trailhead_compute_docker::target(), CLAUDE_CODE);
        let remote = RemoteDaemon::new(control, iroh::SecretKey::generate().to_bytes(), c.clone(), launch);
        let timed = async |what: &str| {
            let start = Instant::now();
            remote.install_hosts(&sid, &request()).await.unwrap();
            let took = start.elapsed();
            eprintln!("{what}: install_hosts took {took:?}");
            took
        };
        timed("cold").await;
        let mut slow = Vec::new();
        for round in 1..=3 {
            let cp = c.checkpoint(&sid, "warm").await.unwrap();
            c.restore(&sid, &cp).await.unwrap();
            remote.forget(&sid).await;
            let took = timed(&format!("restore {round}")).await;
            if took > Duration::from_secs(20) {
                slow.push((round, took));
            }
        }
        assert!(slow.is_empty(), "install_hosts stalled after a restore: {slow:?}");
    };
    let outcome = tokio::spawn(result).await;
    compute.destroy(&SandboxId(name)).await.unwrap();
    let _ = tokio::process::Command::new("docker").args(["rm", "-f", &gateway]).output().await;
    let _ = tokio::process::Command::new("docker").args(["network", "rm", &network]).output().await;
    outcome.unwrap();
}
