//! What moves: easing curves, and a node's background faded in or out.
//!
//! Something that moves is driven by real time (`Time<Real>`), so a slow frame carries it further instead of stretching
//! it, and asks for a redraw (`bevy::window::RequestRedraw`) every frame it moves: a window that updates reactively
//! draws only on input otherwise. It stops asking when it stops moving. A position or size that moves is set in place
//! each frame with [`crate::patch_node`], never by spawning the node again.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use bevy::feathers::theme::{SurfaceLevel, ThemeBackgroundColor, ThemeContext, UiTheme};
use bevy::prelude::*;
use bevy::window::RequestRedraw;

use crate::Appearance;

/// Starts slow and ends at full speed, `t` from 0 to 1: for something arriving to cover what is there.
pub fn ease_in(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t
}

/// Starts at full speed and settles, `t` from 0 to 1: for something leaving to show what is under it.
pub fn ease_out(t: f32) -> f32 {
    let t = 1.0 - t.clamp(0.0, 1.0);
    1.0 - t * t
}

/// A node's background drawn at this share of its tone's opacity, from 0 (not drawn) to 1 (as the tone is): something
/// fading in or out. Put it beside a [`crate::BackgroundTone`] (or a [`crate::Surface`]) and set it with `set_if_neq`
/// as it moves; the kit repaints the background at it, and keeps it through a change of theme.
#[derive(Component, Clone, Copy, PartialEq, Debug)]
pub struct BackgroundFade(pub f32);

impl Default for BackgroundFade {
    fn default() -> Self {
        BackgroundFade(1.0)
    }
}

/// Paints each faded background at its share of its tone, after feathers paints the tone whole.
pub(crate) fn fade_backgrounds(
    theme: Option<Res<UiTheme>>,
    mut faded: Query<(&BackgroundFade, &ThemeBackgroundColor, Option<&ThemeContext>, &mut BackgroundColor)>,
) {
    let Some(theme) = theme else { return };
    for (fade, token, context, mut color) in &mut faded {
        let full = theme.context_color(&token.0, context.map_or(SurfaceLevel::Base, |c| c.0));
        let wanted = full.with_alpha(full.alpha() * fade.0.clamp(0.0, 1.0));
        if color.0 != wanted {
            color.0 = wanted;
        }
    }
}

/// How long a [`Ring`] lasts.
pub const RING_TIME: Duration = Duration::from_millis(500);
/// How far a ringing node swings either way at first, in radians.
const RING_SWING: f32 = 0.35;
/// How many times it swings back and forth while it rings.
const RING_SWINGS: f32 = 3.0;
/// How much larger it grows at the middle of the ring.
const RING_GROWTH: f32 = 0.15;

/// Rings a node as a bell rings, to say something arrived: it swings either way, less each time, and grows a little
/// and shrinks back, over [`RING_TIME`] of real time. Insert `Ring::default()` on the node to ring it, and again to
/// ring it from the start; the kit moves the node's `UiTransform`, puts it back to rest and takes the `Ring` off when it
/// is done. While the desktop asks for less motion ([`Appearance::reduce_motion`]) it is taken off at once, unmoved.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Ring {
    started: Option<Duration>,
}

/// Where a ringing node is `t` of the way through its ring (0 to 1): its turn in radians, clockwise, and its scale.
pub fn ring_pose(t: f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    let turn = RING_SWING * (1.0 - t) * (TAU * RING_SWINGS * t).sin();
    (turn, 1.0 + RING_GROWTH * (PI * t).sin())
}

/// Moves each ringing node to where its ring is now, in place, asking for a redraw while any rings.
pub(crate) fn ring_rings(
    real: Res<Time<Real>>,
    appearance: Option<Res<Appearance>>,
    mut rings: Query<(Entity, &mut Ring, Option<&mut UiTransform>)>,
    redraw: Option<ResMut<Messages<RequestRedraw>>>,
    mut commands: Commands,
) {
    if rings.is_empty() {
        return;
    }
    let still = appearance.is_some_and(|a| a.reduce_motion());
    let now = real.elapsed();
    let mut moving = false;
    for (entity, mut ring, transform) in &mut rings {
        let started = *ring.started.get_or_insert(now);
        let t = now.saturating_sub(started).as_secs_f32() / RING_TIME.as_secs_f32();
        let done = still || t >= 1.0;
        let (turn, scale) = if done { (0.0, 1.0) } else { ring_pose(t) };
        let wanted = UiTransform { rotation: Rot2::radians(turn), scale: Vec2::splat(scale), ..UiTransform::IDENTITY };
        match transform {
            Some(mut transform) => {
                transform.set_if_neq(wanted);
            }
            None => {
                commands.entity(entity).insert(wanted);
            }
        }
        if done {
            commands.entity(entity).remove::<Ring>();
        } else {
            moving = true;
        }
    }
    if let (true, Some(mut redraw)) = (moving, redraw) {
        redraw.write(RequestRedraw);
    }
}

