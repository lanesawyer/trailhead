//! The shell's own chrome drawn to PNGs, named `shell-<view>-<theme>`: the rail holding two apps with its plus, Ask
//! and update buttons, the Ask forms in both themes (a change to be planned first, a new app, and the answer to a
//! planned ask), the update panel listing what a newer build brings, the settings on each of the
//! shell's pages and an app's (Appearance with its previews of switching apps caught partway; Server as read, with an
//! edit pending, with a refusal and after a save that waits for a restart), the panel that says why
//! the connection isn't live, and each way of switching apps caught closing (`transition-<kind>-cover`) and opening
//! (`transition-<kind>-reveal`), and the rail of four apps with one dragged partway down it (`rail-drag`, at 100% and
//! 150%) and after it is let go (`rail-reordered`). What moves is caught at a fixed frame, with time stepped by hand and held still while
//! the picture is taken, so these too are the same from run to run on one renderer. `#[ignore]`d, since it needs a render adapter: `mise run shots` runs every app's, and says
//! where the pictures went.

use std::time::Duration;

use bevy::camera::NormalizedRenderTarget;
use bevy::picking::backend::HitData;
use bevy::picking::events::{Pointer, PointerDrag, PointerDragEnd, PointerDragStart, PointerPress, PointerRelease};
use bevy::picking::pointer::{Location, PointerId};
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use bevy::ui_widgets::Activate;
use trailhead_model::shell::{AskTarget, AskTargets, Asked, AskedProject, AskedTask};
use trailhead_shell::ask::{AskForm, Askable};
use trailhead_shell::connect::ConnectUiPlugin;
use trailhead_shell::connection::{Api, Connection, ConnectionStatus, LocalEndpoint};
use trailhead_shell::cover::SwitchCover;
use trailhead_shell::net::{ApiAccess, Reason};
use trailhead_shell::server_settings::{SectionState, ServerSettings};
use trailhead_shell::settings::{ABOUT, APPEARANCE, CONNECTION, OpenSettings, PageId, SERVER, SettingsPage, ShellDir};
use trailhead_shell::testing::shot::{ShotApp, ShotOptions, ShotTheme, shot, shot_with};
use trailhead_shell::testing::{Other, set_connection, settle, show};
use trailhead_shell::transition::{AppTransition, CoverFrame, Phase, Switching, TransitionKind};
use trailhead_shell::update::{Newer, UpdateReady};
use trailhead_shell::{
    ActiveApp, AppId, Apps, AskButton, Manifest, NewAppButton, OpenApp, ShellApp, ShellPlugin, UpdateButton,
};
use trailhead_ui::{
    BesideRail, Icon, Surface, TextSize, badge, beside_rail, row, setting, setting_toggle, size, space,
    spawn_settings_section, text, text_dim, token,
};

const NOTES: AppId = AppId("notes");
const TRAILS: AppId = AppId("trails");
const CAMP: AppId = AppId("camp");
/// How far time moves a frame, as the shots helper steps it.
const FRAME: Duration = Duration::from_millis(16);

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
        app.add_systems(OnEnter(ActiveApp(NOTES)), spawn_page)
            .add_systems(OnExit(ActiveApp(NOTES)), take_down::<NotesPage>);
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

/// A second app that looks nothing like Notes, to switch to.
struct Trails;

impl ShellApp for Trails {
    fn manifest(&self) -> Manifest {
        Manifest { id: TRAILS, name: "Trails", icon: Icon::Map }
    }

    fn build(&self, _: &mut App) {}

    fn build_ui(&self, app: &mut App) {
        app.add_systems(OnEnter(ActiveApp(TRAILS)), spawn_trails)
            .add_systems(OnExit(ActiveApp(TRAILS)), take_down::<TrailsPage>);
    }
}

#[derive(Component)]
struct TrailsPage;

