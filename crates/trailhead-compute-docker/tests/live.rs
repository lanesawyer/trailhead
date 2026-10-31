//! Against a real Docker engine. Creates and destroys small containers, their networks, their checkpoint images and
//! a gateway of their own, so they don't touch a running factory's.
//!
//! `mise run test-live-docker` (starts from `mise run docker-up` and `mise run docker-image`), or with any Docker
//! socket: `DOCKER_HOST=unix://... TRAILHEAD_DAEMON_BIN=... cargo test -p trailhead-compute-docker --test live -- --ignored`

use std::sync::Arc;
use std::time::Duration;

use trailhead_compute_docker::{DockerCompute, DockerConfig, GATEWAY_BIN, HOME, sandbox_network};
use trailhead_factory::{
    AuthScheme, Compute, Egress, GATEWAY_HOST, GATEWAY_PORT, GatewayConnection, GatewayResponse, GatewayState, Labels,
    Program, SandboxFile, SandboxId, SandboxSpec, Secret,
};

/// A host that's on the allowlists below and answers over HTTPS without a credential.
const ALLOWED: &str = "index.crates.io";
/// Never on an allowlist.
const REFUSED: &str = "example.com";

/// A `DockerCompute` with its own gateway and bridge network, removed by [`Live::finish`].
struct Live {
    compute: DockerCompute,
    gateway: String,
    outbound: String,
    _state: tempfile::TempDir,
}

fn daemon() -> Arc<Vec<u8>> {
    let path = std::env::var("TRAILHEAD_DAEMON_BIN").expect("TRAILHEAD_DAEMON_BIN: the daemon binary for the image");
    Arc::new(std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}")))
}

fn live(tag: &str) -> Live {
    // Only ever listed: nothing that uses this calls GitHub with it.
    live_with(tag, "not-a-real-token")
}

fn live_with(tag: &str, github_token: &str) -> Live {
    let state = tempfile::tempdir().unwrap();
    let mut config = DockerConfig::new(state.path(), daemon());
    let id = std::process::id();
    config.gateway = format!("th-live-gw-{tag}-{id}");
    config.network = format!("th-live-out-{tag}-{id}");
    config.connections = vec![GatewayConnection {
        provider: "github".into(),
        id: "github".into(),
        upstream: "https://api.github.com".into(),
        auth: AuthScheme::Bearer,
        credential: Secret::new(github_token),
    }];
    let (gateway, outbound) = (config.gateway.clone(), config.network.clone());
    Live { compute: DockerCompute::new(config).unwrap(), gateway, outbound, _state: state }
}

impl Live {
    async fn state(&self) -> GatewayState {
        let script = format!("printf '{{\"type\":\"state\"}}' | {GATEWAY_BIN} gateway-ctl");
        let out = docker(&["exec", &self.gateway, "/bin/sh", "-c", &script]).await;
        match serde_json::from_str(&out).unwrap() {
            GatewayResponse::State(state) => state,
            other => panic!("{other:?}"),
        }
    }

    async fn finish(self) {
        docker_quiet(&["rm", "-f", &self.gateway]).await;
        docker_quiet(&["network", "rm", &self.outbound]).await;
    }
}

fn spec(name: &str, labels: Labels, domains: &[&str]) -> SandboxSpec {
    let egress = Egress { domains: domains.iter().map(|d| d.to_string()).collect() };
    SandboxSpec { name: name.into(), labels, egress }
}

fn worker() -> Labels {
    Labels::from([
        ("factory".into(), "trailhead".into()),
        ("repo".into(), "lanesawyer/trailhead".into()),
        ("role".into(), "worker".into()),
    ])
}