#[cfg(test)]
mod tests {
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::{BackgroundTone, Theme, ThemeChoice, token};

    fn ringing_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(50)))
            .add_message::<RequestRedraw>()
            .add_systems(Update, ring_rings);
        app.update();
        app
    }

    #[test]
    fn a_ring_swings_grows_and_comes_back_to_rest_then_ends() {
        assert_eq!(ring_pose(0.0), (0.0, 1.0));
        let (turn, scale) = ring_pose(1.0);
        assert!(turn.abs() < 1e-5 && (scale - 1.0).abs() < 1e-5, "at rest at the end");
        assert!(ring_pose(0.5).1 > 1.1, "grown at the middle");
        let turns: Vec<f32> = (1..20).map(|n| ring_pose(n as f32 / 20.0).0).collect();
        assert!(turns.iter().any(|t| *t > 0.1) && turns.iter().any(|t| *t < -0.1), "it swings both ways");

        let mut app = ringing_app();
        let bell = app.world_mut().spawn((Node::default(), Ring::default())).id();
        let mut redraws = 0;
        let mut moved = false;
        for _ in 0..20 {
            app.update();
            moved |= *app.world().get::<UiTransform>(bell).unwrap() != UiTransform::IDENTITY;
            redraws += app.world_mut().resource_mut::<Messages<RequestRedraw>>().drain().count();
        }
        assert!(moved, "it moved");
        assert!(redraws > 5, "it asked for frames while it moved");
        assert_eq!(*app.world().get::<UiTransform>(bell).unwrap(), UiTransform::IDENTITY, "back at rest");
        assert!(app.world().get::<Ring>(bell).is_none(), "and done");
    }

    #[test]
    fn a_desktop_that_asks_for_less_motion_sees_no_ring() {
        let mut app = ringing_app();
        app.insert_resource(Appearance::new(ThemeChoice::Dark).with_reduce_motion(true));
        let bell = app.world_mut().spawn((Node::default(), Ring::default())).id();
        app.update();
        assert_eq!(*app.world().get::<UiTransform>(bell).unwrap(), UiTransform::IDENTITY);
        assert!(app.world().get::<Ring>(bell).is_none());
    }

    #[test]
    fn the_curves_run_from_nothing_to_all_and_lean_the_way_they_say() {
        for curve in [ease_in, ease_out] {
            assert_eq!((curve(0.0), curve(1.0)), (0.0, 1.0));
            assert_eq!((curve(-1.0), curve(2.0)), (0.0, 1.0), "clamped");
            let mut last = 0.0;
            for step in 1..=20 {
                let now = curve(step as f32 / 20.0);
                assert!(now >= last, "never goes back");
                last = now;
            }
        }
        assert!(ease_in(0.5) < 0.5, "starts slow");
        assert!(ease_out(0.5) > 0.5, "starts fast");
    }

    #[test]
    fn a_faded_background_wears_its_tone_at_its_share() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).insert_resource(Theme::new().resource()).add_systems(Update, fade_backgrounds);
        let node = app.world_mut().spawn((BackgroundTone(token::SURFACE_PANEL), BackgroundFade(0.25))).id();
        app.update();
        let full = Theme::new().color(&token::SURFACE_PANEL);
        let alpha = |app: &App| app.world().get::<BackgroundColor>(node).unwrap().0.alpha();
        assert!((alpha(&app) - full.alpha() * 0.25).abs() < 1e-6);
        app.world_mut().get_mut::<BackgroundFade>(node).unwrap().0 = 1.0;
        app.update();
        assert_eq!(app.world().get::<BackgroundColor>(node).unwrap().0, full);
    }
}
