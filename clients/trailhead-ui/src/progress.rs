//! A progress bar: a track, filled from the left as far as something has come.
//!
//! Set its [`Progress`] to move it; the fill follows in place.

use bevy::prelude::*;

use super::{BackgroundTone, extent, radius, token};

/// How far along, from 0 (nothing done) to 1 (all of it).
#[derive(Component, Clone, Copy, PartialEq, Default, Debug)]
pub struct Progress(pub f32);

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
        Children [
            Node { width: { percent(fraction * 100.0) }, height: percent(100) }
            BackgroundTone({ token::STATUS_OK })
            ProgressFill
        ]
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
}
