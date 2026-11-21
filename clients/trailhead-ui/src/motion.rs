//! What moves: easing curves, and a node's background faded in or out.
//!
//! Something that moves is driven by real time (`Time<Real>`), so a slow frame carries it further instead of stretching
//! it, and asks for a redraw (`bevy::window::RequestRedraw`) every frame it moves: a window that updates reactively
//! draws only on input otherwise. It stops asking when it stops moving. A position or size that moves is set in place
//! each frame with [`crate::patch_node`], never by spawning the node again.

use bevy::feathers::theme::{SurfaceLevel, ThemeBackgroundColor, ThemeContext, UiTheme};
use bevy::prelude::*;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackgroundTone, Theme, token};

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
