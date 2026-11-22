//! The window's settings: one modal, "Settings", with its pages listed down its side. The shell's own come first,
//! under Trailhead: Appearance (the theme, the text size and how the window switches apps), Notifications, Connection,
//! Server and Connectors (while the shell has a connection) and About. Then each app's, under the app's name, in the
//! order the rail shows the apps, whichever app is shown: an app's settings are found in one place, and the list
//! doesn't change under a person who switches apps.
//!
//! An app adds a page with [`ShellApp::settings_pages`](crate::ShellApp::settings_pages): a [`SettingsPage`], whose
//! fill is a system given the page's box when the page is shown. The page is spawned when it is picked and despawned
//! when another is, and the modal when it is closed, so a page keeps nothing drawn between openings.
//!
//! [`OpenSettings`] opens it, at a page or at the one last shown; the gear at the rail's foot triggers it, as Ctrl and
//! `,` (Cmd on macOS) do from anywhere in the window. A shell with one app has no rail: an app with a place for a gear
//! of its own (Lookout's sidebar footer) puts one there with [`SettingsGear`] on it while
//! [`Apps::has_rail`](crate::Apps::has_rail) is false.
//!
//! What is open is [`SettingsShown`], which this module keeps headless; `settings_modal` draws it.

use bevy::ecs::system::{BoxedSystem, SystemId};
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input_focus::FocusedInput;
use bevy::prelude::*;
use bevy::ui_widgets::Activate;

pub use crate::settings_modal::{
    FoundPage, NoSettingMatches, SettingsModal, SettingsPane, SettingsSearch, SettingsStrip, SettingsTab, ShownPage,
};
pub use crate::settings_pages::{
    ConnectionAct, ShellDir, Shows, TextSizePick, ThemePick, apps_text, build_text, files_text, identity_text,
    state_text, update_text,
};
use crate::{AppId, Apps, SHELL};

/// A settings page's name in code: the app it belongs to (the shell's own pages are `shell`'s) and its key there.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PageId {
    pub app: AppId,
    pub key: &'static str,
}

impl PageId {
    pub const fn new(app: AppId, key: &'static str) -> Self {
        PageId { app, key }
    }
}

/// The theme, the text size and how the window switches apps.
pub const APPEARANCE: PageId = PageId::new(SHELL, "appearance");
/// The server, this window's endpoint, and why the connection isn't live. Listed only in a shell with a connection.
pub const CONNECTION: PageId = PageId::new(SHELL, "connection");
/// What the server's user config says, edited from the window. Listed only in a shell with a connection.
pub const SERVER: PageId = PageId::new(SHELL, "server");
/// The connectors the server runs for the apps: each one's settings, secrets, runs and Sync now. Listed only in a shell
/// with a connection.
pub const CONNECTORS: PageId = PageId::new(SHELL, "connectors");
/// How the person wants to be told: a level per source, quiet hours, the sound and the counts. Listed only in a shell
/// with a connection.
pub const NOTIFICATIONS: PageId = PageId::new(SHELL, "notifications");
/// The build, whether a newer one is ready, the apps and where the shell keeps its files.
pub const ABOUT: PageId = PageId::new(SHELL, "about");

/// What the shell's own pages are listed under.
pub const SHELL_GROUP: &str = "Trailhead";

/// A page of settings an app adds to the shell's modal, from [`ShellApp::settings_pages`](crate::ShellApp).
pub struct SettingsPage {
    key: &'static str,
    title: &'static str,
    fill: BoxedSystem<In<Entity>>,
}

impl SettingsPage {
    /// A page named `key` among the app's pages (so `PageId::new(app_id, key)` opens it) and `title` on its tab.
    /// `fill` is a system run with the page's box (a `settings_page()`) each time the page is shown: it spawns the
    /// page's settings and sections under it, reading what it needs to show as any system does. The page is despawned
    /// when another is picked or the modal closes; keep what it shows current from the app's own systems, in place.
    pub fn new<M>(key: &'static str, title: &'static str, fill: impl IntoSystem<In<Entity>, (), M>) -> Self {
        SettingsPage { key, title, fill: Box::new(IntoSystem::into_system(fill)) }
    }
}

/// Opens the settings at `page`, or, without one, at the page last shown (Appearance the first time). On a page no
/// app added it opens at the first. While open, it turns to the page.
#[derive(Event, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenSettings {
    pub page: Option<PageId>,
}

/// Closes the settings, if they are open.
#[derive(Event, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CloseSettings;

