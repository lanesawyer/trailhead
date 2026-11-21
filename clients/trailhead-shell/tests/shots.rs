//! The shell's own chrome drawn to PNGs, named `shell-<view>-<theme>`: the rail holding two apps with its Ask and
//! update buttons, the Ask form, the update panel listing what a newer build brings, the settings on each of the
//! shell's pages and an app's, and the panel that says why the connection isn't live. `#[ignore]`d, since it needs a render adapter: `mise run shots` runs every app's, and says
//! where the pictures went.

use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use trailhead_model::shell::{AskTarget, AskTargets};
use trailhead_shell::ask::Askable;
use trailhead_shell::connect::ConnectUiPlugin;
use trailhead_shell::connection::{Api, Connection, ConnectionStatus, LocalEndpoint};
use trailhead_shell::net::{ApiAccess, Reason};
use trailhead_shell::settings::{ABOUT, APPEARANCE, CONNECTION, OpenSettings, PageId, SettingsPage, ShellDir};
use trailhead_shell::testing::shot::{ShotApp, ShotOptions, shot, shot_with};
use trailhead_shell::testing::{Other, set_connection, settle};
use trailhead_shell::update::{Newer, UpdateReady};
use trailhead_shell::{ActiveApp, AppId, AskButton, Manifest, ShellApp, ShellPlugin, UpdateButton};
use trailhead_ui::{
    BesideRail, Icon, Surface, TextSize, beside_rail, setting, setting_toggle, size, space, spawn_settings_section,
    text, text_dim,
};

const NOTES: AppId = AppId("notes");

/// An app with a page, so the window has something beside the rail.
struct Notes;

#[derive(Component)]
struct NotesPage;

impl ShellApp for Notes {
    fn manifest(&self) -> Manifest {
        Manifest { id: NOTES, name: "Notes", icon: Icon::Flag }
    }

    fn build(&self, _: &mut App) {}

    fn build_ui(&self, app: &mut App) {
        app.add_systems(OnEnter(ActiveApp(NOTES)), spawn_page);
    }

    fn settings_pages(&self) -> Vec<SettingsPage> {
        vec![SettingsPage::new(NOTES_SETTINGS.key, "Notes", notes_settings)]
    }
}

const NOTES_SETTINGS: PageId = PageId::new(NOTES, "notes");

/// An app's page: a section of toggles and a value, as an app's settings are built.
fn notes_settings(In(page): In<Entity>, mut commands: Commands) {
    let list = spawn_settings_section(&mut commands, "List");
    commands.entity(list.section).insert(ChildOf(page));
    let toggle = |label: &str, note: &str, on: bool| setting_toggle(label.to_string(), note.to_string(), on, bsn! {});
    commands
        .spawn_scene(toggle("Pinned notes first", "Pinned notes stay at the top.", true))
        .insert(ChildOf(list.rows));
    commands.spawn_scene(toggle("Show archived", "", false)).insert(ChildOf(list.rows));
    let kept = setting("Kept on this machine", "", bsn_list! { @text("12 drafts", size::BODY) });
    commands.spawn_scene(kept).insert(ChildOf(page));
}

fn spawn_page(mut commands: Commands) {
    let node = Node {
        flex_direction: FlexDirection::Column,
        padding: UiRect::all(px(space::PANEL_INSET)),
        row_gap: px(space::GROUPS),
        ..beside_rail()
    };
    let page = commands.spawn((Name::new("notes"), NotesPage, BesideRail, Surface::Window, node)).id();
    commands.spawn_scene(text("Notes", size::APP_TITLE)).insert(ChildOf(page));
    commands.spawn_scene(text_dim("An app standing in beside the shell's chrome", size::BODY)).insert(ChildOf(page));
}

fn shell() -> ShellPlugin {
    ShellPlugin::new().with(Notes).with(Other).built_as(Some("0123abc".into()))
}

/// Saves the view as it is now, and fails on a frame with nothing drawn in it.
fn save(shot: &mut ShotApp, view: &str) {
    let picture = shot.picture();
    let name = shot.options().name("shell", view);
    assert_eq!(picture.size(), shot.options().pixels(), "{name} is the size asked for");
    assert!(picture.colors() > 16, "{name} has only {} colors: nothing was drawn", picture.colors());
    println!("{}", picture.save(&name).display());
}

fn press<T: Component>(shot: &mut ShotApp) {
    let world = shot.world_mut();
    let found = world.query_filtered::<Entity, With<T>>().iter(world).next().expect("the button to press");
    world.trigger(Activate { entity: found });
    settle(shot);
}

