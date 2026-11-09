//! The heading of a panel or modal: the title on the left, wrapping when long, and a close X pinned top-right.
//!
//! The X's top lines up with the title's top, so a title that wraps to a second line leaves the X in the corner.
//! The header says nothing about what closing does: the X sends feathers' `RequestClose` from itself, which climbs
//! to whatever owns the panel (the modal's root, the inspector) and is answered there. A panel that can't be closed
//! passes `closable: false` and has the same title row without the X.
//!
//! [`PanelTools`] holds the X; a panel puts its own header controls (a modal's arrows and menu) in it before the X.

use bevy::prelude::*;
use bevy::ui_widgets::{Activate, RequestClose};

use super::{ButtonKind, Icon, size, space, text, tool_button};

/// Marks the title's text, so a title that changes is set in place.
#[derive(Component, Default, Clone)]
pub struct PanelTitle;

/// The row at the header's right, ending with the X: insert a panel's own controls at its start.
#[derive(Component, Default, Clone)]
pub struct PanelTools;

/// A panel's title row, with the close X at its right.
pub fn panel_header(title: impl Into<String>) -> impl Scene {
    let title = title.into();
    let close = bsn_list! {
        @tool_button(Icon::X, "Close", ButtonKind::Plain)
        Node { flex_shrink: 0.0 }
        on(|activate: On<Activate>, mut commands: Commands| {
            commands.trigger(RequestClose { source: activate.event_target() });
        })
    };
    bsn! {
        Node {
            align_items: AlignItems::FlexStart,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: { px(space::CONTROLS) },
            flex_shrink: 0.0,
        }
        Children [
            @text(title, size::HEADING) PanelTitle Node { flex_grow: 1.0, flex_shrink: 1.0, min_width: px(0) }
            --
            Node { align_items: AlignItems::Center, column_gap: { px(space::CONTROLS) }, flex_shrink: 0.0 }
            PanelTools
            Children [ {close} ]
        ]
    }
}
