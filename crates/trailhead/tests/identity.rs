mod common;

use common::Instance;

fn whoami(th: &Instance, cwd: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = th.command().current_dir(cwd).args(args).arg("whoami").envs(env.iter().copied()).output().unwrap();
    assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn repo_file_picks_agent_and_project_and_flags_override_it() {
    let th = Instance::new();
    th.stdout(&["project", "add", "REPO", "Repo"]);
    let bot_file = std::path::Path::new("config").join("agents").join("bot");
    assert!(th.stdout(&["agent", "add", "bot"]).contains(&bot_file.display().to_string()));
    th.stdout(&["agent", "add", "other"]);

    let repo = th.dir.path().join("repo");
    let nested = repo.join("src/deep");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(repo.join(".trailhead.toml"), "agent = \"bot\"\nproject = \"REPO\"\n").unwrap();

    // Picked up from a nested working directory.
    assert!(whoami(&th, &nested, &[], &[]).starts_with("agent bot acting for"));
    let out = th.command().current_dir(&nested).args(["add", "from repo"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("REPO-1 "));

    // --as and TRAILHEAD_AGENT beat the file.
    assert!(whoami(&th, &nested, &["--as", "other"], &[]).starts_with("agent other acting for"));
    assert!(whoami(&th, &nested, &[], &[("TRAILHEAD_AGENT", "other")]).starts_with("agent other acting for"));

    // --as-me acts as the human despite the file and the env var.
    let me = whoami(&th, th.dir.path(), &[], &[]);
    assert!(!me.starts_with("agent"));
    assert_eq!(whoami(&th, &nested, &["--as-me"], &[]), me);
    assert_eq!(whoami(&th, &nested, &["--as-me"], &[("TRAILHEAD_AGENT", "other")]), me);

    // A token beats everything.
    let token = std::fs::read_to_string(th.dir.path().join("config/credentials")).unwrap();
    assert_eq!(whoami(&th, &nested, &["--token", token.trim()], &[]), me);

    let out = th.command().current_dir(&nested).args(["--as", "bot", "--as-me", "whoami"]).output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn repo_agent_without_a_saved_token_names_the_file() {
    let th = Instance::new();
    th.stdout(&["whoami"]);
    std::fs::write(th.dir.path().join(".trailhead.toml"), "agent = \"ghost\"\n").unwrap();
    let out = th.run(&["whoami"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(".trailhead.toml sets agent = \"ghost\""), "{err}");
    assert!(err.contains("--as-me"), "{err}");
}

#[test]
fn token_add_agent_issues_a_token_for_the_agent_instead_of_acting_as_it() {
    let th = Instance::new();
    let bot: serde_json::Value = serde_json::from_str(&th.stdout(&["agent", "add", "bot", "--json"])).unwrap();
    let out = th.run(&["token", "add", "t", "--agent", "bot", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // The token is the agent's, not the human's.
    let created: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(created["token"]["principal"], serde_json::json!({"kind": "agent", "id": bot["agent"]["id"]}));
    // The caller stayed the human, so a bad agent is the server's complaint, not a missing saved token.
    let out = th.run(&["token", "add", "t2", "--agent", "nobody"]);
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("no saved token"));
}
