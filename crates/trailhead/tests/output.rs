mod common;

use common::Instance;

#[test]
fn a_closed_stdout_exits_quietly_instead_of_panicking() {
    let th = Instance::new();
    th.stdout(&["project", "add", "PIPE", "Pipe"]);
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = th.command().args(["whoami"]).stdout(writer).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(out.status.code(), Some(141), "{stderr}");
}
