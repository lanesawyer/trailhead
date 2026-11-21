//! How big the window's UI is drawn: [`TextSize`], a step on a fixed ladder from 80% to 200%, which a person moves with
//! Ctrl and `=`/`+`, `-` and `0` (Cmd on macOS), and an app or a settings menu with [`ChangeTextSize`].
//!
//! It scales the whole UI, not text alone: text, icons, spacing, controls and panels grow together, through Bevy's
//! `UiScale`, which this owns. That is what "bigger text" means in an app laid out in named sizes and spaces: text
//! grown on its own would overflow the controls and rows sized to hold it. Ported from bevy-data-explorer's interface
//! size (`src/ui/settings.rs`), with a browser's ladder of steps for the keys instead of its five buttons.
//!
//! Under a size other than 100%, the units `Val::Px` is in (UI pixels) are larger than the window's logical pixels:
//! a `ComputedNode` read through `inverse_scale_factor` and a `ScrollPosition` are in UI pixels, while the window's
//! size, the pointer and a drag's `distance` and `delta` stay logical. Divide a logical length by [`TextSize::scale`]
//! (or `UiScale`) before measuring it against layout. The kit's guide, section 4, has the rule.

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input_focus::FocusedInput;
use bevy::prelude::*;
use bevy::ui::UiSystems;

/// How big the UI is drawn, as a step on [`TextSize::STEPS`]. The window draws at it from the frame it is set, before
/// layout: set it as the app is built to start at a size kept from last time, with no frame drawn at 100% first.
/// Change it with [`ChangeTextSize`], or by setting the resource.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TextSize {
    step: usize,
}

impl TextSize {
    /// The sizes on offer, in percent, smallest first: what Ctrl and `-` or `=` step through, and what a settings
    /// menu lists.
    pub const STEPS: [u32; 8] = [80, 90, 100, 110, 125, 150, 175, 200];
    /// 100%, as the kit's sizes and spaces are written.
    pub const DEFAULT: TextSize = TextSize { step: 2 };

    /// Every size on offer, smallest first.
    pub fn all() -> impl Iterator<Item = TextSize> {
        (0..Self::STEPS.len()).map(|step| TextSize { step })
    }

    /// The size at `step` of [`TextSize::STEPS`], if there is one.
    pub fn at(step: usize) -> Option<TextSize> {
        (step < Self::STEPS.len()).then_some(TextSize { step })
    }

    /// The size on offer nearest `percent`, for one kept by a build with another ladder or written by hand.
    pub fn nearest(percent: u32) -> TextSize {
        Self::all().min_by_key(|size| size.percent().abs_diff(percent)).unwrap_or_default()
    }

    /// Where it is on [`TextSize::STEPS`].
    pub fn step(self) -> usize {
        self.step
    }

    /// How big, in percent: 125.
    pub fn percent(self) -> u32 {
        Self::STEPS[self.step]
    }

    /// How big, as the multiple of a logical pixel a UI pixel is drawn at: 1.25. What `UiScale` is set to.
    pub fn scale(self) -> f32 {
        self.percent() as f32 / 100.0
    }

    /// How a person reads it: "125%".
    pub fn label(self) -> String {
        format!("{}%", self.percent())
    }

    /// The next size up; the largest stays where it is.
    #[must_use]
    pub fn bigger(self) -> TextSize {
        Self::at(self.step + 1).unwrap_or(self)
    }

    /// The next size down; the smallest stays where it is.
    #[must_use]
    pub fn smaller(self) -> TextSize {
        self.step.checked_sub(1).and_then(Self::at).unwrap_or(self)
    }

    /// 100%.
    #[must_use]
    pub fn reset(self) -> TextSize {
        Self::DEFAULT
    }

    pub fn is_default(self) -> bool {
        self == Self::DEFAULT
    }
}

impl Default for TextSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Changes the [`TextSize`]: what the keys trigger, and what an app or a settings menu triggers to change it without
/// touching `UiScale`.
#[derive(Event, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeTextSize {
    Bigger,
    Smaller,
    Reset,
    To(TextSize),
}

