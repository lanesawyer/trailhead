#![cfg(feature = "factory")]

mod common;

use common::Instance;

#[test]
fn factory_commands_are_built_in() {
    let th = Instance::new();
    let out = common::output(th.command().args(["factory", "allowed"]));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("no UI endpoints are allowed"));
}

#[test]
fn disable_and_enable_edit_the_user_config_with_no_server() {
    let th = Instance::new();
    let config = th.dir.path().join("config").join("config.toml");
    std::fs::create_dir_all(th.dir.path().join("config")).unwrap();
    std::fs::write(&config, "[factory]\nrepos = [\"/no/such/repo\"]\n").unwrap();

    let out = common::output(th.command().args(["factory", "disable"]));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("disabled = true") && text.contains("/no/such/repo"), "{text}");

    let again = common::output(th.command().args(["factory", "disable"]));
    assert!(String::from_utf8_lossy(&again.stdout).contains("already disabled"));

    let out = common::output(th.command().args(["factory", "enable"]));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!std::fs::read_to_string(&config).unwrap().contains("disabled"));
}

#[test]
fn factory_rejects_global_flags_given_before_it() {
    let th = Instance::new();
    let out = common::output(th.command().args(["--as", "x", "--url", "http://127.0.0.1:1", "factory", "allowed"]));
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--as") && err.contains("--url") && err.contains("after its command"), "{err}");
}
