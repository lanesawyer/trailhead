//! The Appearance page's "Switching apps" setting: a row of the kit's choice tiles, one for each [`TransitionKind`],
//! each picture a mock window (a strip of rail with a mark for each of two apps, and an app area whose content is one
//! app's or the other's) playing its kind on a loop with a pause between. A preview's cover is drawn by
//! [`crate::cover`], the code that draws the real one, at the preview's size, so the two can't drift apart. `Random`'s
//! plays each drawn kind in turn, and `None`'s switches at once. Pressing a tile picks its kind; the one picked is
//! ringed.
//!
//! The previews exist only while the page is shown, and ask the window to redraw only while they exist.

use std::time::Duration;

use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use bevy::window::RequestRedraw;
use trailhead_ui::{
    BackgroundTone, ChoiceTile, Token, UiPlace, patch_node, radius, setting, space, spawn_choice_tile, token,
};

use crate::cover::{draw_covers, spawn_cover_pieces};
use crate::transition::{AppTransition, COVER, CoverFrame, Direction, Phase, REVEAL, TransitionKind};

/// How long a preview rests on an app between switches.
pub const PAUSE: Duration = Duration::from_millis(900);
/// The mock window's strip of rail, across.
const PREVIEW_RAIL_WIDTH: f32 = 10.0;
/// An app's mark on the mock rail, across and down.
const PREVIEW_MARK: f32 = 4.0;
/// A line of a mock app's content, down.
const PREVIEW_LINE_HEIGHT: f32 = 4.0;
/// Each mock app's lines: their tone, and each line's width as a percentage of the app area.
const LOOKS: [(Token, [f32; 2]); 2] = [(token::FILL_SOLID, [70.0, 45.0]), (token::FILL_ACCENT, [45.0, 80.0])];

/// The setting's row of tiles, filled once it is spawned.
#[derive(Component, Clone, Default)]
pub struct TransitionTiles;

/// A tile: the kind it picks and plays, when it started playing, and the parts of its mock window it moves.
#[derive(Component, Debug)]
pub struct TransitionPreview {
    pub kind: TransitionKind,
    started: Duration,
    area: Entity,
    marks: [Entity; 2],
    lines: [Entity; 2],
}

/// The setting, for the Appearance page: its label and note over the row of tiles.
pub(crate) fn transition_setting() -> impl Scene {
    let note = "How the window moves from one app to the next. Press one to use it.";
    let tiles = bsn_list! {
        Node { flex_wrap: FlexWrap::Wrap, column_gap: { px(space::ROWS) }, row_gap: { px(space::ROWS) } }
        TransitionTiles
    };
    setting("Switching apps", note, tiles)
}

/// What a preview of `kind` shows `gone` into its loop: its cover's frame, the app whose content shows, and the app
/// marked on its rail (which moves as the switch starts, as the real rail's does).
pub fn preview_frame(kind: TransitionKind, gone: Duration) -> (CoverFrame, usize, usize) {
    let lap = PAUSE + COVER + REVEAL;
    let laps = (gone.as_nanos() / lap.as_nanos()) as usize;
    let within = gone.saturating_sub(lap * laps as u32);
    let (from, to) = (laps % 2, 1 - laps % 2);
    let kind = match kind {
        TransitionKind::Random => TransitionKind::DRAWN[laps % TransitionKind::DRAWN.len()],
        kind => kind,
    };
    let direction = if from == 0 { Direction::Down } else { Direction::Up };
    let frame = |phase, t| CoverFrame { kind, direction, phase, t };
    if within < PAUSE {
        (frame(Phase::Revealing, 1.0), from, from)
    } else if kind == TransitionKind::None {
        (frame(Phase::Revealing, 1.0), to, to)
    } else if within < PAUSE + COVER {
        (frame(Phase::Covering, (within - PAUSE).as_secs_f32() / COVER.as_secs_f32()), from, to)
    } else {
        (frame(Phase::Revealing, (within - PAUSE - COVER).as_secs_f32() / REVEAL.as_secs_f32()), to, to)
    }
}

/// The setting's previews: filling, playing and picking.
pub(crate) struct PreviewsUiPlugin;

impl Plugin for PreviewsUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_pick)
            .add_systems(Update, (fill_tiles, play_previews, mark_picked).chain().before(draw_covers).before(UiPlace));
    }
}

/// Fills a new row with a tile for each kind, the one picked ringed.
fn fill_tiles(
    new: Query<Entity, Added<TransitionTiles>>,
    picked: Res<AppTransition>,
    real: Res<Time<Real>>,
    mut commands: Commands,
) {
    for row in &new {
        // Half a pause in, so the previews move soon after the page opens.
        let started = real.elapsed().saturating_sub(PAUSE / 2);
        for kind in TransitionKind::ALL {
            spawn_preview(&mut commands, row, kind, kind == picked.0, started);
        }
    }
}