/// Takes an app's page down as the app is left.
fn take_down<M: Component>(pages: Query<Entity, With<M>>, mut commands: Commands) {
    for page in &pages {
        commands.entity(page).despawn();
    }
}

fn spawn_trails(mut commands: Commands) {
    let node = Node {
        flex_direction: FlexDirection::Column,
        padding: UiRect::all(px(space::PANEL_INSET)),
        row_gap: px(space::GROUPS),
        ..beside_rail()
    };
    let page = commands.spawn((Name::new("trails"), TrailsPage, BesideRail, Surface::Window, node)).id();
    commands.spawn_scene(text("Trails", size::APP_TITLE)).insert(ChildOf(page));
    let badges = commands.spawn_scene(row()).insert(ChildOf(page)).id();
    for (label, tone) in [("Open", token::STATUS_OK), ("Steep", token::STATUS_WARN), ("Closed", token::STATUS_DANGER)] {
        commands.spawn_scene(badge(label, None, Some(tone))).insert(ChildOf(badges));
    }
    let trails = [
        ("Ridge loop", "6.2 km, along the crest and back by the lake"),
        ("Creek path", "3.1 km, flat, shaded most of the way"),
        ("Summit switchbacks", "9.8 km, 740 m of climbing in twenty-two turns"),
        ("Meadow cut-through", "1.4 km, muddy after rain"),
        ("Old quarry road", "5.0 km, wide enough to walk side by side"),
        ("Lookout spur", "0.8 km, steep, with the best view of the valley"),
    ];
    for (name, about) in trails {
        let trail = commands.spawn((Node { flex_direction: FlexDirection::Column, ..default() }, ChildOf(page))).id();
        commands.spawn_scene(text(name, size::HEADING)).insert(ChildOf(trail));
        commands.spawn_scene(text_dim(about, size::BODY)).insert(ChildOf(trail));
    }
}

fn shell() -> ShellPlugin {
    ShellPlugin::new().with(Notes).with(Other).built_as(Some("0123abc".into()))
}

/// Trails first, so a switch to Notes runs down the rail from a page full to its foot.
fn switching_shell() -> ShellPlugin {
    ShellPlugin::new().with(Trails).with(Notes)
}

