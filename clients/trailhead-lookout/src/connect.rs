//! Connecting from the window while not [`Connection::Live`]: paste the factory's endpoint id, then a user token if
//! the server asks for one, and copy the `trailhead factory allow` command for this Lookout.

use bevy::clipboard::Clipboard;
use bevy::feathers::controls::ButtonVariant;
use bevy::prelude::*;
use bevy::scene::bsn;
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;

use crate::connection::{Connection, ConnectionStatus, LocalEndpoint, SendNet};
use crate::net::{NetCommand, Reason};
use crate::widgets::{Icon, button, field, layer, panel, row, size, text_dim, text_input, tool_button};

/// A text field on the connect panel.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectField {
    /// The control plane's endpoint id.
    Target,
    /// A user token.
    Token,
}

/// A button on the connect panel.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectAct {
    Dial,
    SignIn,
    CopyAllow,
}

/// The `trailhead factory allow` command for this Lookout, once its endpoint is up.
pub fn allow_command(endpoint: &LocalEndpoint) -> Option<String> {
    endpoint.0.as_ref().map(|id| format!("trailhead factory allow {id}"))
}

/// Whether the server is waiting for a token, so the token field is worth showing.
pub fn wants_token(status: &ConnectionStatus) -> bool {
    matches!(status.reason, Some(Reason::NeedsLogin | Reason::TokenRejected | Reason::AgentToken))
}

/// Sends what was typed when Connect or Sign in is pressed. The fields keep their text, so a typo can be fixed and
/// sent again.
fn on_act(
    activate: On<Activate>,
    acts: Query<&ConnectAct>,
    fields: Query<(&ConnectField, &EditableText)>,
    mut send: MessageWriter<SendNet>,
) {
    let Ok(act) = acts.get(activate.entity) else { return };
    let typed = |want: ConnectField| {
        fields
            .iter()
            .find(|(f, _)| **f == want)
            .map(|(_, t)| t.value().to_string().trim().to_string())
            .filter(|t| !t.is_empty())
    };
    match act {
        ConnectAct::Dial => {
            if let Some(target) = typed(ConnectField::Target) {
                send.write(SendNet(NetCommand::Connect(target)));
            }
        }
        ConnectAct::SignIn => {
            if let Some(token) = typed(ConnectField::Token) {
                send.write(SendNet(NetCommand::Login(token)));
            }
        }
        ConnectAct::CopyAllow => {}
    }
}

/// The connect panel's logic, without drawing.
pub struct ConnectPlugin;

impl Plugin for ConnectPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_act);
    }
}

/// Draws the connect panel along the bottom of the window while not live.
pub struct ConnectUiPlugin;

impl Plugin for ConnectUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ConnectPlugin)
            .add_observer(copy_allow)
            .add_systems(Startup, spawn_panel)
            .add_systems(Update, show);
    }
}

#[derive(Component)]
struct ConnectPanel;

/// A row shown only while the server wants a token.
#[derive(Component)]
struct TokenRow;

/// The row with the `allow` command, shown once Lookout's endpoint is up.
#[derive(Component)]
struct AllowRow;

#[derive(Component)]
struct AllowText;

fn spawn_panel(mut commands: Commands) {
    let panel = commands
        .spawn_scene(bsn! {
            @panel()
            Node { position_type: PositionType::Absolute, left: px(0), right: px(0), bottom: px(0) }
            // Above the board, which fills the window and is spawned later: without this it takes every click here.
            GlobalZIndex({ layer::CONNECT })
        })
        .insert((Name::new("connect"), ConnectPanel))
        .id();
    let target = input_row(
        &mut commands,
        "Factory endpoint id",
        ConnectField::Target,
        "Connect",
        ConnectAct::Dial,
        ButtonVariant::Primary,
    );
    commands.entity(target).insert(ChildOf(panel));
    let token = input_row(
        &mut commands,
        "User token",
        ConnectField::Token,
        "Sign in",
        ConnectAct::SignIn,
        ButtonVariant::Normal,
    );
    commands.entity(token).insert((TokenRow, ChildOf(panel)));

    let allow = commands.spawn_scene(bsn! { @row() }).insert((AllowRow, ChildOf(panel))).id();
    commands.spawn_scene(bsn! { @text_dim("", size::SECONDARY) }).insert((AllowText, ChildOf(allow)));
    commands
        .spawn_scene(bsn! { @tool_button(Icon::Copy, "Copy the command", ButtonVariant::Plain) })
        .insert((ConnectAct::CopyAllow, ChildOf(allow)));
}

