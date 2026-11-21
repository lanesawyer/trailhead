//! Settings, laid out the one way every app's settings are: Lookout's settings modal, lifted into the kit so an app's
//! settings look like it.
//!
//! - [`settings_page`]: a page of settings (a modal's body, or one tab's page of it), its settings a row apart.
//! - [`spawn_settings_section`]: a group of settings under a dim heading, a group's distance from what is above it,
//!   with its settings in a box of their own for the caller to fill and keep (with `Keyed`).
//! - [`setting`]: one setting, its label over its control (a select, a button group, a value), with a dim note under
//!   it saying what it does when there is one.
//! - [`setting_toggle`]: a setting that is on or off, a toggle switch with its label after it, and its note.
//!
//! A list of toggles stacked bare, with nothing between them, read as one block of text (TRAIL-1610); in a section
//! they stand a row apart under their heading.

use bevy::prelude::*;

use crate::containers::section;
use crate::patch::display;
use crate::spacing::space;
use crate::text::{size, text_dim};
use crate::toggle::toggle_switch;

/// A page of settings, filling the width of what holds it: its settings and sections a row apart, which a section's own
/// margin widens to a group's. Spawn the settings and sections as its children, in order.
pub fn settings_page() -> impl Scene {
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::ROWS) }, min_width: px(0) }
    }
}

/// One setting: `label` over `control`, and `note` dim under it when it is not empty. Pass the control into the scene
/// (as for [`crate::field`]), with the caller's marker on it.
pub fn setting(label: impl Into<String>, note: impl Into<String>, control: impl SceneList) -> impl Scene {
    let label = label.into();
    let note = note.into();
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) } }
        Children [ @text_dim(label, size::SECONDARY) -- {control} -- @setting_note(note) ]
    }
}

/// A setting that is on or off: a toggle switch, on when `checked`, with `label` after it and `note` dim under it when
/// it is not empty. `own` goes on the switch, as for [`toggle_switch`]: a press flips it and sends
/// `ValueChange<bool>` from it.
pub fn setting_toggle(label: impl Into<String>, note: impl Into<String>, checked: bool, own: impl Scene) -> impl Scene {
    let label = label.into();
    let note = note.into();
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) } }
        Children [ @toggle_switch(label, checked, own) -- @setting_note(note) ]
    }
}

/// A setting's note, hidden while it says nothing.
fn setting_note(note: String) -> impl Scene {
    let shown = display(!note.is_empty());
    bsn! {
        @text_dim(note, size::SECONDARY)
        SettingNote
        Node { display: { shown } }
    }
}

/// The dim line under a [`setting`] saying what it does. Set its `Text` with `set_text` to change it.
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct SettingNote;

/// A [`spawn_settings_section`]'s parts.
#[derive(Debug, Clone, Copy)]
pub struct SettingsSection {
    /// The section, heading and all: put it in a [`settings_page`] or a modal's body with `ChildOf`.
    pub section: Entity,
    /// Where its settings go, a row apart. It holds nothing else, so `Keyed` can keep them.
    pub rows: Entity,
}

/// A section of settings under `heading`, spawned unparented: put [`SettingsSection::section`] where it goes and its
/// settings ([`setting`], [`setting_toggle`]) in [`SettingsSection::rows`].
pub fn spawn_settings_section(commands: &mut Commands, heading: impl Into<String>) -> SettingsSection {
    let section = commands.spawn_scene(section(heading)).id();
    let rows = commands
        .spawn((
            Name::new("settings"),
            Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() },
            ChildOf(section),
        ))
        .id();
    SettingsSection { section, rows }
}

#[cfg(test)]
mod tests {
    use bevy::ui::Checked;

    use super::*;

    #[derive(Component, Clone, Default)]
    struct Mine;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        app
    }

    fn spawn(app: &mut App, scene: impl Scene) -> Entity {
        let root = app.world_mut().commands().spawn_scene(scene).id();
        app.world_mut().flush();
        app.update();
        root
    }

    /// The words of each text under `root`, in order, and whether each shows.
    fn texts(app: &App, root: Entity) -> Vec<(String, bool)> {
        let world = app.world();
        let mut found = Vec::new();
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            if let Some(text) = world.get::<Text>(e) {
                let shown = world.get::<Node>(e).is_none_or(|n| n.display != Display::None);
                found.push((text.0.clone(), shown));
            }
            if let Some(children) = world.get::<Children>(e) {
                stack.extend(children.iter().rev());
            }
        }
        found
    }

    #[test]
    fn a_setting_is_its_label_over_its_control_and_its_note_only_when_it_has_one() {
        let mut app = app();
        let control = bsn_list! { @text_dim("value", size::BODY) Mine };
        let with = spawn(&mut app, setting("Appearance", "Desktop follows your desktop.", control));
        let words = texts(&app, with);
        let expected = [("Appearance", true), ("value", true), ("Desktop follows your desktop.", true)];
        assert_eq!(words, expected.map(|(w, s)| (w.to_string(), s)));
        let without = spawn(&mut app, setting("Signed in as", "", bsn_list! { @text_dim("lane", size::BODY) }));
        assert_eq!(texts(&app, without).last(), Some(&(String::new(), false)), "an empty note is hidden");
        let node = app.world().get::<Node>(with).unwrap();
        assert_eq!((node.flex_direction, node.row_gap), (FlexDirection::Column, px(space::STACKED)));
    }

    #[test]
    fn a_toggle_setting_is_a_switch_with_its_label_and_the_callers_marker() {
        let mut app = app();
        let root = spawn(&mut app, setting_toggle("Show archived", "", true, bsn! { Mine }));
        let world = app.world_mut();
        let switch = world.query_filtered::<Entity, With<Mine>>().single(world).unwrap();
        assert!(world.get::<Checked>(switch).is_some());
        assert_eq!(texts(&app, root)[0], ("Show archived".to_string(), true));
    }

    #[test]
    fn a_section_holds_its_heading_then_a_box_of_settings_a_row_apart() {
        let mut app = app();
        let parts = spawn_settings_section(&mut app.world_mut().commands(), "Projects shown");
        app.world_mut().flush();
        app.update();
        let world = app.world();
        let children: Vec<Entity> = world.get::<Children>(parts.section).unwrap().iter().collect();
        assert_eq!(children.len(), 2);
        assert_eq!(world.get::<Text>(children[0]).map(|t| t.0.as_str()), Some("Projects shown"));
        assert_eq!(children[1], parts.rows);
        let rows = world.get::<Node>(parts.rows).unwrap();
        assert_eq!((rows.flex_direction, rows.row_gap), (FlexDirection::Column, px(space::ROWS)));
        assert!(world.get::<Children>(parts.rows).is_none(), "the box is the caller's to fill");
    }

    #[test]
    fn a_page_stands_its_settings_a_row_apart() {
        let mut app = app();
        let page = spawn(&mut app, settings_page());
        let node = app.world().get::<Node>(page).unwrap();
        assert_eq!((node.flex_direction, node.row_gap), (FlexDirection::Column, px(space::ROWS)));
    }
}