/// The [`TextSize`], its keys and [`ChangeTextSize`]. Part of [`UiPlugin`](crate::UiPlugin); add it on its own only
/// where `UiPlugin` isn't (a headless test, or the Trailhead shell's logic, which checks first).
pub struct TextSizePlugin;

impl Plugin for TextSizePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TextSize>()
            .init_resource::<UiScale>()
            .add_observer(on_change)
            .add_observer(on_key)
            .add_systems(PostUpdate, follow_text_size.run_if(resource_changed::<TextSize>).before(UiSystems::Prepare));
    }
}

fn on_change(change: On<ChangeTextSize>, mut size: ResMut<TextSize>) {
    let next = match *change {
        ChangeTextSize::Bigger => size.bigger(),
        ChangeTextSize::Smaller => size.smaller(),
        ChangeTextSize::Reset => size.reset(),
        ChangeTextSize::To(to) => to,
    };
    size.set_if_neq(next);
}

/// Ctrl (Cmd on macOS) with `=` or `+` makes the UI bigger, with `-` smaller, and with `0` puts it back at 100%. Taken
/// at the window, once the press has climbed past everything focused: a text input passes these on (it takes only
/// Ctrl with A, C, X and V, and the arrows), but one composing with an input method keeps every key, and so does a
/// control that stops a press.
fn on_key(
    key: On<FocusedInput<KeyboardInput>>,
    windows: Query<(), With<Window>>,
    held: Option<Res<ButtonInput<KeyCode>>>,
    mut commands: Commands,
) {
    if !windows.contains(key.event_target()) || !key.input.state.is_pressed() {
        return;
    }
    let Some(held) = held else { return };
    let command = if cfg!(target_os = "macos") {
        [KeyCode::SuperLeft, KeyCode::SuperRight]
    } else {
        [KeyCode::ControlLeft, KeyCode::ControlRight]
    };
    if !held.any_pressed(command) || held.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]) {
        return;
    }
    if let Some(change) = change_for(key.input.key_code, &key.input.logical_key) {
        commands.trigger(change);
    }
}

/// The change a key pressed with the command modifier asks for, by where the key is or what it types, so `+` on a
/// layout that has it without Shift counts too.
fn change_for(code: KeyCode, typed: &Key) -> Option<ChangeTextSize> {
    let typed = match typed {
        Key::Character(c) => c.as_str(),
        _ => "",
    };
    match (code, typed) {
        (KeyCode::Equal | KeyCode::NumpadAdd, _) | (_, "=" | "+") => Some(ChangeTextSize::Bigger),
        (KeyCode::Minus | KeyCode::NumpadSubtract, _) | (_, "-") => Some(ChangeTextSize::Smaller),
        (KeyCode::Digit0 | KeyCode::Numpad0, _) | (_, "0") => Some(ChangeTextSize::Reset),
        _ => None,
    }
}

