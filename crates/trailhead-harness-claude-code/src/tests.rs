use serde_json::json;

use super::*;
use crate::stream::*;
use std::os::unix::fs::PermissionsExt;
use trailhead_factory::{FileChange, FileMode, ModelId, RunEvent};

/// The windows the parser passes through must be ones `pause_at` accepts, or a limit on them could never be set.
#[test]
fn quota_windows_are_ones_the_host_config_accepts() {
    let info = json!({"unifiedWindows": {
        "five_hour": {"utilization": 0.22, "resetsAt": 1793182800},
        "seven_day": {"utilization": 0.05, "resetsAt": 1793372400}}});
    let events = quota(&info);
    assert_eq!(events.len(), 2, "{events:?}");
    for event in events {
        let RunEvent::Quota { window, .. } = event else { panic!("not a quota event: {event:?}") };
        assert!(trailhead_factory::host::USAGE_WINDOWS.contains(&window.as_str()), "{window}");
    }
}

#[test]
fn an_unknown_window_is_flagged_once_and_known_ones_never() {
    assert!(!first_unknown_window("five_hour"));
    assert!(!first_unknown_window("seven_day"));
    assert!(first_unknown_window("test_only_window"));
    assert!(!first_unknown_window("test_only_window"));
    assert!(first_unknown_window("another_test_window"));
}

#[test]
fn a_tool_result_ends_the_tool_call() {
    let mut stream = Stream::new(ModelId("sonnet".into()), 0);
    let call = json!({"type": "assistant", "message": {"id": "m1", "model": "sonnet", "usage": {},
        "content": [{"type": "tool_use", "name": "Bash", "input": {"command": "cargo test"}}]}});
    let events = stream.read(&call);
    assert!(events.iter().any(|e| matches!(e, RunEvent::ToolCall { .. })), "{events:?}");
    let result = json!({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
        {"type": "text", "text": "not a result"}]}});
    let events = stream.read(&result);
    assert!(matches!(events.as_slice(), [RunEvent::ToolResult]), "{events:?}");
}

#[test]
fn a_task_subagents_nested_calls_balance() {
    let mut stream = Stream::new(ModelId("sonnet".into()), 0);
    let call = |id: &str, tool: &str, parent: Value| {
        json!({"type": "assistant", "parent_tool_use_id": parent, "message": {"id": format!("m-{id}"),
            "model": "sonnet", "usage": {},
            "content": [{"type": "tool_use", "id": id, "name": tool, "input": {"description": "work"}}]}})
    };
    let result = |id: &str, parent: Value| {
        json!({"type": "user", "parent_tool_use_id": parent, "message": {"content": [
            {"type": "tool_result", "tool_use_id": id, "content": "ok"}]}})
    };
    let (mut calls, mut results) = (0, 0);
    for msg in [
        call("task1", "Task", Value::Null),
        call("n1", "Bash", json!("task1")),
        result("n1", json!("task1")),
        call("n2", "Read", json!("task1")),
        result("n2", json!("task1")),
        result("task1", Value::Null),
    ] {
        for e in stream.read(&msg) {
            match e {
                RunEvent::ToolCall { .. } => calls += 1,
                RunEvent::ToolResult => results += 1,
                _ => {}
            }
        }
    }
    assert_eq!((calls, results), (3, 3));
}

fn run_git(args: &[&str], at: &Path) {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(at)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

async fn started() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    run_git(&["init", "-q"], dir.path());
    run_git(&["commit", "-q", "--allow-empty", "-m", "start"], dir.path());
    let start = git_head(dir.path()).await.unwrap();
    (dir, start)
}

#[tokio::test]
async fn an_embedded_repository_is_read_as_its_files() {
    let (dir, start) = started().await;
    let skill = dir.path().join(".claude/skills/trailhead-ui");
    std::fs::create_dir_all(skill.join("refs")).unwrap();
    std::fs::write(skill.join("SKILL.md"), "a").unwrap();
    std::fs::write(skill.join("refs/b.md"), "b").unwrap();
    std::fs::write(skill.join(".gitignore"), "ignored\n").unwrap();
    std::fs::write(skill.join("ignored"), "x").unwrap();
    run_git(&["init", "-q"], &skill);
    run_git(&["add", "--all"], &skill);
    run_git(&["commit", "-q", "-m", "inner"], &skill);
    let mut paths: Vec<_> = changes_since(dir.path(), &start).await.unwrap().into_iter().map(|c| c.path).collect();
    paths.sort();
    let base = ".claude/skills/trailhead-ui";
    assert_eq!(paths, [".gitignore", "SKILL.md", "refs/b.md"].map(|p| format!("{base}/{p}")));
}

