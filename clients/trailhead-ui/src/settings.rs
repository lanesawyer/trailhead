//! Settings, laid out the one way every app's settings are: Lookout's settings modal, lifted into the kit so an app's
//! settings look like it.
//!
//! - [`settings_page`]: a page of settings (a modal's body, or one tab's page of it), its settings a row apart.
//! - [`spawn_settings_section`]: a group of settings under a heading, more than a group's distance from what is above it,
//!   with its settings in a box of their own for the caller to fill and keep (with `Keyed`).
//! - [`setting`]: one setting, its label over its control (a select, a button group, a value), with a dim note under
//!   it saying what it does when there is one.
//! - [`setting_toggle`]: a setting that is on or off, a toggle switch with its label after it, and its note.
//! - [`spawn_settings_tabs`]: a settings modal with pages, its tabs down the side (under [`crate::tab_heading`]s when
//!   they come in groups) and the page shown beside them.
//!
//! A list of toggles stacked bare, with nothing between them, read as one block of text (TRAIL-1610); in a section
//! they stand a row apart under their heading.

use bevy::prelude::*;
use bevy::ui_widgets::ControlOrientation;

use crate::containers::section_frame;
use crate::patch::display;
use crate::spacing::{extent, space};
use crate::tabs::tab_strip;
use crate::text::{size, text, text_dim};
use crate::toggle::toggle_switch;

/// A page of settings, as wide as what holds it up to [`extent::SETTINGS_MEASURE`], so a control or a note's line
/// never spans a wide modal: its settings and sections a row apart, which a section's own margin widens to a
/// group's. Spawn the settings and sections as its children, in order.
pub fn settings_page() -> impl Scene {
    bsn! {
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: { px(space::ROWS) },
            min_width: px(0),
            max_width: { px(extent::SETTINGS_MEASURE) },
        }
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

/// A section of settings under `heading`, which is a size up from the settings' own labels and in the full text
/// color, so a page's sections can be told from what is in them. Spawned unparented: put [`SettingsSection::section`] where it goes and its
/// settings ([`setting`], [`setting_toggle`]) in [`SettingsSection::rows`].
pub fn spawn_settings_section(commands: &mut Commands, heading: impl Into<String>) -> SettingsSection {
    let heading = heading.into();
    let section = commands
        .spawn_scene(bsn! {
            @section_frame()
            Node { margin: { UiRect::top(px(space::GROUPS)) } }
            Children [ @text(heading, size::HEADING) ]
        })
        .id();
    let rows = commands
        .spawn((
            Name::new("settings"),
            Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() },
            ChildOf(section),
        ))
        .id();
    SettingsSection { section, rows }
}

/// A [`spawn_settings_tabs`]'s parts.
#[derive(Debug, Clone, Copy)]
pub struct SettingsTabs {
    /// The whole: tabs and pane side by side.
    pub root: Entity,
    /// The column the strip is in, [`extent::SETTINGS_TABS_WIDTH`] wide. What goes over the tabs (a search field that
    /// narrows them) is put in as its first child.
    pub side: Entity,
    /// A [`tab_strip`] down the side: fill it with [`crate::tab`]s, a [`crate::tab_heading`] over each group of them,
    /// and answer its `ValueChange<Option<Entity>>` by setting its `SelectedTab` and showing that tab's page.
    pub strip: Entity,
    /// Where the page shown goes: a [`settings_page`], spawned when its tab is picked and despawned when another is.
    pub pane: Entity,
}

/// The tabs of a settings modal with pages, down its side, and the pane beside them that holds the page shown,
/// spawned under `parent` (a modal's body). It grows into a tall modal's height, so a page can fill what is left (a
/// log that scrolls on its own); a page longer than the modal scrolls with the body.
pub fn spawn_settings_tabs(commands: &mut Commands, parent: Entity) -> SettingsTabs {
    let root = commands
        .spawn((
            Name::new("settings tabs"),
            Node { column_gap: px(space::GROUPS), flex_grow: 1.0, min_height: px(0), ..default() },
            ChildOf(parent),
        ))
        .id();
    let side = commands
        .spawn((
            Name::new("settings side"),
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: px(space::ROWS),
                width: px(extent::SETTINGS_TABS_WIDTH),
                flex_shrink: 0.0,
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    let strip = commands
        .spawn_scene(bsn! { @tab_strip(ControlOrientation::Vertical) Node { flex_grow: 1.0 } })
        .insert(ChildOf(side))
        .id();
    let pane = commands
        .spawn((
            Name::new("settings pane"),
            Node {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                min_width: px(0),
                min_height: px(0),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    SettingsTabs { root, side, strip, pane }
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
    fn the_tabs_keep_their_width_down_the_side_and_the_pane_takes_the_rest() {
        let mut app = app();
        let body = app.world_mut().spawn(Node::default()).id();
        let parts = spawn_settings_tabs(&mut app.world_mut().commands(), body);
        app.world_mut().flush();
        app.update();
        let world = app.world();
        let children: Vec<Entity> = world.get::<Children>(parts.root).unwrap().iter().collect();
        assert_eq!(children, [parts.side, parts.pane]);
        let in_side: Vec<Entity> = world.get::<Children>(parts.side).unwrap().iter().collect();
        assert_eq!(in_side, [parts.strip], "what goes over the tabs is the caller's to put first");
        let list = world.get::<bevy::ui_widgets::TabList>(parts.strip).unwrap();
        assert_eq!(list.orientation, ControlOrientation::Vertical);
        let side = world.get::<Node>(parts.side).unwrap();
        assert_eq!((side.width, side.flex_shrink), (px(extent::SETTINGS_TABS_WIDTH), 0.0));
        assert_eq!(world.get::<Node>(parts.strip).unwrap().flex_grow, 1.0, "the rule runs the side's whole height");
        let pane = world.get::<Node>(parts.pane).unwrap();
        assert_eq!((pane.flex_grow, pane.min_width, pane.min_height), (1.0, px(0), px(0)));
        assert_eq!(world.get::<Node>(parts.root).unwrap().flex_grow, 1.0, "fills a tall modal's body");
    }

    #[test]
    fn a_page_stands_its_settings_a_row_apart() {
        let mut app = app();
        let page = spawn(&mut app, settings_page());
        let node = app.world().get::<Node>(page).unwrap();
        assert_eq!((node.flex_direction, node.row_gap), (FlexDirection::Column, px(space::ROWS)));
    }
}