/// Draws the UI at the [`TextSize`], before layout in the frame it changed.
fn follow_text_size(size: Res<TextSize>, mut scale: ResMut<UiScale>) {
    let wanted = size.scale();
    if scale.0 != wanted {
        scale.0 = wanted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::press_key;

    #[test]
    fn the_ladder_runs_from_80_to_200_with_100_in_it_and_stops_at_its_ends() {
        let all: Vec<TextSize> = TextSize::all().collect();
        assert_eq!(all.first().map(|s| s.label()).as_deref(), Some("80%"));
        assert_eq!(all.last().map(|s| s.label()).as_deref(), Some("200%"));
        assert_eq!(TextSize::DEFAULT.percent(), 100);
        assert_eq!(TextSize::DEFAULT.bigger().label(), "110%");
        assert_eq!(TextSize::DEFAULT.bigger().bigger().scale(), 1.25);
        assert_eq!(all[0].smaller(), all[0]);
        assert_eq!(all[7].bigger(), all[7]);
        assert_eq!(all[5].reset(), TextSize::DEFAULT);
        assert_eq!(TextSize::at(8), None);
    }

    #[test]
    fn a_kept_percent_comes_back_as_the_nearest_size_on_offer() {
        assert_eq!(TextSize::nearest(150).label(), "150%");
        assert_eq!(TextSize::nearest(130).label(), "125%");
        assert_eq!(TextSize::nearest(0).label(), "80%");
        assert_eq!(TextSize::nearest(900).label(), "200%");
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).init_resource::<ButtonInput<KeyCode>>().add_plugins(TextSizePlugin);
        app.update();
        app
    }

    fn size(app: &App) -> TextSize {
        *app.world().resource::<TextSize>()
    }

    #[test]
    fn a_change_moves_the_size_and_the_ui_scale_follows_before_the_next_layout() {
        let mut app = app();
        assert_eq!(app.world().resource::<UiScale>().0, 1.0);
        app.world_mut().trigger(ChangeTextSize::Bigger);
        app.world_mut().trigger(ChangeTextSize::Bigger);
        app.update();
        assert_eq!((size(&app).label(), app.world().resource::<UiScale>().0), ("125%".into(), 1.25));
        app.world_mut().trigger(ChangeTextSize::To(TextSize::nearest(80)));
        app.world_mut().trigger(ChangeTextSize::Smaller);
        app.update();
        assert_eq!(app.world().resource::<UiScale>().0, 0.8);
        app.world_mut().trigger(ChangeTextSize::Reset);
        app.update();
        assert_eq!(app.world().resource::<UiScale>().0, 1.0);
    }

    #[test]
    fn a_size_set_while_the_app_is_built_is_drawn_from_the_first_frame() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(TextSizePlugin).insert_resource(TextSize::nearest(150));
        app.update();
        assert_eq!(app.world().resource::<UiScale>().0, 1.5);
    }

    fn press_with(app: &mut App, modifier: Option<KeyCode>, code: KeyCode, typed: &str) {
        let mut held = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        held.reset_all();
        if let Some(modifier) = modifier {
            held.press(modifier);
        }
        let window = app.world_mut().spawn(Window::default()).id();
        press_key(app.world_mut(), window, code, Key::Character(typed.into()));
        app.world_mut().flush();
    }

    const COMMAND: KeyCode = if cfg!(target_os = "macos") { KeyCode::SuperLeft } else { KeyCode::ControlLeft };

    #[test]
    fn the_command_modifier_with_equals_plus_minus_and_zero_moves_the_size() {
        let mut app = app();
        press_with(&mut app, Some(COMMAND), KeyCode::Equal, "=");
        assert_eq!(size(&app).label(), "110%");
        press_with(&mut app, Some(COMMAND), KeyCode::Equal, "+");
        press_with(&mut app, Some(COMMAND), KeyCode::NumpadAdd, "+");
        assert_eq!(size(&app).label(), "150%");
        press_with(&mut app, Some(COMMAND), KeyCode::Minus, "-");
        assert_eq!(size(&app).label(), "125%");
        press_with(&mut app, Some(COMMAND), KeyCode::Digit0, "0");
        assert_eq!(size(&app), TextSize::DEFAULT);
    }

    #[test]
    fn the_keys_without_the_modifier_or_with_alt_are_left_alone() {
        let mut app = app();
        press_with(&mut app, None, KeyCode::Equal, "=");
        press_with(&mut app, Some(KeyCode::ShiftLeft), KeyCode::Equal, "+");
        assert_eq!(size(&app), TextSize::DEFAULT);
        let mut held = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        held.press(COMMAND);
        held.press(KeyCode::AltLeft);
        let window = app.world_mut().spawn(Window::default()).id();
        press_key(app.world_mut(), window, KeyCode::Equal, Key::Character("=".into()));
        app.world_mut().flush();
        assert_eq!(size(&app), TextSize::DEFAULT);
    }

    #[test]
    fn a_press_in_a_focused_field_reaches_the_window_and_counts_once() {
        let mut app = app();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(COMMAND);
        let panel = app.world_mut().spawn(Node::default()).id();
        let field = app.world_mut().spawn((Node::default(), ChildOf(panel))).id();
        press_key(app.world_mut(), field, KeyCode::Equal, Key::Character("=".into()));
        app.world_mut().flush();
        assert_eq!(size(&app).label(), "110%");
    }
}
