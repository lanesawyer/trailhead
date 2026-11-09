//! Placeholder rows, shown where content is on its way.
//!
//! Something like the shape of what is coming, pulsing, so a section being filled in says so rather than showing a
//! stand-in that is swapped out a moment later.
//!
//! Ported from bevy-data-explorer's `widgets/skeleton.rs`, with its palette swapped for the kit's tokens.

use std::f32::consts::TAU;

use bevy::feathers::theme::UiTheme;
use bevy::prelude::*;

use crate::spacing::{radius, space};
use crate::theme::token;

/// Seconds for one pulse.
const PULSE_SECS: f32 = 1.2;
/// The faintest the pulse gets, as a fraction of the bar's color.
const PULSE_LOW: f32 = 0.35;

/// One placeholder bar. Painted by the kit rather than a theme token, since the pulse is a color that changes every
/// frame.
#[derive(Component, Clone, Default)]
pub struct SkeletonBar;

/// A column of `rows` bars, each `row_height` tall.
pub fn spawn_skeleton(commands: &mut Commands, rows: usize, row_height: f32) -> Entity {
    let bars: Vec<Entity> = (0..rows)
        .map(|_| {
            commands
                .spawn((
                    SkeletonBar,
                    Node {
                        width: percent(100),
                        height: px(row_height),
                        border_radius: BorderRadius::all(px(radius::CONTROL)),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                    Pickable::IGNORE,
                ))
                .id()
        })
        .collect();
    commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                width: percent(100),
                row_gap: px(space::LIST_ITEMS),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .add_children(&bars)
        .id()
}

/// How bright a bar is `secs` into the pulse, from [`PULSE_LOW`] to 1.
fn pulse(secs: f32) -> f32 {
    let wave = 0.5 + 0.5 * (secs * TAU / PULSE_SECS).sin();
    PULSE_LOW + (1.0 - PULSE_LOW) * wave
}

pub(crate) fn pulse_skeletons(
    time: Res<Time>,
    theme: Res<UiTheme>,
    mut bars: Query<&mut BackgroundColor, With<SkeletonBar>>,
) {
    let color = theme.color(&token::FILL_SOLID.0).with_alpha(pulse(time.elapsed_secs()));
    for mut background in &mut bars {
        background.set_if_neq(BackgroundColor(color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pulse_stays_between_its_faintest_and_full() {
        for step in 0..100 {
            let alpha = pulse(step as f32 * PULSE_SECS / 100.0);
            assert!((PULSE_LOW - f32::EPSILON..=1.0 + f32::EPSILON).contains(&alpha), "{alpha}");
        }
    }
}
