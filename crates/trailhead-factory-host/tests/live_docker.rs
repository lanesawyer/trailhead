//! A Trailhead task end to end on Docker sandboxes through the gateway, with the real `trailhead` server, the real
//! `trailhead-factory` and a scratch GitHub repo (`docs/factory/sandbox.md#the-gateway-credentials-and-egress-for-docker-sandboxes`).
//!
//! The factory runs a worker on a small task, which clones the repo, edits it and opens a PR, and then a reviewer
//! reviews that PR. While each agent works the test checks, from outside and inside its sandbox, that a host off the
//! allowlist is refused, that no GitHub credential is in the sandbox though GitHub answers through the gateway, and
//! (for the worker) that removing the `role` label cuts its GitHub access. Subscription mode: the OAuth token is in
//! the sandbox until TRAIL-487, so there's no model-key check.
//!
//! `mise run test-live-docker-factory` (after `mise run docker-up` and `mise run docker-image`). It needs a public
//! scratch repo in `TRAILHEAD_LIVE_REPO` (`owner/name`, with a `main` branch), `GITHUB_TOKEN`, `GITHUB_WRITER_TOKEN`
//! (which can push and open PRs there) and `CLAUDE_CODE_OAUTH_TOKEN`, plus `TRAILHEAD_DAEMON_BIN` and
//! `TRAILHEAD_BIN` (the `trailhead` CLI). It spends real model calls and closes its PR and deletes its branch at the
//! end. It shares the engine's `trailhead-gateway` with any other factory, so it refuses to start beside one.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trailhead_compute_docker::{DockerCompute, DockerConfig, GATEWAY_BIN};
use trailhead_factory::{
    AuthScheme, Compute, GATEWAY_GITHUB_ID, GATEWAY_HOST, GATEWAY_PORT, GatewayConnection, GatewayResponse,
    GatewayState, Labels, SandboxId, Secret,
};

/// The Docker label on every sandbox container.
const MARKER: &str = "dev.trailhead.sandbox";
const PROJECT: &str = "LIVE";
const TASK: &str = "LIVE-1";
const DEADLINE: Duration = Duration::from_secs(45 * 60);

#[derive(Clone)]
struct Env {
    repo: String,
    read: String,
    writer: String,
    cli: String,
    daemon: String,
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("needs {name}")).trim().to_string()
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

async fn sandbox_containers() -> Vec<String> {
    let label = format!("label={MARKER}");
    let format = format!("{{{{.Label \"{MARKER}\"}}}}");
    docker(&["ps", "-a", "--filter", &label, "--format", &format]).await.lines().map(str::to_string).collect()
}

/// What `curl` gets from `url` in the sandbox: the HTTP status, or `000` when it never got an answer.
async fn curl(sandbox: &str, url: &str, headers: &[&str]) -> String {
    let mut args = vec!["exec", sandbox, "curl", "-s", "-o", "/dev/null", "-m", "15", "-w", "%{http_code}"];
    for h in headers {
        args.extend(["-H", h]);
    }
    args.push(url);
    let out = tokio::process::Command::new("docker").args(&args).output().await.unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Everything the sandbox's processes were started with, and git's settings and credential files.
async fn secrets_surface(sandbox: &str) -> String {
    let script = "cat /proc/[0-9]*/environ 2>/dev/null | tr '\\0' '\\n'; git config --global --list 2>/dev/null; \
                  cat ~/.git-credentials ~/.config/gh/hosts.yml ~/.netrc 2>/dev/null; true";
    docker(&["exec", sandbox, "/bin/bash", "-c", script]).await
}

async fn agent_running(sandbox: &str) -> bool {
    let status = tokio::process::Command::new("docker")
        .args(["exec", sandbox, "pgrep", "-f", "--", "--output-format"])
        .stdout(Stdio::null())
        .status()
        .await
        .unwrap();
    status.success()
}

async fn gateway_state() -> GatewayState {
    let script = format!("printf '{{\"type\":\"state\"}}' | {GATEWAY_BIN} gateway-ctl");
    let out = docker(&["exec", GATEWAY_HOST, "/bin/sh", "-c", &script]).await;
    match serde_json::from_str(&out).unwrap() {
        GatewayResponse::State(state) => state,
        other => panic!("{other:?}"),
    }
}

async fn github(token: &str, method: reqwest::Method, repo: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let url = format!("https://api.github.com/repos/{repo}{path}");
    let mut req = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token)
        .header("User-Agent", "trailhead-live-test")
        .header("Accept", "application/vnd.github+json");
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// The scratch repo's PR for the task, by the key in its title.
async fn task_pr(env: &Env) -> Option<Value> {
    let (_, prs) = github(&env.read, reqwest::Method::GET, &env.repo, "/pulls?state=all&per_page=30", None).await;
    prs.as_array()?.iter().find(|p| p["title"].as_str().is_some_and(|t| t.contains(TASK))).cloned()
}

/// Whether anything has reviewed or commented on PR `number`: the reviewer publishes through the gateway.
async fn reviewed(env: &Env, number: u64) -> bool {
    for path in [format!("/pulls/{number}/reviews"), format!("/issues/{number}/comments")] {
        let (_, found) = github(&env.read, reqwest::Method::GET, &env.repo, &path, None).await;
        if found.as_array().is_some_and(|a| !a.is_empty()) {
            return true;
        }
    }
    false
}

/// The checks that go with one agent working in `sandbox`.
async fn probe(env: &Env, compute: &DockerCompute, sandbox: &str, role: &str) {
    println!("probing the {role} in {sandbox}");

    // An off-allowlist host is refused while the agent works.
    let status = curl(sandbox, "https://example.com", &[]).await;
    assert_ne!(status, "200", "{role}: example.com answered");
    let state = gateway_state().await;
    let refused = state.sandboxes.iter().find(|s| s.name == sandbox).map(|s| s.refused.clone()).unwrap_or_default();
    assert!(refused.keys().any(|h| h.contains("example.com")), "{role}: the gateway logged no refusal: {refused:?}");

    // No GitHub credential in the sandbox.
    let surface = secrets_surface(sandbox).await;
    for token in [&env.writer, &env.read] {
        assert!(!surface.contains(token.as_str()), "{role}: a GitHub token is in the sandbox");
    }

    // GitHub answers through the gateway, with the gateway's credential in place of the junk the sandbox sends.
    let base = format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}/v1/gateway/github/{GATEWAY_GITHUB_ID}");
    let junk = ["Authorization: Bearer junk"];
    let own = format!("{base}/repos/{}/pulls?per_page=1", env.repo);
    assert_eq!(curl(sandbox, &own, &junk).await, "200", "{role}: this repo's pulls");
    let other = format!("{base}/repos/{}-other/pulls", env.repo);
    assert_eq!(curl(sandbox, &other, &junk).await, "403", "{role}: another repo");

    if role == "worker" {
        // The kill switch: without its role label the sandbox gets nothing, and gets it back with the label.
        let id = SandboxId(sandbox.to_string());
        let labels = compute.list(&Labels::new()).await.unwrap().into_iter().find(|s| s.id == id).unwrap().labels;
        let mut without = labels.clone();
        without.remove("role");
        compute.set_labels(&id, &without).await.unwrap();
        let cut = curl(sandbox, &own, &junk).await;
        compute.set_labels(&id, &labels).await.unwrap();
        assert_eq!(cut, "403", "worker: GitHub after the role label was removed");
        assert_eq!(curl(sandbox, &own, &junk).await, "200", "worker: GitHub with the role label back");
    }
}

