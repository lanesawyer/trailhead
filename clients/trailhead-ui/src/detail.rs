//! A detail modal's body: a wide main column for the thing itself beside a narrower sidebar of its facts, the way a
//! task tracker lays out one task. Put it in a [`super::Modal`] whose width is [`extent::DETAIL_WIDTH`].

use bevy::prelude::*;

use crate::spacing::{extent, space, stroke};
use crate::theme::{BorderTone, token};

/// The two columns [`spawn_detail_panes`] made, for the caller to fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailPanes {
    /// The wide column: title, description, what belongs to the thing.
    pub main: Entity,
    /// The sidebar: one section per fact.
    pub side: Entity,
}

/// The main column, which takes what the sidebar leaves.
#[derive(Component, Clone, Copy, Default)]
pub struct DetailMain;

/// The sidebar of facts.
#[derive(Component, Clone, Copy, Default)]
pub struct DetailSide;

/// Spawns the two columns under `parent` (a modal's body) and returns them.
pub fn spawn_detail_panes(commands: &mut Commands, parent: Entity) -> DetailPanes {
    let row = commands
        .spawn((
            Node { column_gap: px(space::GROUPS), align_items: AlignItems::FlexStart, ..default() },
            ChildOf(parent),
        ))
        .id();
    let main = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                min_width: px(0),
                row_gap: px(space::ROWS),
                ..default()
            },
            DetailMain,
            ChildOf(row),
        ))
        .id();
    let side = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                flex_shrink: 0.0,
                width: px(extent::DETAIL_SIDEBAR_WIDTH),
                row_gap: px(space::ROWS),
                padding: UiRect::left(px(space::PANEL_INSET)),
                border: UiRect::left(px(stroke::HAIRLINE)),
                ..default()
            },
            BorderTone(token::BORDER),
            DetailSide,
            ChildOf(row),
        ))
        .id();
    DetailPanes { main, side }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidebar_keeps_its_width_and_the_main_column_takes_the_rest() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        let parent = app.world_mut().spawn(Node::default()).id();
        let panes = spawn_detail_panes(&mut app.world_mut().commands(), parent);
        app.world_mut().flush();
        let side = app.world().get::<Node>(panes.side).unwrap();
        assert_eq!((side.width, side.flex_shrink), (px(extent::DETAIL_SIDEBAR_WIDTH), 0.0));
        let main = app.world().get::<Node>(panes.main).unwrap();
        assert_eq!((main.flex_grow, main.min_width), (1.0, px(0)));
        let row = app.world().get::<ChildOf>(panes.main).unwrap().parent();
        assert_eq!(app.world().get::<ChildOf>(row).unwrap().parent(), parent);
        assert_eq!(app.world().get::<ChildOf>(panes.side).unwrap().parent(), row);
    }
}
