//! The Trailhead shell: one window that holds several apps (`docs/app-platform.md`).
//!
//! This crate is the contract an app is written against. An app is a type that implements [`ShellApp`]: it names
//! itself in a [`Manifest`] and adds its systems to the Bevy app. The shell decides which app is shown, as the
//! [`ActiveApp`] state, and an app hangs everything it does on that state:
//!
//! - It spawns what it draws in `OnEnter(ActiveApp(ID))` and despawns it in `OnExit(ActiveApp(ID))`. An app that is
//!   switched away from is torn down, and built again when it is switched back to.
//! - Its systems run under `in_state(ActiveApp(ID))`.
//! - Its own views are a sub-state of [`ActiveApp`], so leaving the app leaves the view, which tears that down too.
//!   Added with [`RememberView::add_remembered_view`], the app comes back to the view it was on.
//! - What must outlive a switch or a restart (the view it was on, an unsent draft) goes in the [`LocalStore`].
//!
//! An app draws beside the shell's rail of apps: it fills `trailhead_ui::beside_rail`, or has a
//! `trailhead_ui::sidebar` and fills `beside_sidebar`. Everything it draws comes from the `trailhead-ui` kit.
//!
//! The shell holds the one connection to the Trailhead server ([`connection`]), which every app shares: its state,
//! the mirror of the task engine's records ([`mirror`]), and the outbox of [`commands`]. An app reads the API with a
//! [`net::Fetch`], and runs work of its own on the net thread while a control plane is held as
//! [`net::LinkJob`]s, which it gives the shell from [`ShellApp::link_jobs`].
//!
//! The rail switches apps through [`transition`]: a cover closes over the app shown, the app is switched while it is
//! covered, and the cover opens on the app picked, as the person picked in the settings.
//!
//! A binary builds a [`Shell`], gives it its apps in the order the rail shows them, and runs it. Underneath is
//! [`ShellPlugin`], which runs headless under `MinimalPlugins` with `StatesPlugin` for tests;
//! [`ShellPlugin::windowed`] adds the rail, the panel that says why the connection is not live ([`connect`]), and
//! each app's drawing. While connected to a server with an apps repo, the rail's Ask button asks it for a change to an
//! app or a new one ([`ask`]). The gear under it opens the window's [`settings`], whose pages are the shell's own and
//! those each app adds with [`ShellApp::settings_pages`].

use std::path::PathBuf;

use bevy::prelude::*;
use trailhead_ui::{Icon, TextSize, TextSizePlugin, Theme};

use crate::connection::{Connection, ConnectionPlugin, SendNet};
use crate::net::{LinkJob, NetCommand, NetConfig};
use crate::settings::SettingsPage;
use crate::transition::{AppTransition, TRANSITION, TransitionKind, TransitionPlugin};

pub mod ask;
mod ask_panel;
pub mod commands;
pub mod connect;
pub mod connection;
pub mod cover;
#[cfg(any(test, feature = "test-support"))]
pub mod fixture;
pub mod icon;
pub mod lint;
pub mod mirror;
pub mod net;
mod place;
mod rail;
pub mod settings;
mod settings_modal;
mod settings_pages;
mod store;
pub mod sync;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod transition;
pub mod update;
mod update_panel;
pub mod view;
mod window;

pub use rail::{AskButton, OpenApp, Rail, SettingsButton, UpdateButton};
pub use store::LocalStore;
pub use view::RememberView;
pub use window::{Shell, ThemeFile, theme_from_word, theme_word};

/// An app's name in code: what its state, its local store and its files are keyed by. Lowercase, and never changed
/// once the app has stored anything.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AppId(pub &'static str);

/// What the shell shows of an app before any of the app's own code runs.
#[derive(Clone, Copy, Debug)]
pub struct Manifest {
    pub id: AppId,
    /// The name a person sees, under the pointer on the rail.
    pub name: &'static str,
    /// The app's button on the rail.
    pub icon: Icon,
}

/// The app the shell is showing. An app gates its systems and its views on its own value of this.
#[derive(States, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ActiveApp(pub AppId);

/// The shell's apps, in the order the rail shows them.
#[derive(Resource, Debug)]
pub struct Apps(Vec<Manifest>);

impl Apps {
    /// The apps given, as the shell holds them: a test of an app's drawing outside a shell makes its own.
    pub fn new(apps: Vec<Manifest>) -> Self {
        Apps(apps)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Manifest> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the shell draws a rail: only for more than one app. Without one there is no settings gear at its foot,
    /// and an app with a place for one puts a [`settings::SettingsGear`] of its own there.
    pub fn has_rail(&self) -> bool {
        self.len() > 1
    }
}

/// An app the shell can hold.
pub trait ShellApp: Send + Sync + 'static {
    fn manifest(&self) -> Manifest;

