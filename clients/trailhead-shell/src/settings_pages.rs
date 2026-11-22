//! The shell's own settings pages ([`crate::settings`]):
//!
//! - **Appearance**: the theme (light, dark or the desktop's), kept in the shell's `theme` file, and the text size,
//!   every step of the kit's [`TextSize`] with the one drawn at marked, kept in `shell.json`. Both apply at once, and a
//!   change made elsewhere (the keys, the desktop) shows here in place. Then how the window switches apps
//!   ([`crate::transition`]), each way a small preview playing it ([`crate::previews`]), kept in `shell.json`.
//! - **Connection**: the server dialed, the state of the link and why it isn't live, who it signed in as, this
//!   window's endpoint id and the `trailhead factory allow` command for it, both with a copy button, and Re-dial and
//!   Disconnect. Listed only in a shell with a connection.
//! - **Notifications**: how the person wants to be told, kept on the server ([`crate::notifications_page`]). Listed
//!   only in a shell with a connection.
//! - **Server**: what the server's user config says, edited from the window ([`crate::server_page`]). Listed only in
//!   a shell with a connection.
//! - **About**: the build the launcher started (or that the shell was run directly), with a link to its commit on
//!   GitHub once the server has said which repo it is of ([`BuildRepo`]), whether a newer one is ready,
//!   the apps the window holds, and where the shell keeps its files. Read-only.
//!
//! Each page's texts are spawned empty and kept current in place by `refresh_*`, so a page shows what is true now
//! from its first frame to its last.

use std::path::PathBuf;

use bevy::clipboard::Clipboard;
use bevy::prelude::*;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::Activate;
use trailhead_ui::{
    Appearance, ButtonGroup, ButtonKind, ChangeTextSize, ExternalLink, Icon, Picked, TextSize, ThemeChoice, UiPlace,
    button, button_group, copy_block, link_button, row, set_display, set_text, setting, size, spawn_settings_section,
    text,
};

use crate::connect::allow_command;
use crate::connection::{Api, Connection, ConnectionStatus, Dialed, LocalEndpoint, SendNet};
use crate::net::NetCommand;
use crate::previews::transition_setting;
use crate::settings::{ABOUT, APPEARANCE, CONNECTION, CloseSettings, NOTIFICATIONS, SERVER, SHELL_GROUP, SettingsPage};
use crate::update::{BuildRepo, ThisBuild, UpdateReady};
use crate::{AppId, Apps, SHELL};

/// The shell's directory, where it keeps the theme, the window's place and each app's `LocalStore`. Set by
/// [`Shell::run`](crate::Shell::run); a shell without it keeps nothing on this machine.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct ShellDir(pub PathBuf);

/// The shell's own pages, in the order they are listed: Notifications, Connection and Server only with a connection.
pub(crate) fn shell_pages(connected: bool) -> Vec<(AppId, &'static str, SettingsPage)> {
    let mut pages = vec![SettingsPage::new(APPEARANCE.key, "Appearance", appearance_page)];
    if connected {
        let notifications = crate::notifications_page::notifications_page;
        pages.push(SettingsPage::new(NOTIFICATIONS.key, "Notifications", notifications));
        pages.push(SettingsPage::new(CONNECTION.key, "Connection", connection_page));
        pages.push(SettingsPage::new(SERVER.key, "Server", crate::server_page::server_page));
    }
    pages.push(SettingsPage::new(ABOUT.key, "About", about_page));
    pages.into_iter().map(|page| (SHELL, SHELL_GROUP, page)).collect()
}

/// The theme's buttons, one per [`ThemeChoice::ALL`].
#[derive(Component, Clone, Default)]
pub struct ThemePick;

/// The text size's buttons, one per [`TextSize::all`].
#[derive(Component, Clone, Default)]
pub struct TextSizePick;

/// A text on a page, and what it shows.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shows {
    #[default]
    Server,
    State,
    Identity,
    Endpoint,
    Allow,
    Build,
    Update,
    Apps,
    Files,
}

/// A button on the Connection page.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionAct {
    #[default]
    Redial,
    Disconnect,
    CopyEndpoint,
    CopyAllow,
}

/// The modifier the window's own keys take, as this machine's keyboard names it.
const COMMAND_KEY: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