async fn cli(env: &Env, dir: &Path, url: &str, args: &[&str]) -> String {
    let out = tokio::process::Command::new(&env.cli)
        .current_dir(dir)
        .env("TRAILHEAD_NO_KEYRING", "1")
        .env("TRAILHEAD_DATA_DIR", dir.join("data"))
        .env("TRAILHEAD_CONFIG_DIR", dir.join("config"))
        .env("TRAILHEAD_URL", url)
        .env_remove("TRAILHEAD_TOKEN")
        .env_remove("TRAILHEAD_AGENT")
        .env_remove("TRAILHEAD_PROJECT")
        .args(["--as-me"])
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(out.status.success(), "trailhead {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn factory_toml(repo: &str) -> String {
    let (owner, name) = repo.split_once('/').expect("TRAILHEAD_LIVE_REPO is owner/name");
    let profile = |role: &str| {
        format!(
            "[factory.profiles.{role}]\nrole = \"{role}\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\n\
             model = \"sonnet\"\nbudget = {{ usd = 2, minutes = 20, steps = 40 }}\n"
        )
    };
    format!(
        "project = \"{PROJECT}\"\n\n[factory.repo]\nowner = \"{owner}\"\nname = \"{name}\"\nmax_sandboxes = 2\n\
         max_attempts = 2\n\n[factory.compute]\nkind = \"docker\"\n\n[factory.gates]\n\
         1 = {{ require = [\"checks\", \"agent_review\"], auto_merge = false }}\n\n[factory.merge]\nmode = \"off\"\n\n\
         {}\n{}",
        profile("worker"),
        profile("reviewer")
    )
}

/// Runs until the PR is reviewed and both roles were probed.
async fn run(env: &Env, dir: &Path, factory: &mut tokio::process::Child) {
    let (mut worker_probed, mut reviewer_probed) = (false, false);
    let start = Instant::now();
    let mut pr: Option<u64> = None;
    let mut compute: Option<DockerCompute> = None;
    while start.elapsed() < DEADLINE {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if let Some(status) = factory.try_wait().unwrap() {
            panic!("the factory exited early ({status}); see {}", dir.join("factory.log").display());
        }
        if !sandbox_containers().await.is_empty() {
            // Made once the factory has started the gateway, so this doesn't start it first.
            let c = compute.get_or_insert_with(|| second_control_plane(env, dir));
            for s in c.list(&Labels::new()).await.unwrap() {
                let (probed, role) = match s.labels.get("role").map(String::as_str) {
                    Some("worker") => (&mut worker_probed, "worker"),
                    Some("reviewer") => (&mut reviewer_probed, "reviewer"),
                    _ => continue,
                };
                if !*probed && agent_running(&s.id.0).await {
                    probe(env, c, &s.id.0, role).await;
                    *probed = true;
                }
            }
        }
        if pr.is_none()
            && let Some(p) = task_pr(env).await
        {
            println!("PR #{} {}", p["number"], p["title"]);
            pr = p["number"].as_u64();
        }
        if let Some(number) = pr
            && worker_probed
            && reviewer_probed
            && reviewed(env, number).await
        {
            return;
        }
    }
    panic!(
        "no finished review in {DEADLINE:?} (worker probed: {worker_probed}, reviewer probed: {reviewer_probed}, PR: {pr:?})"
    );
}

/// A `DockerCompute` over the factory's state directory and gateway, to change a live sandbox's labels the way the
/// control plane does. It carries the factory's connection, so its sync leaves the gateway's credential alone.
fn second_control_plane(env: &Env, dir: &Path) -> DockerCompute {
    let daemon = Arc::new(std::fs::read(&env.daemon).unwrap());
    let mut config = DockerConfig::new(dir.join("data/factory/docker"), daemon);
    config.connections = vec![GatewayConnection {
        provider: "github".into(),
        id: GATEWAY_GITHUB_ID.into(),
        upstream: "https://api.github.com".into(),
        auth: AuthScheme::Bearer,
        credential: Secret::new(env.writer.clone()),
    }];
    DockerCompute::new(config).unwrap()
}

/// Removes what the run left on the engine and the scratch repo, whether or not the test passed. Drops after the
/// factory process (declared first), so nothing recreates what it removes.
struct Cleanup(Env);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let env = self.0.clone();
        // Its own thread and runtime: this also runs while a failed assertion unwinds.
        let _ = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(cleanup(&env))
        })
        .join();
    }
}