fn close_modals(shot: &mut ShotApp) {
    let world = shot.world_mut();
    let open: Vec<Entity> = world.query_filtered::<Entity, With<trailhead_ui::ModalRoot>>().iter(world).collect();
    for modal in open {
        world.despawn(modal);
    }
    settle(shot);
}

fn open_settings(shot: &mut ShotApp, page: PageId) {
    shot.world_mut().trigger(OpenSettings { page: Some(page) });
    settle(shot);
}

/// The settings on each of the shell's pages, then the app's, with what a connected window launched by the launcher
/// knows: a server dialed and its reason for not being live yet, this window's endpoint, a newer build.
fn settings(shot: &mut ShotApp) {
    let world = shot.world_mut();
    world.insert_resource(ShellDir("/home/someone/.config/trailhead/shell".into()));
    world.resource_mut::<LocalEndpoint>().0 = Some("4f7c2a9e1b".into());
    world.resource_mut::<trailhead_shell::connection::Dialed>().0 = Some("7204d50f3c".into());
    open_settings(shot, APPEARANCE);
    save(shot, "settings-appearance");
    set_connection(shot, Connection::Syncing);
    shot.world_mut().resource_mut::<ConnectionStatus>().reason = Some(Reason::SigningIn);
    settle(shot);
    save(shot, "settings-connection-syncing");
    set_connection(shot, Connection::Live);
    shot.world_mut().resource_mut::<ConnectionStatus>().reason = None;
    let access = ApiAccess {
        base: "http://127.0.0.1:7410".into(),
        token: String::new(),
        user: "someone".into(),
        user_id: "u1".into(),
    };
    shot.world_mut().insert_resource(Api(access));
    open_settings(shot, CONNECTION);
    save(shot, "settings-connection");
    open_settings(shot, ABOUT);
    save(shot, "settings-about");
    open_settings(shot, NOTES_SETTINGS);
    save(shot, "settings-app");
    close_modals(shot);
}

/// A server with an apps repo holding two apps, and a newer build with a few changes.
fn live_with_news(shot: &mut ShotApp) {
    set_connection(shot, Connection::Live);
    let target = |project: &str, name: &str| AskTarget {
        project: project.into(),
        name: name.into(),
        path: Some(format!("apps/{}", name.to_lowercase())),
    };
    let targets = vec![target("NOTE", "Notes"), target("TICK", "Ticklist")];
    let askable = AskTargets { configured: true, repo: Some("o/apps".into()), targets };
    shot.world_mut().insert_resource(Askable(Some(askable)));
    let subjects = [
        "feat(notes): pin a note to the top [NOTE-12]",
        "fix(ticklist): a ticked item stays ticked after a restart [TICK-5] (#41)",
        "feat(shell)!: ask for a new app from the rail [TRAIL-1580]",
        "Merge branch 'main' into notes-search",
    ]
    .map(String::from);
    shot.world_mut().insert_resource(UpdateReady(Some(Newer::new("4567def", &subjects, 6))));
    settle(shot);
}

#[test]
#[ignore = "needs a render adapter: run with `mise run shots`"]
fn shots() {
    let mut shot = shot(shell(), ShotOptions { size: (1024, 720), ..ShotOptions::default() });
    live_with_news(&mut shot);
    save(&mut shot, "rail");

    press::<AskButton>(&mut shot);
    save(&mut shot, "ask");
    close_modals(&mut shot);

    press::<UpdateButton>(&mut shot);
    save(&mut shot, "update");
    close_modals(&mut shot);

    settings(&mut shot);

    // The same at 150%, as Ctrl and = leave it.
    shot.set_text_size(TextSize::nearest(150));
    save(&mut shot, "rail");
    press::<AskButton>(&mut shot);
    save(&mut shot, "ask");
    close_modals(&mut shot);
    press::<UpdateButton>(&mut shot);
    save(&mut shot, "update");
    close_modals(&mut shot);
    open_settings(&mut shot, APPEARANCE);
    save(&mut shot, "settings-appearance");
    drop(shot);

    // The connect panel is drawn only by a shell told how to connect, so this one adds it by hand.
    let mut shot = shot_with(shell(), ShotOptions { size: (1024, 720), ..ShotOptions::default() }, ConnectUiPlugin);
    let world = shot.world_mut();
    world.resource_mut::<ConnectionStatus>().reason = Some(Reason::NotAllowed);
    world.resource_mut::<LocalEndpoint>().0 = Some("4f7c2a9e1b".into());
    settle(&mut shot);
    save(&mut shot, "connect");
}