fn value(which: Shows, label: &str, note: &str) -> impl Scene {
    setting(label.to_string(), note.to_string(), bsn_list! { @text("", size::BODY) which })
}

/// A value in a box of its own, with the button that copies it.
fn copyable(which: Shows, label: &str, note: &str, act: ConnectionAct) -> impl Scene {
    let hint = format!("Copy the {}", label.to_lowercase());
    let control = bsn_list! { @copy_block(bsn! { which }, hint, bsn! { act }) };
    setting(label.to_string(), note.to_string(), control)
}

fn appearance_page(
    In(page): In<Entity>,
    appearance: Option<Res<Appearance>>,
    text_size: Res<TextSize>,
    mut commands: Commands,
) {
    let choice = appearance.map_or(ThemeChoice::Desktop, |a| a.choice);
    let themes = ThemeChoice::ALL.iter().map(|c| (c.label().to_string(), Some(c.icon()))).collect();
    let at = ThemeChoice::ALL.iter().position(|c| *c == choice);
    let note = "Desktop follows your desktop's light or dark setting.";
    let control = bsn_list! { @button_group(themes, at) ThemePick };
    commands.spawn_scene(setting("Theme", note, control)).insert(ChildOf(page));

    let sizes = TextSize::all().map(|s| (s.label(), None)).collect();
    let note = format!(
        "How big everything in the window is drawn, in every app. {COMMAND_KEY} and = makes it bigger, {COMMAND_KEY} and - \
         smaller, and {COMMAND_KEY} and 0 puts it back at 100%."
    );
    let control = bsn_list! { @button_group(sizes, Some(text_size.step())) TextSizePick };
    commands.spawn_scene(setting("Text size", note, control)).insert(ChildOf(page));

    commands.spawn_scene(transition_setting()).insert(ChildOf(page));
}

fn connection_page(In(page): In<Entity>, mut commands: Commands) {
    let server = spawn_settings_section(&mut commands, "Server");
    commands.entity(server.section).insert(ChildOf(page));
    commands.spawn_scene(value(Shows::Server, "Trailhead server", "Its endpoint id.")).insert(ChildOf(server.rows));
    commands.spawn_scene(value(Shows::State, "State", "")).insert(ChildOf(server.rows));
    commands.spawn_scene(value(Shows::Identity, "Signed in as", "")).insert(ChildOf(server.rows));
    let acts = commands.spawn_scene(row()).insert(ChildOf(server.rows)).id();
    commands
        .spawn_scene(button("Re-dial", Some(Icon::Refresh), ButtonKind::Normal))
        .insert((ConnectionAct::Redial, ChildOf(acts)));
    commands
        .spawn_scene(button("Disconnect", None, ButtonKind::Normal))
        .insert((ConnectionAct::Disconnect, ChildOf(acts)));

    let window = spawn_settings_section(&mut commands, "This window");
    commands.entity(window.section).insert(ChildOf(page));
    let note = "What your Trailhead server knows this window by.";
    let endpoint = copyable(Shows::Endpoint, "Endpoint id", note, ConnectionAct::CopyEndpoint);
    commands.spawn_scene(endpoint).insert(ChildOf(window.rows));
    let note = "Run it on your Trailhead server's machine to let this window connect.";
    let allow = copyable(Shows::Allow, "Allow command", note, ConnectionAct::CopyAllow);
    commands.spawn_scene(allow).insert(ChildOf(window.rows));
}

/// The About page's link to the commit this build is of.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct CommitLink;

fn about_page(In(page): In<Entity>, mut commands: Commands) {
    // Hidden until the server has said which repo the build is of: `refresh_about` gives it its address.
    let build = bsn_list! {
        @row()
        Children [
            @text("", size::BODY) Shows::Build
            -- @link_button(Icon::ExternalLink, "View the commit", "", ButtonKind::Plain) CommitLink Node { display: Display::None }
        ]
    };
    commands.spawn_scene(setting("Build", "", build)).insert(ChildOf(page));
    commands.spawn_scene(value(Shows::Update, "Updates", "")).insert(ChildOf(page));
    commands
        .spawn_scene(value(Shows::Apps, "Apps in this window", "Each app's name, and its id."))
        .insert(ChildOf(page));
    let note = "The theme, where the window was, and what each app keeps (apps/<id>.json).";
    commands.spawn_scene(value(Shows::Files, "Files", note)).insert(ChildOf(page));
}

