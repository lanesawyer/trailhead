use crate::check::check_result;

#[tokio::test(start_paused = true)]
async fn a_step_that_fails_is_tried_again_and_the_feed_says_why_meanwhile() {
    use trailhead_factory::FactoryState;
    let feed = super::surface::Feed::new(FactoryState::Starting { reason: None });
    let (_drain_tx, drain) = tokio::sync::watch::channel(false);
    let tries = std::sync::atomic::AtomicU32::new(0);
    let watching = feed.clone();
    let ready = super::until_ready(&feed, &drain, || {
        let n = tries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let seen = watching.state();
        async move {
            match n {
                0 => Err(super::Error::context("docker", "Socket not found")),
                _ => Ok(seen),
            }
        }
    })
    .await;
    let Some(FactoryState::Starting { reason: Some(reason) }) = ready else { panic!("{ready:?}") };
    assert!(reason.contains("Socket not found"), "{reason}");
}

#[tokio::test(start_paused = true)]
async fn a_step_still_failing_when_the_server_drains_gives_up_and_the_feed_says_stopped() {
    use trailhead_factory::FactoryState;
    let feed = super::surface::Feed::new(FactoryState::Starting { reason: None });
    let (drain_tx, drain) = tokio::sync::watch::channel(false);
    let waiting = super::until_ready(&feed, &drain, || async { Err::<(), _>(super::Error::context("docker", "down")) });
    let stop = async {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        drain_tx.send_replace(true);
    };
    let (ready, ()) = tokio::join!(waiting, stop);
    assert_eq!(ready, None);
    assert_eq!(feed.state(), FactoryState::Stopped);
}

/// The smallest config file that parses: a repo and a worker and a reviewer profile.
const BASE_CONFIG: &str = "[repo]\nowner = \"o\"\nname = \"n\"\n[profiles.worker]\nrole = \"worker\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\nmodel = \"sonnet\"\nbudget = { usd = 5, minutes = 60, steps = 40 }\n[profiles.reviewer]\nrole = \"reviewer\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\nmodel = \"opus\"\nbudget = { usd = 2, minutes = 20, steps = 20 }\n";

/// `BASE_CONFIG` with `extra` (more sections) put in front of it.
fn parse_with(extra: &str) -> trailhead_factory::FactoryConfig {
    trailhead_factory::FactoryConfig::parse_table(&format!("{extra}\n{BASE_CONFIG}"), "TRAIL", None).unwrap()
}

#[test]
fn a_changed_setting_other_than_merge_mode_warns_once() {
    let base = parse_with("");
    let started = super::started_baseline(&base);
    let mut warned = None;
    // A mode change is applied live, so it isn't a reason to restart.
    assert!(super::changed_without_restart(&started, parse_with("[merge]\nmode = \"on\""), &mut warned).is_empty());
    let edited = parse_with("[checks]\nfmt = \"cargo fmt\"");
    assert_eq!(super::changed_without_restart(&started, edited.clone(), &mut warned), ["checks"]);
    assert!(super::changed_without_restart(&started, edited, &mut warned).is_empty());
    assert!(super::changed_without_restart(&started, base, &mut warned).is_empty());
}

#[test]
fn several_changed_sections_are_listed_in_order() {
    let started = super::started_baseline(&parse_with(""));
    let mut warned = None;
    // Listed in the order the sections are compared, not the order the file has them.
    let edited = parse_with("[egress]\nalways = [\"example.com\"]\n[checks]\nfmt = \"cargo fmt\"");
    assert_eq!(super::changed_without_restart(&started, edited.clone(), &mut warned), ["checks", "egress"]);
    assert!(super::changed_without_restart(&started, edited, &mut warned).is_empty());
}

#[test]
fn a_follow_up_edit_adding_a_section_warns_again_with_both() {
    let started = super::started_baseline(&parse_with(""));
    let mut warned = None;
    let first = parse_with("[checks]\nfmt = \"cargo fmt\"");
    assert_eq!(super::changed_without_restart(&started, first.clone(), &mut warned), ["checks"]);
    assert!(super::changed_without_restart(&started, first, &mut warned).is_empty());
    let second = parse_with("[checks]\nfmt = \"cargo fmt\"\n[egress]\nalways = [\"example.com\"]");
    assert_eq!(super::changed_without_restart(&started, second.clone(), &mut warned), ["checks", "egress"]);
    assert!(super::changed_without_restart(&started, second, &mut warned).is_empty());
}

#[test]
fn egress_added_at_runtime_is_not_a_change_to_the_file() {
    let mut config = parse_with("");
    let started = super::started_baseline(&config);
    // What `wire` does in subscription mode, after the baseline is taken.
    config.egress.always.push("api.anthropic.com".into());
    let reread = parse_with("");
    assert!(super::changed_without_restart(&started, reread, &mut None).is_empty());
    assert_ne!(started.egress, config.egress);
}