fn spawn_preview(commands: &mut Commands, row: Entity, kind: TransitionKind, chosen: bool, started: Duration) {
    let tile = spawn_choice_tile(commands, kind.label(), chosen);
    commands.entity(tile.root).insert(ChildOf(row));
    let rail = commands
        .spawn((
            Node {
                width: px(PREVIEW_RAIL_WIDTH),
                flex_shrink: 0.0,
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::top(px(space::LABEL_INSET)),
                row_gap: px(space::CONTROLS),
                ..default()
            },
            BackgroundTone(token::SURFACE_PANEL),
            ChildOf(tile.picture),
        ))
        .id();
    let mark = |commands: &mut Commands| {
        let node = Node {
            width: px(PREVIEW_MARK),
            height: px(PREVIEW_MARK),
            border_radius: BorderRadius::all(px(radius::SMALL)),
            ..default()
        };
        commands.spawn((node, BackgroundTone(token::FILL_SOLID), ChildOf(rail))).id()
    };
    let marks = [mark(commands), mark(commands)];
    let content = commands
        .spawn((
            Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(space::LABEL_INSET)),
                row_gap: px(space::CONTROLS),
                ..default()
            },
            BackgroundTone(token::SURFACE_WINDOW),
            ChildOf(tile.picture),
        ))
        .id();
    let (tone, widths) = &LOOKS[0];
    let line = |commands: &mut Commands, width: f32| {
        let node = Node { width: percent(width), height: px(PREVIEW_LINE_HEIGHT), ..default() };
        commands.spawn((node, BackgroundTone(tone.clone()), ChildOf(content))).id()
    };
    let lines = [line(commands, widths[0]), line(commands, widths[1])];
    let area = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: percent(0),
                top: percent(0),
                width: percent(100),
                height: percent(100),
                overflow: Overflow::clip(),
                ..default()
            },
            preview_frame(kind, Duration::ZERO).0,
            ChildOf(content),
        ))
        .id();
    spawn_cover_pieces(commands, area);
    commands.entity(tile.root).insert(TransitionPreview { kind, started, area, marks, lines });
}

/// Moves each preview to where its loop is now, in place, asking the window to redraw while any is shown.
fn play_previews(
    real: Res<Time<Real>>,
    previews: Query<&TransitionPreview>,
    mut frames: Query<&mut CoverFrame>,
    mut nodes: Query<&mut Node>,
    tones: Query<&BackgroundTone>,
    redraw: Option<ResMut<Messages<RequestRedraw>>>,
    mut commands: Commands,
) {
    if previews.is_empty() {
        return;
    }
    if let Some(mut redraw) = redraw {
        redraw.write(RequestRedraw);
    }
    let tone = |commands: &mut Commands, entity: Entity, wanted: Token| {
        if tones.get(entity).is_ok_and(|t| t.0 != wanted) {
            commands.entity(entity).insert(BackgroundTone(wanted));
        }
    };
    for preview in &previews {
        let (frame, shown, marked) = preview_frame(preview.kind, real.elapsed().saturating_sub(preview.started));
        if let Ok(mut at) = frames.get_mut(preview.area) {
            at.set_if_neq(frame);
        }
        for (app, mark) in preview.marks.into_iter().enumerate() {
            tone(&mut commands, mark, if app == marked { token::FILL_ACCENT } else { token::FILL_SOLID });
        }
        let (wanted, widths) = &LOOKS[shown];
        for (line, width) in preview.lines.into_iter().zip(widths) {
            tone(&mut commands, line, wanted.clone());
            if let Ok(node) = nodes.get_mut(line) {
                patch_node(node, |node| node.width = percent(*width));
            }
        }
    }
}

/// Rings the tile of the kind picked, wherever it was picked.
fn mark_picked(picked: Res<AppTransition>, mut tiles: Query<(&TransitionPreview, &mut ChoiceTile)>) {
    for (preview, mut tile) in &mut tiles {
        tile.set_if_neq(ChoiceTile { chosen: preview.kind == picked.0 });
    }
}

/// A tile pressed: switch apps its way from now on. The shell keeps it for the next launch.
fn on_pick(activate: On<Activate>, previews: Query<&TransitionPreview>, mut picked: ResMut<AppTransition>) {
    if let Ok(preview) = previews.get(activate.entity) {
        picked.set_if_neq(AppTransition(preview.kind));
    }
}

#[cfg(test)]
mod tests {
    use trailhead_ui::{Appearance, ModalPlugin, ThemeChoice};