/// What the Connection page says of the link: live, or what it is doing and why it isn't.
pub fn state_text(connection: Connection, status: &ConnectionStatus) -> String {
    let state = match connection {
        Connection::Live => return "Live".into(),
        Connection::Syncing => "Syncing",
        Connection::Offline => "Offline",
    };
    match &status.reason {
        Some(reason) => format!("{state}: {reason}"),
        None => state.into(),
    }
}

/// Who the token acts as, which is only known while signed in.
pub fn identity_text(connection: Connection, api: Option<&Api>) -> String {
    match api {
        Some(api) if connection == Connection::Live => api.0.user.clone(),
        _ => "not signed in".into(),
    }
}

/// What the About page says of the build.
pub fn build_text(this: Option<&ThisBuild>) -> String {
    match this {
        Some(build) => build.0.clone(),
        None => "Run directly, not by the launcher".into(),
    }
}

/// What the About page says of a newer build.
pub fn update_text(this: Option<&ThisBuild>, ready: Option<&UpdateReady>) -> String {
    if this.is_none() {
        return "Not looked for: only a window the launcher started (trailhead app) offers to update".into();
    }
    match ready.and_then(|r| r.0.as_ref()) {
        Some(newer) => match newer.total {
            0 => format!("A newer build is ready: {}", newer.rev),
            1 => format!("A newer build is ready: {}, with 1 change", newer.rev),
            n => format!("A newer build is ready: {}, with {n} changes", newer.rev),
        },
        None => "This is the newest build".into(),
    }
}

/// The apps the window holds, one per line, with their ids.
pub fn apps_text(apps: &Apps) -> String {
    let lines: Vec<String> = apps.iter().map(|m| format!("{} ({})", m.name, m.id.0)).collect();
    lines.join("\n")
}

/// Where the shell keeps its files.
pub fn files_text(dir: Option<&ShellDir>) -> String {
    match dir {
        Some(dir) => dir.0.display().to_string(),
        None => "Nowhere: this window keeps nothing on this machine".into(),
    }
}

/// Keeps the Connection page's texts current, and Re-dial pressable only with a server to dial.
#[expect(clippy::too_many_arguments)]
fn refresh_connection(
    mut texts: Query<(&Shows, &mut Text)>,
    acts: Query<(Entity, &ConnectionAct, Has<InteractionDisabled>)>,
    dialed: Res<Dialed>,
    endpoint: Res<LocalEndpoint>,
    connection: Res<State<Connection>>,
    status: Res<ConnectionStatus>,
    api: Option<Res<Api>>,
    mut commands: Commands,
) {
    for (which, text) in &mut texts {
        let wanted = match which {
            Shows::Server => dialed.0.clone().unwrap_or_else(|| "none dialed".into()),
            Shows::State => state_text(*connection.get(), &status),
            Shows::Identity => identity_text(*connection.get(), api.as_deref()),
            Shows::Endpoint => endpoint.0.clone().unwrap_or_else(|| "starting".into()),
            Shows::Allow => allow_command(&endpoint).unwrap_or_else(|| "once the endpoint is up".into()),
            _ => continue,
        };
        set_text(text, &wanted);
    }
    for (button, act, disabled) in &acts {
        let off = match act {
            ConnectionAct::Redial => dialed.0.is_none(),
            ConnectionAct::CopyEndpoint | ConnectionAct::CopyAllow => endpoint.0.is_none(),
            ConnectionAct::Disconnect => false,
        };
        if off && !disabled {
            commands.entity(button).insert(InteractionDisabled);
        } else if !off && disabled {
            commands.entity(button).remove::<InteractionDisabled>();
        }
    }
}

/// Keeps the About page's link to the build's commit: shown once the server has said which repo the build is of,
/// hidden in a shell run directly.
fn show_commit_link(
    mut links: Query<(Entity, &mut ExternalLink), With<CommitLink>>,
    mut nodes: Query<&mut Node>,
    this: Option<Res<ThisBuild>>,
    repo: Option<Res<BuildRepo>>,
) {
    let commit = this.as_deref().zip(repo.as_deref()).and_then(|(build, repo)| repo.commit_url(&build.0));
    for (link, mut to) in &mut links {
        if let Some(url) = commit.as_ref().filter(|url| **url != to.url) {
            to.url = url.clone();
        }
        set_display(&mut nodes, link, commit.is_some());
    }
}