#[tokio::test]
async fn a_panicked_loop_is_marked_stopped_on_its_board() {
    use trailhead_factory::ControlState;
    let (up, other) = (trailhead_control::StatusBoard::new(), trailhead_control::StatusBoard::new());
    up.control(ControlState::Up);
    other.control(ControlState::Up);
    let mut loops = tokio::task::JoinSet::new();
    let mut boards = std::collections::HashMap::new();
    let id = loops.spawn(async { panic!("boom") }).id();
    boards.insert(id, ("o/n".to_string(), up.clone()));
    let id = loops.spawn(std::future::pending::<()>()).id();
    boards.insert(id, ("o/m".to_string(), other.clone()));
    let e = loops.join_next().await.unwrap().unwrap_err();
    let feed = super::surface::Feed::new(trailhead_factory::FactoryState::Running);
    feed.set_boards(vec![("o/n".to_string(), up.clone()), ("o/m".to_string(), other.clone())]);
    super::loop_panicked(&e, &feed, &boards);
    let reply = feed.reply();
    assert!(reply.repos[0].stopped.as_ref().is_some_and(|m| m.contains("panic")));
    assert_eq!(reply.repos[1].stopped, None);
    assert_eq!(up.snapshot().control, ControlState::Stopped);
    let (_, message) = up.snapshot().last_error.unwrap();
    assert!(message.starts_with("loop panicked: "), "{message}");
    assert!(other.snapshot().last_error.is_none());
    assert_eq!(other.snapshot().control, ControlState::Up);
}

#[test]
fn a_loop_that_returned_an_error_is_marked_stopped_on_its_board_and_in_the_feed() {
    use trailhead_factory::ControlState;
    let (down, other) = (trailhead_control::StatusBoard::new(), trailhead_control::StatusBoard::new());
    down.control(ControlState::Up);
    other.control(ControlState::Up);
    let feed = super::surface::Feed::new(trailhead_factory::FactoryState::Running);
    feed.set_boards(vec![("o/n".to_string(), down.clone()), ("o/m".to_string(), other.clone())]);
    super::loop_stopped("o/n", &down, &feed, "the store is gone");
    assert_eq!(down.snapshot().control, ControlState::Stopped);
    let reply = feed.reply();
    assert_eq!(reply.repos[0].stopped.as_deref(), Some("the store is gone"));
    assert_eq!(reply.repos[1].stopped, None);
    assert_eq!(other.snapshot().control, ControlState::Up);
}

/// Paths under `dir`, and a store in it.
async fn instance(
    dir: &std::path::Path,
    user_config: &str,
) -> (trailhead_client::paths::Paths, std::sync::Arc<trailhead_core::Store>) {
    let paths = trailhead_client::paths::Paths { data_dir: dir.join("data"), config_dir: dir.join("config") };
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_dir.join("config.toml"), user_config).unwrap();
    let store = trailhead_core::Store::open_local(&dir.join("t.db")).await.unwrap();
    (paths, std::sync::Arc::new(store))
}

#[tokio::test]
async fn no_repos_listed_runs_no_factory() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, store) = instance(dir.path(), "url = \"http://127.0.0.1:7241\"\n").await;
    let started = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).unwrap();
    assert!(started.running.is_none());
    assert_eq!(started.feed.state(), trailhead_factory::FactoryState::Off);
}

#[tokio::test]
async fn disabled_runs_nothing_even_with_a_broken_repo() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, store) = instance(dir.path(), "[factory]\nrepos = [\"/no/such/repo\"]\ndisabled = true\n").await;
    assert!(!super::configured(&paths));
    let started = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).unwrap();
    assert!(started.running.is_none());
    assert_eq!(started.feed.state(), trailhead_factory::FactoryState::Disabled);
}

/// Why a started control plane says it is still starting, once it says so; then stops it.
async fn why_starting(started: super::InServer) -> String {
    let running = started.running.expect("the server starts with the control plane trying");
    let said = async {
        loop {
            if let trailhead_factory::FactoryState::Starting { reason: Some(why) } = started.feed.state() {
                return why;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    let why = tokio::time::timeout(std::time::Duration::from_secs(10), said).await.expect("a reason");
    running.stop(std::time::Duration::from_secs(5)).await;
    why
}

#[tokio::test]
async fn a_repo_that_cant_be_read_leaves_the_server_up_and_the_control_plane_saying_why() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-repo");
    let config = format!("[factory]\nrepos = [{:?}]\n", missing.display().to_string());
    let (paths, store) = instance(dir.path(), &config).await;
    let started = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).unwrap();
    let why = why_starting(started).await;
    assert!(why.contains("no-such-repo"), "{why}");

    let (paths, store) = instance(dir.path(), "[factory]\nrepos = 3\n").await;
    let err = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).err().unwrap().to_string();
    assert!(err.contains("config.toml"), "only the user config itself refuses the server: {err}");
}