/// A field with `label` over its input, and a button beside it, in a row. Returns the row.
fn input_row(
    commands: &mut Commands,
    label: &str,
    which: ConnectField,
    caption: &str,
    act: ConnectAct,
    variant: ButtonVariant,
) -> Entity {
    let line = commands.spawn_scene(bsn! { @row() Node { align_items: AlignItems::FlexEnd } }).id();
    commands
        .spawn_scene(bsn! {
            @field(label.to_string())
            Node { flex_grow: 1.0 }
            Children [ @text_input(1.0, bsn! { which }) ]
        })
        .insert(ChildOf(line));
    commands.spawn_scene(bsn! { @button(caption.to_string(), None, variant) act }).insert(ChildOf(line));
    line
}

/// Shows the panel while not live, the token row while the server wants one, and the `allow` command once known.
#[allow(clippy::type_complexity)]
fn show(
    connection: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    endpoint: Res<LocalEndpoint>,
    mut nodes: ParamSet<(
        Single<&mut Node, With<ConnectPanel>>,
        Single<&mut Node, With<TokenRow>>,
        Single<&mut Node, With<AllowRow>>,
    )>,
    mut allow: Single<&mut Text, With<AllowText>>,
) {
    if !(connection.is_changed() || status.is_changed() || endpoint.is_changed()) {
        return;
    }
    let shown = |on: bool| if on { Display::Flex } else { Display::None };
    nodes.p0().display = shown(*connection.get() != Connection::Live);
    nodes.p1().display = shown(wants_token(&status));
    let command = allow_command(&endpoint);
    nodes.p2().display = shown(command.is_some());
    allow.0 = command.map_or_else(String::new, |c| format!("Allow this Lookout on the factory's machine: {c}"));
}

fn copy_allow(
    activate: On<Activate>,
    acts: Query<&ConnectAct>,
    endpoint: Res<LocalEndpoint>,
    mut clipboard: ResMut<Clipboard>,
) {
    if acts.get(activate.entity) != Ok(&ConnectAct::CopyAllow) {
        return;
    }
    if let Some(command) = allow_command(&endpoint)
        && let Err(e) = clipboard.set_text(command)
    {
        warn!(error = %e, "couldn't copy the allow command");
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .init_state::<Connection>()
            .add_message::<SendNet>()
            .add_plugins(ConnectPlugin);
        app
    }

    fn press(app: &mut App, act: ConnectAct) {
        let button = app.world_mut().spawn(act).id();
        app.world_mut().trigger(Activate { entity: button });
        app.update();
    }

    fn sent(app: &mut App) -> Vec<String> {
        let messages = app.world_mut().resource_mut::<Messages<SendNet>>().drain().collect::<Vec<_>>();
        messages
            .into_iter()
            .map(|SendNet(c)| match c {
                NetCommand::Connect(t) => format!("connect {t}"),
                NetCommand::Login(t) => format!("login {t}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn connect_and_sign_in_send_what_was_typed_trimmed() {
        let mut app = app();
        app.world_mut().spawn((ConnectField::Target, EditableText::new("  7204d50f\n")));
        app.world_mut().spawn((ConnectField::Token, EditableText::new("th_user ")));
        press(&mut app, ConnectAct::Dial);
        press(&mut app, ConnectAct::SignIn);
        assert_eq!(sent(&mut app), ["connect 7204d50f", "login th_user"]);
    }

    #[test]
    fn empty_fields_send_nothing() {
        let mut app = app();
        app.world_mut().spawn((ConnectField::Target, EditableText::new("   ")));
        press(&mut app, ConnectAct::Dial);
        press(&mut app, ConnectAct::SignIn);
        assert!(sent(&mut app).is_empty());
    }

    #[test]
    fn the_token_row_is_for_a_server_that_wants_one() {
        let wants = |reason| wants_token(&ConnectionStatus { reason });
        assert!(wants(Some(Reason::NeedsLogin)));
        assert!(wants(Some(Reason::TokenRejected)));
        assert!(wants(Some(Reason::AgentToken)));
        assert!(!wants(Some(Reason::NoTarget)));
        assert!(!wants(Some(Reason::NotAllowed)));
        assert!(!wants(None));
        assert_eq!(allow_command(&LocalEndpoint(Some("abc".into()))).as_deref(), Some("trailhead factory allow abc"));
        assert_eq!(allow_command(&LocalEndpoint(None)), None);
    }
}