    use super::*;
    use crate::ShellPlugin;
    use crate::settings::{ABOUT, APPEARANCE, CloseSettings, OpenSettings};
    use crate::testing::{settle, windowed};
    use crate::tests::{Fake, MAPS, NOTES};

    #[test]
    fn a_preview_rests_then_covers_switching_its_mark_at_once_then_reveals_the_other_app() {
        let kind = TransitionKind::Curtain;
        let at = |gone: Duration| preview_frame(kind, gone);
        let (frame, shown, marked) = at(Duration::ZERO);
        assert_eq!((frame.phase, frame.t, shown, marked), (Phase::Revealing, 1.0, 0, 0), "at rest, open");
        let (frame, shown, marked) = at(PAUSE + COVER / 2);
        assert_eq!(
            (frame.phase, frame.t, frame.direction, shown, marked),
            (Phase::Covering, 0.5, Direction::Down, 0, 1)
        );
        let (frame, shown, marked) = at(PAUSE + COVER + REVEAL / 2);
        assert_eq!((frame.phase, frame.t, shown, marked), (Phase::Revealing, 0.5, 1, 1));
        let lap = PAUSE + COVER + REVEAL;
        let (frame, shown, _) = at(lap + PAUSE + COVER / 2);
        assert_eq!((frame.direction, shown), (Direction::Up, 1), "and back up the rail");
        let kinds: Vec<TransitionKind> =
            (0..5).map(|n| preview_frame(TransitionKind::Random, lap * n + PAUSE).0.kind).collect();
        assert_eq!(kinds, TransitionKind::DRAWN, "Random plays each drawn kind in turn");
        let (_, shown, marked) = preview_frame(TransitionKind::None, PAUSE + COVER / 2);
        assert_eq!((shown, marked), (1, 1), "None switches as the rail's mark moves");
    }

    fn app() -> App {
        let mut app = windowed(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)));
        app.insert_resource(Appearance::new(ThemeChoice::Desktop))
            .add_plugins(ModalPlugin)
            .add_message::<RequestRedraw>();
        settle(&mut app);
        app
    }

    fn previews(app: &mut App) -> Vec<(Entity, TransitionKind, bool)> {
        let world = app.world_mut();
        let mut tiles = world.query::<(Entity, &TransitionPreview, &ChoiceTile)>();
        tiles.iter(world).map(|(e, p, t)| (e, p.kind, t.chosen)).collect()
    }

    fn redraws(app: &mut App) -> usize {
        app.world_mut().resource_mut::<Messages<RequestRedraw>>().drain().count()
    }

    #[test]
    fn the_previews_play_only_while_appearance_is_shown_and_a_press_picks_one() {
        let mut app = app();
        redraws(&mut app);
        app.update();
        assert_eq!(redraws(&mut app), 0, "nothing asks for redraws at rest");
        app.world_mut().trigger(OpenSettings { page: Some(APPEARANCE) });
        settle(&mut app);
        let shown = previews(&mut app);
        let kinds: Vec<TransitionKind> = shown.iter().map(|(_, k, _)| *k).collect();
        assert_eq!(kinds, TransitionKind::ALL, "one for each kind, in order");
        let chosen: Vec<TransitionKind> = shown.iter().filter(|(_, _, c)| *c).map(|(_, k, _)| *k).collect();
        assert_eq!(chosen, [TransitionKind::Fade], "the kind picked is ringed");
        assert!(redraws(&mut app) > 0, "they play");

        let iris = shown.iter().find(|(_, k, _)| *k == TransitionKind::Iris).unwrap().0;
        app.world_mut().trigger(Activate { entity: iris });
        settle(&mut app);
        assert_eq!(*app.world().resource::<AppTransition>(), AppTransition(TransitionKind::Iris));
        let chosen: Vec<TransitionKind> =
            previews(&mut app).iter().filter(|(_, _, c)| *c).map(|(_, k, _)| *k).collect();
        assert_eq!(chosen, [TransitionKind::Iris]);
        let tiles: Vec<Entity> = previews(&mut app).iter().map(|(e, _, _)| *e).collect();
        assert_eq!(tiles, shown.iter().map(|(e, _, _)| *e).collect::<Vec<_>>(), "picked in place");

        app.world_mut().trigger(OpenSettings { page: Some(ABOUT) });
        settle(&mut app);
        assert!(previews(&mut app).is_empty(), "gone with the page");
        redraws(&mut app);
        app.update();
        assert_eq!(redraws(&mut app), 0, "and stop asking for redraws");
        app.world_mut().trigger(OpenSettings { page: Some(APPEARANCE) });
        settle(&mut app);
        assert_eq!(previews(&mut app).len(), TransitionKind::ALL.len());
        app.world_mut().trigger(CloseSettings);
        settle(&mut app);
        assert!(previews(&mut app).is_empty(), "gone with the settings");
    }
}
