//! The task board (`docs/factory/ui.md#views`), read-only: tasks by status with subtasks under their parent, each
//! a card with badges (priority, grade, holder, open questions) and icon actions, filtered by project,
//! parent and holder, with the closed columns behind a toggle.
//!
//! [`build`] is a pure function over the mirror's JSON, so the queries are tested without a window. [`BoardPlugin`]
//! keeps a [`BoardView`] from the mirror and the [`Filter`]; [`BoardUiPlugin`] draws it with feathers, greyed out
//! with the reason whenever the connection isn't [`Connection::Live`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, ControlOrientation};
use jiff::Timestamp;
use trailhead_model::{Agent, Claim, Comment, CommentKind, Dependency, Project, Status, Task, User};
use uuid::Uuid;

use crate::author::{Act, Authoring};
use crate::connection::Connection;
use crate::factory::{FactoryStatus, PrLink, Run, pr_link};
use crate::inspector::{Neighbours, Selection, Subject};
use crate::mirror::{Mirror, Mirrored, Rev, rows};
use crate::search::{Deep, DeepHit};
use crate::widgets;
use crate::widgets::{
    ButtonKind, Hint, Icon, Keyed, Picked, ScrollBoth, SelectEdit, Token, badge, button, card as card_shell,
    card_found, card_row, card_title, card_under, field, grade_tone, link_tool_button, priority_tone, select, space,
    status_label, token, tool_button,
};

/// The views of the app: the task board, the trail map, and one project's page ([`crate::project`]).
#[derive(States, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    #[default]
    Board,
    Map,
    Project,
    /// Every project, in a table.
    Projects,
}

/// How many cards a column builds at first, and how many more each "show more" adds. A column holds hundreds
/// (`open` held 366 on 2026-11-05) and every card is a dozen entities, so building them all would make each redraw
/// cost in proportion to the backlog.
pub const PAGE: usize = 40;

mod header;
mod model;
mod panel;
mod source;
#[cfg(test)]
mod testing;
mod tree;

pub use header::*;
pub use model::*;
use panel::*;
pub use source::*;
pub use tree::*;

/// Keeps [`Filter`], [`BoardView`] and the entities of the columns and cards from the mirror.
pub struct BoardPlugin;

impl Plugin for BoardPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Filter>()
            .init_resource::<Authoring>()
            .init_resource::<BoardView>()
            .init_resource::<Shown>()
            .init_resource::<Tree>()
            .init_resource::<Selection>()
            .init_resource::<Neighbours>()
            .add_systems(Update, (rebuild_view, sync_tree, follow_neighbours).chain())
            .add_observer(on_pick)
            .add_observer(on_toggle_closed)
            .add_observer(on_toggle_factory);
    }
}

/// Draws the board while in [`View::Board`], and every [`BoardCard`] in any view.
pub struct BoardUiPlugin;

impl Plugin for BoardUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(BoardPlugin)
            .init_state::<View>()
            .add_systems(OnEnter(View::Board), spawn_panel)
            .add_systems(OnExit(View::Board), (save_scroll, despawn_panel).chain())
            .init_resource::<BoardScroll>()
            .add_observer(on_card_press)
            .add_observer(on_more_press)
            .add_systems(
                Update,
                (dress_columns, dress_slots, update_counts, dress_header, update_header)
                    .chain()
                    .after(sync_tree)
                    .run_if(in_state(View::Board)),
            )
            // Cards are drawn wherever they are, on the board or on a project's page.
            .add_systems(Update, (dress_cards, tools_follow_connection));
    }
}