/// Keeps the About page's texts current.
fn refresh_about(
    mut texts: Query<(&Shows, &mut Text)>,
    this: Option<Res<ThisBuild>>,
    ready: Option<Res<UpdateReady>>,
    apps: Res<Apps>,
    dir: Option<Res<ShellDir>>,
) {
    for (which, text) in &mut texts {
        let wanted = match which {
            Shows::Build => build_text(this.as_deref()),
            Shows::Update => update_text(this.as_deref(), ready.as_deref()),
            Shows::Apps => apps_text(&apps),
            Shows::Files => files_text(dir.as_deref()),
            _ => continue,
        };
        set_text(text, &wanted);
    }
}

/// Shows a theme chosen elsewhere (the desktop's turning, another window) on the theme's buttons.
fn show_theme(appearance: Option<Res<Appearance>>, mut groups: Query<&mut ButtonGroup, With<ThemePick>>) {
    let Some(appearance) = appearance else { return };
    let at = ThemeChoice::ALL.iter().position(|c| *c == appearance.choice);
    for mut group in &mut groups {
        group.set_if_neq(ButtonGroup { chosen: at });
    }
}

/// Shows a size picked with the keys on the text size's buttons.
fn show_text_size(text_size: Res<TextSize>, mut groups: Query<&mut ButtonGroup, With<TextSizePick>>) {
    for mut group in &mut groups {
        group.set_if_neq(ButtonGroup { chosen: Some(text_size.step()) });
    }
}

/// A theme picked: wear it. The window keeps it for the next launch.
fn on_theme_pick(picked: On<Picked>, picks: Query<(), With<ThemePick>>, appearance: Option<ResMut<Appearance>>) {
    let (true, Some(mut appearance)) = (picks.contains(picked.entity), appearance) else { return };
    if let Some(choice) = ThemeChoice::ALL.get(picked.index) {
        appearance.choice = *choice;
    }
}

/// A text size picked: draw at it, as the keys would. The shell keeps it for the next launch.
fn on_text_size_pick(picked: On<Picked>, picks: Query<(), With<TextSizePick>>, mut commands: Commands) {
    if picks.contains(picked.entity)
        && let Some(to) = TextSize::at(picked.index)
    {
        commands.trigger(ChangeTextSize::To(to));
    }
}

/// Re-dial and Disconnect. Disconnect closes the settings too, so the connect panel under them is what's left.
fn on_connection_act(
    activate: On<Activate>,
    acts: Query<&ConnectionAct>,
    dialed: Res<Dialed>,
    mut send: MessageWriter<SendNet>,
    mut commands: Commands,
) {
    match acts.get(activate.entity) {
        Ok(ConnectionAct::Redial) => {
            if let Some(target) = &dialed.0 {
                send.write(SendNet(NetCommand::Connect(target.clone())));
            }
        }
        Ok(ConnectionAct::Disconnect) => {
            send.write(SendNet(NetCommand::Disconnect));
            commands.trigger(CloseSettings);
        }
        _ => {}
    }
}

/// The copy buttons, with the window's clipboard.
fn on_copy(
    activate: On<Activate>,
    acts: Query<&ConnectionAct>,
    endpoint: Res<LocalEndpoint>,
    clipboard: Option<ResMut<Clipboard>>,
) {
    let words = match acts.get(activate.entity) {
        Ok(ConnectionAct::CopyEndpoint) => endpoint.0.clone(),
        Ok(ConnectionAct::CopyAllow) => allow_command(&endpoint),
        _ => return,
    };
    if let (Some(words), Some(mut clipboard)) = (words, clipboard)
        && let Err(e) = clipboard.set_text(words)
    {
        warn!(error = %e, "couldn't copy to the clipboard");
    }
}

/// The shell's pages: what keeps them current and what their controls do.
pub(crate) struct SettingsPagesUiPlugin;

impl Plugin for SettingsPagesUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_theme_pick)
            .add_observer(on_text_size_pick)
            .add_systems(Update, (refresh_about, show_commit_link, show_theme, show_text_size).before(UiPlace));
        if app.world().contains_resource::<State<Connection>>() {
            app.add_observer(on_connection_act)
                .add_observer(on_copy)
                .add_systems(Update, refresh_connection.before(UiPlace));
        }
    }
}
