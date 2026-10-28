//! Against a real Docker engine. Creates and destroys one small container and its checkpoint images.
//!
//! `mise run test-live-docker` (starts colima first with `mise run docker-up`), or with any Docker socket:
//! `DOCKER_HOST=unix://... cargo test -p trailhead-compute-docker --test live -- --ignored`

use trailhead_compute_docker::{DockerCompute, DockerConfig, HOME};
use trailhead_factory::{Compute, Egress, Labels, Program, SandboxFile, SandboxSpec};

/// Small, and has the `sh`, `setsid` and coreutils the supervisor needs.
const IMAGE: &str = "alpine:3";

#[tokio::test]
#[ignore = "needs a Docker engine and creates a real container"]
async fn lifecycle_against_docker() {
    let state = tempfile::tempdir().unwrap();
    let mut config = DockerConfig::new(state.path());
    config.image = IMAGE.into();
    config.network = "trailhead-live".into();
    let compute = DockerCompute::new(config).unwrap();
    pull(IMAGE).await;

    let name = format!("th-live-{}", std::process::id());
    let labels = Labels::from([
        ("factory".into(), "trailhead".into()),
        ("repo".into(), "lanesawyer/trailhead".into()),
        ("role".into(), "worker".into()),
    ]);
    let spec = SandboxSpec { name: name.clone(), labels: labels.clone(), egress: Egress::none() };

    let sandbox = compute.create(&spec).await.unwrap();
    // Spawned so a failed assertion still reaches the destroy below.
    let (c, sandbox_id) = (compute.clone(), sandbox.id.clone());
    let result = async move {
        let compute = c;
        assert_eq!(sandbox.labels, labels);
        assert_eq!(compute.create(&spec).await.unwrap().id, sandbox.id, "create is idempotent");

        let script = SandboxFile {
            path: "/root/.trailhead/sleeper.sh".into(),
            contents: b"#!/bin/sh\necho up\nexec sleep 600\n".to_vec(),
            mode: 0o755,
        };
        let secret = SandboxFile { path: "/root/.trailhead/secret".into(), contents: b"s3cret".to_vec(), mode: 0o600 };
        let sleeper = Program {
            name: "sleeper".into(),
            files: vec![script, secret],
            cmd: "/root/.trailhead/sleeper.sh".into(),
            args: vec![],
        };
        compute.start(&sandbox.id, &sleeper).await.unwrap();
        assert_eq!(docker(&["exec", &name, "stat", "-c", "%a %U", "/root/.trailhead/secret"]).await, "600 root");
        assert!(compute.logs(&sandbox.id, "sleeper").await.unwrap().contains("up"));
        // Starting it again replaces it rather than running two.
        compute.start(&sandbox.id, &sleeper).await.unwrap();

        let broken = Program {
            name: "broken".into(),
            files: vec![],
            cmd: "/bin/sh".into(),
            args: vec!["-c".into(), "echo boom; exit 3".into()],
        };
        let err = compute.start(&sandbox.id, &broken).await.expect_err("a program that exits at once fails start");
        assert!(err.to_string().contains("boom"), "{err}");

        let cp = compute.checkpoint(&sandbox.id, "live test").await.unwrap();
        compute.restore(&sandbox.id, &cp).await.unwrap();
        // The supervisor brings the sleeper back from the checkpoint.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(compute.logs(&sandbox.id, "sleeper").await.unwrap().contains("up"), "program restarted after restore");
        compute.wake(&sandbox.id).await.unwrap();

        let networks = ["inspect", "-f", "{{len .NetworkSettings.Networks}}", &name];
        assert_eq!(docker(&networks).await, "0", "no egress, no network");
        compute.set_egress(&sandbox.id, &Egress { domains: vec!["crates.io".into()] }).await.unwrap();
        assert_eq!(docker(&networks).await, "1");
        compute.set_egress(&sandbox.id, &Egress::none()).await.unwrap();
        assert_eq!(docker(&networks).await, "0");

        let mut cut = labels.clone();
        cut.remove("role");
        compute.set_labels(&sandbox.id, &cut).await.unwrap();
        let workers = Labels::from([("role".into(), "worker".into())]);
        assert!(compute.list(&workers).await.unwrap().iter().all(|s| s.name != name), "role label removed");
        let found = compute.list(&Labels::from([("factory".into(), "trailhead".into())])).await.unwrap();
        assert_eq!(found.iter().find(|s| s.name == name).map(|s| &s.labels), Some(&cut));
    };
    let outcome = tokio::spawn(result).await;
    compute.destroy(&sandbox_id).await.unwrap();
    compute.destroy(&sandbox_id).await.unwrap();
    assert!(compute.list(&Labels::new()).await.unwrap().iter().all(|s| s.id != sandbox_id), "destroyed");
    outcome.unwrap();
}

async fn pull(image: &str) {
    docker(&["pull", "-q", image]).await;
}

async fn docker(args: &[&str]) -> String {
    let out = tokio::process::Command::new("docker").args(args).output().await.unwrap();
    assert!(out.status.success(), "docker {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The sandbox image from `mise run docker-image`: files land owned by its unprivileged user, and its programs run
/// as that user.
#[tokio::test]
#[ignore = "needs a Docker engine and trailhead-sandbox:latest"]
async fn programs_run_as_the_sandbox_user() {
    let state = tempfile::tempdir().unwrap();
    let mut config = DockerConfig::new(state.path());
    config.network = "trailhead-live".into();
    let compute = DockerCompute::new(config).unwrap();
    let name = format!("th-live-user-{}", std::process::id());
    let spec = SandboxSpec { name: name.clone(), labels: Labels::new(), egress: Egress::none() };
    let sandbox = compute.create(&spec).await.unwrap();

    let (c, n) = (compute.clone(), name.clone());
    let result = async move {
        let secret =
            SandboxFile { path: format!("{HOME}/.trailhead/secrets.json"), contents: b"{}".to_vec(), mode: 0o600 };
        let whoami = Program {
            name: "whoami".into(),
            files: vec![secret],
            cmd: "/bin/sh".into(),
            args: vec!["-c".into(), "id -un; cat ~/.trailhead/secrets.json; exec sleep 600".into()],
        };
        c.start(&sandbox.id, &whoami).await.unwrap();
        let secrets = format!("{HOME}/.trailhead/secrets.json");
        assert_eq!(docker(&["exec", &n, "stat", "-c", "%a %U", &secrets]).await, "600 trailhead");
        let log = c.logs(&sandbox.id, "whoami").await.unwrap();
        assert!(log.starts_with("trailhead\n{}"), "{log}");
        assert!(docker(&["exec", &n, "claude", "--version"]).await.contains("Claude Code"));
    };
    let outcome = tokio::spawn(result).await;
    compute.destroy(&trailhead_factory::SandboxId(name)).await.unwrap();
    outcome.unwrap();
}
