//! The `Connection` state (`docs/factory/ui.md#connection-gating`) and the Bevy side of the net thread.
//!
//! Every command system runs only in [`Connection::Live`], and the UI greys out its controls otherwise, showing
//! [`ConnectionStatus::reason`].

use bevy::prelude::*;

use crate::commands::{CommandsPlugin, Reply};
use crate::detail::{DetailPlugin, DetailReply};
use crate::factory::{FactoryItem, FactoryPlugin, FactoryUpdate, apply_factory};
use crate::logs::{LogItem, LogsPlugin};
use crate::mirror::{MirrorPlugin, apply_sync};
use crate::net::{ApiAccess, Link, NetCommand, NetConfig, NetEvent, NetHandle, Reason};
use crate::search::{SearchPlugin, SearchReply};
use crate::sync::SyncItem;

#[derive(States, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Connection {
    /// No path to the factory: nothing dialed yet, refused, unreachable, or no usable login.
    #[default]
    Offline,
    /// Dialed, and signing in or catching up.
    Syncing,
    /// Signed in over the tunnel; commands may run.
    Live,
}

impl From<Link> for Connection {
    fn from(link: Link) -> Self {
        match link {
            Link::Offline => Connection::Offline,
            Link::Syncing => Connection::Syncing,
            Link::Live => Connection::Live,
        }
    }
}

/// Why the connection isn't [`Connection::Live`], for the UI to show; `None` when it is.
#[derive(Resource, Default, Debug)]
pub struct ConnectionStatus {
    pub reason: Option<Reason>,
}

/// Lookout's own endpoint id, to give to `trailhead factory allow`. Empty until the endpoint is up.
#[derive(Resource, Default, Debug)]
pub struct LocalEndpoint(pub Option<String>);

/// The control plane's endpoint id, as the net thread parsed it once it holds one (so not the text typed); `None`
/// after a Connect that failed to parse or open, and once disconnected. The net thread sets it ([`NetEvent::Dialed`]),
/// and the id handed to start is shown until it says otherwise. Not proof of a link: [`Connection`] says whether
/// there is one.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Dialed(pub Option<String>);

/// The forwarded API and the user's token, present once signed in. It stays after a drop, so the last known
/// address is still there when the link comes back; check [`Connection`] before using it.
#[derive(Resource, Debug)]
pub struct Api(pub ApiAccess);

/// A [`NetCommand`] to send to the net thread.
#[derive(Message, Debug)]
pub struct SendNet(pub NetCommand);

#[derive(Resource)]
struct Net(NetHandle);

/// Runs the net thread and mirrors it into the [`Connection`] state and its resources.
pub struct ConnectionPlugin {
    config: NetConfig,
    /// A control plane endpoint id to dial at start; defaults to the one dialed last time.
    target: Option<String>,
}

impl ConnectionPlugin {
    pub fn new(config: NetConfig, target: Option<String>) -> Self {
        let target = target.or_else(|| config.saved_target());
        ConnectionPlugin { config, target }
    }
}

impl Plugin for ConnectionPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<Connection>()
            .init_resource::<ConnectionStatus>()
            .init_resource::<LocalEndpoint>()
            .insert_resource(Dialed(self.target.clone()))
            .add_plugins((MirrorPlugin, FactoryPlugin, CommandsPlugin, SearchPlugin, DetailPlugin, LogsPlugin))
            .add_message::<SendNet>()
            .insert_resource(Net(NetHandle::spawn(self.config.clone(), self.target.clone())))
            .add_systems(PreUpdate, (forward_commands, apply_events.before(apply_sync).before(apply_factory)).chain());
    }
}

fn forward_commands(net: Res<Net>, mut commands: MessageReader<SendNet>, mut factory: MessageWriter<FactoryItem>) {
    for SendNet(command) in commands.read() {
        if matches!(command, NetCommand::Disconnect) {
            // The status stream goes with the control plane; the mirror keeps its last state, marked unreachable.
            factory.write(FactoryItem(FactoryUpdate::Unreachable));
        }
        net.0.send(command.clone());
    }
}

