//! The update notice. A shell the launcher started (`trailhead app`, which names the build in
//! [`BUILD_ENV`](trailhead_model::shell::BUILD_ENV)) asks the server which build is newest every [`POLL`] while the
//! connection is live, and when that isn't its own build it offers to restart onto it: [`UpdateReady`] says so, the
//! rail shows it, and [`Restart`] ends the shell with [`RESTART_CODE`], which the launcher answers by starting the
//! newest build.
//!
//! It polls `GET /v1/shell/build` rather than hearing of a build on the event stream: that stream carries the task
//! engine's events, which are stored and undoable, and a build is neither.

use std::time::Duration;

use bevy::prelude::*;
use trailhead_model::shell::{RESTART_CODE, ShellBuild};

use crate::LocalStore;
use crate::connection::{Connection, SendNet};
use crate::net::{Fetch, NetCommand};

/// How often a live shell asks for the newest build.
pub const POLL: Duration = Duration::from_secs(30);

/// The build this shell was started from: the commit the launcher named.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct ThisBuild(pub String);

/// The newer build the launcher would start, once the server has said there is one; `None` while this shell is the
/// newest.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct UpdateReady(pub Option<String>);

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
            .init_resource::<Asked>()
            .add_message::<BuildReply>()
            .add_message::<SendNet>()
            .add_systems(OnEnter(Connection::Live), |mut asked: ResMut<Asked>| asked.0 = None)
            .add_systems(Update, (ask.run_if(in_state(Connection::Live)), hear).chain())
            .add_observer(restart);
    }
}

fn ask(time: Res<Time<Real>>, mut asked: ResMut<Asked>, mut net: MessageWriter<SendNet>) {
    let now = time.elapsed();
    if asked.0.is_some_and(|at| now.saturating_sub(at) < POLL) {
        return;
    }
    asked.0 = Some(now);
    net.write(SendNet(NetCommand::Fetch(read())));
}

fn read() -> Fetch {
    Fetch::new(|api| async move {
        BuildReply(match api {
            Some(api) => api.shell_build(None).await.map_err(|e| e.chain()),
            None => Err("not connected".into()),
        })
    })
}

/// Notes a newer build than this one. A failed read leaves the notice as it was.
fn hear(mut replies: MessageReader<BuildReply>, this: Res<ThisBuild>, mut ready: ResMut<UpdateReady>) {
    for BuildReply(reply) in replies.read() {
        match reply {
            Ok(build) => {
                let newer = build.newest().filter(|n| *n != this.0).map(String::from);
                ready.set_if_neq(UpdateReady(newer));
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

    /// How many reads of the newest build were sent.
    #[derive(Resource, Default)]
    struct Asks(usize);

    fn count(mut sent: MessageReader<SendNet>, mut asks: ResMut<Asks>) {
        asks.0 += sent.read().filter(|SendNet(c)| matches!(c, NetCommand::Fetch(_))).count();
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
        app.world().resource::<UpdateReady>().0.clone()
    }

    fn asks(app: &mut App) -> usize {
        std::mem::take(&mut app.world_mut().resource_mut::<Asks>().0)
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
    fn it_asks_only_while_live_and_then_every_poll() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell(dir.path());
        assert_eq!(asks(&mut app), 0, "not while offline");
        set_connection(&mut app, Connection::Live);
        assert_eq!(asks(&mut app), 1, "at once when live");
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
