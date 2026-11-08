//! A dropdown for a fixed set of choices, over feathers' `FeathersSelect`.
//!
//! Anything picked from a fixed set (a filter, a priority, a status) is one of these, never a button that cycles
//! through values. The caller hands over the options and which is chosen, tags the select with a component of its
//! own, and observes [`Picked`] for that tag.

use bevy::ecs::system::SystemParam;
use bevy::feathers::controls::{FeathersMenuPopup, FeathersSelect, OptionIndex, list_rows_from_strings};
use bevy::prelude::*;
use bevy::ui::Selected;
use bevy::ui_widgets::{ScrollArea, ValueChange};

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
            @options: { list_rows_from_strings(options.clone(), selected) },
            @max_visible: { max_visible },
        }
        SelectOptions({ options })
        on(pick)
    }
}

/// Turns the select's `ValueChange<Entity>` (the picked row) into a [`Picked`] with the row's index.
fn pick(change: On<ValueChange<Entity>>, rows: Query<&OptionIndex>, mut commands: Commands) {
    if let Ok(option) = rows.get(change.value) {
        commands.trigger(Picked { entity: change.source, index: option.0 });
    }
}

/// The options a select was last given, kept so [`SelectEdit::set`] can tell whether they changed.
#[derive(Component, Clone, PartialEq, Debug, Default)]
pub struct SelectOptions(pub Vec<String>);

/// Changes a spawned select in place: which row is selected, and its rows when its options changed. The select, its
/// popup and everything around it stay the same entities, so a select you just used is not despawned under you.
#[derive(SystemParam)]
pub struct SelectEdit<'w, 's> {
    options: Query<'w, 's, &'static SelectOptions>,
    children: Query<'w, 's, &'static Children>,
    rows: Query<'w, 's, (&'static OptionIndex, Has<Selected>)>,
    areas: Query<'w, 's, (), With<ScrollArea>>,
    popups: Query<'w, 's, &'static Visibility, With<FeathersMenuPopup>>,
    commands: Commands<'w, 's>,
}

impl SelectEdit<'_, '_> {
    /// Whether the select's popup is showing.
    pub fn is_open(&self, select: Entity) -> bool {
        self.children.iter_descendants(select).any(|e| self.popups.get(e).is_ok_and(|v| *v != Visibility::Hidden))
    }

    /// The rows shown as selected, by their index in the options: one, or none before anything is picked.
    pub fn selected_rows(&self, select: Entity) -> Vec<usize> {
        self.children
            .iter_descendants(select)
            .filter_map(|e| self.rows.get(e).ok())
            .filter(|(_, selected)| *selected)
            .map(|(index, _)| index.0)
            .collect()
    }

    /// Shows `selected` of `options`. The rows are rebuilt only if `options` differ from the last ones, and nothing
    /// is touched while the popup is open (returns `false` then, so the caller can try again).
    pub fn set(&mut self, select: Entity, options: &[String], selected: Option<usize>) -> bool {
        if self.is_open(select) {
            return false;
        }
        let rows: Vec<_> = self.children.iter_descendants(select).filter(|e| self.rows.contains(*e)).collect();
        if self.options.get(select).is_ok_and(|o| o.0 == options) {
            for row in rows {
                let Ok((index, is_selected)) = self.rows.get(row) else { continue };
                let want = Some(index.0) == selected;
                if want && !is_selected {
                    self.commands.entity(row).insert(Selected);
                } else if !want && is_selected {
                    self.commands.entity(row).remove::<Selected>();
                }
            }
            return true;
        }
        let Some(area) = self.children.iter_descendants(select).find(|e| self.areas.contains(*e)) else {
            return true;
        };
        for row in rows {
            self.commands.entity(row).despawn();
        }
        self.commands
            .entity(area)
            .queue_spawn_related_scenes::<Children>(list_rows_from_strings(options.to_vec(), selected));
        self.commands.entity(select).insert(SelectOptions(options.to_vec()));
        true
    }
}