// A Bevy system takes what it reads and writes as parameters.
#[allow(clippy::too_many_arguments)]
fn apply_events(
    net: Res<Net>,
    mut commands: Commands,
    mut next: ResMut<NextState<Connection>>,
    mut status: ResMut<ConnectionStatus>,
    mut dialed: ResMut<Dialed>,
    mut endpoint: ResMut<LocalEndpoint>,
    mut sync: MessageWriter<SyncItem>,
    mut factory: MessageWriter<FactoryItem>,
    mut replies: MessageWriter<Reply>,
    mut searches: MessageWriter<SearchReply>,
    mut logs: MessageWriter<LogItem>,
    mut details: MessageWriter<DetailReply>,
) {
    for event in net.0.drain() {
        match event {
            NetEvent::Endpoint(id) => endpoint.0 = Some(id),
            NetEvent::Dialed(id) => dialed.0 = id,
            NetEvent::State(link, reason) => {
                next.set(link.into());
                status.reason = reason;
            }
            NetEvent::Access(access) => commands.insert_resource(Api(access)),
            NetEvent::Reply(reply) => {
                replies.write(reply);
            }
            NetEvent::Search(reply) => {
                searches.write(reply);
            }
            NetEvent::Logs(update) => {
                logs.write(LogItem(update));
            }
            NetEvent::Detail(reply) => {
                details.write(reply);
            }
            NetEvent::Sync(item) => {
                sync.write(item);
            }
            NetEvent::Factory(update) => {
                factory.write(FactoryItem(update));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Sender;

    use bevy::state::app::StatesPlugin;

    use super::*;

    fn app() -> (App, Sender<NetEvent>) {
        let (handle, events) = NetHandle::detached();
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .init_state::<Connection>()
            .init_resource::<ConnectionStatus>()
            .init_resource::<LocalEndpoint>()
            .init_resource::<Dialed>()
            .add_message::<SendNet>()
            .add_message::<SyncItem>()
            .add_message::<FactoryItem>()
            .add_message::<Reply>()
            .add_message::<SearchReply>()
            .add_message::<LogItem>()
            .add_message::<DetailReply>()
            .insert_resource(Net(handle))
            .add_systems(PreUpdate, (forward_commands, apply_events).chain());
        (app, events)
    }

    fn connect(app: &mut App, text: &str) {
        app.world_mut().write_message(SendNet(NetCommand::Connect(text.into())));
    }

    fn dialed(app: &App) -> Option<String> {
        app.world().resource::<Dialed>().0.clone()
    }

    fn failed(text: &str) -> NetEvent {
        NetEvent::State(Link::Offline, Some(Reason::BadTarget(text.into())))
    }

    #[test]
    fn a_typed_connect_shows_nothing_until_the_net_thread_holds_it() {
        let (mut app, events) = app();
        connect(&mut app, "foo");
        app.update();
        assert_eq!(dialed(&app), None);
        events.send(NetEvent::Dialed(Some("abc".into()))).unwrap();
        app.update();
        assert_eq!(dialed(&app), Some("abc".into()));
    }

    #[test]
    fn a_retried_failure_leaves_nothing_dialed_though_the_state_is_unchanged() {
        let (mut app, events) = app();
        app.world_mut().resource_mut::<Dialed>().0 = Some("old".into());
        for round in 0..2 {
            connect(&mut app, "foo");
            events.send(NetEvent::Dialed(None)).unwrap();
            // The net thread reports a state only when it changes, so the retry brings none.
            if round == 0 {
                events.send(failed("foo")).unwrap();
            }
            app.update();
            assert_eq!(dialed(&app), None);
        }
    }

    #[test]
    fn states_arriving_around_a_connect_do_not_change_what_the_net_thread_holds() {
        let (mut app, events) = app();
        connect(&mut app, "abc");
        events.send(NetEvent::State(Link::Offline, Some(Reason::Unreachable("old".into())))).unwrap();
        events.send(NetEvent::Dialed(Some("abc".into()))).unwrap();
        app.update();
        assert_eq!(dialed(&app), Some("abc".into()));
        // A failed health check on the target it keeps probing leaves it shown.
        events.send(NetEvent::State(Link::Offline, Some(Reason::Unreachable("down".into())))).unwrap();
        app.update();
        assert_eq!(dialed(&app), Some("abc".into()));
    }

    #[test]
    fn a_disconnect_clears_it_when_the_net_thread_says_so() {
        let (mut app, events) = app();
        events.send(NetEvent::Dialed(Some("abc".into()))).unwrap();
        app.update();
        app.world_mut().write_message(SendNet(NetCommand::Disconnect));
        events.send(NetEvent::Dialed(None)).unwrap();
        app.update();
        assert_eq!(dialed(&app), None);
    }
}
