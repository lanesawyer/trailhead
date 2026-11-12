//! A dropdown for a fixed set of choices, over feathers' `FeathersSelect`.
//!
//! Anything picked from a fixed set (a filter, a priority, a status) is one of these, never a button that cycles
//! through values. The caller hands over the options and which is chosen, tags the select with a component of its
//! own, and observes [`Picked`] for that tag. [`select_toned`] gives each option a color (a status, a priority), which
//! its row and, once it is chosen, the select's caption show. [`select_iconed`] puts an icon before each option's
//! words, and before the caption the icon of the option chosen.
//!
//! The caption stays on one line, cut off with an ellipsis at the select's width, so a long option doesn't wrap the
//! row it sits in.

use bevy::ecs::system::SystemParam;
use bevy::feathers::controls::{FeathersListRow, FeathersMenuButton, FeathersMenuPopup, FeathersSelect, OptionIndex};
use bevy::feathers::display::caption;
use bevy::prelude::*;
use bevy::text::{LineBreak, TextLayout, TextLayoutInfo};
use bevy::ui::{InteractionDisabled, Selected};
use bevy::ui_widgets::{ScrollArea, ValueChange};

use crate::icons::{Icon, icon_text};
use crate::space;
use crate::text::{Size, size};
use crate::theme::{TextTone, Token, token};
use crate::truncate::truncate_to_width;

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
    let rows = (0..options.len()).map(|i| -> Box<dyn SceneList> {
        let row = option_row(options, tones, icons, selected, i);
        Box::new(bsn_list! { @{row} })
    });
    Box::new(rows.collect::<Vec<_>>())
}

