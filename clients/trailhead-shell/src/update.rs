//! The update notice. A shell the launcher started (`trailhead app`, which names the build in
//! [`BUILD_ENV`](trailhead_model::shell::BUILD_ENV)) asks the server which build is newest every [`POLL`] while the
//! connection is live, and when that isn't its own build it offers to restart onto it: [`UpdateReady`] says so, the
//! rail shows it, and [`Restart`] ends the shell with [`RESTART_CODE`], which the launcher answers by starting the
//! newest build. Each ask names this build, so the answer says what the newer one brings: the commits since this
//! one, each parsed by [`parse_change`] into the [`Change`] the notice's panel shows.
//!
//! It polls `GET /v1/shell/build` rather than hearing of a build on the event stream: that stream carries the task
//! engine's events, which are stored and undoable, and a build is neither.

use std::time::Duration;

use bevy::prelude::*;
use trailhead_model::shell::{LOCAL_STATE_ENV, RESTART_CODE, ShellBuild};

use crate::LocalStore;
use crate::connection::{Connection, SendNet};
use crate::net::{Fetch, NetCommand};

/// How often a live shell asks for the newest build.
pub const POLL: Duration = Duration::from_secs(30);

/// The build this shell was started from: the commit the launcher named.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct ThisBuild(pub String);

/// The state file of a builder on this machine, when the launcher named one
/// ([`LOCAL_STATE_ENV`]): the launcher builds the server's builds here, and starts only what it has built. The
/// newest build is then read from this file, and the server is asked only what it brings.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct BuiltHere(pub Option<std::path::PathBuf>);

impl BuiltHere {
    /// The build this machine built last, from its builder's state file.
    fn newest(file: &std::path::Path) -> Option<String> {
        let state: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
        state["current"]["rev"].as_str().map(String::from)
    }
}

/// The newer build the launcher would start, once the server has said there is one; `None` while this shell is the
/// newest.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct UpdateReady(pub Option<Newer>);

/// A newer build than this one, and what it brings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Newer {
    pub rev: String,
    /// The commits since this build, newest first: at most
    /// [`MAX_CHANGES`](trailhead_model::shell::MAX_CHANGES) of them. Empty when the server didn't say (one older than
    /// the list, or one whose builder doesn't have this build).
    pub changes: Vec<Change>,
    /// How many commits there are since this build in all, more than `changes` holds when the list was capped.
    pub total: usize,
}

impl Newer {
    pub fn new(rev: impl Into<String>, subjects: &[String], total: usize) -> Newer {
        let changes: Vec<Change> = subjects.iter().map(|s| parse_change(s)).collect();
        Newer { rev: rev.into(), total: total.max(changes.len()), changes }
    }

    /// The line over the changes: how many there are, and how many of them are listed when not all.
    pub fn summary(&self) -> String {
        let (listed, total) = (self.changes.len(), self.total);
        match total {
            0 => "A newer build is ready.".into(),
            1 => "1 change since the build you're running".into(),
            _ if listed < total => format!("The newest {listed} of {total} changes since the build you're running"),
            _ => format!("{total} changes since the build you're running"),
        }
    }

    /// The hint on the rail's update button.
    pub fn hint(&self) -> String {
        match self.total {
            0 => "Update ready".into(),
            1 => "Update ready: 1 change".into(),
            n => format!("Update ready: {n} changes"),
        }
    }
}

/// One commit since this build, as the notice's panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// A conventional subject's `type(scope)`, with its `!` when it breaks something: `feat(ticklist)`. `None` for a
    /// subject that isn't conventional, which is shown whole as the message.
    pub kind: Option<String>,
    pub message: String,
    /// The task key the subject ends with, without its brackets: `TICK-5`.
    pub key: Option<String>,
}

/// A commit's subject as a [`Change`]: `type(scope): message [KEY-n]`, with GitHub's ` (#n)` after it dropped. A
/// subject that doesn't start with a conventional `type(scope):` is the message whole.
pub fn parse_change(subject: &str) -> Change {
    let subject = subject.trim();
    let conventional = subject.split_once(": ").filter(|(head, _)| is_conventional(head));
    let Some((kind, rest)) = conventional else {
        return Change { kind: None, message: subject.to_string(), key: None };
    };
    let rest = without_pull_request(rest.trim());
    let (message, key) = match split_key(rest) {
        Some((message, key)) => (message, Some(key.to_string())),
        None => (rest, None),
    };
    Change { kind: Some(kind.to_string()), message: message.to_string(), key }
}

