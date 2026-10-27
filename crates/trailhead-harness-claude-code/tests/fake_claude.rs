//! `ClaudeCode` against a stand-in `claude` script that speaks `stream-json` and edits the checkout.

use std::path::{Path, PathBuf};
use std::process::Command;

use tokio::sync::mpsc;
use trailhead_factory::{
    Endpoint, Feedback, Harness, ModelId, Role, RunContext, RunEvent, RunOutcome, TaskKey, TaskSpec, VerdictKind,
};
use trailhead_harness_claude_code::{ClaudeCode, PLACEHOLDER_KEY};

/// Writes its arguments and environment to `$FAKE_LOG`, then plays the scenario in `$FAKE_SCENARIO`.
const FAKE: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo "BASE=$ANTHROPIC_BASE_URL"; echo "KEY=$ANTHROPIC_API_KEY"; } > "$FAKE_LOG"
asst() { echo "{\"type\":\"assistant\",\"message\":{\"id\":\"$1\",\"model\":\"claude-sonnet-x\",\"content\":[$2],\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":1}}}"; }
echo '{"type":"system","subtype":"init","session_id":"s1"}'
case "$FAKE_SCENARIO" in
worker)
  asst m1 '{"type":"text","text":"Looking."}'
  asst m1 '{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}'
  echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"README.md"}]}}'
  mkdir -p src && echo 'pub fn hi() {}' > src/greet.rs
  echo 'v2' > README.md
  rm old.txt
  asst m2 '{"type":"tool_use","id":"t2","name":"Edit","input":{}}'
  echo '{"type":"result","subtype":"success","is_error":false,"result":"done","structured_output":{"outcome":"done","summary":"Adds hi()."}}'
  ;;
review)
  asst r1 '{"type":"text","text":"Reviewing."}'
  echo '{"type":"result","subtype":"success","is_error":false,"result":"{\"outcome\":\"request_changes\",\"summary\":\"Missing a test for hi().\"}"}'
  ;;
question)
  asst q1 '{"type":"text","text":"Hmm."}'
  echo '{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"outcome":"needs_input","summary":"Ambiguous.","question":"Which greeting?"}}'
  ;;
nothing)
  asst n1 '{"type":"text","text":"Done."}'
  echo '{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"outcome":"done","summary":"Nothing to do."}}'
  ;;
loop)
  for i in 1 2 3 4 5 6; do asst "l$i" '{"type":"tool_use","id":"x","name":"Bash","input":{}}'; done
  sleep 30
  ;;
error)
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"API Error: 403 forbidden"}'
  ;;
esac
"#;

struct Rig {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    log: PathBuf,
    harness: ClaudeCode,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("claude");
    std::fs::write(&fake, FAKE).unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("README.md"), "v1\n").unwrap();
    std::fs::write(repo.join("old.txt"), "bye\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "init"]);

    let log = tmp.path().join("log");
    Rig { _tmp: tmp, repo, log, harness: ClaudeCode::new(fake) }
}

fn ctx(r: &Rig, role: Role, max_steps: u32) -> RunContext {
    RunContext {
        role,
        task: TaskSpec { key: TaskKey("TRAIL-1".into()), title: "Add a greeting".into(), description: "hi()".into() },
        workdir: r.repo.clone(),
        model: ModelId("claude-sonnet-x".into()),
        endpoint: Endpoint { base_url: "https://api.sprites.dev/v1/gateway/anthropic/c1".into() },
        max_steps,
        diff: (role == Role::Reviewer).then(|| "+pub fn hi() {}".into()),
    }
}

async fn play(r: &Rig, scenario: &str, ctx: RunContext) -> (RunOutcome, Vec<RunEvent>) {
    // SAFETY: the scenarios run one at a time in a single test, so nothing reads these concurrently.
    unsafe {
        std::env::set_var("FAKE_SCENARIO", scenario);
        std::env::set_var("FAKE_LOG", &r.log);
    }
    let (tx, mut rx) = mpsc::unbounded_channel();
    let out = r.harness.run(ctx, tx).await.unwrap();
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    (out, events)
}