    /// Adds the app's state, resources and logic. Runs headless, so tests build an app without a window.
    fn build(&self, app: &mut App);

    /// Adds what spawns and draws. Only a windowed shell calls it, after [`ShellApp::build`].
    fn build_ui(&self, _app: &mut App) {}

    /// Paints the app's own tokens onto the theme every app shares: the colors that encode its data
    /// (`Theme::paint`) and its aliases of the kit's meanings (`Theme::alias`).
    fn paint(&self, _theme: &mut Theme) {}

    /// The app's work on the net thread while a control plane is held, such as a stream of its own. A connected
    /// shell takes them once, before it calls [`ShellApp::build`]; they report over the app's own channels, whose
    /// other ends the app adds in `build`.
    fn link_jobs(&self) -> Vec<Box<dyn LinkJob>> {
        Vec::new()
    }

    /// The app's pages in the shell's settings, listed under its name after the shell's own. The shell takes them
    /// once, after [`ShellApp::build`]; each page's fill runs when the page is shown ([`settings::SettingsPage`]).
    fn settings_pages(&self) -> Vec<SettingsPage> {
        Vec::new()
    }
}

/// The shell's own entry in the [`LocalStore`].
pub(crate) const SHELL: AppId = AppId("shell");
/// Where the shell keeps the id of the app last shown.
const ACTIVE: &str = "active";
/// Where the shell keeps the [`TextSize`] picked, in percent, for every app.
const TEXT_SIZE: &str = "text_size";

/// The shell and its apps. The first app given is shown at first, unless the [`LocalStore`] remembers another.
#[derive(Default)]
pub struct ShellPlugin {
    apps: Vec<Box<dyn ShellApp>>,
    windowed: bool,
    store: Option<PathBuf>,
    /// The net configuration, and the control plane to dial ahead of the one dialed last time.
    connection: Option<(NetConfig, Option<String>)>,
    /// A user token to sign in with at startup.
    token: Option<String>,
    /// The build the launcher started this shell from, which turns on the update notice ([`update`]).
    build: Option<String>,
}

impl ShellPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an app, after the ones already given.
    #[must_use]
    pub fn with(mut self, app: impl ShellApp) -> Self {
        self.apps.push(Box::new(app));
        self
    }

    /// Draws: the rail, and each app's [`ShellApp::build_ui`].
    #[must_use]
    pub fn windowed(mut self) -> Self {
        self.windowed = true;
        self
    }

    /// The kit's theme with every app's tokens painted on, for the kit's `ThemePlugin`.
    pub fn theme(&self) -> Theme {
        let mut theme = Theme::new();
        for shell_app in &self.apps {
            shell_app.paint(&mut theme);
        }
        theme
    }

    /// Keeps the [`LocalStore`] in this directory, one file an app. Without it nothing outlives the process.
    #[must_use]
    pub fn store_in(mut self, dir: PathBuf) -> Self {
        self.store = Some(dir);
        self
    }

    /// Runs the connection ([`ConnectionPlugin`]) with the apps' [`ShellApp::link_jobs`], dialing `target`, or the
    /// control plane dialed last time without one. Without this the shell has no connection, and an app that needs
    /// one can't be built.
    #[must_use]
    pub fn connecting(mut self, config: NetConfig, target: Option<String>) -> Self {
        self.connection = Some((config, target));
        self
    }

    /// Signs in with this user token at startup, when connected.
    #[must_use]
    pub fn signing_in_with(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }

    /// Says which build the launcher started this shell from. While connected, the shell then offers to restart
    /// onto a newer one ([`update`]); without it there is no notice.
    #[must_use]
    pub fn built_as(mut self, build: Option<String>) -> Self {
        self.build = build;
        self
    }
}

