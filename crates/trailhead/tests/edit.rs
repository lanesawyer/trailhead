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
