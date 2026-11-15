mod common;

use common::Instance;

#[test]
fn edit_moves_a_task_under_a_parent_and_back() {
    let th = Instance::new();
    th.stdout(&["project", "add", "QA", "Queue"]);
    th.stdout(&["add", "queue", "-p", "QA"]);
    th.stdout(&["add", "work", "-p", "QA"]);
    th.stdout(&["edit", "QA-2", "--parent", "QA-1"]);
    assert!(th.stdout(&["ls", "-p", "QA", "--parent", "QA-1"]).contains("QA-2"));
    th.stdout(&["edit", "QA-2", "--parent", "none"]);
    assert!(!th.stdout(&["ls", "-p", "QA", "--parent", "QA-1"]).contains("QA-2"));
    assert!(!th.run(&["edit", "QA-2", "--parent", "QA-99"]).status.success());
}

#[test]
fn edit_sets_and_removes_metadata() {
    let th = Instance::new();
    th.stdout(&["project", "add", "QA", "Queue"]);
    th.stdout(&["add", "work", "-p", "QA"]);
    th.stdout(&["edit", "QA-1", "--meta", "factory.attempt=169", "--meta", "note=hi"]);
    let shown = th.stdout(&["show", "QA-1"]);
    assert!(shown.contains(r#""attempt":169"#) && shown.contains(r#""note":"hi""#), "{shown}");
    th.stdout(&["edit", "QA-1", "--meta", "factory.attempt=null"]);
    let shown = th.stdout(&["show", "QA-1"]);
    assert!(!shown.contains("attempt") && shown.contains(r#""note":"hi""#), "{shown}");
    assert!(!th.run(&["edit", "QA-1", "--meta", "nonsense"]).status.success());
    let bad = th.run(&["edit", "QA-1", "--meta", "factory.attempt_base=3x"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("factory.attempt_base"));
    th.stdout(&["edit", "QA-1", "--meta", "factory.attempt_base=3", "--meta", "factory.sandbox=trail-1-a1"]);
    th.stdout(&["edit", "QA-1", "--meta", "factory.attempt_base=null"]);
}

#[test]
fn edit_rejects_a_parent_that_would_make_a_cycle() {
    let th = Instance::new();
    th.stdout(&["project", "add", "QA", "Queue"]);
    th.stdout(&["add", "a", "-p", "QA"]);
    th.stdout(&["add", "b", "-p", "QA"]);
    th.stdout(&["edit", "QA-1", "--parent", "QA-2"]);
    assert!(!th.run(&["edit", "QA-2", "--parent", "QA-1"]).status.success());
}

#[test]
fn add_prints_possible_duplicates() {
    let th = Instance::new();
    th.stdout(&["project", "add", "QA", "Queue"]);
    let first = th.stdout(&["add", "Rotate signing keys", "-p", "QA"]);
    assert!(!first.contains("possible duplicates"), "{first}");
    let dup = th.stdout(&["add", "Rotate the signing keys", "-p", "QA"]);
    assert!(dup.trim_end().ends_with("possible duplicates: QA-1"), "{dup}");
    let other = th.stdout(&["add", "Write onboarding docs", "-p", "QA"]);
    assert!(!other.contains("possible duplicates"), "{other}");
}

#[test]
fn ls_flags_every_task_awaiting_changes_past_one_page() {
    let th = Instance::new();
    th.stdout(&["project", "add", "QA", "Queue"]);
    for n in 1..=101 {
        let key = format!("QA-{n}");
        th.stdout(&["add", "work", "-p", "QA"]);
        th.stdout(&["review", &key]);
        th.stdout(&["verdict", &key, "request-changes", "no"]);
    }
    let listed = th.stdout(&["ls", "-p", "QA", "--limit", "200"]);
    assert_eq!(listed.matches("(changes requested)").count(), 101, "{listed}");
}

#[test]
fn ls_title_finds_only_the_exact_title_ignoring_case_and_padding() {
    let th = Instance::new();
    th.stdout(&["project", "add", "QA", "Queue"]);
    th.stdout(&["add", "Fix the login", "-p", "QA"]);
    th.stdout(&["add", "Fix the login page", "-p", "QA"]);
    th.stdout(&["add", "Other", "-p", "QA"]);
    let found = th.stdout(&["ls", "-p", "QA", "--title", "  fix THE login "]);
    assert!(found.contains("QA-1") && found.contains("Fix the login"), "{found}");
    assert!(!found.contains("QA-2") && !found.contains("QA-3"), "{found}");
}