async fn cleanup(env: &Env) {
    for name in sandbox_containers().await {
        docker_quiet(&["rm", "-f", &name]).await;
        docker_quiet(&["network", "rm", &format!("trailhead-sb-{name}")]).await;
    }
    docker_quiet(&["rm", "-f", GATEWAY_HOST]).await;
    if let Some(pr) = task_pr(env).await {
        let number = pr["number"].as_u64().unwrap();
        github(
            &env.writer,
            reqwest::Method::PATCH,
            &env.repo,
            &format!("/pulls/{number}"),
            Some(json!({"state": "closed"})),
        )
        .await;
        if let Some(branch) = pr["head"]["ref"].as_str() {
            github(&env.writer, reqwest::Method::DELETE, &env.repo, &format!("/git/refs/heads/{branch}"), None).await;
        }
    }
}

#[tokio::test]
#[ignore = "needs a Docker engine, a scratch repo and real credentials: `mise run test-live-docker-factory`"]
async fn a_task_runs_on_docker_through_the_gateway() {
    let env = Env {
        repo: var("TRAILHEAD_LIVE_REPO"),
        read: var("GITHUB_TOKEN"),
        writer: var("GITHUB_WRITER_TOKEN"),
        cli: var("TRAILHEAD_BIN"),
        daemon: var("TRAILHEAD_DAEMON_BIN"),
    };
    let oauth = var("CLAUDE_CODE_OAUTH_TOKEN");
    let gateway = docker(&["ps", "-aq", "--filter", "name=^trailhead-gateway$"]).await;
    assert!(
        sandbox_containers().await.is_empty() && gateway.is_empty(),
        "sandboxes or a gateway are already on this engine; stop that factory first"
    );
    let _cleanup = Cleanup(env.clone());

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}");
    std::fs::write(dir.join(".trailhead.toml"), factory_toml(&env.repo)).unwrap();
    cli(&env, dir, &url, &["project", "add", PROJECT, "Live Docker factory test"]).await;
    let description =
        "Add a file HELLO.md at the repo root containing the single line `hello from docker`. Change nothing else.";
    let key = cli(&env, dir, &url, &["add", "Add HELLO.md", "-p", PROJECT, "--grade", "1", "-d", description]).await;
    assert!(key.contains(TASK), "{key}");

    let log = std::fs::File::create(dir.join("factory.log")).unwrap();
    let mut factory = tokio::process::Command::new(env!("CARGO_BIN_EXE_trailhead-factory"))
        .args(["run", "--create-agent", "--poll-secs", "5", "--server", &format!("127.0.0.1:{port}")])
        .arg("--config")
        .arg(dir.join(".trailhead.toml"))
        .arg("--daemon")
        .arg(&env.daemon)
        .current_dir(dir)
        .env("TRAILHEAD_NO_KEYRING", "1")
        .env("TRAILHEAD_DATA_DIR", dir.join("data"))
        .env("TRAILHEAD_CONFIG_DIR", dir.join("config"))
        .env("GITHUB_TOKEN", &env.read)
        .env("GITHUB_WRITER_TOKEN", &env.writer)
        .env("CLAUDE_CODE_OAUTH_TOKEN", oauth)
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    run(&env, dir, &mut factory).await;
    println!("{}", cli(&env, dir, &url, &["show", TASK]).await);
    factory.kill().await.unwrap();
    cli(&env, dir, &url, &["server", "stop"]).await;
}
