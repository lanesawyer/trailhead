//! `ClaudeCode` against a stand-in `claude` script that speaks `stream-json` and edits the checkout.

use std::path::{Path, PathBuf};
use std::process::Command;

use tokio::sync::mpsc;
use trailhead_factory::{
    CheckResult, Endpoint, Feedback, Harness, ModelId, Role, RunContext, RunEvent, RunOutcome, TaskKey, TaskSpec,
    VerdictKind,
};
use trailhead_harness_claude_code::{ClaudeCode, PLACEHOLDER_KEY, prompt};

/// Writes its arguments and environment to `$FAKE_LOG`, then plays the scenario in `$FAKE_SCENARIO`.
const FAKE: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo "BASE=$ANTHROPIC_BASE_URL"; echo "KEY=$ANTHROPIC_API_KEY"; echo "OAUTH=$CLAUDE_CODE_OAUTH_TOKEN"; echo "FACTORY_TASK=$TRAILHEAD_FACTORY_TASK"; } > "$FAKE_LOG"
asst() { echo "{\"type\":\"assistant\",\"message\":{\"id\":\"$1\",\"model\":\"claude-sonnet-x\",\"content\":[$2],\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":1}}}"; }
echo '{"type":"system","subtype":"init","session_id":"s1"}'
case "$FAKE_SCENARIO" in
worker)
  echo '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour","resetsAt":1793182800,"unifiedWindows":{"five_hour":{"utilization":0.22,"resetsAt":1793182800},"seven_day":{"utilization":0.05,"resetsAt":1793372400}}}}'
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
flaky)
  case " $* " in
  *" --resume s1 "*)
    asst f2 '{"type":"text","text":"Picking up again."}'
    echo '{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"outcome":"needs_input","summary":"x","question":"Resumed?"}}'
    ;;
  *)
    asst f1 '{"type":"text","text":"Working."}'
    echo '{"type":"assistant","message":{"id":"e1","model":"<synthetic>","content":[{"type":"text","text":"API Error"}],"usage":{"input_tokens":0,"output_tokens":0}}}'
    echo '{"type":"result","subtype":"success","is_error":true,"result":"API Error: Connection dropped (ECONNRESET)"}'
    ;;
  esac
  ;;
esac
"#;

struct Rig {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    log: PathBuf,
    bin: PathBuf,
    harness: ClaudeCode,
}

impl Rig {
    fn harness_binary(&self) -> PathBuf {
        self.bin.clone()
    }
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
    Rig { _tmp: tmp, repo, log, bin: fake.clone(), harness: ClaudeCode::new(fake) }
}

fn ctx(r: &Rig, role: Role, max_steps: u32) -> RunContext {
    RunContext {
        role,
        task: TaskSpec {
            key: TaskKey("TRAIL-1".into()),
            title: "Add a greeting".into(),
            description: "hi()".into(),
            previous_failure: None,
            context: Default::default(),
        },
        workdir: r.repo.clone(),
        model: ModelId("claude-sonnet-x".into()),
        endpoint: Endpoint { base_url: "https://api.sprites.dev/v1/gateway/anthropic/c1".into() },
        max_steps,
        diff: (role == Role::Reviewer).then(|| "+pub fn hi() {}".into()),
        checks: Vec::new(),
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
    subscription_mode_uses_only_the_oauth_token().await;
    dropped_api_calls_resume_the_session().await;
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
    let quota: Vec<_> = events.iter().filter(|e| matches!(e, RunEvent::Quota { .. })).cloned().collect();
    assert_eq!(
        quota,
        [
            RunEvent::Quota { window: "five_hour".into(), used_percent: 22, resets_at: 1793182800 },
            RunEvent::Quota { window: "seven_day".into(), used_percent: 5, resets_at: 1793372400 },
        ],
        "both windows from Claude Code's rate_limit_event"
    );
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
            RunEvent::ToolCall { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tools, ["Bash", "Edit"]);
    let first = events.iter().find_map(|e| match e {
        RunEvent::ToolCall { detail, .. } => Some(detail.clone()),
        _ => None,
    });
    assert_eq!(first, Some(Some("ls".to_string())), "the Bash call's command is its detail");

    let log = std::fs::read_to_string(&r.log).unwrap();
    assert!(log.contains("BASE=https://api.sprites.dev/v1/gateway/anthropic/c1"), "{log}");
    assert!(log.contains(&format!("KEY={PLACEHOLDER_KEY}")), "{log}");
    assert!(log.contains("--json-schema") && log.contains("claude-sonnet-x"), "{log}");
    // A factory worker leaves tracking to the factory: no repo MCP servers, and it knows which task it's on.
    assert!(log.contains("--strict-mcp-config"), "{log}");
    assert!(log.lines().any(|l| l.starts_with("FACTORY_TASK=") && l.len() > "FACTORY_TASK=".len()), "{log}");
    assert!(log.contains("skip AGENTS.md's"), "the prompt says the factory tracks the task: {log}");
}

async fn reviewer_verdict_from_result_text() {
    let r = rig();
    let mut c = ctx(&r, Role::Reviewer, 10);
    c.checks = vec![
        CheckResult { name: "test".into(), passed: true, output: String::new() },
        CheckResult { name: "lint".into(), passed: false, output: "warning: unused import".into() },
    ];
    let (out, _) = play(&r, "review", c).await;
    assert_eq!(out, RunOutcome::Verdict { kind: VerdictKind::RequestChanges, body: "Missing a test for hi().".into() });
    let log = std::fs::read_to_string(&r.log).unwrap();
    assert!(log.contains("+pub fn hi() {}"), "the diff is in the reviewer's prompt");
    assert!(log.contains("- test: passed") && log.contains("- lint: FAILED"), "check results: {log}");
    assert!(log.contains("warning: unused import") && log.contains("don't build or run tests"), "{log}");
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

async fn subscription_mode_uses_only_the_oauth_token() {
    let r = rig();
    // A key or token already in the environment must not reach the agent in either mode.
    // SAFETY: as in `play`.
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", "sk-should-not-leak");
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "env-token");
    }
    play(&r, "question", ctx(&r, Role::Worker, 10)).await;
    let log = std::fs::read_to_string(&r.log).unwrap();
    assert!(log.contains(&format!("KEY={PLACEHOLDER_KEY}")) && log.contains("OAUTH=\n"), "connector mode: {log}");

    let subscription =
        Rig { harness: ClaudeCode::new(r.harness_binary()).with_oauth_token("sub-\ntoken\n".into()), ..r };
    play(&subscription, "question", ctx(&subscription, Role::Worker, 10)).await;
    let log = std::fs::read_to_string(&subscription.log).unwrap();
    assert!(log.contains("OAUTH=sub-token"), "{log}");
    assert!(log.contains("KEY=\n") && log.contains("BASE=\n"), "no key or gateway in subscription mode: {log}");
    // SAFETY: as in `play`.
    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
    }
}

