//! A progress bar: a track, filled from the left in the accent as far as something has come, and a [`meter`]: a bar
//! with words over and under it.
//!
//! Set its [`Progress`] to move it and its [`ProgressTone`] to recolor it; the fill follows in place.

use bevy::prelude::*;

use super::{BackgroundTone, Token, extent, radius, size, space, text, text_dim, token};

/// How far along, from 0 (nothing done) to 1 (all of it).
#[derive(Component, Clone, Copy, PartialEq, Default, Debug)]
pub struct Progress(pub f32);

/// The color a bar fills in: the accent unless set, a status (`STATUS_WARN`) for one running out of room. Insert a
/// new one to change it.
#[derive(Component, Clone, PartialEq, Debug)]
pub struct ProgressTone(pub Token);

impl Default for ProgressTone {
    fn default() -> Self {
        ProgressTone(token::FILL_ACCENT)
    }
}

/// Which text of a [`meter`] this is.
#[derive(Component, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum MeterPart {
    #[default]
    Label,
    Value,
    Note,
}

/// The part of a bar that fills.
#[derive(Component, Clone, Copy, Default)]
pub(crate) struct ProgressFill;

/// A bar `fraction` full (clamped to 0..=1), filling the width it is given.
pub fn progress_bar(fraction: f32) -> impl Scene {
    let fraction = fraction.clamp(0.0, 1.0);
    bsn! {
        Node {
            flex_grow: 1.0,
            height: { px(extent::PROGRESS_HEIGHT) },
            border_radius: { BorderRadius::all(px(radius::SMALL)) },
            overflow: Overflow::clip(),
        }
        BackgroundTone({ token::FILL_SOLID })
        Progress({ fraction })
        ProgressTone
        Children [
            Node { width: { percent(fraction * 100.0) }, height: percent(100) }
            BackgroundTone({ token::FILL_ACCENT })
            ProgressFill
        ]
    }
}

/// How full something is, in a column: `label` and `value` on a line (a usage window and its percent), a
/// [`progress_bar`] `fraction` full in `tone` under them, and a dim `note` under that (when it resets). Each text
/// carries its [`MeterPart`], for updating in place; the bar carries the [`Progress`] and [`ProgressTone`].
pub fn meter(
    label: impl Into<String>,
    value: impl Into<String>,
    note: impl Into<String>,
    fraction: f32,
    tone: Token,
) -> impl Scene {
    let (label, value, note) = (label.into(), value.into(), note.into());
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) }, flex_shrink: { 0.0_f32 } }
        Children [
            Node { justify_content: JustifyContent::SpaceBetween, column_gap: { px(space::CONTROLS) } }
            Children [
                @text(label, size::SECONDARY)
                MeterPart::Label
                --
                @text(value, size::SECONDARY)
                MeterPart::Value
            ]
            --
            // A bar fills its row; in this column it stays its own height.
            @progress_bar(fraction)
            Node { flex_grow: { 0.0_f32 } }
            ProgressTone({ tone })
            --
            @text_dim(note, size::SMALL)
            MeterPart::Note
        ]
    }
}

/// Paints each bar's fill in its tone when that changes.
pub(crate) fn tone_progress(
    bars: Query<(&ProgressTone, &Children), Changed<ProgressTone>>,
    fills: Query<&BackgroundTone, With<ProgressFill>>,
    mut commands: Commands,
) {
    for (ProgressTone(tone), children) in &bars {
        for child in children {
            if fills.get(*child).is_ok_and(|had| had.0 != *tone) {
                commands.entity(*child).insert(BackgroundTone(tone.clone()));
            }
        }
    }
}

/// Fills each bar whose progress changed as far as it says.
pub(crate) fn fill_progress(
    bars: Query<(&Progress, &Children), Changed<Progress>>,
    mut fills: Query<&mut Node, With<ProgressFill>>,
) {
    for (Progress(fraction), children) in &bars {
        let width = percent(fraction.clamp(0.0, 1.0) * 100.0);
        for child in children {
            if let Ok(mut fill) = fills.get_mut(*child)
                && fill.width != width
            {
                fill.width = width;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fill_follows_the_progress_and_never_spills() {
        let mut app = App::new();
        app.add_systems(Update, fill_progress);
        let fill = app.world_mut().spawn((Node::default(), ProgressFill)).id();
        let bar = app.world_mut().spawn(Progress(0.25)).add_child(fill).id();
        app.update();
        assert_eq!(app.world().get::<Node>(fill).unwrap().width, percent(25));
        app.world_mut().entity_mut(bar).insert(Progress(3.0));
        app.update();
        assert_eq!(app.world().get::<Node>(fill).unwrap().width, percent(100));
    }

    #[test]
    fn the_fill_takes_the_bars_tone() {
        let mut app = App::new();
        app.add_systems(Update, tone_progress);
        let fill = app.world_mut().spawn((Node::default(), BackgroundTone(token::FILL_ACCENT), ProgressFill)).id();
        let bar = app.world_mut().spawn(ProgressTone::default()).add_child(fill).id();
        app.update();
        assert_eq!(app.world().get::<BackgroundTone>(fill).unwrap().0, token::FILL_ACCENT);
        app.world_mut().entity_mut(bar).insert(ProgressTone(token::STATUS_WARN));
        app.update();
        assert_eq!(app.world().get::<BackgroundTone>(fill).unwrap().0, token::STATUS_WARN);
    }
}