// The scenarios share process-wide environment variables, so they run in one test, in sequence.
#[tokio::test]
async fn claude_code_sessions() {
    worker_changes_are_collected().await;
    reviewer_verdict_from_result_text().await;
    questions_park_the_task().await;
    no_changes_is_a_failure().await;
    step_limit_kills_the_agent().await;
    cli_errors_fail_the_run().await;
    resume_carries_feedback().await;
}

async fn worker_changes_are_collected() {
    let r = rig();
    let (out, events) = play(&r, "worker", ctx(&r, Role::Worker, 10)).await;

    let RunOutcome::Changes { commits, summary } = out else { panic!("{out:?}") };
    assert_eq!(summary, "Adds hi().");
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].message, "TRAIL-1: Add a greeting");
    let mut changes: Vec<_> =
        commits[0].changes.iter().map(|c| (c.path.as_str(), c.contents.as_deref().map(|b| b.to_vec()))).collect();
    changes.sort();
    assert_eq!(
        changes,
        [
            ("README.md", Some(b"v2\n".to_vec())),
            ("old.txt", None),
            ("src/greet.rs", Some(b"pub fn hi() {}\n".to_vec())),
        ]
    );

    let steps = events.iter().filter(|e| matches!(e, RunEvent::StepStarted { .. })).count();
    assert_eq!(steps, 2, "two model responses, though m1 arrived as two messages");
    let usage: u64 = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Usage { usage, .. } => Some(usage.input_tokens),
            _ => None,
        })
        .sum();
    assert_eq!(usage, 200, "usage counted once per response");
    let tools: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::ToolCall { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tools, ["Bash", "Edit"]);

    let log = std::fs::read_to_string(&r.log).unwrap();
    assert!(log.contains("BASE=https://api.sprites.dev/v1/gateway/anthropic/c1"), "{log}");
    assert!(log.contains(&format!("KEY={PLACEHOLDER_KEY}")), "{log}");
    assert!(log.contains("--json-schema") && log.contains("claude-sonnet-x"), "{log}");
}

async fn reviewer_verdict_from_result_text() {
    let r = rig();
    let (out, _) = play(&r, "review", ctx(&r, Role::Reviewer, 10)).await;
    assert_eq!(out, RunOutcome::Verdict { kind: VerdictKind::RequestChanges, body: "Missing a test for hi().".into() });
    let log = std::fs::read_to_string(&r.log).unwrap();
    assert!(log.contains("+pub fn hi() {}"), "the diff is in the reviewer's prompt");
    assert!(log.contains("request_changes"), "the reviewer's schema offers verdicts");
}

async fn questions_park_the_task() {
    let r = rig();
    let (out, _) = play(&r, "question", ctx(&r, Role::Worker, 10)).await;
    assert_eq!(out, RunOutcome::NeedsInput { question: "Which greeting?".into() });
}

async fn no_changes_is_a_failure() {
    let r = rig();
    let (out, _) = play(&r, "nothing", ctx(&r, Role::Worker, 10)).await;
    assert!(matches!(out, RunOutcome::Failed { ref reason } if reason.contains("changed nothing")), "{out:?}");
}

async fn step_limit_kills_the_agent() {
    let r = rig();
    let started = std::time::Instant::now();
    let (out, _) = play(&r, "loop", ctx(&r, Role::Worker, 3)).await;
    assert!(matches!(out, RunOutcome::Failed { ref reason } if reason.contains("step limit of 3")), "{out:?}");
    assert!(started.elapsed().as_secs() < 10, "killed rather than waited out");
}

async fn cli_errors_fail_the_run() {
    let r = rig();
    let (out, _) = play(&r, "error", ctx(&r, Role::Worker, 10)).await;
    assert!(matches!(out, RunOutcome::Failed { ref reason } if reason.contains("403")), "{out:?}");
}

async fn resume_carries_feedback() {
    let r = rig();
    // SAFETY: as in `play`.
    unsafe {
        std::env::set_var("FAKE_SCENARIO", "question");
        std::env::set_var("FAKE_LOG", &r.log);
    }
    let (tx, _rx) = mpsc::unbounded_channel();
    let feedback = Feedback { body: "Use a doc comment.".into() };
    r.harness.resume(ctx(&r, Role::Worker, 10), feedback, tx).await.unwrap();
    let log = std::fs::read_to_string(&r.log).unwrap();
    assert!(log.contains("Use a doc comment."), "{log}");
}
