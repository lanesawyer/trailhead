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
//! [`net::LinkJob`]s, which it gives the shell from [`ShellApp::link_jobs`]. Its own data in the app data store it
//! declares with [`ShellApp::collections`]: the shell mirrors those records ([`records`]) beside the tasks.
//!
//! The rail switches apps through [`transition`]: a cover closes over the app shown, the app is switched while it is
//! covered, and the cover opens on the app picked, as the person picked in the settings.
//!
//! A binary builds a [`Shell`], gives it its apps in the order the rail shows them until a person drags them into
//! their own ([`MoveApp`]), and runs it. Underneath is
//! [`ShellPlugin`], which runs headless under `MinimalPlugins` with `StatesPlugin` for tests;
//! [`ShellPlugin::windowed`] adds the rail, the panel that says why the connection is not live ([`connect`]), and
//! each app's drawing. While connected to a server with an apps repo, the rail's Ask button asks it for a change to an
//! app or a new one ([`ask`]). The bell under it, with the count of unread [`notifications`], opens their panel, and
//! the gear under that opens the window's [`settings`], whose pages are the shell's own and those each app adds with
//! [`ShellApp::settings_pages`].

use std::path::PathBuf;

use bevy::prelude::*;
use trailhead_ui::{Icon, TextSize, TextSizePlugin, Theme};

use crate::connection::{Connection, ConnectionPlugin, SendNet};
use crate::net::{LinkJob, NetCommand, NetConfig};
use crate::records::{Collections, RecordMirror};
use crate::settings::SettingsPage;
use crate::transition::{AppTransition, TRANSITION, TransitionKind, TransitionPlugin};

pub mod ask;
mod ask_panel;
pub mod commands;
pub mod connect;
pub mod connection;
pub mod connector_settings;
mod connectors_page;
pub mod cover;
pub mod desktop;
#[cfg(any(test, feature = "test-support"))]
pub mod fixture;
pub mod icon;
pub mod lint;
pub mod mirror;
pub mod net;
pub mod notification_settings;
pub mod notifications;
mod notifications_page;
mod notifications_panel;
mod place;
mod previews;
mod rail;
pub mod record_sync;
pub mod records;
mod server_page;
pub mod server_settings;
pub mod settings;
mod settings_modal;
mod settings_pages;
pub mod sound;
mod store;
pub mod sync;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod transition;
pub mod update;
mod update_panel;
pub mod view;
mod window;

pub use notifications_panel::{
    BellCount, CardPart, EmptyLine, FilterPick, MarkAllRead, MarkOne, NotificationCard, NotificationFilter,
    NotificationList, NotificationsPanel, OpenNotifications, PanelNotice, ShowOlderButton,
};
pub use rail::{AppCount, AskButton, BellButton, NewAppButton, OpenApp, Rail, SettingsButton, UpdateButton};
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

/// The shell's apps, in the order the rail shows them: the person's, once they have moved one ([`MoveApp`]), else
/// the order the binary gave them in.
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

    /// Where `id` is on the rail, from the top.
    pub fn position(&self, id: AppId) -> Option<usize> {
        self.0.iter().position(|m| m.id == id)
    }

    /// Moves `id` to place `to` (or the end, past it), the others keeping their order. Whether it moved.
    pub(crate) fn move_app(&mut self, id: AppId, to: usize) -> bool {
        let Some(at) = self.position(id) else { return false };
        let to = to.min(self.0.len() - 1);
        if at == to {
            return false;
        }
        let app = self.0.remove(at);
        self.0.insert(to, app);
        true
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

    /// What the app keeps in the app data store: its `collections.toml`, and the collections it reads, each with the
    /// type its records' data deserializes into. A connected shell mirrors them ([`records`]); the shell takes them
    /// once, before [`ShellApp::build`]. Nothing by default.
    fn collections(&self) -> Collections {
        Collections::none()
    }
}

/// Moves an app to place `to` on the rail (and everywhere that follows its order), keeping the order for the next
/// launch: what dragging an app's button on the rail, or Alt and Up or Down on it, does.
#[derive(Event, Clone, Copy, PartialEq, Eq, Debug)]
pub struct MoveApp {
    pub app: AppId,
    pub to: usize,
}

