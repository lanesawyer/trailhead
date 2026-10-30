#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;

use common::Instance;

/// Puts a fake `trailhead-factory` that echoes its arguments and exits 3 in a directory, returned for PATH.
fn fake_factory(th: &Instance) -> std::path::PathBuf {
    let bin = th.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("trailhead-factory");
    std::fs::write(&exe, "#!/bin/sh\necho \"fake factory: $@\"\nexit 3\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[test]
fn factory_runs_trailhead_factory_from_path_with_its_args_and_exit_code() {
    let th = Instance::new();
    let bin = fake_factory(&th);
    let out = common::output(th.command().env("PATH", format!("{}:/usr/bin:/bin", bin.display())).args([
        "factory",
        "run",
        "--poll-secs",
        "5",
        "-h",
    ]));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fake factory: run --poll-secs 5 -h\n");
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn factory_not_installed_says_how_to_install_it() {
    let th = Instance::new();
    let out = common::output(th.command().env("PATH", "/nonexistent").args(["factory", "run"]));
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("trailhead-factory") && err.contains("cargo install --locked --path"), "{err}");
}