/// An app with nothing to draw, to fill the rail.
struct Plain(AppId, &'static str, Icon);

impl ShellApp for Plain {
    fn manifest(&self) -> Manifest {
        Manifest { id: self.0, name: self.1, icon: self.2 }
    }

    fn build(&self, _: &mut App) {}
}

/// Four apps, so a drag has others to pass.
fn four_app_shell() -> ShellPlugin {
    ShellPlugin::new().with(Notes).with(Trails).with(Plain(CAMP, "Camp", Icon::Footprints)).with(Plain(
        AppId("signs"),
        "Signs",
        Icon::Signpost,
    ))
}

/// Where a pointer at `logical` on the window is, as picking reports it.
fn pointer(logical: Vec2) -> Pointer {
    let target = NormalizedRenderTarget::None { width: 1, height: 1 };
    Pointer::new(PointerId::Mouse, Location { target, position: logical })
}

/// The rail's button for `id`, and its middle on the window, in logical pixels.
fn rail_button(shot: &mut ShotApp, id: AppId) -> (Entity, Vec2) {
    let ui_scale = shot.world().resource::<UiScale>().0;
    let world = shot.world_mut();
    let mut buttons = world.query::<(Entity, &OpenApp, &ComputedNode, &UiGlobalTransform)>();
    let (entity, _, node, transform) = buttons.iter(world).find(|(_, open, _, _)| open.0 == id).unwrap();
    (entity, transform.translation * node.inverse_scale_factor() * ui_scale)
}

/// Presses Notes on the rail and drags it `places` buttons down (up, less than none), as picking reports it, and lets
/// the others slide aside.
fn drag_notes(shot: &mut ShotApp, places: f32) -> (Entity, Vec2, Vec2) {
    let (notes, from) = rail_button(shot, NOTES);
    // Trails and Camp are next to each other in every order these shots leave the rail in.
    let step = (rail_button(shot, CAMP).1.y - rail_button(shot, TRAILS).1.y).abs();
    let distance = Vec2::new(0.0, step * places);
    let (button, hit) = (PointerButton::Primary, HitData::new(Entity::PLACEHOLDER, 0.0, None, None));
    let world = shot.world_mut();
    world.trigger(PointerPress { entity: notes, pointer: pointer(from), button, hit: hit.clone(), count: 1 });
    world.flush();
    world.trigger(PointerDragStart { entity: notes, pointer: pointer(from), button, hit });
    world.trigger(PointerDrag { entity: notes, pointer: pointer(from + distance), button, distance, delta: distance });
    world.flush();
    for _ in 0..12 {
        shot.update();
    }
    (notes, from, distance)
}

/// Lets Notes go where it was dragged.
fn drop_notes(shot: &mut ShotApp, (notes, from, distance): (Entity, Vec2, Vec2)) {
    let (button, hit) = (PointerButton::Primary, HitData::new(Entity::PLACEHOLDER, 0.0, None, None));
    let world = shot.world_mut();
    world.trigger(PointerRelease { entity: notes, pointer: pointer(from + distance), button, hit });
    world.trigger(PointerDragEnd { entity: notes, pointer: pointer(from + distance), button, distance });
    world.flush();
    settle(shot);
}

/// The rail of four apps with Notes held between Camp and Signs, the gap open where it will land, then let go there.
fn reordering(shot: &mut ShotApp) {
    let held = drag_notes(shot, 2.6);
    hold_still(shot, true);
    save(shot, "rail-drag");
    hold_still(shot, false);
    drop_notes(shot, held);
    let order: Vec<&str> = shot.world().resource::<Apps>().iter().map(|m| m.name).collect();
    assert_eq!(order, ["Trails", "Camp", "Notes", "Signs"], "let go between Camp and Signs");
    save(shot, "rail-reordered");
    // Held at 150%, where the pointer's pixels and the UI's differ.
    shot.set_text_size(TextSize::nearest(150));
    let held = drag_notes(shot, -1.6);
    hold_still(shot, true);
    save(shot, "rail-drag");
    hold_still(shot, false);
    drop_notes(shot, held);
    let order: Vec<&str> = shot.world().resource::<Apps>().iter().map(|m| m.name).collect();
    assert_eq!(order, ["Trails", "Notes", "Camp", "Signs"], "and back up between Trails and Camp at 150%");
}

/// Holds time still while a picture is taken, so what moves stays where it was caught.
fn hold_still(shot: &mut ShotApp, still: bool) {
    let step = if still { Duration::ZERO } else { FRAME };
    shot.insert_resource(TimeUpdateStrategy::ManualDuration(step));
}

/// Runs frames until `caught` holds, then saves the view held still there.
fn catch(shot: &mut ShotApp, view: &str, caught: impl Fn(&ShotApp) -> bool) {
    for _ in 0..200 {
        if caught(shot) {
            hold_still(shot, true);
            save(shot, view);
            hold_still(shot, false);
            return;
        }
        shot.update();
    }
    panic!("{view} was never caught");
}

fn switch_frame(shot: &ShotApp) -> Option<CoverFrame> {
    shot.world().resource::<Switching>().frame()
}

/// Each way of switching from Trails to Notes, caught about half closed and about half open again (eased).
fn transitions(shot: &mut ShotApp) {
    for kind in TransitionKind::DRAWN {
        show(shot, TRAILS);
        shot.insert_resource(AppTransition(kind));
        let world = shot.world_mut();
        let notes = world.query::<(Entity, &OpenApp)>().iter(world).find(|(_, o)| o.0 == NOTES).unwrap().0;
        world.trigger(Activate { entity: notes });
        let name = kind.word();
        let at = |phase: Phase, from: f32| {
            move |shot: &ShotApp| switch_frame(shot).is_some_and(|f| f.phase == phase && f.t >= from)
        };
        catch(shot, &format!("transition-{name}-cover"), at(Phase::Covering, 0.7));
        catch(shot, &format!("transition-{name}-reveal"), at(Phase::Revealing, 0.3));
        for _ in 0..30 {
            shot.update();
        }
        let world = shot.world_mut();
        assert_eq!(world.query_filtered::<(), With<SwitchCover>>().iter(world).count(), 0, "{name}'s cover is gone");
    }
}

/// The Appearance page with its previews caught partway through switching.
fn appearance(shot: &mut ShotApp) {
    open_settings(shot, APPEARANCE);
    let closing = |shot: &ShotApp| {
        let world = shot.world();
        let mut frames = world.try_query::<&CoverFrame>().unwrap();
        frames.iter(world).any(|f| f.kind == TransitionKind::Iris && f.phase == Phase::Covering && f.t >= 0.5)
    };
    catch(shot, "settings-appearance", closing);
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
    appearance(shot);
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

/// Starts the Ask form afresh holding `draft`, as if typed.
fn draft(shot: &mut ShotApp, draft: impl FnOnce(&mut AskForm)) {
    let mut form = shot.world_mut().resource_mut::<AskForm>();
    *form = AskForm::default();
    draft(&mut form);
}

/// The Ask forms in `theme`: a change to Notes with "Plan this first" on and a model chosen, a new app, and what the
/// form says once a planned ask is made.
fn asking(shot: &mut ShotApp, theme: ShotTheme) {
    shot.set_theme(theme);
    let text = "Pin a note to the top of the list, and keep it there after a restart.";
    draft(shot, |f| {
        f.text = text.into();
        f.plan = true;
        f.model = Some("opus");
    });
    press::<AskButton>(shot);
    save(shot, "ask-change-planned");
    close_modals(shot);

    let text = "Plan a day's hike: the trails I want to walk, how long each takes, and what to pack.";
    draft(shot, |f| {
        f.text = text.into();
        f.name = "Hike Planner".into();
    });
    press::<NewAppButton>(shot);
    save(shot, "ask-new-app");
    let asked = Asked {
        project: AskedProject { key: "HP".into(), name: "Hike Planner".into(), created: true },
        task: Some(AskedTask { key: "HP-1".into(), title: "Hike Planner".into() }),
        planned: true,
        notes: Vec::new(),
    };
    shot.world_mut().resource_mut::<AskForm>().phase = trailhead_shell::ask::Phase::Asked(asked);
    settle(shot);
    save(shot, "ask-planned");
    close_modals(shot);
    shot.set_theme(ShotTheme::Dark);
}

/// What a server with an apps repo and a factory says of its user config.
fn server_config() -> trailhead_model::config::ServerConfig {
    serde_json::from_value(serde_json::json!({
        "path": "/home/someone/.config/trailhead/config.toml",
        "version": "v1",
        "factory": {"repos": ["~/Dev/trailhead", "~/Dev/trailhead-apps"], "disabled": false,
                    "pause_at": {"five_hour": 80}, "max_sandboxes": null, "defaults": ["disabled", "max_sandboxes"]},
        "factory_built": true,
        "shell": {"repo": "~/Dev/trailhead-apps", "package": null, "profile": "release", "jobs": 3, "poll_secs": 60,
                  "defaults": ["package", "profile", "jobs", "poll_secs"]},
        "problem": null,
        "running": {},
        "restart_needed": false,
        "waiting": [],
        "foreground": false
    }))
    .unwrap()
}

/// The Server page as read, with an edit pending, with the server's refusal shown, and after a save that waits for
/// a restart.
fn server(shot: &mut ShotApp) {
    set_connection(shot, Connection::Live);
    let build = trailhead_model::shell::ShellBuild {
        configured: true,
        current: Some("4567def0123".into()),
        ..Default::default()
    };
    shot.world_mut().resource_mut::<ServerSettings>().answered(server_config(), Some(build));
    open_settings(shot, SERVER);
    save(shot, "settings-server");

    let edit = |shot: &mut ShotApp, edit: &dyn Fn(&mut ServerSettings)| {
        edit(&mut shot.world_mut().resource_mut::<ServerSettings>());
        settle(shot);
    };
    edit(shot, &|s| {
        s.builds.jobs = "4".into();
        s.factory.repos.push("~/Dev/bevy-data-explorer".into());
    });
    save(shot, "settings-server-edited");

    edit(shot, &|s| {
        s.factory.five_hour = "101".into();
        s.factory_save = SectionState {
            refusal: Some(
                "/home/someone/.config/trailhead/config.toml: [factory] pause_at for five_hour is 101; it's a percent, \
                 0 to 100"
                    .into(),
            ),
            ..Default::default()
        };
    });
    save(shot, "settings-server-refused");

    let mut saved = server_config();
    saved.version = "v2".into();
    saved.shell.as_mut().unwrap().jobs = 4;
    saved.shell.as_mut().unwrap().defaults.retain(|d| d != "jobs");
    saved.restart_needed = true;
    saved.waiting = vec!["shell.jobs".into()];
    edit(shot, &|s| {
        s.builds_save = SectionState { saved: true, ..Default::default() };
        s.factory_save = SectionState::default();
        s.factory.five_hour = "80".into();
        s.factory.repos.pop();
        s.answered(saved.clone(), s.build.clone());
    });
    save(shot, "settings-server-saved");
    close_modals(shot);
}

#[test]
#[ignore = "needs a render adapter: run with `mise run shots`"]
fn shots() {
    let mut shot = shot(shell(), ShotOptions { size: (1024, 720), ..ShotOptions::default() });
    live_with_news(&mut shot);
    save(&mut shot, "rail");

    asking(&mut shot, ShotTheme::Dark);
    asking(&mut shot, ShotTheme::Light);

    press::<UpdateButton>(&mut shot);
    save(&mut shot, "update");
    close_modals(&mut shot);

    settings(&mut shot);

    // The same at 150%, as Ctrl and = leave it.
    shot.set_text_size(TextSize::nearest(150));
    save(&mut shot, "rail");
    press::<AskButton>(&mut shot);
    save(&mut shot, "ask-change");
    close_modals(&mut shot);
    press::<UpdateButton>(&mut shot);
    save(&mut shot, "update");
    close_modals(&mut shot);
    close_modals(&mut shot);
    appearance(&mut shot);
    drop(shot);

    // The connect panel is drawn only by a shell told how to connect, so this one adds it by hand.
    let mut shot = shot_with(shell(), ShotOptions { size: (1024, 720), ..ShotOptions::default() }, ConnectUiPlugin);
    let world = shot.world_mut();
    world.resource_mut::<ConnectionStatus>().reason = Some(Reason::NotAllowed);
    world.resource_mut::<LocalEndpoint>().0 = Some("4f7c2a9e1b".into());
    settle(&mut shot);
    save(&mut shot, "connect");
    drop(shot);

    // Tall enough to show the whole Server page.
    let mut shot =
        trailhead_shell::testing::shot::shot(shell(), ShotOptions { size: (1024, 1500), ..ShotOptions::default() });
    server(&mut shot);
    drop(shot);

    let mut shot = trailhead_shell::testing::shot::shot(
        switching_shell(),
        ShotOptions { size: (1024, 720), ..ShotOptions::default() },
    );
    hold_still(&mut shot, false);
    transitions(&mut shot);
    drop(shot);

    let mut shot = trailhead_shell::testing::shot::shot(
        four_app_shell(),
        ShotOptions { size: (1024, 360), ..ShotOptions::default() },
    );
    hold_still(&mut shot, false);
    reordering(&mut shot);
}
