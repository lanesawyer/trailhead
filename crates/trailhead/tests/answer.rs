mod common;

use common::Instance;

#[test]
fn answer_takes_a_task_key() {
    let th = Instance::new();
    th.stdout(&["project", "add", "QA", "Questions"]);
    th.stdout(&["add", "pick one", "-p", "QA"]);
    th.stdout(&["ask", "QA-1", "a or b?"]);
    assert!(th.stdout(&["show", "QA-1"]).contains("needs_input"));
    assert!(th.stdout(&["answer", "QA-1", "a"]).contains("resolved"));
    assert!(th.stdout(&["show", "QA-1"]).lines().nth(1).unwrap().starts_with("open"));
    let out = th.run(&["answer", "QA-1", "again"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("has no open question"));
}

#[test]
fn edit_moves_a_task_under_a_parent_and_back() {
    let th = Instance::new();
    th.stdout(&["project", "add", "QA", "Questions"]);
    th.stdout(&["add", "queue", "-p", "QA"]);
    th.stdout(&["add", "work", "-p", "QA"]);
    th.stdout(&["edit", "QA-2", "--parent", "QA-1"]);
    assert!(th.stdout(&["ls", "-p", "QA", "--parent", "QA-1"]).contains("QA-2"));
    th.stdout(&["edit", "QA-2", "--parent", "none"]);
    assert!(!th.stdout(&["ls", "-p", "QA", "--parent", "QA-1"]).contains("QA-2"));
    assert!(!th.run(&["edit", "QA-2", "--parent", "QA-99"]).status.success());
}