#[tokio::test]
async fn a_repo_file_with_a_key_this_build_doesnt_know_leaves_the_server_up() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join(".trailhead.toml"), "project = \"P\"\n[factory.from_a_newer_trailhead]\nx = 1\n").unwrap();
    let config = format!("[factory]\nrepos = [{:?}]\n", repo.display().to_string());
    let (paths, store) = instance(dir.path(), &config).await;
    let started = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).unwrap();
    let why = why_starting(started).await;
    assert!(why.contains("from_a_newer_trailhead") && why.contains(".trailhead.toml"), "{why}");
}

#[tokio::test]
async fn checking_a_user_config_refuses_what_keeps_the_control_plane_from_starting_in_its_words() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-repo");
    let config = format!("[factory]\nrepos = [{:?}]\n", missing.display().to_string());
    let (paths, store) = instance(dir.path(), &config).await;
    let user_config = paths.config_dir.join("config.toml");
    let checked = super::check_user_config(&user_config, &config).err().unwrap().to_string();
    let started = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).unwrap();
    assert_eq!(checked, why_starting(started).await);
    let bad = "[factory]\npause_at = { five_hour = 101 }\n";
    let err = super::check_user_config(&user_config, bad).err().unwrap().to_string();
    assert!(err.starts_with(&user_config.display().to_string()) && err.contains("0 to 100"), "{err}");
    let off = format!("{config}disabled = true\n");
    assert!(super::check_user_config(&user_config, &off).unwrap().disabled, "a disabled factory reads no repo");
}

#[cfg(not(feature = "docker"))]
#[tokio::test]
async fn docker_compute_without_the_feature_is_a_configuration_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let profile = |role: &str| {
        format!(
            "[factory.profiles.{role}]\nrole = \"{role}\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\n\
             model = \"sonnet\"\nbudget = {{ usd = 1, minutes = 1, steps = 1 }}\n"
        )
    };
    let file = format!(
        "project = \"P\"\n[factory.repo]\nowner = \"o\"\nname = \"r\"\n[factory.compute]\nkind = \"docker\"\n\
         [factory.checks]\nfmt = \"f\"\nlint = \"l\"\ntest = \"t\"\n{}{}",
        profile("worker"),
        profile("reviewer")
    );
    std::fs::write(repo.join(".trailhead.toml"), file).unwrap();
    let config = format!("[factory]\nrepos = [{:?}]\n", repo.display().to_string());
    let (paths, store) = instance(dir.path(), &config).await;
    let started = super::start(paths, store, "127.0.0.1:7241".parse().unwrap()).unwrap();
    let why = why_starting(started).await;
    assert!(why.contains("`docker` feature"), "{why}");
}

#[test]
fn a_servers_event_inside_a_repo_span_goes_to_the_factory_log_only() {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::writer::BoxMakeWriter;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Capture {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    let (server, factory) = (Capture::default(), Capture::default());
    let (s, f) = (server.clone(), factory.clone());
    let subscriber = crate::logging::server_subscriber(
        "info".into(),
        &trailhead_control::logs::LogBuffer::default(),
        BoxMakeWriter::new(move || s.clone()),
        false,
        Some(BoxMakeWriter::new(move || f.clone())),
        false,
    );
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "trailhead_core", "outside any repo");
        let span = tracing::info_span!(target: "trailhead_factory_server", "repo", repo = "a/b");
        let _in = span.enter();
        tracing::info!(target: "trailhead_core", "inside a repo");
        tracing::info!(target: "trailhead_control", "the factory's own");
    });
    let (server, factory) = (server.text(), factory.text());
    assert!(server.contains("outside any repo"), "{server}");
    assert!(!server.contains("inside a repo") && !server.contains("the factory's own"), "{server}");
    assert!(factory.contains("inside a repo") && factory.contains("the factory's own"), "{factory}");
    assert!(!factory.contains("outside any repo"), "{factory}");
}

#[test]
fn check_result_counts_failures() {
    check_result(0).unwrap();
    assert_eq!(check_result(1).unwrap_err().to_string(), "1 check failed");
    assert_eq!(check_result(2).unwrap_err().to_string(), "2 checks failed");
    assert!(matches!(check_result(3), Err(crate::Error::ChecksFailed(3))));
}
