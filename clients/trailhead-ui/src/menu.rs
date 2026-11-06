//! Menus: a button showing an icon and a chevron, and a popup under it for the caller to fill (ported from
//! bevy-data-explorer's `widgets/menu.rs`).
//!
//! The chevron isn't optional: any icon but the ellipsis reads as a button that acts when pressed, so the chevron
//! says this one opens something. Feathers' menu places the popup (below its button, or above when there's no room,
//! ending at the button's right edge when there's no room to the right) and closes it on Escape or a click outside.

use bevy::feathers::controls::{FeathersMenu, FeathersMenuPopup, FeathersMenuToolButton};
use bevy::input_focus::tab_navigation::TabIndex;
use bevy::prelude::*;
use bevy::ui_widgets::popover::{Popover, PopoverAlign, PopoverPlacement, PopoverSide};
use bevy::ui_widgets::{MenuAction, MenuEvent};

use super::{Hint, Icon, button_icon, layer, space};

/// Width of a menu popup.
pub const MENU_WIDTH: f32 = 280.0;

/// What [`icon_menu`] built.
pub struct Menu {
    /// Holds the button and the popup.
    pub root: Entity,
    pub button: Entity,
    /// Fill this with the menu's items.
    pub popup: Entity,
}

/// A menu behind a button showing `icon` and a chevron, with `hint` over it, under `parent`.
pub fn icon_menu(commands: &mut Commands, parent: Entity, icon: Icon, hint: impl Into<String>) -> Menu {
    let hint = hint.into();
    let popup = spawn_popup(commands);
    let button = commands
        .spawn_scene(bsn! {
            @FeathersMenuToolButton { @caption: { bsn_list! { @button_icon(icon) } } }
            Hint({ hint })
        })
        .id();
    let root = commands.spawn_scene(bsn! { @FeathersMenu }).add_children(&[button, popup]).id();
    commands.entity(parent).add_child(root);
    Menu { root, button, popup }
}

/// Close the menu holding `inside`, once what was pressed in it is done.
pub fn close_menu_holding(commands: &mut Commands, inside: Entity) {
    for action in [MenuAction::FocusRoot, MenuAction::CloseAll] {
        commands.trigger(MenuEvent { source: inside, action });
    }
}

/// Where a popup may open, best first: below its button, starting or ending at its edge, then above.
fn placements() -> Vec<PopoverPlacement> {
    [PopoverSide::Bottom, PopoverSide::Top]
        .into_iter()
        .flat_map(|side| {
            [PopoverAlign::Start, PopoverAlign::End].map(|align| PopoverPlacement { side, align, gap: 2.0 })
        })
        .collect()
}

fn spawn_popup(commands: &mut Commands) -> Entity {
    commands
        .spawn_scene(bsn! {
            @FeathersMenuPopup
            Popover { positions: { placements() } }
            Node {
                width: { px(MENU_WIDTH) },
                flex_direction: { FlexDirection::Column },
                row_gap: { px(space::ROWS) },
                padding: { UiRect::all(px(space::PANEL_INSET)) },
            }
            GlobalZIndex({ layer::MENU })
            // Takes the keyboard itself, so a click on something in it that can't doesn't close it under the pointer.
            TabIndex(-1)
        })
        .id()
}
