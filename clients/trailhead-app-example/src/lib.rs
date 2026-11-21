//! The example app: the reference a new app is copied from, and the second app that proves the shell's contract.
//!
//! It is one page with a button that counts its presses. The count is the app's logic ([`Presses`], changed by the
//! [`Press`] event, which runs headless); the page ([`Example::build_ui`]) is spawned on entering the app, despawned
//! on leaving it, and its count line is updated in place. The count is kept in the [`LocalStore`], so it outlives a
//! switch to another app and a restart of the shell.

use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use trailhead_shell::{ActiveApp, AppId, LocalStore, Manifest, ShellApp};
use trailhead_ui::{BesideRail, ButtonKind, Icon, Surface, beside_rail, button, set_text, size, space, text, text_dim};

/// The example app's id.
pub const EXAMPLE: AppId = AppId("example");

/// Where the count is kept in the [`LocalStore`].
const PRESSES: &str = "presses";

/// The example app, to give to `ShellPlugin::with`.
pub struct Example;

/// How many times a person has pressed the page's button, across restarts.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Presses(pub u32);

/// A person pressed the button. The page triggers it; a test can trigger it with no page.
#[derive(Event, Debug, Clone, Copy)]
pub struct Press;

/// The page's root, present only while the app is shown.
#[derive(Component)]
pub struct ExamplePage;

/// The line that says how many times the button was pressed.
#[derive(Component)]
struct CountLine;

/// The page's button.
#[derive(Component)]
struct PressButton;

impl ShellApp for Example {
    fn manifest(&self) -> Manifest {
        Manifest { id: EXAMPLE, name: "Example", icon: Icon::Flag }
    }

    fn build(&self, app: &mut App) {
        let kept = app.world().resource::<LocalStore>().get(EXAMPLE, PRESSES).unwrap_or_default();
        app.insert_resource(Presses(kept)).add_observer(count_press);
    }

    fn build_ui(&self, app: &mut App) {
        app.add_observer(on_button)
            .add_systems(OnEnter(ActiveApp(EXAMPLE)), spawn_page)
            .add_systems(OnExit(ActiveApp(EXAMPLE)), despawn_page)
            .add_systems(Update, show_count.run_if(in_state(ActiveApp(EXAMPLE))));
    }
}

fn count_press(_: On<Press>, mut presses: ResMut<Presses>, mut store: ResMut<LocalStore>) {
    presses.0 += 1;
    store.set(EXAMPLE, PRESSES, presses.0);
}

/// What the count line says.
fn count_words(presses: u32) -> String {
    match presses {
        1 => "Pressed 1 time".to_string(),
        n => format!("Pressed {n} times"),
    }
}

fn on_button(activate: On<Activate>, buttons: Query<(), With<PressButton>>, mut commands: Commands) {
    if buttons.contains(activate.entity) {
        commands.trigger(Press);
    }
}

fn spawn_page(mut commands: Commands, presses: Res<Presses>) {
    let page = commands
        .spawn((
            Name::new("example"),
            ExamplePage,
            BesideRail,
            Surface::Window,
            Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexStart,
                min_width: px(0),
                min_height: px(0),
                padding: UiRect::all(px(space::PANEL_INSET)),
                row_gap: px(space::GROUPS),
                ..beside_rail()
            },
        ))
        .id();
    commands.queue_spawn_scene(text("Example", size::APP_TITLE)).insert(ChildOf(page));
    commands
        .queue_spawn_scene(text_dim("The example app: what a new app is copied from", size::BODY))
        .insert(ChildOf(page));
    commands.queue_spawn_scene(text(count_words(presses.0), size::BODY)).insert((CountLine, ChildOf(page)));
    commands.queue_spawn_scene(button("Press", None, ButtonKind::Normal)).insert((PressButton, ChildOf(page)));
}

fn despawn_page(mut commands: Commands, pages: Query<Entity, With<ExamplePage>>) {
    for page in &pages {
        commands.entity(page).despawn();
    }
}

/// Keeps the count line in step with the count, in place.
fn show_count(presses: Res<Presses>, mut lines: Query<&mut Text, With<CountLine>>) {
    if presses.is_changed() {
        for line in &mut lines {
            set_text(line, &count_words(presses.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use trailhead_shell::ShellPlugin;
    use trailhead_shell::testing::{OTHER, Other, headless, settle, show, windowed};

    use super::*;

    fn count(app: &App) -> u32 {
        app.world().resource::<Presses>().0
    }

    fn pages(app: &mut App) -> usize {
        let world = app.world_mut();
        world.query::<&ExamplePage>().iter(world).count()
    }

    fn count_line(app: &mut App) -> (Entity, String) {
        let world = app.world_mut();
        let (entity, text) = world.query_filtered::<(Entity, &Text), With<CountLine>>().single(world).unwrap();
        (entity, text.0.clone())
    }

    #[test]
    fn pressing_adds_to_the_count_and_it_is_kept_for_the_next_shell() {
        let dir = tempfile::tempdir().unwrap();
        let shell = || ShellPlugin::new().with(Example).store_in(dir.path().to_path_buf());
        let mut first = headless(shell());
        assert_eq!(count(&first), 0);
        first.world_mut().trigger(Press);
        first.world_mut().trigger(Press);
        first.update();
        assert_eq!(count(&first), 2);
        assert_eq!(first.world().resource::<LocalStore>().get::<u32>(EXAMPLE, PRESSES), Some(2));
        drop(first);
        assert_eq!(count(&headless(shell())), 2);
    }

    #[test]
    fn switching_away_despawns_the_page_and_switching_back_shows_the_kept_count() {
        let mut app = windowed(ShellPlugin::new().with(Example).with(Other));
        assert_eq!(pages(&mut app), 1);
        app.world_mut().trigger(Press);
        app.world_mut().trigger(Press);
        show(&mut app, OTHER);
        assert_eq!(pages(&mut app), 0);
        app.world_mut().trigger(Press);
        show(&mut app, EXAMPLE);
        assert_eq!(pages(&mut app), 1);
        assert_eq!(count_line(&mut app).1, "Pressed 3 times");
    }

    #[test]
    fn pressing_the_button_changes_the_count_line_and_nothing_else_is_respawned() {
        let mut app = windowed(ShellPlugin::new().with(Example).with(Other));
        let (line, words) = count_line(&mut app);
        assert_eq!(words, "Pressed 0 times");
        let all = |app: &mut App| {
            let world = app.world_mut();
            let page = world.query_filtered::<Entity, With<ExamplePage>>().single(world).unwrap();
            let mut all: Vec<Entity> = world.query::<Entity>().iter(world).collect();
            all.sort();
            (page, all)
        };
        let (page, before) = all(&mut app);
        let button = {
            let world = app.world_mut();
            world.query_filtered::<Entity, With<PressButton>>().single(world).unwrap()
        };
        app.world_mut().trigger(Activate { entity: button });
        settle(&mut app);
        assert_eq!(count(&app), 1);
        assert_eq!(count_line(&mut app), (line, "Pressed 1 time".to_string()));
        let (page_after, after) = all(&mut app);
        assert_eq!(page_after, page);
        assert_eq!(after, before);
    }
}