impl Plugin for ShellPlugin {
    fn build(&self, app: &mut App) {
        let manifests: Vec<Manifest> = self.apps.iter().map(|a| a.manifest()).collect();
        assert!(!manifests.is_empty(), "a shell needs at least one app");
        for (at, manifest) in manifests.iter().enumerate() {
            assert!(manifest.id != SHELL, "`{}` is the shell's own id", SHELL.0);
            assert!(manifests[..at].iter().all(|m| m.id != manifest.id), "two apps are called `{}`", manifest.id.0);
        }
        let ids: Vec<AppId> = manifests.iter().map(|m| m.id).chain([SHELL]).collect();
        let store = LocalStore::open(self.store.clone(), &ids);
        let remembered: Option<String> = store.get(SHELL, ACTIVE);
        let first = manifests.iter().find(|m| Some(m.id.0) == remembered.as_deref()).unwrap_or(&manifests[0]).id;
        // Set as the app is built, so the first frame is laid out at the size kept and none is drawn at 100% first.
        let text_size = store.get::<u32>(SHELL, TEXT_SIZE).map(TextSize::nearest);
        let transition = store.get::<String>(SHELL, TRANSITION).and_then(|w| TransitionKind::from_word(&w));
        if !app.is_plugin_added::<TextSizePlugin>() {
            app.add_plugins(TextSizePlugin);
        }
        if let Some(text_size) = text_size {
            app.insert_resource(text_size);
        }
        app.insert_resource(Apps::new(manifests))
            .insert_resource(store)
            .insert_state(ActiveApp(first))
            .insert_resource(AppTransition(transition.unwrap_or_default()))
            .add_plugins(TransitionPlugin)
            .add_systems(Startup, spawn_camera)
            .add_systems(Update, ((remember_active, remember_text_size, remember_transition), store::save).chain());
        if let Some((config, target)) = &self.connection {
            let jobs = self.apps.iter().flat_map(|a| a.link_jobs());
            app.add_plugins(ConnectionPlugin::new(config.clone(), target.clone()).with_jobs(jobs));
            if let Some(token) = self.token.clone() {
                app.add_systems(Startup, move |mut send: MessageWriter<SendNet>| {
                    send.write(SendNet(NetCommand::Login(token.clone())));
                });
            }
        }
        if let Some(build) = &self.build {
            app.add_plugins(update::UpdatePlugin(build.clone()));
        }
        app.add_plugins((ask::AskPlugin, settings::SettingsPlugin));
        for shell_app in &self.apps {
            shell_app.build(app);
        }
        let connected = app.world().contains_resource::<State<Connection>>();
        let mut pages = settings_pages::shell_pages(connected);
        for shell_app in &self.apps {
            let manifest = shell_app.manifest();
            pages.extend(shell_app.settings_pages().into_iter().map(|page| (manifest.id, manifest.name, page)));
        }
        settings::register(app, pages);
        if self.windowed {
            app.add_plugins((
                rail::RailUiPlugin,
                settings_modal::SettingsModalUiPlugin,
                cover::CoverUiPlugin,
            ));
            if self.build.is_some() {
                app.add_plugins(update_panel::UpdatePanelUiPlugin);
            }
            app.add_plugins(ask_panel::AskPanelUiPlugin);
            if self.connection.is_some() {
                app.add_plugins(connect::ConnectUiPlugin);
            }
            for shell_app in &self.apps {
                shell_app.build_ui(app);
            }
        }
    }
}

/// The one camera every app's UI is drawn through.
fn spawn_camera(mut commands: Commands) {
    commands.spawn(Camera2d);
}

fn remember_active(active: Res<State<ActiveApp>>, mut store: ResMut<LocalStore>) {
    if active.is_changed() {
        store.set(SHELL, ACTIVE, active.get().0.0);
    }
}

/// Keeps the way of switching apps picked for the next launch.
fn remember_transition(picked: Res<AppTransition>, mut store: ResMut<LocalStore>) {
    if picked.is_changed() {
        store.set(SHELL, TRANSITION, picked.0.word());
    }
}

