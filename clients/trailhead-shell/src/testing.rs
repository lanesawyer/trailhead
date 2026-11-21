//! For tests of the shell and of an app built on it, behind the `test-support` feature: a test app holding the shell
//! with the pieces of its connection a test needs, ways to drive it, and a second app to switch to. With
//! [`crate::fixture`] it fills the mirror without a server; [`server`] (the `test-server` feature) is a real one.
//!
//! [`headless`] runs an app's logic ([`ShellApp::build`]) under `MinimalPlugins`; [`windowed`] also runs its drawing
//! ([`ShellApp::build_ui`]) and the rail, but not the kit's `UiPlugin` or `ThemePlugin`, so it checks what is spawned
//! and kept, not how it looks. Both give the app the shell's [`Connection`] state (`Offline` until a test sets it with
//! [`set_connection`]), its [`ConnectionStatus`], the mirror and the outbox, with no net thread: a test fills the
//! mirror with [`fill`] and [`feed`], reads what the app sent with [`sent`], and answers it with [`answer`].
//!
//! `shot` (the `shots` feature) draws the shell to PNGs, for an app's shots test.

use std::time::Duration;

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use trailhead_ui::Icon;

use crate::commands::{CommandDone, CommandId, CommandsPlugin, Outcome, Submit};
use crate::connection::{Connection, ConnectionStatus, Dialed, LocalEndpoint, SendNet};
use crate::mirror::MirrorPlugin;
use crate::net::{NetCommand, Reason};
use crate::sync::{Snapshot, SyncItem};
use crate::transition::Switching;
use crate::{ActiveApp, AppId, Manifest, ShellApp, ShellPlugin};

#[cfg(feature = "test-server")]
pub mod server;
#[cfg(feature = "shots")]
pub mod shot;

/// The id of [`Other`].
pub const OTHER: AppId = AppId("other");

/// An app that does nothing, to stand in for "another app is shown".
pub struct Other;

impl ShellApp for Other {
    fn manifest(&self) -> Manifest {
        Manifest { id: OTHER, name: "Other", icon: Icon::Board }
    }

    fn build(&self, _: &mut App) {}
}

/// What the shell's connection gives its apps, without the net thread: the [`Connection`] state, the
/// [`ConnectionStatus`], the [`LocalEndpoint`] and the control plane [`Dialed`] (neither known), the mirror, the outbox
/// and the [`SendNet`] messages, which go nowhere. For a shell that was not told how to connect: one that was runs the
/// real thing.
pub struct TestConnection;

impl Plugin for TestConnection {
    fn build(&self, app: &mut App) {
        app.init_state::<Connection>()
            .init_resource::<ConnectionStatus>()
            .init_resource::<LocalEndpoint>()
            .init_resource::<Dialed>()
            .add_message::<SendNet>()
            .add_plugins((MirrorPlugin, CommandsPlugin));
    }
}

/// The shell and its apps' logic under `MinimalPlugins`, with a [`TestConnection`], settled.
pub fn headless(shell: ShellPlugin) -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin, TestConnection)).add_plugins(shell);
    settle(&mut app);
    app
}

/// [`headless`], drawn: the shell windowed (its rail and each app's `build_ui`), with the assets scenes need.
pub fn windowed(shell: ShellPlugin) -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin, TestConnection))
        .init_asset::<bevy::text::Font>()
        .init_asset::<Image>()
        .add_plugins(shell.windowed());
    settle(&mut app);
    app
}

/// Runs enough frames for state changes to land and queued scenes to spawn.
pub fn settle(app: &mut App) {
    for _ in 0..4 {
        app.update();
    }
}

/// Shows the app `id` at once, and settles: the switch a press on the rail makes, without its transition, so the app is
/// shown when this returns whatever time a test keeps. A transition under way is ended first. To test a transition,
/// trigger [`SwitchApp`](crate::transition::SwitchApp) and step the frames.
pub fn show(app: &mut App, id: AppId) {
    app.world_mut().resource_mut::<Switching>().cancel();
    app.world_mut().resource_mut::<NextState<ActiveApp>>().set(ActiveApp(id));
    settle(app);
}