/// `type`, `type(scope)`, either with a `!`: a lowercase word and a scope without parentheses.
fn is_conventional(head: &str) -> bool {
    let head = head.strip_suffix('!').unwrap_or(head);
    let (kind, scope) = match head.split_once('(') {
        Some((kind, scope)) => match scope.strip_suffix(')') {
            Some(scope) => (kind, Some(scope)),
            None => return false,
        },
        None => (head, None),
    };
    let word = !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_lowercase());
    word && scope.is_none_or(|s| !s.is_empty() && !s.contains(['(', ')']) && !s.contains(char::is_whitespace))
}

/// The text before a trailing ` (#12)`, or all of it.
fn without_pull_request(text: &str) -> &str {
    let number = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    text.strip_suffix(')')
        .and_then(|t| t.rsplit_once(" (#"))
        .filter(|(_, n)| number(n))
        .map_or(text, |(before, _)| before.trim_end())
}

/// A trailing `[KEY-12]` split off: the text before it and the key.
fn split_key(text: &str) -> Option<(&str, &str)> {
    let (before, key) = text.strip_suffix(']')?.rsplit_once('[')?;
    let (project, number) = key.split_once('-')?;
    let project_ok = project.starts_with(|c: char| c.is_ascii_uppercase())
        && project.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let number_ok = !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit());
    (project_ok && number_ok).then_some((before.trim_end(), key))
}

/// The server's answer about its builds.
#[derive(Message, Debug, Clone)]
pub struct BuildReply(pub Result<ShellBuild, String>);

/// Ends the shell asking the launcher to start it again on the newest build, once what the apps keep is saved.
#[derive(Event, Debug, Clone, Copy)]
pub struct Restart;

/// When the newest build was last asked for, on the real clock.
#[derive(Resource, Default)]
struct Asked(Option<Duration>);

/// The notice's logic, for a shell that knows its build.
pub(crate) struct UpdatePlugin(pub String);

impl Plugin for UpdatePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ThisBuild(self.0.clone()))
            .init_resource::<UpdateReady>()
            .insert_resource(BuiltHere(std::env::var_os(LOCAL_STATE_ENV).filter(|f| !f.is_empty()).map(Into::into)))
            .init_resource::<Asked>()
            .add_message::<BuildReply>()
            .add_message::<SendNet>()
            .add_systems(OnEnter(Connection::Live), |mut asked: ResMut<Asked>| asked.0 = None)
            .add_systems(Update, (ask.run_if(in_state(Connection::Live)), hear).chain())
            .add_observer(restart);
    }
}

fn ask(time: Res<Time<Real>>, this: Res<ThisBuild>, mut asked: ResMut<Asked>, mut net: MessageWriter<SendNet>) {
    let now = time.elapsed();
    if asked.0.is_some_and(|at| now.saturating_sub(at) < POLL) {
        return;
    }
    asked.0 = Some(now);
    net.write(SendNet(NetCommand::Fetch(read(this.0.clone()))));
}

/// Asks for the newest build and the commits since `since`, this shell's own.
fn read(since: String) -> Fetch {
    Fetch::new(move |api| {
        let since = since.clone();
        async move {
            BuildReply(match api {
                Some(api) => api.shell_build(Some(&since)).await.map_err(|e| e.chain()),
                None => Err("not connected".into()),
            })
        }
    })
}

/// Notes a newer build than this one, and what it brings. A failed read leaves the notice as it was.
fn hear(
    mut replies: MessageReader<BuildReply>,
    this: Res<ThisBuild>,
    here: Res<BuiltHere>,
    mut ready: ResMut<UpdateReady>,
) {
    for BuildReply(reply) in replies.read() {
        match reply {
            Ok(build) => {
                let newest = match &here.0 {
                    Some(file) => BuiltHere::newest(file),
                    None => build.newest().map(String::from),
                };
                let newer = newest.filter(|n| *n != this.0);
                ready.set_if_neq(UpdateReady(newer.map(|n| Newer::new(n, &build.changes, build.changes_total))));
            }
            Err(e) => debug!(error = %e, "couldn't ask the server for the newest build of the shell"),
        }
    }
}

fn restart(_: On<Restart>, mut store: ResMut<LocalStore>, mut exit: MessageWriter<AppExit>) {
    store.flush();
    exit.write(AppExit::from_code(RESTART_CODE));
}