async fn dropped_api_calls_resume_the_session() {
    let r = rig();
    let (out, events) = play(&r, "flaky", ctx(&r, Role::Worker, 10)).await;
    assert_eq!(out, RunOutcome::NeedsInput { question: "Resumed?".into() }, "the resumed session finished the run");
    let log = std::fs::read_to_string(&r.log).unwrap();
    assert!(log.contains("--resume\ns1"), "resumed the same session: {log}");
    let steps = events.iter().filter(|e| matches!(e, RunEvent::StepStarted { .. })).count();
    assert_eq!(steps, 2, "steps from both processes count, and Claude Code's own <synthetic> message doesn't");

    // An error resuming can't fix fails the attempt as before.
    let (out, _) = play(&r, "error", ctx(&r, Role::Worker, 10)).await;
    assert!(matches!(out, RunOutcome::Failed { ref reason } if reason.contains("403")), "{out:?}");
    assert!(!std::fs::read_to_string(&r.log).unwrap().contains("--resume"));
}

#[test]
fn a_reviewer_with_no_check_results_is_still_told_not_to_build() {
    let r = rig();
    let p = prompt(&ctx(&r, Role::Reviewer, 0), None);
    assert!(p.contains("don't build or run tests") && !p.contains("already ran these checks"), "{p}");
}

#[test]
fn prompt_carries_answers_and_notes_and_stays_bounded() {
    use trailhead_factory::{Answered, TaskNote};
    let r = rig();
    let plain = prompt(&ctx(&r, Role::Worker, 0), None);
    assert!(!plain.contains("Lane's answers") && !plain.contains("Decisions and notes"), "{plain}");

    let mut c = ctx(&r, Role::Worker, 0);
    c.task.context.answers.push(Answered {
        question: "Which crate?".into(),
        answer: "The core crate.".into(),
        by: "lane".into(),
        date: "2026-10-29".into(),
    });
    for i in 0..100 {
        c.task.context.notes.push(TaskNote {
            author: "lane".into(),
            date: "2026-10-29".into(),
            body: format!("note {i}: {}", "x".repeat(500)),
        });
    }
    let p = prompt(&c, None);
    let (task_at, answers_at) = (p.find("Task:\nhi()").unwrap(), p.find("Questions you asked earlier").unwrap());
    assert!(
        task_at < answers_at && p.contains("Q: Which crate?") && p.contains("A (lane, 2026-10-29): The core crate.")
    );
    assert!(p.contains("Decisions and notes on this task"));
    assert!(p.contains("note 99:") && !p.contains("note 0:"), "the oldest notes go first");
    assert!(p.len() < 9000, "{}", p.len());
}