/// Row `i` of [`option_rows`].
fn option_row(
    options: &[String],
    tones: &[Option<Token>],
    icons: &[Icon],
    selected: Option<usize>,
    i: usize,
) -> Box<dyn Scene> {
    {
        let label = options[i].clone();
        let tone = tones.get(i).cloned().flatten().unwrap_or(token::TEXT);
        let icon_tone = tone.clone();
        let inside: Box<dyn SceneList> = match icons.get(i) {
            Some(&icon) => Box::new(bsn_list! {
                @icon_box(icon, icon_tone)
                -- @caption(label) TextTone({ tone }) TextLayout { linebreak: LineBreak::NoWrap }
            }),
            None => {
                Box::new(bsn_list! { @caption(label) TextTone({ tone }) TextLayout { linebreak: LineBreak::NoWrap } })
            }
        };
        if Some(i) == selected {
            Box::new(bsn! { @FeathersListRow Selected OptionIndex(i) Children [ { inside } ] })
        } else {
            Box::new(bsn! { @FeathersListRow OptionIndex(i) Children [ { inside } ] })
        }
    }
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
    chosen_icons: Query<(), With<ChosenIcon>>,
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
        if !parents.iter_ancestors(text).any(|e| chosen_icons.contains(e)) {
            commands.entity(text).insert(CaptionFit::default());
        }
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

/// The size feathers sets a select's caption at, which its cut is measured by.
const CAPTION_SIZE: Size = size::CARD_TITLE;

/// A select's caption, cut to the room its button leaves it. Feathers writes the chosen row's whole words into the
/// caption's `Text` each time a row is selected, so this keeps what it last saw there (`full`) and what it left
/// (`shown`): a `Text` that isn't `shown` was written by feathers and is the new `full`. (Words feathers writes that
/// equal `shown` exactly, as when one option's text is another's cut, are taken for our own and `full` stays.)
///
/// The cut is made only when the words, laid out whole, are wider than the room (`full_px`, measured the frame after
/// feathers writes them, while they are still uncut). A select that hugs its caption always has the room for it, so
/// it is never cut. A cut is kept while it fits the room and the room hasn't grown past the one it was made at: a
/// select whose width comes from its content (not from outside it) narrows to the cut, and the narrower room is no
/// reason to cut again. (So a content-sized select, once cut, stays cut when its container widens: its own width
/// hugs the cut and never grows past `cut_room`; only a new pick brings the whole words back.)
///
/// Both decisions allow half a pixel of slack, as layout rounds each node's edges to physical pixels on its own.
#[derive(Component, Default, Debug)]
pub(crate) struct CaptionFit {
    full: String,
    shown: String,
    /// The width the whole words lay out at, in logical pixels, once measured.
    full_px: Option<f32>,
    /// The room, in logical pixels, the current cut was made at.
    cut_room: f32,
}

/// The horizontal margins of a node, in logical pixels, which its computed size leaves out.
fn margins(node: &Node) -> f32 {
    [node.margin.left, node.margin.right].into_iter().map(|m| if let Val::Px(m) = m { m } else { 0.0 }).sum()
}

/// Ends each select's caption with an ellipsis where the select's width cuts it off.
#[allow(clippy::type_complexity)]
pub(crate) fn fit_select_captions(
    mut captions: Query<(Entity, &mut CaptionFit, &mut Text, &TextLayoutInfo)>,
    parents: Query<&ChildOf>,
    buttons: Query<(&ComputedNode, &Node, &Children), With<FeathersMenuButton>>,
    nodes: Query<(&ComputedNode, &Node)>,
) {
    for (entity, mut fit, mut text, info) in &mut captions {
        if text.0 != fit.shown {
            // Feathers wrote these words; they are laid out whole this frame, and measured on the next.
            fit.full.clone_from(&text.0);
            fit.shown.clone_from(&text.0);
            fit.full_px = None;
            fit.cut_room = 0.0;
            continue;
        }
        if fit.full_px.is_none() && info.size.x > 0.0 {
            fit.full_px = Some(info.size.x / info.scale_factor.max(f32::EPSILON));
        }
        let Some(full_px) = fit.full_px else { continue };
        // The child of the button the caption sits in, which its siblings' widths are taken from.
        let mut within = entity;
        let mut button = None;
        for ancestor in parents.iter_ancestors(entity) {
            if let Ok(found) = buttons.get(ancestor) {
                button = Some(found);
                break;
            }
            within = ancestor;
        }
        let Some((room, layout, kids)) = button else { continue };
        if room.size.x <= 0.0 {
            continue;
        }
        let inset = room.content_inset();
        let taken: f32 = kids
            .iter()
            .filter_map(|c| nodes.get(c).ok().map(|n| (c, n)))
            .map(|(c, (computed, node))| {
                let size = if c == within { 0.0 } else { computed.size.x * computed.inverse_scale_factor };
                size + margins(node)
            })
            .sum();
        let gap = match layout.column_gap {
            Val::Px(gap) => gap * kids.len().saturating_sub(1) as f32,
            _ => 0.0,
        };
        let width = (room.size.x - inset.min_inset.x - inset.max_inset.x) * room.inverse_scale_factor - taken - gap;
        let shown_px = info.size.x / info.scale_factor.max(f32::EPSILON);
        let cut = fit.shown != fit.full;
        if cut && shown_px <= width + 0.5 && width <= fit.cut_room + 0.5 {
            continue;
        }
        let wanted = if full_px <= width + 0.5 {
            fit.full.clone()
        } else {
            fit.cut_room = width;
            truncate_to_width(&fit.full, width, CAPTION_SIZE)
        };
        if text.0 != wanted {
            text.0.clone_from(&wanted);
        }
        if fit.shown != wanted {
            fit.shown = wanted;
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
    /// is touched while the popup is open (returns `false` then, so the caller can try again once it has closed).
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
        // Each row is a child of the list from the start, so a select despawned before its rows' scenes resolve takes
        // them with it. Queued as related scenes, they would arrive after it had gone, as roots drawn over the
        // window's top left corner.
        for i in 0..options.len() {
            let row = option_row(options, tones, &icons, selected, i);
            self.commands.spawn_scene(bsn! { @{row} }).insert(ChildOf(area));
        }
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
            .add_systems(Update, (style_select_captions, show_chosen_icons, fit_select_captions).chain());
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

    /// Every child of the select's scroll area: the rows, and anything else left in the list.
    fn area_children(app: &mut App) -> Vec<(Entity, bool)> {
        let world = app.world_mut();
        let area = world.query_filtered::<Entity, With<ScrollArea>>().single(world).unwrap();
        let kids: Vec<Entity> = world.get::<Children>(area).map_or(vec![], |c| c.iter().collect());
        kids.into_iter().map(|e| (e, world.get::<FeathersListRow>(e).is_some())).collect()
    }

    #[test]
    fn options_changed_twice_in_a_row_leave_only_the_last_rows() {
        let mut app = app();
        let names = |n: usize| (0..n).map(|i| format!("o{i}")).collect::<Vec<_>>();
        let select = app.world_mut().commands().spawn_scene(select(names(5), Some(0), 8)).id();
        settle(&mut app);
        assert_eq!(area_children(&mut app).len(), 5);
        let edit_now = |app: &mut App, select: Entity, n: usize| {
            app.world_mut().run_system_once(move |mut edit: SelectEdit| edit.set(select, &names(n), Some(0))).unwrap()
        };
        // The second rebuild comes before a frame has passed: the first one's rows are already there to replace.
        assert!(edit_now(&mut app, select, 3));
        assert!(edit_now(&mut app, select, 2));
        settle(&mut app);
        assert_eq!(area_children(&mut app).len(), 2);
        assert!(area_children(&mut app).iter().all(|(_, row)| *row));
    }

    #[test]
    fn rows_queued_for_a_select_despawned_before_they_spawn_leave_nothing_behind() {
        let mut app = app();
        let select = app.world_mut().commands().spawn_scene(select(vec!["a".into(), "b".into()], Some(0), 8)).id();
        settle(&mut app);
        let names = vec!["a".to_string(), "b".into(), "c".into()];
        app.world_mut().run_system_once(move |mut edit: SelectEdit| edit.set(select, &names, Some(0))).unwrap();
        // The select goes before its new rows have spawned, as the board does when the page changes.
        app.world_mut().entity_mut(select).despawn();
        settle(&mut app);
        let world = app.world_mut();
        let orphans = world.query_filtered::<Entity, (With<FeathersListRow>, Without<ChildOf>)>().iter(world).count();
        assert_eq!(orphans, 0, "no row outlives its select");
    }

    #[test]
    fn an_options_words_stay_on_one_line_so_the_popup_fits_its_rows() {
        let mut app = app();
        let options = vec!["all".to_string(), "factory/trail-1121-a2".to_string()];
        app.world_mut().commands().spawn_scene(select(options, Some(0), 8));
        settle(&mut app);
        let world = app.world_mut();
        let rows: Vec<Entity> = world.query_filtered::<Entity, With<FeathersListRow>>().iter(world).collect();
        assert_eq!(rows.len(), 2);
        for row in rows {
            let caption = world.get::<Children>(row).unwrap()[0];
            assert!(world.get::<Text>(caption).is_some());
            // Wrapped, the popup is measured with its words on several lines and comes out taller than its rows.
            assert_eq!(world.get::<TextLayout>(caption).unwrap().linebreak, LineBreak::NoWrap);
        }
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

    /// Stands in for layout, which these tests don't run: the button's width, and the width the caption's whole
    /// words lay out at.
    fn lay_out(app: &mut App, button_width: f32, words_width: f32) {
        let world = app.world_mut();
        let button = world.query_filtered::<Entity, With<FeathersMenuButton>>().single(world).unwrap();
        world.entity_mut(button).insert(ComputedNode {
            size: Vec2::new(button_width, 20.0),
            inverse_scale_factor: 1.0,
            ..default()
        });
        let mut captions = world.query_filtered::<&mut TextLayoutInfo, With<CaptionFit>>();
        for mut info in captions.iter_mut(world) {
            info.scale_factor = 1.0;
            info.size = Vec2::new(words_width, 14.0);
        }
    }

    #[test]
    fn a_caption_cut_off_at_the_selects_width_ends_with_an_ellipsis() {
        let mut app = app();
        let long = "factory/trail-1121-a2-with-a-long-name".to_string();
        let options = vec![long.clone(), "all".to_string()];
        let select = app.world_mut().commands().spawn_scene(select(options.clone(), Some(0), 8)).id();
        settle(&mut app);
        lay_out(&mut app, 100.0, 300.0);
        settle(&mut app);
        let cut = button_texts(&mut app).pop().unwrap();
        assert!(cut.ends_with('\u{2026}') && cut.len() < long.len(), "{cut}");
        // Feathers rewriting the caption on a pick doesn't leave it cut from the cut.
        app.world_mut().run_system_once(move |mut edit: SelectEdit| edit.set(select, &options, Some(1))).unwrap();
        lay_out(&mut app, 100.0, 20.0);
        settle(&mut app);
        assert_eq!(button_texts(&mut app).pop().unwrap(), "all");
    }

    #[test]
    fn a_select_that_hugs_its_caption_never_cuts_it() {
        let mut app = app();
        let options = vec!["Debug".to_string(), "Info".to_string()];
        app.world_mut().commands().spawn_scene(select(options, Some(0), 8));
        settle(&mut app);
        // The button is as wide as its words (35 px) and the insets around them, which the estimate would cut.
        let inset = {
            let world = app.world_mut();
            let button = world.query_filtered::<Entity, With<FeathersMenuButton>>().single(world).unwrap();
            let node = world.get::<Node>(button).unwrap();
            [node.padding.left, node.padding.right]
                .into_iter()
                .map(|v| if let Val::Px(v) = v { v } else { 0.0 })
                .sum::<f32>()
        };
        // Rounding can leave the button a fraction of a pixel narrower than the words.
        for slack in [0.0, -0.3] {
            for _ in 0..4 {
                lay_out(&mut app, 35.0 + inset + slack, 35.0);
                settle(&mut app);
                assert_eq!(button_texts(&mut app).pop().unwrap(), "Debug");
            }
        }
    }

    #[test]
    fn a_select_that_hugs_its_cut_caption_keeps_it_cut_and_does_not_cut_it_further() {
        let mut app = app();
        let long = "factory/trail-1121-a2-with-a-long-name".to_string();
        app.world_mut().commands().spawn_scene(select(vec![long, "all".to_string()], Some(0), 8));
        settle(&mut app);
        lay_out(&mut app, 100.0, 300.0);
        settle(&mut app);
        let first = button_texts(&mut app).pop().unwrap();
        assert!(first.ends_with('\u{2026}'), "{first}");
        // From here the button is as wide as the caption it shows (the stand-in has no insets), as in a plain row.
        let glyph = CAPTION_SIZE.px() * 0.58;
        for _ in 0..6 {
            let words = button_texts(&mut app).pop().unwrap().chars().count() as f32 * glyph;
            lay_out(&mut app, words, words);
            settle(&mut app);
            assert_eq!(button_texts(&mut app).pop().unwrap(), first);
        }
    }
}
