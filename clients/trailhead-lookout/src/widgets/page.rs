//! The panel node the Project and Projects pages share.

use bevy::prelude::*;
use trailhead_ui::{beside_sidebar, space};

/// A page's panel beside the sidebar: a column whose padding leaves room for its scrollbar. The Project and Projects
/// pages both use it, so they don't drift apart.
pub fn page_node() -> Node {
    Node {
        min_width: px(0),
        min_height: px(0),
        flex_direction: FlexDirection::Column,
        padding: UiRect::all(px(space::PANEL_INSET)),
        row_gap: px(space::GROUPS),
        ..beside_sidebar()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_panel_has_room_for_a_scrollbar() {
        assert!(trailhead_ui::has_scrollbar_room(&page_node()));
    }
}