/// Puts the connection in `to`, and settles.
pub fn set_connection(app: &mut App, to: Connection) {
    app.world_mut().resource_mut::<NextState<Connection>>().set(to);
    settle(app);
}

/// Replaces the mirror with `snapshot`'s records (made with [`crate::fixture`]'s `*_json`), as a sync would, and
/// settles.
pub fn fill(app: &mut App, snapshot: Snapshot) {
    feed(app, SyncItem::Snapshot(snapshot));
}

/// Hands the mirror one item off the sync (an event made with [`crate::fixture::sync_event`]), and settles.
pub fn feed(app: &mut App, item: SyncItem) {
    app.world_mut().write_message(item);
    settle(app);
}

/// What the app has sent since the last look.
pub fn sent(app: &mut App) -> Vec<Submit> {
    app.world_mut().resource_mut::<Messages<Submit>>().drain().collect()
}

/// The server's answer to the command sent from `source`, and settles.
pub fn answer(app: &mut App, source: Entity, outcome: Outcome) {
    app.world_mut().write_message(CommandDone { source, id: CommandId(0), outcome });
    settle(app);
}

/// The connection's state.
pub fn state(app: &App) -> Connection {
    *app.world().resource::<State<Connection>>().get()
}

/// Why the connection isn't live, without the wait before the next try.
pub fn reason(app: &App) -> Option<Reason> {
    app.world().resource::<ConnectionStatus>().reason.as_ref().map(|r| r.cause().clone())
}

/// Runs the app until `done` holds, or fails with where it got stuck.
pub async fn until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    for _ in 0..400 {
        app.update();
        if done(app) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never reached: {what}; stuck at {:?} ({:?})", state(app), reason(app));
}

/// Sends `command` to the net thread.
pub fn send(app: &mut App, command: NetCommand) {
    app.world_mut().write_message(SendNet(command));
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trailhead_client::http::Call;
    use trailhead_model::{Task, TaskKey};

    use super::*;
    use crate::commands::{Method, Request};
    use crate::fixture::{sid, sync_event, task_json};
    use crate::mirror::Mirrored;

    /// What came back to the app.
    #[derive(Resource, Default)]
    struct Answers(Vec<(Entity, Outcome)>);

    fn titles(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        world.query::<&Mirrored<Task>>().iter(world).map(|t| t.0.title.clone()).collect()
    }

    #[test]
    fn a_test_app_is_filled_from_fixtures_goes_live_and_hears_answers() {
        let mut app = headless(ShellPlugin::new().with(Other));
        app.init_resource::<Answers>().add_systems(
            Update,
            |mut done: MessageReader<CommandDone>, mut answers: ResMut<Answers>| {
                answers.0.extend(done.read().map(|d| (d.source, d.outcome.clone())));
            },
        );
        assert_eq!(state(&app), Connection::Offline);
        let task = |title: &str, version: i64| {
            task_json(json!({"id": "t1", "key": "HOME-1", "title": title, "version": version}))
        };
        fill(&mut app, Snapshot { at: 1, tasks: vec![task("old", 1)], ..default() });
        assert_eq!(titles(&mut app), ["old"]);
        let event =
            json!({"id": 2, "entity": "task", "entity_id": sid("t1"), "kind": "task.updated", "after": task("new", 2)});
        feed(&mut app, sync_event(event));
        assert_eq!(titles(&mut app), ["new"]);

        set_connection(&mut app, Connection::Live);
        assert_eq!(state(&app), Connection::Live);
        let source = app.world_mut().spawn_empty().id();
        let request = Request::new(Call::Release(TaskKey("HOME-1".into())), None);
        app.world_mut().write_message(Submit { source, request });
        let out = sent(&mut app);
        assert_eq!((out.len(), out[0].request.method()), (1, Method::Post));
        answer(&mut app, source, Outcome::Ok { version: None });
        assert_eq!(app.world().resource::<Answers>().0, [(source, Outcome::Ok { version: None })]);
    }
}