/// The shell's own entry in the [`LocalStore`].
pub(crate) const SHELL: AppId = AppId("shell");
/// Where the shell keeps the id of the app last shown.
const ACTIVE: &str = "active";
/// Where the shell keeps the [`TextSize`] picked, in percent, for every app.
const TEXT_SIZE: &str = "text_size";
/// Where the shell keeps the person's order of the apps on the rail, as their ids.
const RAIL_ORDER: &str = "rail_order";

/// The apps `listed` in the person's order `kept`: those it names first, in its order, then the rest as listed. An id
/// it names that isn't listed (an app no longer in the shell) is dropped.
fn in_kept_order(mut listed: Vec<Manifest>, kept: Option<Vec<String>>) -> Vec<Manifest> {
    let mut ordered = Vec::with_capacity(listed.len());
    for id in kept.unwrap_or_default() {
        if let Some(at) = listed.iter().position(|m| m.id.0 == id) {
            ordered.push(listed.remove(at));
        }
    }
    ordered.extend(listed);
    ordered
}

/// The shell and its apps, on the rail in the order given until the person moves them. The first app on the rail is
/// shown at first, unless the [`LocalStore`] remembers another.
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
        let manifests = in_kept_order(manifests, store.get(SHELL, RAIL_ORDER));
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
            .add_observer(on_move_app)
            .add_systems(Startup, spawn_camera)
            .add_systems(Update, ((remember_active, remember_text_size, remember_transition), store::save).chain());
        let kept: Vec<(AppId, Collections)> = self
            .apps
            .iter()
            .map(|a| (a.manifest().id, a.collections()))
            .filter(|(_, collections)| !collections.is_empty())
            .collect();
        if let Some((config, target)) = &self.connection {
            let jobs = self.apps.iter().flat_map(|a| a.link_jobs());
            let records = kept.iter().map(|(id, collections)| collections.spec(*id));
            app.add_plugins(
                ConnectionPlugin::new(config.clone(), target.clone()).with_jobs(jobs).with_records(records),
            );
            if let Some(token) = self.token.clone() {
                app.add_systems(Startup, move |mut send: MessageWriter<SendNet>| {
                    send.write(SendNet(NetCommand::Login(token.clone())));
                });
            }
        }
        if let Some(build) = &self.build {
            app.add_plugins(update::UpdatePlugin(build.clone()));
        }
        if let Some(mut mirror) = app.world_mut().get_resource_mut::<RecordMirror>() {
            for (id, collections) in &kept {
                mirror.keep(*id, collections);
            }
        }
        app.add_plugins((ask::AskPlugin, settings::SettingsPlugin));
        for shell_app in &self.apps {
            shell_app.build(app);
        }
        let connected = app.world().contains_resource::<State<Connection>>();
        if connected {
            app.add_plugins((
                server_settings::ServerSettingsPlugin,
                connector_settings::ConnectorSettingsPlugin,
                notifications::NotificationsPlugin,
                notification_settings::NotificationSettingsPlugin,
            ));
        }
        let mut pages = settings_pages::shell_pages(connected);
        let apps = app.world().resource::<Apps>();
        let mut on_rail: Vec<&dyn ShellApp> = self.apps.iter().map(|a| a.as_ref()).collect();
        on_rail.sort_by_key(|a| apps.position(a.manifest().id));
        for shell_app in on_rail {
            let manifest = shell_app.manifest();
            pages.extend(shell_app.settings_pages().into_iter().map(|page| (manifest.id, manifest.name, page)));
        }
        settings::register(app, pages);
        if self.windowed {
            app.add_plugins((
                rail::RailUiPlugin,
                settings_modal::SettingsModalUiPlugin,
                cover::CoverUiPlugin,
                previews::PreviewsUiPlugin,
            ));
            if self.build.is_some() {
                app.add_plugins(update_panel::UpdatePanelUiPlugin);
            }
            app.add_plugins(ask_panel::AskPanelUiPlugin);
            if self.connection.is_some() {
                app.add_plugins(connect::ConnectUiPlugin);
            }
            if connected {
                app.add_plugins((
                    server_page::ServerPageUiPlugin,
                    connectors_page::ConnectorsPageUiPlugin,
                    notifications_page::NotificationsPageUiPlugin,
                    notifications_panel::NotificationsUiPlugin,
                    desktop::DesktopPlugin,
                ));
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

/// Moves an app on the rail, keeps the order, and lists the apps' settings pages in it.
fn on_move_app(
    moved: On<MoveApp>,
    mut apps: ResMut<Apps>,
    mut pages: ResMut<settings::SettingsPages>,
    mut store: ResMut<LocalStore>,
) {
    if !apps.move_app(moved.app, moved.to) {
        return;
    }
    let order: Vec<&str> = apps.iter().map(|m| m.id.0).collect();
    store.set(SHELL, RAIL_ORDER, order);
    pages.follow(&apps);
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
                    seen.0.push(format!("enter {}", id.0));
                })
                .add_systems(OnExit(ActiveApp(id)), move |mut seen: ResMut<Seen>| {
                    seen.0.push(format!("exit {}", id.0));
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

    const TREES: AppId = AppId("trees");

    fn ids(app: &App) -> Vec<AppId> {
        app.world().resource::<Apps>().iter().map(|m| m.id).collect()
    }

    #[test]
    fn the_persons_order_comes_first_then_new_apps_as_listed_and_gone_ones_are_dropped() {
        let listed = |ids: &[AppId]| ids.iter().map(|id| Manifest { id: *id, name: id.0, icon: Icon::Board }).collect();
        let kept = |ids: &[&str]| Some(ids.iter().map(|s| s.to_string()).collect());
        let order = |m: Vec<Manifest>| m.into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(order(in_kept_order(listed(&[NOTES, MAPS, TREES]), None)), [NOTES, MAPS, TREES]);
        let moved = in_kept_order(listed(&[NOTES, MAPS, TREES]), kept(&["trees", "gone", "notes", "trees"]));
        assert_eq!(order(moved), [TREES, NOTES, MAPS]);
    }

    #[test]
    fn a_move_is_kept_and_a_restart_shows_the_rail_in_it_with_a_new_app_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let at = || dir.path().to_path_buf();
        let mut first = app(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).with(Fake(TREES)).store_in(at()));
        first.world_mut().trigger(MoveApp { app: TREES, to: 0 });
        first.world_mut().trigger(MoveApp { app: NOTES, to: 9 });
        first.update();
        assert_eq!(ids(&first), [TREES, MAPS, NOTES]);
        // Nothing remembered as shown, so the first on the person's rail is.
        first.world_mut().resource_mut::<LocalStore>().remove(SHELL, ACTIVE);
        first.update();
        drop(first);
        let again = app(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).with(Fake(TREES)).store_in(at()));
        assert_eq!(ids(&again), [TREES, MAPS, NOTES]);
        assert_eq!(active(&again), TREES);
        drop(again);
        // Maps is gone from this build and Lakes is new.
        const LAKES: AppId = AppId("lakes");
        let changed = app(ShellPlugin::new().with(Fake(LAKES)).with(Fake(NOTES)).with(Fake(TREES)).store_in(at()));
        assert_eq!(ids(&changed), [TREES, NOTES, LAKES]);
    }

    struct Paged(AppId);

    impl ShellApp for Paged {
        fn manifest(&self) -> Manifest {
            Manifest { id: self.0, name: self.0.0, icon: Icon::Board }
        }

        fn build(&self, _: &mut App) {}

        fn settings_pages(&self) -> Vec<SettingsPage> {
            vec![SettingsPage::new("page", "Page", |_: In<Entity>| {})]
        }
    }

    #[test]
    fn the_apps_settings_pages_follow_the_rail() {
        let groups = |app: &App| -> Vec<&'static str> {
            let pages = app.world().resource::<settings::SettingsPages>();
            pages.iter().map(|p| p.group).filter(|g| *g != settings::SHELL_GROUP).collect()
        };
        let mut app = app(ShellPlugin::new().with(Paged(NOTES)).with(Paged(MAPS)).with(Paged(TREES)));
        assert_eq!(groups(&app), ["notes", "maps", "trees"]);
        app.world_mut().trigger(MoveApp { app: TREES, to: 0 });
        assert_eq!(groups(&app), ["trees", "notes", "maps"]);
        let first = app.world().resource::<settings::SettingsPages>().iter().next().unwrap().group;
        assert_eq!(first, settings::SHELL_GROUP, "the shell's own stay first");
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
