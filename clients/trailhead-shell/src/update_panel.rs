//! The update notice's panel. Pressing the rail's update button ([`crate::rail::UpdateButton`]) opens a modal saying
//! what the newer build brings: how many changes, a row for each (its `type(scope)` as a badge, its message cut to one
//! line, its task key dim at the end), and Restart and Later in its footer. Later, Escape, the X and a click outside
//! close it; Restart triggers [`Restart`]. A newer answer from the server while it is open updates it in place.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::ui_widgets::{Activate, RequestClose};
use trailhead_ui::{
    ButtonKind, Icon, Keyed, Modal, ModalSlots, badge, button, display, open_modal, row, set_text, size, space, text,
    text_dim, text_truncated,
};

use crate::rail::UpdateButton;
use crate::update::{Change, Restart, UpdateReady};

/// The panel's modal.
#[derive(Component)]
pub struct UpdatePanel;

impl Modal for UpdatePanel {
    fn title(&self) -> String {
        "Update ready".into()
    }
}

/// The line over the changes, saying how many there are.
#[derive(Component)]
pub struct ChangeSummary;

/// The column holding a row for each change, and nothing else.
#[derive(Component)]
pub struct ChangeList;

/// Which part of a change's row this is.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChangePart {
    /// The `type(scope)` badge, hidden for a subject that isn't conventional.
    #[default]
    Kind,
    Message,
    /// The task key, hidden when there is none.
    Key,
}

/// What a button in the panel's footer does.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelAct {
    #[default]
    Restart,
    Later,
}

/// The rows of the open panel, by key.
#[derive(Resource, Default)]
struct Rows(Keyed<Change>);

/// The panel, for a shell that knows its build.
pub(crate) struct UpdatePanelUiPlugin;

impl Plugin for UpdatePanelUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Rows>()
            .add_observer(on_update_button)
            .add_observer(on_act)
            .add_systems(Update, (fill_panel, show_summary, sync_rows, close_when_newest).chain());
    }
}

fn on_update_button(
    activate: On<Activate>,
    buttons: Query<(), With<UpdateButton>>,
    open: Query<(), With<UpdatePanel>>,
    mut rows: ResMut<Rows>,
    mut commands: Commands,
) {
    if buttons.contains(activate.entity) && open.is_empty() {
        rows.0.forget();
        open_modal(&mut commands, UpdatePanel);
    }
}

fn on_act(activate: On<Activate>, acts: Query<&PanelAct>, mut commands: Commands) {
    match acts.get(activate.entity) {
        Ok(PanelAct::Restart) => commands.trigger(Restart),
        Ok(PanelAct::Later) => commands.trigger(RequestClose { source: activate.entity }),
        Err(_) => {}
    }
}

/// Fills a panel that has just spawned: the summary line and the column for the rows in its body, the buttons in its
/// footer. The rows themselves are [`sync_rows`]'s.
fn fill_panel(
    new: Query<&ModalSlots, (With<UpdatePanel>, Added<ModalSlots>)>,
    ready: Res<UpdateReady>,
    mut commands: Commands,
) {
    for slots in &new {
        let summary = ready.0.as_ref().map(|n| n.summary()).unwrap_or_default();
        commands.spawn_scene(text(summary, size::BODY)).insert((ChangeSummary, ChildOf(slots.body)));
        commands.spawn((
            Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() },
            ChangeList,
            ChildOf(slots.body),
        ));
        let restart = bsn! { @button("Restart", Some(Icon::Refresh), ButtonKind::Primary) PanelAct::Restart };
        commands.spawn_scene(restart).insert(ChildOf(slots.footer));
        commands
            .spawn_scene(bsn! { @button("Later", None, ButtonKind::Normal) PanelAct::Later })
            .insert(ChildOf(slots.footer));
    }
}

fn show_summary(ready: Res<UpdateReady>, mut lines: Query<&mut Text, With<ChangeSummary>>) {
    let Some(newer) = ready.0.as_ref().filter(|_| ready.is_changed()) else { return };
    let words = newer.summary();
    for line in &mut lines {
        set_text(line, &words);
    }
}

/// A row for each change, kept in step with the newest answer: rows are added, moved and removed, never redrawn.
fn sync_rows(
    ready: Res<UpdateReady>,
    lists: Query<Entity, With<ChangeList>>,
    added: Query<(), Added<ChangeList>>,
    mut rows: ResMut<Rows>,
    mut commands: Commands,
) {
    if !ready.is_changed() && added.is_empty() {
        return;
    }
    let Ok(list) = lists.single() else { return };
    let changes = ready.0.as_ref().map_or(&[][..], |n| &n.changes[..]);
    let wanted = [(list, keyed(changes))];
    // A row's key is made from all of its change, so a known key's change never differs and no row needs updating.
    rows.0.sync(
        &mut commands,
        &wanted,
        |commands, _, change| commands.spawn_scene(change_row(change)).id(),
        |_, _, _| {},
    );
}

