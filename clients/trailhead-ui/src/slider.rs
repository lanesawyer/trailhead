//! A slider over feathers' `FeathersSlider`, which feathers styles and drives.
//!
//! Ported from bevy-data-explorer's `widgets/slider.rs`.

use bevy::feathers::controls::FeathersSlider;
use bevy::feathers::theme::UiTheme;
use bevy::feathers::tokens;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::{InteractionDisabled, Pressed};
use bevy::ui_widgets::{Slider, SliderPrecision, SliderValue, TrackClick};

/// Spawn a slider over `range`, showing its value to `decimals` places.
///
/// The kit adds `bevy::ui_widgets::slider_self_update`, so a drag writes the value back; read `SliderValue`, or
/// observe `ValueChange<f32>` on the slider.
///
/// `SliderPrecision` is added explicitly because feathers' own scene omits it while the system that moves the fill and
/// rewrites the value text requires it. Without it the slider drags but never redraws.
///
/// Feathers hands the track `TrackClick::Drag`, which treats a click anywhere on the track as the start of a drag from
/// wherever the value already was: clicking halfway didn't set it to halfway, it just took hold. `TrackClick::Snap`
/// puts it where it was clicked, which is what a track is for. Dragging from there still works: the drag records the
/// value at the moment it starts, which is the snapped one.
pub fn spawn_slider(commands: &mut Commands, value: f32, range: (f32, f32), decimals: i32) -> Entity {
    commands
        .spawn_scene(bsn! {
            @FeathersSlider {
                @min: { range.0 },
                @max: { range.1 }
            }
            SliderValue({ value })
            // After the scene, so this patches the track behavior it set.
            Slider { track_click: { TrackClick::Snap } }
            SliderPrecision({ decimals })
        })
        .id()
}

/// Paint every slider in the theme now in place.
///
/// Feathers draws a slider's bar as a gradient it colors itself, and only when the slider is spawned, hovered, pressed
/// or disabled, never when the theme changes. So a slider built before the theme settled stayed feathers' own blue
/// until the pointer passed over it. The same tokens feathers would choose, chosen again.
#[allow(clippy::type_complexity)]
pub(crate) fn repaint_sliders(
    theme: Res<UiTheme>,
    mut sliders: Query<
        (Has<InteractionDisabled>, Has<Pressed>, &Hovered, &mut BackgroundGradient),
        With<FeathersSlider>,
    >,
) {
    for (disabled, pressed, hovered, mut gradient) in &mut sliders {
        let (bar, track) = if disabled {
            (tokens::SLIDER_BAR_DISABLED, tokens::SLIDER_BG_DISABLED)
        } else if pressed {
            (tokens::SLIDER_BAR_PRESSED, tokens::SLIDER_BG_PRESSED)
        } else if hovered.0 {
            (tokens::SLIDER_BAR_HOVER, tokens::SLIDER_BG_HOVER)
        } else {
            (tokens::SLIDER_BAR, tokens::SLIDER_BG)
        };
        let (bar, track) = (theme.color(&bar), theme.color(&track));
        if let [Gradient::Linear(linear)] = &mut gradient.0[..]
            && let [first, second, third, fourth] = &mut linear.stops[..]
        {
            first.color = bar;
            second.color = bar;
            third.color = track;
            fourth.color = track;
        }
    }
}