#[cfg(test)]
mod tests {
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::ShellPlugin;
    use crate::testing::{Other, headless, set_connection};

    const NOTES: crate::AppId = crate::AppId("notes");

    /// How many reads of the newest build were sent, and the last one.
    #[derive(Resource, Default)]
    struct Asks(usize, Option<Fetch>);

    fn count(mut sent: MessageReader<SendNet>, mut asks: ResMut<Asks>) {
        for SendNet(command) in sent.read() {
            if let NetCommand::Fetch(fetch) = command {
                asks.0 += 1;
                asks.1 = Some(fetch.clone());
            }
        }
    }

    fn shell(dir: &std::path::Path) -> App {
        let mut app = headless(ShellPlugin::new().with(Other).built_as(Some("b".into())).store_in(dir.to_path_buf()));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)))
            .init_resource::<Asks>()
            .add_systems(PostUpdate, count);
        app
    }

    fn answer(app: &mut App, current: &str, previous: Option<&str>, bad: &[&str]) {
        let build = ShellBuild {
            configured: true,
            current: Some(current.into()),
            previous: previous.map(Into::into),
            bad: bad.iter().map(|b| b.to_string()).collect(),
            ..default()
        };
        app.world_mut().write_message(BuildReply(Ok(build)));
        app.update();
    }

    fn ready(app: &App) -> Option<String> {
        app.world().resource::<UpdateReady>().0.as_ref().map(|n| n.rev.clone())
    }

    fn asks(app: &mut App) -> usize {
        std::mem::take(&mut app.world_mut().resource_mut::<Asks>().0)
    }

    /// A server that answers one request with `body`, and says what the request asked for.
    fn serve_once(body: String) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).unwrap();
            let line = String::from_utf8_lossy(&request[..n]).lines().next().unwrap_or_default().to_string();
            let head =
                format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n", body.len());
            write!(stream, "{head}connection: close\r\n\r\n{body}").unwrap();
            line
        });
        (base, served)
    }

    fn change(kind: Option<&str>, message: &str, key: Option<&str>) -> Change {
        Change { kind: kind.map(Into::into), message: message.into(), key: key.map(Into::into) }
    }

    #[test]
    fn a_subject_is_parsed_into_its_kind_message_and_key() {
        let cases = [
            (
                "feat(ticklist): a Completed view [TICK-5] (#2)",
                change(Some("feat(ticklist)"), "a Completed view", Some("TICK-5")),
            ),
            (
                "chore(pin): the framework at 5be6ee23, for the window's kept size and place [APPS-2]",
                change(
                    Some("chore(pin)"),
                    "the framework at 5be6ee23, for the window's kept size and place",
                    Some("APPS-2"),
                ),
            ),
            (
                "Merge pull request #1 from lanesawyer/factory/apps-3",
                change(None, "Merge pull request #1 from lanesawyer/factory/apps-3", None),
            ),
            (
                "fix(shell): the rail: its foot [TRAIL-9]",
                change(Some("fix(shell)"), "the rail: its foot", Some("TRAIL-9")),
            ),
            ("feat(x)!: drop the old API", change(Some("feat(x)!"), "drop the old API", None)),
            ("docs: a guide (#14)", change(Some("docs"), "a guide", None)),
            ("Revert \"feat(x): y\"", change(None, "Revert \"feat(x): y\"", None)),
            ("wip(a b): no [lower-1]", change(None, "wip(a b): no [lower-1]", None)),
        ];
        for (subject, want) in cases {
            assert_eq!(parse_change(subject), want, "{subject}");
        }
    }

    #[test]
    fn the_count_of_changes_is_said_in_words() {
        let subjects = |n: usize| (0..n).map(|i| format!("fix: {i}")).collect::<Vec<_>>();
        let words = |listed, total| {
            let newer = Newer::new("c", &subjects(listed), total);
            (newer.summary(), newer.hint())
        };
        assert_eq!(words(0, 0), ("A newer build is ready.".into(), "Update ready".into()));
        assert_eq!(words(1, 1), ("1 change since the build you're running".into(), "Update ready: 1 change".into()));
        assert_eq!(words(3, 3), ("3 changes since the build you're running".into(), "Update ready: 3 changes".into()));
        assert_eq!(words(3, 0).0, "3 changes since the build you're running", "a total under the list is the list");
        assert_eq!(
            words(30, 41),
            ("The newest 30 of 41 changes since the build you're running".into(), "Update ready: 41 changes".into())
        );
    }

    #[test]
    fn the_poll_names_this_build_and_a_reply_with_changes_sets_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell(dir.path());
        set_connection(&mut app, Connection::Live);
        let fetch = app.world_mut().resource_mut::<Asks>().1.take().expect("a read was sent");
        let reply = serde_json::json!({
            "configured": true, "current": "c", "previous": "b", "building": null, "error": null, "bad": [],
            "changes": ["feat(ticklist): a Completed view [TICK-5] (#2)", "Merge pull request #1 from someone/branch"],
            "changes_total": 41,
        });
        let (base, served) = serve_once(reply.to_string());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let fetched = runtime.block_on(fetch.run(Some(trailhead_client::http::Client::new(base, "token"))));
        assert_eq!(served.join().unwrap(), "GET /v1/shell/build?since=b HTTP/1.1");
        fetched.deliver(app.world_mut());
        app.update();
        let newer = app.world().resource::<UpdateReady>().0.clone().unwrap();
        assert_eq!((newer.rev.as_str(), newer.total), ("c", 41));
        assert_eq!(
            newer.changes,
            [
                change(Some("feat(ticklist)"), "a Completed view", Some("TICK-5")),
                change(None, "Merge pull request #1 from someone/branch", None)
            ]
        );
    }

    #[test]
    fn a_newer_build_sets_the_notice_and_this_build_or_a_bad_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell(dir.path());
        answer(&mut app, "b", Some("a"), &[]);
        assert_eq!(ready(&app), None, "this shell is the newest");
        answer(&mut app, "c", Some("b"), &[]);
        assert_eq!(ready(&app), Some("c".into()));
        answer(&mut app, "c", Some("b"), &["c"]);
        assert_eq!(ready(&app), None, "the launcher wouldn't start a bad build");
        app.world_mut().write_message(BuildReply(Err("offline".into())));
        app.update();
        assert_eq!(ready(&app), None);
    }

    #[test]
    fn a_shell_whose_machine_builds_for_itself_offers_what_was_built_here_not_what_the_server_has() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state.json");
        let mut app = shell(dir.path());
        app.insert_resource(BuiltHere(Some(state.clone())));
        answer(&mut app, "c", Some("b"), &[]);
        assert_eq!(
            ready(&app),
            None,
            "the server has a newer build, but this machine hasn't built it: nothing to start"
        );
        std::fs::write(&state, r#"{"current": {"rev": "b", "at": "2026-11-21T00:00:00Z"}}"#).unwrap();
        answer(&mut app, "c", Some("b"), &[]);
        assert_eq!(ready(&app), None, "what was built here is what this shell runs");
        std::fs::write(&state, r#"{"current": {"rev": "c", "at": "2026-11-21T00:00:00Z"}}"#).unwrap();
        answer(&mut app, "c", Some("b"), &[]);
        assert_eq!(ready(&app).as_deref(), Some("c"), "built here now, so it is offered");
    }

    #[test]
    fn it_asks_only_while_live_and_then_every_poll() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell(dir.path());
        assert_eq!(asks(&mut app), 0, "not while offline");
        set_connection(&mut app, Connection::Live);
        assert_eq!(asks(&mut app), 2, "at once when live, beside the Ask form's read of what can be asked about");
        for _ in 0..(POLL.as_secs() - 5) {
            app.update();
        }
        assert_eq!(asks(&mut app), 0);
        for _ in 0..5 {
            app.update();
        }
        assert_eq!(asks(&mut app), 1, "again after a poll");
    }

    #[test]
    fn a_shell_not_started_by_the_launcher_never_asks() {
        let mut app = headless(ShellPlugin::new().with(Other));
        set_connection(&mut app, Connection::Live);
        assert!(app.world().get_resource::<UpdateReady>().is_none());
    }

    #[test]
    fn restarting_saves_what_the_apps_keep_and_exits_with_the_restart_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell(dir.path());
        app.world_mut().resource_mut::<LocalStore>().set(NOTES, "draft", "buy rope");
        app.world_mut().trigger(Restart);
        let exits: Vec<AppExit> = app.world_mut().resource_mut::<Messages<AppExit>>().drain().collect();
        assert_eq!(exits, [AppExit::from_code(RESTART_CODE)]);
        let kept = std::fs::read_to_string(dir.path().join("notes.json")).unwrap();
        assert!(kept.contains("buy rope"), "{kept}");
    }
}