/// Each change with a key that stays with it as newer changes come in above it: its words, and which of the changes
/// with those words it is.
fn keyed(changes: &[Change]) -> Vec<(String, Change)> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    changes
        .iter()
        .rev()
        .map(|c| {
            let words = format!(
                "{}\u{1f}{}\u{1f}{}",
                c.kind.as_deref().unwrap_or_default(),
                c.message,
                c.key.as_deref().unwrap_or_default()
            );
            let nth = seen.entry(words.clone()).or_default();
            *nth += 1;
            (format!("{words}\u{1f}{nth}"), c.clone())
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// A change's row: its kind as a badge, its message cut to the room left, its key dim at the end.
fn change_row(change: &Change) -> impl Scene {
    let (kinded, keyed) = (change.kind.is_some(), change.key.is_some());
    let kind = change.kind.clone().unwrap_or_default();
    let message = change.message.clone();
    let key = change.key.clone().unwrap_or_default();
    bsn! {
        @row()
        Children [
            @badge(kind, None, None) ChangePart::Kind Node { display: { display(kinded) } }
            --
            @text_truncated(message, size::BODY) ChangePart::Message
            --
            @text_dim(key, size::SECONDARY) ChangePart::Key Node { display: { display(keyed) } }
        ]
    }
}

/// Closes the panel once there is no newer build to restart onto.
fn close_when_newest(ready: Res<UpdateReady>, panels: Query<Entity, With<UpdatePanel>>, mut commands: Commands) {
    if ready.is_changed() && ready.0.is_none() {
        for panel in &panels {
            commands.trigger(RequestClose { source: panel });
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use trailhead_model::shell::RESTART_CODE;
    use trailhead_ui::{BadgeLabel, ModalPlugin, ModalRoot, Truncated};

    use super::*;
    use crate::ShellPlugin;
    use crate::tests::{Fake, MAPS, NOTES};
    use crate::update::Newer;

    fn windowed() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_plugins(ModalPlugin)
            .add_plugins(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).built_as(Some("a".into())).windowed());
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    fn newer(app: &mut App, subjects: &[&str], total: usize) {
        let subjects: Vec<String> = subjects.iter().map(|s| s.to_string()).collect();
        app.world_mut().resource_mut::<UpdateReady>().0 = Some(Newer::new("b", &subjects, total));
        settle(app);
    }

    fn one<F: bevy::ecs::query::QueryFilter>(app: &mut App) -> Entity {
        app.world_mut().query_filtered::<Entity, F>().single(app.world()).unwrap()
    }

    fn open(app: &mut App) -> Entity {
        let button = one::<With<UpdateButton>>(app);
        app.world_mut().trigger(Activate { entity: button });
        settle(app);
        one::<With<UpdatePanel>>(app)
    }

    fn act(app: &mut App, act: PanelAct) -> Entity {
        let mut buttons = app.world_mut().query::<(Entity, &PanelAct)>();
        buttons.iter(app.world()).find(|(_, a)| **a == act).map(|(e, _)| e).unwrap()
    }

    fn entities(app: &mut App) -> Vec<Entity> {
        let mut all: Vec<Entity> = app.world_mut().query::<Entity>().iter(app.world()).collect();
        all.sort();
        all
    }

    fn summary(app: &mut App) -> (Entity, String) {
        let world = app.world_mut();
        world
            .query_filtered::<(Entity, &Text), With<ChangeSummary>>()
            .single(world)
            .map(|(e, t)| (e, t.0.clone()))
            .unwrap()
    }

    /// Each row: its entity, its badge's words (`None` when hidden), its message and its key (`None` when hidden).
    fn rows(app: &mut App) -> Vec<(Entity, Option<String>, String, Option<String>)> {
        let list = one::<With<ChangeList>>(app);
        let world = app.world();
        let rows: Vec<Entity> = world.get::<Children>(list).map(|c| c.iter().collect()).unwrap_or_default();
        let shown = |e: Entity| world.get::<Node>(e).is_some_and(|n| n.display != Display::None);
        let words = |e: Entity| world.get::<Text>(e).map(|t| t.0.clone()).unwrap_or_default();
        rows.into_iter()
            .map(|row| {
                let parts: Vec<Entity> = world.get::<Children>(row).unwrap().iter().collect();
                let part = |p: ChangePart| *parts.iter().find(|e| world.get::<ChangePart>(**e) == Some(&p)).unwrap();
                let kind = part(ChangePart::Kind);
                let label = world
                    .get::<Children>(kind)
                    .and_then(|c| c.iter().find(|e| world.get::<BadgeLabel>(*e).is_some()))
                    .map(words);
                let message = world.get::<Truncated>(part(ChangePart::Message)).unwrap().full.clone();
                let key = part(ChangePart::Key);
                (row, label.filter(|_| shown(kind)), message, shown(key).then(|| words(key)))
            })
            .collect()
    }

    fn texts(rows: &[(Entity, Option<String>, String, Option<String>)]) -> Vec<(Option<&str>, &str, Option<&str>)> {
        rows.iter().map(|(_, k, m, key)| (k.as_deref(), m.as_str(), key.as_deref())).collect()
    }

    const SUBJECTS: [&str; 3] = [
        "feat(ticklist): a Completed view [TICK-5] (#2)",
        "Merge pull request #1 from lanesawyer/factory/apps-3",
        "fix(shell)!: the rail: its foot [TRAIL-9]",
    ];

    #[test]
    fn opening_the_panel_shows_the_count_and_a_row_for_each_change() {
        let mut app = windowed();
        newer(&mut app, &SUBJECTS, 3);
        let panel = open(&mut app);
        assert!(app.world().get::<ModalRoot>(panel).is_some());
        assert_eq!(summary(&mut app).1, "3 changes since the build you're running");
        let rows = rows(&mut app);
        assert_eq!(
            texts(&rows),
            [
                (Some("feat(ticklist)"), "a Completed view", Some("TICK-5")),
                (None, "Merge pull request #1 from lanesawyer/factory/apps-3", None),
                (Some("fix(shell)!"), "the rail: its foot", Some("TRAIL-9")),
            ]
        );
        let list = one::<With<ChangeList>>(&mut app);
        let gap = app.world().get::<Node>(list).unwrap().row_gap;
        assert_eq!(gap, px(space::ROWS), "the rows stand apart");
        let mut kinds = app.world_mut().query::<(&ChangePart, &Node)>();
        let pads: Vec<UiRect> =
            kinds.iter(app.world()).filter(|(p, _)| **p == ChangePart::Kind).map(|(_, n)| n.padding).collect();
        let badge_inset = UiRect::axes(px(space::ICON_LABEL), px(space::BADGE_INSET));
        assert_eq!(pads, [badge_inset; 3], "hiding a badge patches its node and keeps the rest of it");
    }

    #[test]
    fn with_no_changes_listed_it_says_only_that_a_newer_build_is_ready() {
        let mut app = windowed();
        newer(&mut app, &[], 0);
        open(&mut app);
        assert_eq!(summary(&mut app).1, "A newer build is ready.");
        assert!(rows(&mut app).is_empty());
    }

    #[test]
    fn a_newer_answer_while_open_updates_the_panel_in_place() {
        let mut app = windowed();
        newer(&mut app, &SUBJECTS[1..], 2);
        open(&mut app);
        let (line, _) = summary(&mut app);
        let before = rows(&mut app);
        newer(&mut app, &SUBJECTS, 41);
        assert_eq!(summary(&mut app), (line, "The newest 3 of 41 changes since the build you're running".into()));
        let after = rows(&mut app);
        assert_eq!(after.len(), 3);
        assert_eq!((after[1].0, after[2].0), (before[0].0, before[1].0), "the rows already shown keep their entities");
        assert_eq!(after[0].2, "a Completed view");
    }

    #[test]
    fn later_closes_the_panel_and_leaves_the_rest_of_the_window_as_it_was() {
        let mut app = windowed();
        newer(&mut app, &SUBJECTS, 3);
        let before = entities(&mut app);
        open(&mut app);
        let later = act(&mut app, PanelAct::Later);
        app.world_mut().trigger(Activate { entity: later });
        settle(&mut app);
        assert_eq!(app.world_mut().query::<&UpdatePanel>().iter(app.world()).count(), 0);
        assert_eq!(entities(&mut app), before);
        let exits = app.world_mut().resource_mut::<Messages<AppExit>>().drain().count();
        assert_eq!(exits, 0, "nothing restarted");

        open(&mut app);
        assert_eq!(rows(&mut app).len(), 3, "it opens again with its rows");
    }

    #[test]
    fn restart_exits_with_the_restart_code() {
        let mut app = windowed();
        newer(&mut app, &SUBJECTS, 3);
        open(&mut app);
        let restart = act(&mut app, PanelAct::Restart);
        app.world_mut().trigger(Activate { entity: restart });
        app.world_mut().flush();
        let exits: Vec<AppExit> = app.world_mut().resource_mut::<Messages<AppExit>>().drain().collect();
        assert_eq!(exits, [AppExit::from_code(RESTART_CODE)]);
    }

    #[test]
    fn the_panel_closes_once_this_is_the_newest_build() {
        let mut app = windowed();
        newer(&mut app, &SUBJECTS, 3);
        open(&mut app);
        app.world_mut().resource_mut::<UpdateReady>().0 = None;
        settle(&mut app);
        assert_eq!(app.world_mut().query::<&UpdatePanel>().iter(app.world()).count(), 0);
    }
}