/// On a button that opens the settings: the rail's gear, or an app's own in a shell with one app.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct SettingsGear;

/// A page in the modal's list.
#[derive(Clone, Copy, Debug)]
pub struct PageEntry {
    pub id: PageId,
    pub title: &'static str,
    /// What it is listed under: [`SHELL_GROUP`], or its app's name.
    pub group: &'static str,
    pub(crate) fill: SystemId<In<Entity>>,
}

/// Every page, in the order the modal lists them.
#[derive(Resource, Debug, Default)]
pub struct SettingsPages(Vec<PageEntry>);

impl SettingsPages {
    pub fn iter(&self) -> impl Iterator<Item = &PageEntry> {
        self.0.iter()
    }

    pub fn get(&self, id: PageId) -> Option<&PageEntry> {
        self.0.iter().find(|p| p.id == id)
    }

    /// Lists the apps' pages in the order of the rail, after the shell's own, once the person has moved an app.
    pub(crate) fn follow(&mut self, apps: &Apps) {
        self.0.sort_by_key(|p| if p.id.app == SHELL { None } else { Some(apps.position(p.id.app)) });
    }
}

/// The page the settings show, `None` while they are closed.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SettingsShown {
    page: Option<PageId>,
    /// The page shown last, which the next opening without a page goes back to.
    last: Option<PageId>,
}

impl SettingsShown {
    pub fn page(&self) -> Option<PageId> {
        self.page
    }
}

/// Lists each app's pages after the shell's, under the app's name, registering each page's fill.
pub(crate) fn register(app: &mut App, pages: Vec<(AppId, &'static str, SettingsPage)>) {
    let mut listed: Vec<PageEntry> = Vec::new();
    for (owner, group, page) in pages {
        let id = PageId::new(owner, page.key);
        assert!(listed.iter().all(|p| p.id != id), "`{}` has two settings pages called `{}`", owner.0, page.key);
        let fill = app.world_mut().register_boxed_system(page.fill);
        listed.push(PageEntry { id, title: page.title, group, fill });
    }
    app.insert_resource(SettingsPages(listed));
}

/// The settings' logic: which page is open, and the ways of opening and closing it.
pub(crate) struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SettingsShown>()
            .add_observer(on_open)
            .add_observer(on_close)
            .add_observer(on_gear)
            .add_observer(on_key);
    }
}

fn on_open(open: On<OpenSettings>, pages: Res<SettingsPages>, mut shown: ResMut<SettingsShown>) {
    let listed = |id: PageId| pages.get(id).map(|p| p.id);
    let page = match open.page {
        Some(wanted) => listed(wanted).or_else(|| {
            warn!(app = wanted.app.0, page = wanted.key, "no such settings page; opening the first");
            None
        }),
        None => shown.page.or(shown.last).and_then(listed),
    };
    let Some(page) = page.or_else(|| pages.iter().next().map(|p| p.id)) else { return };
    shown.set_if_neq(SettingsShown { page: Some(page), last: Some(page) });
}

fn on_close(_: On<CloseSettings>, mut shown: ResMut<SettingsShown>) {
    if shown.page.is_some() {
        shown.page = None;
    }
}

fn on_gear(activate: On<Activate>, gears: Query<(), With<SettingsGear>>, mut commands: Commands) {
    if gears.contains(activate.entity) {
        commands.trigger(OpenSettings::default());
    }
}

/// Ctrl (Cmd on macOS) and `,` open the settings from anywhere in the window, taken at the window as the kit's text
/// size keys are, once the press has climbed past everything focused.
fn on_key(
    key: On<FocusedInput<KeyboardInput>>,
    windows: Query<(), With<Window>>,
    held: Option<Res<ButtonInput<KeyCode>>>,
    mut commands: Commands,
) {
    if !windows.contains(key.event_target()) || !key.input.state.is_pressed() || key.input.repeat {
        return;
    }
    let Some(held) = held else { return };
    let command = if cfg!(target_os = "macos") {
        [KeyCode::SuperLeft, KeyCode::SuperRight]
    } else {
        [KeyCode::ControlLeft, KeyCode::ControlRight]
    };
    if !held.any_pressed(command) || held.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]) {
        return;
    }
    let comma = key.input.key_code == KeyCode::Comma || matches!(&key.input.logical_key, Key::Character(c) if c == ",");
    if comma {
        commands.trigger(OpenSettings::default());
    }
}
