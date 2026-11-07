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

/// The control plane's endpoint id as last given to dial (typed, or saved from last time); `None` once disconnected.
/// It is what was asked for, not proof of a link: [`Connection`] says whether there is one.
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

fn forward_commands(
    net: Res<Net>,
    mut commands: MessageReader<SendNet>,
    mut dialed: ResMut<Dialed>,
    mut factory: MessageWriter<FactoryItem>,
) {
    for SendNet(command) in commands.read() {
        match command {
            NetCommand::Connect(target) => dialed.0 = Some(target.trim().to_string()),
            NetCommand::Disconnect => {
                dialed.0 = None;
                // The status stream goes with the control plane; the mirror keeps its last state, marked stopped.
                factory.write(FactoryItem(FactoryUpdate::Stopped));
            }
            _ => {}
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