/// What `curl` gets from `url` in the sandbox: the HTTP status, or `000` when it never got an answer. `-f`-less, so
/// a proxy's refusal shows as its status.
async fn curl(sandbox: &str, url: &str) -> String {
    let out = tokio::process::Command::new("docker")
        .args(["exec", sandbox, "curl", "-s", "-o", "/dev/null", "-m", "10", "-w", "%{http_code}", url])
        .output()
        .await
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

async fn sh(sandbox: &str, script: &str) -> String {
    docker(&["exec", sandbox, "/bin/bash", "-c", script]).await
}

#[tokio::test]
#[ignore = "needs a Docker engine and trailhead-sandbox:latest"]
async fn lifecycle_against_docker() {
    let env = live("life");
    let compute = env.compute.clone();
    let name = format!("th-live-{}", std::process::id());
    let labels = worker();
    let spec = spec(&name, labels.clone(), &[]);

    let sandbox = compute.create(&spec).await.unwrap();
    // Spawned so a failed assertion still reaches the destroy below.
    let (c, sandbox_id) = (compute.clone(), sandbox.id.clone());
    let result = async move {
        let compute = c;
        assert_eq!(sandbox.labels, labels);
        assert_eq!(compute.create(&spec).await.unwrap().id, sandbox.id, "create is idempotent");

        let script = SandboxFile {
            path: format!("{HOME}/.trailhead/sleeper.sh"),
            contents: b"#!/bin/sh\necho up\nexec sleep 600\n".to_vec(),
            mode: 0o755,
        };
        let secret =
            SandboxFile { path: format!("{HOME}/.trailhead/secret"), contents: b"s3cret".to_vec(), mode: 0o600 };
        let sleeper = Program {
            name: "sleeper".into(),
            files: vec![script, secret],
            cmd: format!("{HOME}/.trailhead/sleeper.sh"),
            args: vec![],
        };
        compute.start(&sandbox.id, &sleeper).await.unwrap();
        let secret = format!("{HOME}/.trailhead/secret");
        assert_eq!(docker(&["exec", &name, "stat", "-c", "%a %U", &secret]).await, "600 trailhead");
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
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(compute.logs(&sandbox.id, "sleeper").await.unwrap().contains("up"), "program restarted after restore");
        compute.wake(&sandbox.id).await.unwrap();

        let mut cut = worker();
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
    env.finish().await;
    outcome.unwrap();
}

/// Everything the gateway is for, on one pair of sandboxes: the cases share a gateway to keep the VM's memory free.
#[tokio::test]
#[ignore = "needs a Docker engine, trailhead-sandbox:latest and the internet"]
async fn the_gateway_is_the_only_way_out() {
    let env = live("egress");
    let compute = env.compute.clone();
    let id = std::process::id();
    let (a, b) = (format!("th-live-a-{id}"), format!("th-live-b-{id}"));
    let sandbox_a = compute.create(&spec(&a, worker(), &[ALLOWED])).await.unwrap();
    let sandbox_b = compute.create(&spec(&b, Labels::new(), &[ALLOWED])).await.unwrap();
    assert_eq!(sandbox_a.id.0, a);

    let result = {
        let (compute, a, b, gateway) = (compute.clone(), a.clone(), b.clone(), env.gateway.clone());
        let refusals = {
            let script = format!("printf '{{\"type\":\"state\"}}' | {GATEWAY_BIN} gateway-ctl");
            let gateway = gateway.clone();
            let a = a.clone();
            move |host: String| {
                let (script, gateway, a) = (script.clone(), gateway.clone(), a.clone());
                async move {
                    let out = docker(&["exec", &gateway, "/bin/sh", "-c", &script]).await;
                    let GatewayResponse::State(state) = serde_json::from_str(&out).unwrap() else { panic!("{out}") };
                    let sandbox = state.sandboxes.iter().find(|s| s.name == a).unwrap();
                    sandbox.refused.get(&host).copied().unwrap_or(0)
                }
            }
        };
        async move {
            let allowed = format!("https://{ALLOWED}/config.json");
            let refused = format!("https://{REFUSED}/");

            // Off the allowlist, refused; on it, allowed.
            assert_eq!(curl(&a, &allowed).await, "200", "on the allowlist");
            assert_ne!(curl(&a, &refused).await, "200", "off the allowlist");
            assert_eq!(refusals(REFUSED.into()).await, 1, "it was the gateway that refused");

            // Without the proxy there's no route and no DNS.
            let direct = format!(
                "curl -s -o /dev/null -m 5 --noproxy '*' -w '%{{http_code}}' https://1.1.1.1/ || true; \
                 getent hosts {ALLOWED} || echo unresolved"
            );
            let out = sh(&a, &direct).await;
            assert!(out.starts_with("000") && out.ends_with("unresolved"), "{out}");

            // The capabilities a raw socket or a new route would need are gone.
            let caps = sh(&a, "grep CapBnd /proc/self/status").await;
            let bnd = u64::from_str_radix(caps.split_whitespace().last().unwrap(), 16).unwrap();
            assert_eq!(bnd & (1 << 12 | 1 << 13), 0, "NET_ADMIN and NET_RAW dropped: {caps}");

            // Narrowing takes effect when set_egress returns: new requests are refused and open tunnels close.
            let hold = format!(
                "exec 3<>/dev/tcp/{GATEWAY_HOST}/{GATEWAY_PORT}; \
                 printf 'CONNECT {ALLOWED}:443 HTTP/1.1\\r\\nHost: {ALLOWED}:443\\r\\n\\r\\n' >&3; \
                 (head -n 1 <&3; cat <&3 >/dev/null; echo closed) > /tmp/tunnel 2>&1 &"
            );
            sh(&a, &hold).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert!(sh(&a, "cat /tmp/tunnel").await.contains("200"), "the tunnel opened");
            compute.set_egress(&SandboxId(a.clone()), &Egress::none()).await.unwrap();
            assert_ne!(curl(&a, &allowed).await, "200", "narrowed");
            assert_eq!(refusals(ALLOWED.into()).await, 1, "it was the gateway that refused");
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(sh(&a, "cat /tmp/tunnel").await.ends_with("closed"), "the open tunnel was closed");
            compute.set_egress(&SandboxId(a.clone()), &Egress { domains: vec![ALLOWED.into()] }).await.unwrap();
            assert_eq!(curl(&a, &allowed).await, "200", "widened again");

            // A sandbox can't reach another, by name, by address or through the gateway.
            let format = format!("{{{{(index .NetworkSettings.Networks \"{}\").IPAddress}}}}", sandbox_network(&b));
            let b_ip = docker(&["inspect", "-f", &format, &b]).await;
            assert!(!b_ip.is_empty());
            let reach =
                format!("curl -s -o /dev/null -m 5 --noproxy '*' -w '%{{http_code}}' http://{b_ip}:8000/ || true");
            assert_eq!(sh(&a, &reach).await, "000", "directly");
            assert_ne!(curl(&a, &format!("http://{b_ip}:8000/")).await, "200", "through the proxy");
            assert_eq!(refusals(b_ip.clone()).await, 1, "it was the gateway that refused");
            assert!(sh(&a, &format!("getent hosts {b} || echo unresolved")).await.ends_with("unresolved"));

            // The connector list is for sandboxes with a role.
            let list = format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}/v1/gateway/list");
            let read = format!("curl -s -m 5 {list}");
            let listed = sh(&a, &read).await;
            assert!(listed.contains("\"provider\":\"github\""), "{listed}");
            assert!(listed.contains(&format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}/v1/gateway/github/github")));
            assert!(!sh(&b, &read).await.contains("github"), "no role, no connections");
            assert!(!listed.contains("not-a-real-token"));

            // The kill switch: losing the role loses the connection.
            let mut cut = worker();
            cut.remove("role");
            compute.set_labels(&SandboxId(a.clone()), &cut).await.unwrap();
            assert!(!sh(&a, &read).await.contains("github"), "role label removed");
            compute.set_labels(&SandboxId(a.clone()), &worker()).await.unwrap();
            assert!(sh(&a, &read).await.contains("github"), "role label back");

            // The credential reached the gateway on stdin, not on a command line it could be read from.
            let inspect = docker(&["inspect", &gateway]).await;
            assert!(!inspect.contains("not-a-real-token"));
        }
    };
    let outcome = tokio::spawn(result).await;
    compute.destroy(&sandbox_a.id).await.unwrap();
    compute.destroy(&sandbox_b.id).await.unwrap();
    let networks = docker(&["network", "ls", "--format", "{{.Name}}"]).await;
    let leftover = networks.lines().filter(|n| n.contains(&sandbox_network(&a)) || n.contains(&sandbox_network(&b)));
    assert_eq!(leftover.count(), 0, "destroy removes the sandbox's network");
    env.finish().await;
    outcome.unwrap();
}

/// Subscription mode: the token stays in the sandbox (until TRAIL-487) and Claude Code reaches the model API through
/// the proxy, with only that host allowed. Spends one small call on the subscription.
#[tokio::test]
#[ignore = "needs CLAUDE_CODE_OAUTH_TOKEN, so it's `mise run test-live-docker-credentials`"]
async fn credentials_subscription_reaches_the_model_through_the_proxy() {
    let token = std::env::var("CLAUDE_CODE_OAUTH_TOKEN").expect("needs CLAUDE_CODE_OAUTH_TOKEN");
    // As `ClaudeCode::with_oauth_token` does: a pasted token can carry line breaks.
    let token: String = token.split_whitespace().collect();
    let env = live("sub");
    let compute = env.compute.clone();
    let name = format!("th-live-sub-{}", std::process::id());
    let sandbox = compute.create(&spec(&name, worker(), &["api.anthropic.com"])).await.unwrap();

    // `-e NAME` with no value: docker takes it from its own environment, so the token is on no command line.
    let output = tokio::process::Command::new("docker")
        .env("CLAUDE_CODE_OAUTH_TOKEN", token)
        .args(["exec", "-e", "CLAUDE_CODE_OAUTH_TOKEN", "-e", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1"])
        .args([&name, "claude", "-p", "Reply with the single word ok, nothing else.", "--model", "haiku"])
        .output()
        .await
        .unwrap();
    let refused = env.state().await.sandboxes.iter().find(|s| s.name == name).map(|s| s.refused.clone());
    compute.destroy(&sandbox.id).await.unwrap();
    env.finish().await;
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    println!("claude: {out}; refused: {refused:?}");
    assert!(output.status.success(), "claude failed: {out} {err}; refused: {refused:?}");
    assert!(out.to_ascii_lowercase().contains("ok"), "{out}");
}

/// The real token, read-only: one GET through the gateway, and a refusal that never reaches GitHub. The sandbox sends
/// a junk credential, so a 200 also shows the gateway replaced it.
#[tokio::test]
#[ignore = "needs GITHUB_WRITER_TOKEN, so it's `mise run test-live-docker-credentials`"]
async fn credentials_github_token_is_added_by_the_gateway() {
    let token = std::env::var("GITHUB_WRITER_TOKEN").expect("needs GITHUB_WRITER_TOKEN");
    let env = live_with("github", token.trim());
    let compute = env.compute.clone();
    let name = format!("th-live-github-{}", std::process::id());
    let sandbox = compute.create(&spec(&name, worker(), &[])).await.unwrap();

    let base = format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}/v1/gateway/github/github");
    let get = |path: &str| {
        format!("curl -s -o /dev/null -m 15 -w '%{{http_code}}' -H 'Authorization: Bearer junk' {base}{path}")
    };
    let allowed = sh(&name, &get("/repos/lanesawyer/trailhead/pulls?per_page=1")).await;
    let refused = sh(&name, &get("/repos/lanesawyer/other/pulls")).await;
    let environment = sh(&name, "env").await;
    compute.destroy(&sandbox.id).await.unwrap();
    env.finish().await;
    assert_eq!(allowed, "200", "this repo's pulls, with the gateway's credential");
    assert_eq!(refused, "403", "another repo");
    assert!(!environment.contains(token.trim()), "the token isn't in the sandbox");
}

/// Waits for the background check to bring the gateway back with `name` registered and allowed `ALLOWED`.
async fn gateway_recovers(env: &Live, name: &str) {
    for _ in 0..60 {
        let ok = curl(name, &format!("https://{ALLOWED}/config.json")).await == "200";
        if ok && env.state().await.sandboxes.iter().any(|s| s.name == name) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("the gateway never got {name}'s state back");
}

#[tokio::test]
#[ignore = "needs a Docker engine, trailhead-sandbox:latest and the internet"]
async fn the_gateway_gets_its_state_back() {
    let env = live("restart");
    let compute = env.compute.clone();
    let name = format!("th-live-restart-{}", std::process::id());
    let sandbox = compute.create(&spec(&name, worker(), &[ALLOWED])).await.unwrap();

    let result = {
        let (compute, env_gateway, name) = (compute.clone(), env.gateway.clone(), name.clone());
        async move {
            // A restart empties its memory; a running sandbox gets its way out back with no `Compute` call.
            docker(&["restart", &env_gateway]).await;
            gateway_recovers(&env, &name).await;
            assert_eq!(env.state().await.connections.len(), 1, "the credential is pushed again");

            // Gone altogether: recreated, joined to the network again.
            docker(&["rm", "-f", &env_gateway]).await;
            gateway_recovers(&env, &name).await;

            // A new control plane finds the gateway as it left it and pushes the same state.
            let again = DockerCompute::new(reborn(&env)).unwrap();
            assert!(again.list(&Labels::new()).await.unwrap().iter().any(|s| s.name == name));
            gateway_recovers(&env, &name).await;
            drop(compute);
            env
        }
    };
    let outcome = tokio::spawn(result).await;
    compute.destroy(&sandbox.id).await.unwrap();
    let env = outcome.unwrap();
    env.finish().await;
}

fn reborn(env: &Live) -> DockerConfig {
    let mut config = DockerConfig::new(env._state.path(), daemon());
    config.gateway = env.gateway.clone();
    config.network = env.outbound.clone();
    config
}

async fn docker(args: &[&str]) -> String {
    let out = tokio::process::Command::new("docker").args(args).output().await.unwrap();
    assert!(out.status.success(), "docker {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// For cleanup, which shouldn't hide the test's own failure.
async fn docker_quiet(args: &[&str]) {
    let _ = tokio::process::Command::new("docker").args(args).output().await;
}

/// The sandbox image from `mise run docker-image`: files land owned by its unprivileged user, and its programs run
/// as that user.
#[tokio::test]
#[ignore = "needs a Docker engine and trailhead-sandbox:latest"]
async fn programs_run_as_the_sandbox_user() {
    let env = live("user");
    let compute = env.compute.clone();
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
    compute.destroy(&SandboxId(name)).await.unwrap();
    env.finish().await;
    outcome.unwrap();
}

/// A pooled worker checkpoints after every task, restored from the last: its images must stay one layer each and
/// the old warm ones must go, however many tasks it runs (TRAIL-176).
#[tokio::test]
#[ignore = "needs a Docker engine and trailhead-sandbox:latest"]
async fn warm_checkpoints_stay_flat_and_old_ones_go() {
    let env = live("warm");
    let compute = env.compute.clone();
    let name = format!("th-live-warm-{}", std::process::id());
    let spec = SandboxSpec { name: name.clone(), labels: Labels::new(), egress: Egress::none() };
    let sandbox = compute.create(&spec).await.unwrap();

    let (c, n) = (compute.clone(), name.clone());
    let result = async move {
        c.preflight(&sandbox.id).await.unwrap();
        let filter = format!("label=dev.trailhead.checkpoint-of={n}");
        let mut warm = None;
        for task in 0..6 {
            if let Some(cp) = &warm {
                c.restore(&sandbox.id, cp).await.unwrap();
            }
            // Each task rewrites the same big file, as a build rewrites `target`.
            sh(&n, "head -c 20000000 /dev/urandom > $HOME/target.bin; echo done >> $HOME/log").await;
            let cp = c.checkpoint(&sandbox.id, "warm").await.unwrap();
            // The control plane labels the sandbox with the new warm checkpoint after the checkpoint returns.
            let labels = Labels::from([("warm".into(), cp.0.clone())]);
            c.set_labels(&sandbox.id, &labels).await.unwrap();
            let layers = docker(&["image", "inspect", "-f", "{{len .RootFS.Layers}}", &cp.0]).await;
            assert_eq!(layers, "1", "task {task}");
            let images = docker(&["images", "--filter", &filter, "--format", "{{.Repository}}:{{.Tag}}"]).await;
            assert!(images.lines().count() <= 2, "task {task}: {images}");
            warm = Some(cp);
        }
        let cp = warm.unwrap();
        c.restore(&sandbox.id, &cp).await.unwrap();
        assert_eq!(sh(&n, "wc -l < $HOME/log").await.trim(), "6");
        assert_eq!(sh(&n, "id -un").await.trim(), "trailhead");
    };
    let outcome = tokio::spawn(result).await;
    compute.destroy(&SandboxId(name)).await.unwrap();
    env.finish().await;
    outcome.unwrap();
}
