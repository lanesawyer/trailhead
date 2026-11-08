//! A dropdown for a fixed set of choices, over feathers' `FeathersSelect`.
//!
//! Anything picked from a fixed set (a filter, a priority, a status) is one of these, never a button that cycles
//! through values. The caller hands over the options and which is chosen, tags the select with a component of its
//! own, and observes [`Picked`] for that tag. [`select_toned`] gives each option a color (a status, a priority), which
//! its row and, once it is chosen, the select's caption show. [`select_iconed`] puts an icon before each option's
//! words, and before the caption the icon of the option chosen.
//!
//! The caption stays on one line, cut off at the select's width, so a long option doesn't wrap the row it sits in.

use bevy::ecs::system::SystemParam;
use bevy::feathers::controls::{FeathersListRow, FeathersMenuButton, FeathersMenuPopup, FeathersSelect, OptionIndex};
use bevy::feathers::display::caption;
use bevy::prelude::*;
use bevy::text::{LineBreak, TextLayout};
use bevy::ui::{InteractionDisabled, Selected};
use bevy::ui_widgets::{ScrollArea, ValueChange};

use crate::icons::{Icon, icon_text};
use crate::space;
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
    dressed_select(options, tones, Vec::new(), selected, max_visible)
}

/// A [`select`] with `icons` before its options' words, one per option.
pub fn select_iconed(
    options: Vec<String>,
    icons: Vec<Icon>,
    selected: Option<usize>,
    max_visible: usize,
) -> impl Scene {
    dressed_select(options, Vec::new(), icons, selected, max_visible)
}

fn dressed_select(
    options: Vec<String>,
    tones: Vec<Option<Token>>,
    icons: Vec<Icon>,
    selected: Option<usize>,
    max_visible: usize,
) -> impl Scene {
    bsn! {
        @FeathersSelect {
            @options: { option_rows(&options, &tones, &icons, selected) },
            @max_visible: { max_visible },
        }
        SelectOptions({ options })
        SelectTones({ tones })
        SelectIcons({ icons })
        on(pick)
    }
}

/// A select's rows: feathers' `list_rows_from_strings`, with each caption in its tone, after its icon.
///
/// Feathers copies the first text it meets, level by level, under the chosen row into the select's caption, so the
/// icon sits one level down, in a box of its own, where the words are met first.
fn option_rows(
    options: &[String],
    tones: &[Option<Token>],
    icons: &[Icon],
    selected: Option<usize>,
) -> Box<dyn SceneList> {
    let rows = options.iter().enumerate().map(|(i, label)| -> Box<dyn SceneList> {
        let label = label.clone();
        let tone = tones.get(i).cloned().flatten().unwrap_or(token::TEXT);
        let icon_tone = tone.clone();
        let inside: Box<dyn SceneList> = match icons.get(i) {
            Some(&icon) => Box::new(bsn_list! {
                @icon_box(icon, icon_tone)
                -- @caption(label) TextTone({ tone })
            }),
            None => Box::new(bsn_list! { @caption(label) TextTone({ tone }) }),
        };
        if Some(i) == selected {
            bsn! { @FeathersListRow Selected OptionIndex(i) Children [ { inside } ] }.into()
        } else {
            bsn! { @FeathersListRow OptionIndex(i) Children [ { inside } ] }.into()
        }
    });
    Box::new(rows.collect::<Vec<_>>())
}

/// An option's icon in `tone`, boxed so feathers doesn't take it for the option's words.
fn icon_box(icon: Icon, tone: Token) -> impl Scene {
    bsn! {
        Node { margin: { UiRect::right(px(space::ICON_LABEL)) } }
        Children [ @icon_text(icon) TextTone({ tone }) ]
    }
}

/// The icons of a [`select_iconed`]'s options, empty for one without.
#[derive(Component, Clone, PartialEq, Debug, Default)]
pub struct SelectIcons(pub Vec<Icon>);

/// The icon before a [`select_iconed`]'s caption, which shows the chosen option's.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct ChosenIcon;

/// The tones of a [`select_toned`]'s options, empty for a plain [`select`].
#[derive(Component, Clone, PartialEq, Debug, Default)]
pub struct SelectTones(pub Vec<Option<Token>>);

