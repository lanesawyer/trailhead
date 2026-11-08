//! A dropdown for a fixed set of choices, over feathers' `FeathersSelect`.
//!
//! Anything picked from a fixed set (a filter, a priority, a status) is one of these, never a button that cycles
//! through values. The caller hands over the options and which is chosen, tags the select with a component of its
//! own, and observes [`Picked`] for that tag. [`select_toned`] gives each option a color (a status, a priority), which
//! its row and, once it is chosen, the select's caption show.
//!
//! The caption stays on one line, cut off at the select's width, so a long option doesn't wrap the row it sits in.

use bevy::ecs::system::SystemParam;
use bevy::feathers::controls::{FeathersListRow, FeathersMenuButton, FeathersMenuPopup, FeathersSelect, OptionIndex};
use bevy::feathers::display::caption;
use bevy::prelude::*;
use bevy::text::{LineBreak, TextLayout};
use bevy::ui::{InteractionDisabled, Selected};
use bevy::ui_widgets::{ScrollArea, ValueChange};

use crate::theme::{TextTone, Token, token};

/// Which option of a select (or a [`super::button_group`]) was picked, triggered on the control's own entity.
#[derive(EntityEvent, Debug, Clone, Copy)]
pub struct Picked {
    pub entity: Entity,
    pub index: usize,
}

/// A dropdown of `options`, showing `selected`. At most `max_visible` rows show before the list scrolls.
pub fn select(options: Vec<String>, selected: Option<usize>, max_visible: usize) -> impl Scene {
    select_toned(options, Vec::new(), selected, max_visible)
}

/// A [`select`] whose options are shown in `tones`, one per option (`None` for the usual text color).
pub fn select_toned(
    options: Vec<String>,
    tones: Vec<Option<Token>>,
    selected: Option<usize>,
    max_visible: usize,
) -> impl Scene {
    bsn! {
        @FeathersSelect {
            @options: { option_rows(&options, &tones, selected) },
            @max_visible: { max_visible },
        }
        SelectOptions({ options })
        SelectTones({ tones })
        on(pick)
    }
}

/// A select's rows: feathers' `list_rows_from_strings`, with each caption in its tone.
fn option_rows(options: &[String], tones: &[Option<Token>], selected: Option<usize>) -> Box<dyn SceneList> {
    let rows = options.iter().enumerate().map(|(i, label)| -> Box<dyn SceneList> {
        let label = label.clone();
        let tone = tones.get(i).cloned().flatten().unwrap_or(token::TEXT);
        if Some(i) == selected {
            bsn! { @FeathersListRow Selected OptionIndex(i) Children [ @caption(label) TextTone({ tone }) ] }.into()
        } else {
            bsn! { @FeathersListRow OptionIndex(i) Children [ @caption(label) TextTone({ tone }) ] }.into()
        }
    });
    Box::new(rows.collect::<Vec<_>>())
}

/// The tones of a [`select_toned`]'s options, empty for a plain [`select`].
#[derive(Component, Clone, PartialEq, Debug, Default)]
pub struct SelectTones(pub Vec<Option<Token>>);

/// Keeps each select's caption on one line, and in the tone of the option it shows. Feathers copies the chosen row's
/// words into the caption when a row is selected; this follows it with the row's color.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn style_select_captions(
    new_texts: Query<Entity, Added<Text>>,
    newly_selected: Query<(Entity, &OptionIndex), (Added<Selected>, With<FeathersListRow>)>,
    selects: Query<&SelectTones>,
    buttons: Query<(), With<FeathersMenuButton>>,
    texts: Query<Option<&TextTone>, With<Text>>,
    parents: Query<&ChildOf>,
    children: Query<&Children>,
    mut commands: Commands,
) {
    let caption_of = |select: Entity| {
        let button = children.iter_descendants(select).find(|e| buttons.contains(*e))?;
        children.iter_descendants(button).find(|e| texts.contains(*e))
    };
    for text in &new_texts {
        let in_button = parents.iter_ancestors(text).find(|e| buttons.contains(*e));
        let Some(button) = in_button else { continue };
        if parents.iter_ancestors(button).any(|e| selects.contains(e)) {
            commands.entity(text).insert(TextLayout { linebreak: LineBreak::NoWrap, ..default() });
            commands.entity(button).entry::<Node>().and_modify(|mut node| {
                node.min_width = px(0);
                node.overflow = Overflow::clip_x();
            });
        }
    }
    for (row, index) in &newly_selected {
        let Some(select) = parents.iter_ancestors(row).find(|e| selects.contains(*e)) else { continue };
        let Ok(tones) = selects.get(select) else { continue };
        if tones.0.is_empty() {
            continue;
        }
        let Some(caption) = caption_of(select) else { continue };
        let tone = tones.0.get(index.0).cloned().flatten().unwrap_or(token::TEXT);
        if texts.get(caption).ok().flatten().is_none_or(|t| t.0 != tone) {
            commands.entity(caption).insert(TextTone(tone));
        }
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
    tones: Query<'w, 's, &'static SelectTones>,
    children: Query<'w, 's, &'static Children>,
    rows: Query<'w, 's, (&'static OptionIndex, Has<Selected>)>,
    areas: Query<'w, 's, (), With<ScrollArea>>,
    popups: Query<'w, 's, &'static Visibility, With<FeathersMenuPopup>>,
    buttons: Query<'w, 's, Has<InteractionDisabled>, With<FeathersMenuButton>>,
    commands: Commands<'w, 's>,
}

impl SelectEdit<'_, '_> {
    /// Whether the select's popup is showing.
    pub fn is_open(&self, select: Entity) -> bool {
        self.children.iter_descendants(select).any(|e| self.popups.get(e).is_ok_and(|v| *v != Visibility::Hidden))
    }

    /// Lets the select be opened, or not. Changes nothing when it already is as asked.
    pub fn set_enabled(&mut self, select: Entity, enabled: bool) {
        for button in self.children.iter_descendants(select) {
            match self.buttons.get(button) {
                Ok(true) if enabled => {
                    self.commands.entity(button).remove::<InteractionDisabled>();
                }
                Ok(false) if !enabled => {
                    self.commands.entity(button).insert(InteractionDisabled);
                }
                _ => {}
            }
        }
    }

    /// Whether the select can be opened.
    pub fn is_enabled(&self, select: Entity) -> bool {
        !self.children.iter_descendants(select).any(|e| self.buttons.get(e).is_ok_and(|disabled| disabled))
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
        let tones = self.tones.get(select).map(|t| t.0.clone()).unwrap_or_default();
        self.set_toned(select, options, &tones, selected)
    }

    /// [`SelectEdit::set`] for a [`select_toned`], whose options' tones may change with them.
    pub fn set_toned(
        &mut self,
        select: Entity,
        options: &[String],
        tones: &[Option<Token>],
        selected: Option<usize>,
    ) -> bool {
        if self.is_open(select) {
            return false;
        }
        let rows: Vec<_> = self.children.iter_descendants(select).filter(|e| self.rows.contains(*e)).collect();
        let same_tones = self.tones.get(select).map_or(tones.is_empty(), |t| t.0 == tones);
        if self.options.get(select).is_ok_and(|o| o.0 == options) && same_tones {
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
        self.commands.entity(area).queue_spawn_related_scenes::<Children>(option_rows(options, tones, selected));
        self.commands.entity(select).insert((SelectOptions(options.to_vec()), SelectTones(tones.to_vec())));
        true
    }
}
