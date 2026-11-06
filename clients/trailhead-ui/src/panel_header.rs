//! The heading of a panel or modal: the title on the left, wrapping when long, and a close X pinned top-right.
//!
//! The X's top lines up with the title's top, so a title that wraps to a second line leaves the X in the corner.
//! The header says nothing about what closing does: the X sends feathers' `RequestClose` from itself, which climbs
//! to whatever owns the panel (the modal's root, the inspector) and is answered there. A panel that can't be closed
//! passes `closable: false` and has the same title row without the X.

use bevy::feathers::controls::ButtonVariant;
use bevy::prelude::*;
use bevy::ui_widgets::{Activate, RequestClose};

use super::{Icon, size, space, text, tool_button};

/// A panel's title row; with `closable`, the close X at its right.
pub fn panel_header(title: impl Into<String>, closable: bool) -> impl Scene {
    let title = title.into();
    let close: Box<dyn SceneList> = if closable {
        Box::new(bsn_list! {
            @tool_button(Icon::X, "Close", ButtonVariant::Plain)
            Node { flex_shrink: 0.0 }
            on(|activate: On<Activate>, mut commands: Commands| {
                commands.trigger(RequestClose { source: activate.event_target() });
            })
        })
    } else {
        Box::new(bsn_list! {})
    };
    bsn! {
        Node {
            align_items: AlignItems::FlexStart,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: { px(space::CONTROLS) },
            flex_shrink: 0.0,
        }
        Children [
            @text(title, size::HEADING) Node { flex_grow: 1.0, flex_shrink: 1.0, min_width: px(0) }
            --
            {close}
        ]
    }
}