/// Keeps each select's caption on one line, and in the tone of the option it shows. Feathers copies the chosen row's
/// words into the caption when a row is selected; this follows it with the row's color.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn style_select_captions(
    new_texts: Query<Entity, Added<Text>>,
    newly_selected: Query<(Entity, &OptionIndex), (Added<Selected>, With<FeathersListRow>)>,
    selects: Query<(&SelectTones, &SelectIcons)>,
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
        let Some(select) = parents.iter_ancestors(button).find(|e| selects.contains(*e)) else { continue };
        commands.entity(text).insert(TextLayout { linebreak: LineBreak::NoWrap, ..default() });
        commands.entity(button).entry::<Node>().and_modify(|mut node| {
            node.min_width = px(0);
            node.overflow = Overflow::clip_x();
        });
        // The caption is the button's first text; an icon of its own is the only other, which needs none.
        let first_icon = selects.get(select).ok().and_then(|(_, icons)| icons.0.first().copied());
        if let Some(icon) = first_icon
            && parents.get(text).is_ok_and(|p| p.parent() == button)
        {
            let shown = commands.spawn_scene(icon_box(icon, token::TEXT)).insert(ChosenIcon).id();
            commands.entity(button).insert_children(0, &[shown]);
        }
    }
    for (row, index) in &newly_selected {
        let Some(select) = parents.iter_ancestors(row).find(|e| selects.contains(*e)) else { continue };
        let Ok((tones, _)) = selects.get(select) else { continue };
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

/// Shows the chosen option's icon, in its tone, before each [`select_iconed`]'s caption.
pub(crate) fn show_chosen_icons(
    shown: Query<(Entity, &ChildOf), With<ChosenIcon>>,
    parents: Query<&ChildOf>,
    children: Query<&Children>,
    selects: Query<(&SelectTones, &SelectIcons)>,
    rows: Query<&OptionIndex, (With<Selected>, With<FeathersListRow>)>,
    mut glyphs: Query<(&mut Text, &TextTone)>,
    mut commands: Commands,
) {
    for (icon_box, button) in &shown {
        let Some(select) = parents.iter_ancestors(button.parent()).find(|e| selects.contains(*e)) else { continue };
        let Ok((tones, icons)) = selects.get(select) else { continue };
        let Some(index) = children.iter_descendants(select).find_map(|e| rows.get(e).ok()) else { continue };
        let Some(icon) = icons.0.get(index.0) else { continue };
        let tone = tones.0.get(index.0).cloned().flatten().unwrap_or(token::TEXT);
        let Some(glyph) = children.iter_descendants(icon_box).find(|e| glyphs.contains(*e)) else { continue };
        let Ok((mut text, shown_tone)) = glyphs.get_mut(glyph) else { continue };
        if text.0 != icon.glyph() {
            text.0 = icon.glyph().to_string();
        }
        if shown_tone.0 != tone {
            commands.entity(glyph).insert(TextTone(tone));
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
    icons: Query<'w, 's, &'static SelectIcons>,
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
        let icons = self.icons.get(select).map(|i| i.0.clone()).unwrap_or_default();
        self.commands
            .entity(area)
            .queue_spawn_related_scenes::<Children>(option_rows(options, tones, &icons, selected));
        self.commands.entity(select).insert((SelectOptions(options.to_vec()), SelectTones(tones.to_vec())));
        true
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;
    use bevy::input_focus::InputFocus;

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .add_plugins(bevy::feathers::controls::SelectPlugin)
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_resource::<InputFocus>()
            .insert_resource(crate::theme::Theme::new().resource())
            .add_systems(Update, (style_select_captions, show_chosen_icons).chain());
        app
    }

    fn settle(app: &mut App) {
        app.world_mut().flush();
        for _ in 0..3 {
            app.update();
        }
    }

    /// The texts in the select's button, first to last: its icon, then its caption.
    fn button_texts(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        let button = world.query_filtered::<Entity, With<FeathersMenuButton>>().single(world).unwrap();
        let mut found = Vec::new();
        let mut stack = vec![button];
        while let Some(e) = stack.pop() {
            if let Some(text) = world.get::<Text>(e) {
                found.push(text.0.clone());
            }
            if let Some(children) = world.get::<Children>(e) {
                stack.extend(children.iter().rev());
            }
        }
        found
    }

    #[test]
    fn an_iconed_select_shows_the_chosen_options_icon_before_its_words() {
        let mut app = app();
        let options = vec!["Note".to_string(), "Question".to_string()];
        let select = app
            .world_mut()
            .commands()
            .spawn_scene(select_iconed(options.clone(), vec![Icon::Comment, Icon::Help], Some(0), 2))
            .id();
        settle(&mut app);
        assert_eq!(button_texts(&mut app)[..2], [Icon::Comment.glyph(), "Note"]);
        app.world_mut().run_system_once(move |mut edit: SelectEdit| edit.set(select, &options, Some(1))).unwrap();
        settle(&mut app);
        assert_eq!(button_texts(&mut app)[..2], [Icon::Help.glyph(), "Question"]);
    }
}