/// Keeps the text size picked (with the kit's keys, or by an app) for the next launch.
fn remember_text_size(size: Res<TextSize>, mut store: ResMut<LocalStore>) {
    if size.is_changed() {
        store.set(SHELL, TEXT_SIZE, size.percent());
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    use bevy::state::app::StatesPlugin;

    use super::*;
    use crate::connection::Connection;

    pub(crate) const NOTES: AppId = AppId("notes");
    pub(crate) const MAPS: AppId = AppId("maps");

    /// What the test apps saw of the shell's switching.
    #[derive(Resource, Default, Debug, PartialEq, Eq)]
    pub(crate) struct Seen(pub Vec<String>);

    pub(crate) struct Fake(pub AppId);

    impl ShellApp for Fake {
        fn manifest(&self) -> Manifest {
            Manifest { id: self.0, name: self.0.0, icon: Icon::Board }
        }

        fn build(&self, app: &mut App) {
            let id = self.0;
            app.init_resource::<Seen>()
                .add_systems(OnEnter(ActiveApp(id)), move |mut seen: ResMut<Seen>| {
                    seen.0.push(format!("enter {}", id.0))
                })
                .add_systems(OnExit(ActiveApp(id)), move |mut seen: ResMut<Seen>| {
                    seen.0.push(format!("exit {}", id.0))
                });
        }

        fn build_ui(&self, app: &mut App) {
            let id = self.0;
            app.add_systems(Startup, move |mut seen: ResMut<Seen>| seen.0.push(format!("draws {}", id.0)));
        }
    }

    pub(crate) fn app(shell: ShellPlugin) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin)).add_plugins(shell);
        app.update();
        app
    }

    fn active(app: &App) -> AppId {
        app.world().resource::<State<ActiveApp>>().get().0
    }

    fn show(app: &mut App, id: AppId) {
        app.world_mut().resource_mut::<NextState<ActiveApp>>().set(ActiveApp(id));
        app.update();
    }

    #[test]
    fn the_first_app_is_shown_and_a_switch_leaves_one_app_and_enters_the_other() {
        let mut app = app(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        assert_eq!(active(&app), NOTES);
        show(&mut app, MAPS);
        assert_eq!(active(&app), MAPS);
        assert_eq!(app.world().resource::<Seen>().0, ["enter notes", "exit notes", "enter maps"]);
        assert_eq!(app.world().resource::<Apps>().iter().map(|m| m.id).collect::<Vec<_>>(), [NOTES, MAPS]);
    }

    #[test]
    fn there_is_one_camera_and_a_headless_shell_draws_nothing() {
        let mut app = app(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        let world = app.world_mut();
        assert_eq!(world.query::<&Camera2d>().iter(world).count(), 1);
        assert_eq!(world.query::<&Node>().iter(world).count(), 0);
        assert!(!world.resource::<Seen>().0.iter().any(|s| s.starts_with("draws")));
    }

    #[test]
    fn the_app_last_shown_is_shown_again_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let shell = || ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).store_in(dir.path().to_path_buf());
        let mut first = app(shell());
        show(&mut first, MAPS);
        first.update();
        drop(first);
        assert_eq!(active(&app(shell())), MAPS);
        // An app that is no longer in the shell is forgotten, not shown.
        let without = app(ShellPlugin::new().with(Fake(NOTES)).store_in(dir.path().to_path_buf()));
        assert_eq!(active(&without), NOTES);
    }

    #[test]
    fn the_text_size_picked_is_drawn_from_the_first_frame_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let shell = || ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).store_in(dir.path().to_path_buf());
        let mut first = app(shell());
        assert_eq!(*first.world().resource::<TextSize>(), TextSize::DEFAULT);
        first.world_mut().trigger(trailhead_ui::ChangeTextSize::Bigger);
        first.world_mut().trigger(trailhead_ui::ChangeTextSize::Bigger);
        first.update();
        drop(first);
        let again = app(shell());
        assert_eq!(again.world().resource::<TextSize>().label(), "125%");
        assert_eq!(again.world().resource::<UiScale>().0, 1.25, "laid out at it from the first frame");
        // Kept for the shell, not for an app.
        let kept = LocalStore::open(Some(dir.path().to_path_buf()), &[SHELL, NOTES]);
        assert_eq!((kept.get::<u32>(SHELL, TEXT_SIZE), kept.get::<u32>(NOTES, TEXT_SIZE)), (Some(125), None));
    }

    /// An app with a link job, noting when the shell takes its jobs and when it builds it.
    struct Linked(Arc<Mutex<Vec<&'static str>>>);

    struct Idle;

    impl LinkJob for Idle {
        fn start(&mut self, _link: net::HeldLink) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(std::future::pending())
        }
    }

    impl ShellApp for Linked {
        fn manifest(&self) -> Manifest {
            Manifest { id: NOTES, name: "notes", icon: Icon::Board }
        }

        fn link_jobs(&self) -> Vec<Box<dyn LinkJob>> {
            self.0.lock().unwrap().push("jobs");
            vec![Box::new(Idle)]
        }

        fn build(&self, _app: &mut App) {
            self.0.lock().unwrap().push("build");
        }
    }

    #[test]
    fn a_connected_shell_takes_the_link_jobs_before_it_builds_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let config = NetConfig {
            config_dir: dir.path().join("shell"),
            trailhead_config_dir: dir.path().join("config"),
            trailhead_data_dir: dir.path().join("data"),
            keyring: false,
            network: net::Network::Direct(Arc::new(iroh::address_lookup::MemoryLookup::new())),
            status_retry: net::STATUS_RETRY,
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let app = app(ShellPlugin::new().with(Linked(seen.clone())).connecting(config, None));
        assert_eq!(*seen.lock().unwrap(), ["jobs", "build"]);
        assert_eq!(*app.world().resource::<State<Connection>>().get(), Connection::Offline);
    }

    #[test]
    fn a_shell_not_told_how_to_connect_has_no_connection() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let app = app(ShellPlugin::new().with(Linked(seen.clone())));
        assert_eq!(*seen.lock().unwrap(), ["build"]);
        assert!(app.world().get_resource::<State<Connection>>().is_none());
    }

    #[test]
    #[should_panic(expected = "two apps are called `notes`")]
    fn two_apps_with_one_id_are_refused() {
        let _ = app(ShellPlugin::new().with(Fake(NOTES)).with(Fake(NOTES)));
    }
}
