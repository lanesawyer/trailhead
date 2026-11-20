//! Helpers for tests of anything built on the kit, here and in apps that depend on it (turn on the `test-support`
//! feature in `[dev-dependencies]`).

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input_focus::FocusedInput;
use bevy::prelude::*;

/// Triggers a key press on `target` as if it had focus, without running a frame.
///
/// The press goes to a real [`Window`] it spawns: it climbs to the window last and stops there, where an entity
/// without `Window` would be handed the press again forever. To press with nothing focused, make `target` the
/// window.
pub fn press_key(world: &mut World, target: Entity, key_code: KeyCode, logical_key: Key) {
    let window = world.spawn(Window::default()).id();
    let input = KeyboardInput { key_code, logical_key, state: ButtonState::Pressed, text: None, repeat: false, window };
    world.trigger(FocusedInput::new(target, input, window));
}

/// Presses Escape on `target`; see [`press_key`].
pub fn press_escape(world: &mut World, target: Entity) {
    press_key(world, target, KeyCode::Escape, Key::Escape);
}

#[cfg(test)]
mod tests {
    use bevy::input_focus::FocusedInput;

    use super::*;

    #[derive(Resource, Default)]
    struct Seen(Vec<(Entity, KeyCode)>);

    #[test]
    fn a_press_reaches_the_target_once_as_a_pressed_key() {
        let mut app = App::new();
        app.init_resource::<Seen>().add_observer(|key: On<FocusedInput<KeyboardInput>>, mut seen: ResMut<Seen>| {
            assert!(key.input.state.is_pressed() && !key.input.repeat);
            seen.0.push((key.focused_entity, key.input.key_code));
        });
        let target = app.world_mut().spawn_empty().id();
        press_escape(app.world_mut(), target);
        assert_eq!(app.world().resource::<Seen>().0, [(target, KeyCode::Escape)]);
    }
}