#[tokio::test]
async fn a_nested_repository_with_no_commit_is_read_as_its_files() {
    let (dir, start) = started().await;
    let inner = dir.path().join("a/inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("f"), "x").unwrap();
    run_git(&["init", "-q"], &inner);
    let paths: Vec<_> = changes_since(dir.path(), &start).await.unwrap().into_iter().map(|c| c.path).collect();
    assert_eq!(paths, ["a/inner/f"]);
    assert!(!inner.join(".git").exists());
}

#[tokio::test]
async fn a_file_deleted_from_an_embedded_repository_is_not_published() {
    let (dir, start) = started().await;
    let inner = dir.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("keep"), "a").unwrap();
    std::fs::write(inner.join("gone"), "b").unwrap();
    run_git(&["init", "-q"], &inner);
    run_git(&["add", "--all"], &inner);
    run_git(&["commit", "-q", "-m", "inner"], &inner);
    std::fs::remove_file(inner.join("gone")).unwrap();
    let paths: Vec<_> = changes_since(dir.path(), &start).await.unwrap().into_iter().map(|c| c.path).collect();
    assert_eq!(paths, ["inner/keep"]);
}

#[tokio::test]
async fn a_tracked_directory_that_becomes_an_embedded_repository_lists_each_path_once() {
    let dir = tempfile::tempdir().unwrap();
    run_git(&["init", "-q"], dir.path());
    let inner = dir.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("x"), "a").unwrap();
    std::fs::write(inner.join("y"), "b").unwrap();
    run_git(&["add", "--all"], dir.path());
    run_git(&["commit", "-q", "-m", "start"], dir.path());
    let start = git_head(dir.path()).await.unwrap();
    run_git(&["init", "-q"], &inner);
    std::fs::write(inner.join("x"), "changed").unwrap();
    run_git(&["add", "--all"], &inner);
    run_git(&["commit", "-q", "-m", "inner"], &inner);
    run_git(&["rm", "-r", "-q", "--cached", "inner"], dir.path());
    let mut paths: Vec<_> = changes_since(dir.path(), &start).await.unwrap().into_iter().map(|c| c.path).collect();
    paths.sort();
    assert_eq!(paths, ["inner/x", "inner/y"].map(String::from));
}

fn only(changes: Vec<FileChange>) -> FileChange {
    let [change] = <[FileChange; 1]>::try_from(changes).unwrap_or_else(|c| panic!("{c:?}"));
    change
}

#[tokio::test]
async fn an_executable_file_is_published_as_executable() {
    let (dir, start) = started().await;
    std::fs::write(dir.path().join("run.sh"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(dir.path().join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.path().join("notes.txt"), "n").unwrap();
    let mut changes = changes_since(dir.path(), &start).await.unwrap();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let modes: Vec<_> = changes.iter().map(|c| (c.path.as_str(), c.mode)).collect();
    assert_eq!(modes, [("notes.txt", FileMode::Regular), ("run.sh", FileMode::Executable)]);
}

#[tokio::test]
async fn making_a_file_executable_is_a_change() {
    let (dir, _) = started().await;
    std::fs::write(dir.path().join("run.sh"), "#!/bin/sh\n").unwrap();
    run_git(&["add", "run.sh"], dir.path());
    run_git(&["commit", "-q", "-m", "script"], dir.path());
    let start = git_head(dir.path()).await.unwrap();
    std::fs::set_permissions(dir.path().join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let change = only(changes_since(dir.path(), &start).await.unwrap());
    assert_eq!(
        change,
        FileChange { path: "run.sh".into(), contents: Some(b"#!/bin/sh\n".to_vec()), mode: FileMode::Executable }
    );
}

#[tokio::test]
async fn a_symlink_to_a_file_is_published_as_the_link_not_its_target() {
    let (dir, _) = started().await;
    std::fs::write(dir.path().join("real.txt"), "contents").unwrap();
    run_git(&["add", "real.txt"], dir.path());
    run_git(&["commit", "-q", "-m", "real"], dir.path());
    let start = git_head(dir.path()).await.unwrap();
    std::os::unix::fs::symlink("real.txt", dir.path().join("link")).unwrap();
    let change = only(changes_since(dir.path(), &start).await.unwrap());
    assert_eq!(
        change,
        FileChange { path: "link".into(), contents: Some(b"real.txt".to_vec()), mode: FileMode::Symlink }
    );
}

#[tokio::test]
async fn a_symlink_out_of_the_worktree_is_published_as_a_link_not_a_copy() {
    let (dir, start) = started().await;
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret");
    std::fs::write(&secret, "s3cret").unwrap();
    std::os::unix::fs::symlink(&secret, dir.path().join("file-link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("dir-link")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("nothing"), dir.path().join("dangling")).unwrap();
    let mut changes = changes_since(dir.path(), &start).await.unwrap();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let target = |p: &Path| Some(p.as_os_str().as_encoded_bytes().to_vec());
    let link = |path: &str, to: &Path| FileChange { path: path.into(), contents: target(to), mode: FileMode::Symlink };
    assert_eq!(
        changes,
        [
            link("dangling", &outside.path().join("nothing")),
            link("dir-link", outside.path()),
            link("file-link", &secret),
        ]
    );
}
