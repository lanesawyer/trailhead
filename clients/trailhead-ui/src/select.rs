//! A dropdown for a fixed set of choices, over feathers' `FeathersSelect`.
//!
//! Anything picked from a fixed set (a filter, a priority, a status) is one of these, never a button that cycles
//! through values. The caller hands over the options and which is chosen, tags the select with a component of its
//! own, and observes [`Picked`] for that tag.

use bevy::feathers::controls::{FeathersSelect, OptionIndex, list_rows_from_strings};
use bevy::prelude::*;
use bevy::ui_widgets::ValueChange;

/// Which option of a select was picked, triggered on the select's own entity.
#[derive(EntityEvent, Debug, Clone, Copy)]
pub struct Picked {
    pub entity: Entity,
    pub index: usize,
}

/// A dropdown of `options`, showing `selected`. At most `max_visible` rows show before the list scrolls.
pub fn select(options: Vec<String>, selected: Option<usize>, max_visible: usize) -> impl Scene {
    bsn! {
        @FeathersSelect {
            @options: { list_rows_from_strings(options, selected) },
            @max_visible: { max_visible },
        }
        on(pick)
    }
}

/// Turns the select's `ValueChange<Entity>` (the picked row) into a [`Picked`] with the row's index.
fn pick(change: On<ValueChange<Entity>>, rows: Query<&OptionIndex>, mut commands: Commands) {
    if let Ok(option) = rows.get(change.value) {
        commands.trigger(Picked { entity: change.source, index: option.0 });
    }
}
