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

#[test]
fn a_picture_is_attached_listed_and_counted() {
    let th = Instance::new();
    th.stdout(&["project", "add", "PIC", "Pictures"]);
    th.stdout(&["add", "Draw it", "-p", "PIC"]);
    let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13];
    png.extend(b"IHDR");
    png.extend([0, 0, 0, 40, 0, 0, 0, 30, 8, 6, 0, 0, 0, 0, 0, 0, 0, 0]);
    let file = th.dir.path().join("board-dark.png");
    std::fs::write(&file, png).unwrap();
    let file = file.to_str().unwrap();

    let added = th.stdout(&["attach", "PIC-1", file, "--role", "after", "--set", "run-1"]);
    assert!(added.contains("  board-dark  after  run-1  40x30  34 B"), "{added}");
    th.stdout(&["attach", "PIC-1", file, "--name", "board-light"]);
    let list = th.stdout(&["attachments", "PIC-1"]);
    assert_eq!(list.lines().count(), 2, "{list}");
    assert!(list.contains("  board-light  40x30"), "{list}");
    assert!(th.stdout(&["show", "PIC-1"]).contains("\n2 pictures"));

    std::fs::write(file, b"not a png at all, but long enough to be read as one").unwrap();
    let out = th.run(&["attach", "PIC-1", file]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("isn't a PNG"));
}
