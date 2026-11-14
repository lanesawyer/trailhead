//! Text and choices edited where they are read: click, change, and save, or put it back.
//!
//! [`inline_text`] is a text (a task's title, its description) that turns into a text input when clicked, typing at
//! the text's own size and lined up with it, so starting an edit moves nothing. Under the input are Save and Cancel.
//! One line also saves on Enter; several lines take Enter as a new line. Esc cancels either. Clicking away leaves the
//! edit open: with a Save button there, saving on blur would be a surprise, and throwing the words away worse. A value
//! that didn't change saves nothing. [`inline_field`] is a sidebar row, a label and the kit's [`select_toned`] (status,
//! priority, grade), always shown so it reads as something to change; picking another option is the change, with no
//! Save or Cancel.
//!
//! Neither saves anything itself. A change triggers [`InlineEdited`] (or [`InlineChosen`]) on the widget with the old
//! and the new value, and the widget says "Saving…" (a text dims its new value; a field's select can't be opened)
//! until the caller answers on its component: [`InlineText::confirm`] keeps the value, [`InlineText::fail`] puts the old one back and shows the error
//! beside it, and [`InlineText::fail_keeping`] does too but keeps what was typed for the next edit (a save that lost
//! to someone else's change, to try again over theirs). The caller decides what saving means, and may refuse.
//!
//! The kit's modal rules hold: an Esc that cancels an edit is not passed on, so it doesn't also close the modal the
//! text sits in. The hint shows while the value is empty, and in the input while it is empty during an edit (feathers'
//! input has no placeholder of its own; the hint is laid over it, as the kit's search box does).

use bevy::ecs::system::SystemParam;
use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::{FocusCause, FocusedInput, InputFocus};
use bevy::picking::events::PointerClick;
use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;

use crate::buttons::{ButtonKind, button};
use crate::containers::row;
use crate::input::text_input_sized;
use crate::notice::{Notice, Tone, notice};
use crate::patch::{display, patch_node, set_text};
use crate::select::{Picked, SelectEdit, select_toned};
use crate::spacing::{extent, space};
use crate::text::{Size, size, text, text_dim};
use crate::theme::{TextTone, Token, token};

/// Where an inline widget is in its edit.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum InlineMode {
    /// Showing the value.
    #[default]
    Reading,
    /// Showing the input (a text only: a field's select is always shown).
    Editing,
    /// A change was emitted and the caller hasn't answered.
    Saving,
}

/// A change to an [`inline_text`], triggered on it. The widget shows `new` and says it is saving until the caller
/// calls [`InlineText::confirm`] or [`InlineText::fail`].
#[derive(EntityEvent, Clone, Debug, PartialEq)]
pub struct InlineEdited {
    pub entity: Entity,
    pub old: String,
    pub new: String,
}

/// An option picked in an [`inline_field`] other than the one shown, triggered on it. The caller answers with
/// [`InlineField::confirm`] or [`InlineField::fail`].
#[derive(EntityEvent, Clone, Debug, PartialEq)]
pub struct InlineChosen {
    pub entity: Entity,
    pub old: Option<usize>,
    pub new: usize,
}

/// The state of an [`inline_text`], on its root.
#[derive(Component, Clone, Default, Debug)]
pub struct InlineText {
    value: String,
    hint: String,
    multiline: bool,
    mode: InlineMode,
    error: String,
    /// What was typed in an edit that couldn't be saved, which the next edit starts from.
    draft: Option<String>,
}

impl InlineText {
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn mode(&self) -> InlineMode {
        self.mode
    }

    /// The error shown beside the text, empty if none.
    pub fn error(&self) -> &str {
        &self.error
    }

    /// Shows a value that came from elsewhere (the server changed it). Ignored while it is being edited or saved, so
    /// it doesn't take the words from under the typing.
    pub fn set_value(&mut self, value: &str) {
        if self.mode == InlineMode::Reading && self.value != value {
            self.value = value.to_string();
        }
    }

    /// The save went through: the new value stays.
    pub fn confirm(&mut self) {
        if self.mode == InlineMode::Saving {
            self.mode = InlineMode::Reading;
        }
    }

    /// The save was refused: back to `old`, with `message` beside it.
    pub fn fail(&mut self, old: impl Into<String>, message: impl Into<String>) {
        self.value = old.into();
        self.error = message.into();
        self.mode = InlineMode::Reading;
    }

    /// The save was refused but what was typed is worth keeping: back to `old` (which a newer value from elsewhere may
    /// replace), with `message` beside it, and the next edit starts from `draft`.
    pub fn fail_keeping(&mut self, old: impl Into<String>, draft: impl Into<String>, message: impl Into<String>) {
        self.fail(old, message);
        self.draft = Some(draft.into());
    }

    /// What the next edit starts from, when an edit couldn't be saved.
    pub fn draft(&self) -> Option<&str> {
        self.draft.as_deref()
    }

    /// Ends an edit: `draft` is what to save, or `None` to put the value back. Returns the change, if there is one.
    fn finish(&mut self, draft: Option<String>) -> Option<(String, String)> {
        if self.mode != InlineMode::Editing {
            return None;
        }
        match draft {
            Some(new) if new != self.value => {
                let old = std::mem::replace(&mut self.value, new.clone());
                self.mode = InlineMode::Saving;
                Some((old, new))
            }
            _ => {
                self.mode = InlineMode::Reading;
                None
            }
        }
    }
}

/// The state of an [`inline_field`], on its root.
#[derive(Component, Clone, Default, Debug)]
pub struct InlineField {
    options: Vec<String>,
    tones: Vec<Option<Token>>,
    selected: Option<usize>,
    mode: InlineMode,
    error: String,
}

impl InlineField {
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn mode(&self) -> InlineMode {
        self.mode
    }

    pub fn error(&self) -> &str {
        &self.error
    }

    /// Shows a choice that came from elsewhere, and new options in their tones (as [`inline_field`] takes them).
    /// Ignored while it is being saved.
    pub fn set(&mut self, options: Vec<String>, tones: Vec<Option<Token>>, selected: Option<usize>) {
        if self.mode == InlineMode::Reading
            && (self.options != options || self.tones != tones || self.selected != selected)
        {
            self.options = options;
            self.tones = tones;
            self.selected = selected;
        }
    }

    /// The save went through: the choice stays.
    pub fn confirm(&mut self) {
        if self.mode == InlineMode::Saving {
            self.mode = InlineMode::Reading;
        }
    }

    /// The save was refused: back to `old`, with `message` beside it.
    pub fn fail(&mut self, old: Option<usize>, message: impl Into<String>) {
        self.selected = old;
        self.error = message.into();
        self.mode = InlineMode::Reading;
    }
}

/// How many options a field's select shows before it scrolls.
const FIELD_ROWS: usize = 6;

/// The pieces of an inline widget, found from its root by what they are.
#[derive(Component, Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Part {
    /// The clickable box holding the value and the hint.
    #[default]
    Shown,
    Value,
    Hint,
    /// The box holding the input or the select.
    Editor,
    /// The hint laid over an empty input while editing.
    EditHint,
    Saving,
    Error,
}

/// The text entity of an [`inline_text`]'s input.
#[derive(Component, Clone, Default)]
struct InlineInput;

/// A button under an [`inline_text`]'s input.
#[derive(Component, Clone, Copy, Default, PartialEq, Eq, Debug)]
enum EditAction {
    #[default]
    Save,
    Cancel,
}

/// A text that becomes an input when clicked, `lines` high while editing (more than one allows newlines). `hint`
/// shows while it is empty.
pub fn inline_text(value: impl Into<String>, hint: impl Into<String>, lines: f32) -> impl Scene {
    inline_text_sized(value, hint, lines, size::BODY)
}

/// An [`inline_text`] read at `font_size` (one of [`size`]'s), such as a detail view's title.
pub fn inline_text_sized(value: impl Into<String>, hint: impl Into<String>, lines: f32, font_size: Size) -> impl Scene {
    let (value, hint) = (value.into(), hint.into());
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) } }
        InlineText { value: { value.clone() }, hint: { hint.clone() }, multiline: { lines > 1.0 } }
        Children [
            @shown(value, hint.clone(), font_size)
            --
            Node {
                display: { Display::None },
                flex_direction: FlexDirection::Column,
                row_gap: { px(space::ROWS) },
                width: percent(100),
            }
            Part::Editor
            Children [
                Node { width: percent(100) }
                Children [
                    // Pulled out by the input's own inset, so its text sits where the shown text did.
                    @text_input_sized(lines, font_size, bsn! { InlineInput })
                    Node {
                        width: { Val::Auto },
                        padding: { UiRect::horizontal(px(space::INPUT_INSET)) },
                        margin: { UiRect::horizontal(px(-space::INPUT_INSET)) },
                    }
                    --
                    @text_dim(hint.clone(), font_size)
                    Part::EditHint
                    Node { display: { Display::None }, position_type: { PositionType::Absolute }, left: px(0) }
                    Pickable::IGNORE
                ]
                --
                @row()
                Children [
                    @button("Save", None, ButtonKind::Primary) EditAction::Save
                    --
                    @button("Cancel", None, ButtonKind::Normal) EditAction::Cancel
                ]
            ]
            --
            @status()
        ]
    }
}

/// A sidebar row: `label` and a [`select_toned`] of `options` in `tones` (one per option, or none), showing `selected`.
pub fn inline_field(
    label: impl Into<String>,
    options: Vec<String>,
    tones: Vec<Option<Token>>,
    selected: Option<usize>,
) -> impl Scene {
    let label = label.into();
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) } }
        InlineField { options: { options.clone() }, tones: { tones.clone() }, selected: { selected } }
        Children [
            Node { column_gap: { px(space::CONTROLS) }, align_items: AlignItems::Center }
            Children [
                @text_dim(label, size::SECONDARY)
                Node { width: { px(extent::FIELD_LABEL_WIDTH) }, flex_shrink: { 0.0_f32 } }
                --
                @select_toned(options, tones, selected, FIELD_ROWS)
                Node { flex_grow: 1.0, min_width: px(0) }
            ]
            --
            @status()
        ]
    }
}

/// The clickable value, and its hint while the value is empty.
fn shown(value: String, hint: String, font_size: Size) -> impl Scene {
    bsn! {
        Node { flex_direction: FlexDirection::Column, width: percent(100) }
        Part::Shown
        on(open_edit)
        Children [
            @text(value, font_size)
            Part::Value
            --
            @text_dim(hint, font_size)
            Part::Hint
        ]
    }
}

/// "Saving…" while a change is in flight, and the caller's error once it is refused.
fn status() -> impl Scene {
    bsn! {
        Node { column_gap: { px(space::CONTROLS) }, align_items: AlignItems::Center }
        Children [
            @text_dim("Saving…", size::SECONDARY)
            Part::Saving
            Node { display: { Display::None } }
            --
            @notice()
            Part::Error
        ]
    }
}

/// Finds the roots of inline widgets.
type Roots<'w, 's> = Query<'w, 's, (), Or<(With<InlineText>, With<InlineField>)>>;

/// The inline widget a part (or anything inside one) belongs to.
fn root_of(start: Entity, parents: &Query<&ChildOf>, roots: &Roots) -> Option<Entity> {
    std::iter::once(start).chain(parents.iter_ancestors(start)).find(|e| roots.contains(*e))
}

/// A click on the text starts its edit: the input takes the text and the keyboard.
#[derive(SystemParam)]
struct Opening<'w, 's> {
    parents: Query<'w, 's, &'static ChildOf>,
    roots: Roots<'w, 's>,
    texts: Query<'w, 's, &'static mut InlineText>,
    children: Query<'w, 's, &'static Children>,
    inputs: Query<'w, 's, &'static mut EditableText, With<InlineInput>>,
    focus: ResMut<'w, InputFocus>,
}

fn open_edit(click: On<PointerClick>, mut open: Opening) {
    if click.button != PointerButton::Primary {
        return;
    }
    begin_edit(click.entity, &mut open);
}

/// Starts the edit of the inline text `start` is in.
fn begin_edit(start: Entity, open: &mut Opening) {
    let Some(root) = root_of(start, &open.parents, &open.roots) else { return };
    let Ok(mut text) = open.texts.get_mut(root) else { return };
    if text.mode != InlineMode::Reading {
        return;
    }
    text.mode = InlineMode::Editing;
    text.error.clear();
    let value = text.draft.take().unwrap_or_else(|| text.value.clone());
    let input = open.children.iter_descendants(root).find(|e| open.inputs.contains(*e));
    if let Some(input) = input {
        if let Ok(mut editable) = open.inputs.get_mut(input) {
            editable.editor_mut().set_text(&value);
            editable.queue_edit(bevy::text::TextEdit::TextEnd(false));
        }
        open.focus.set(input, FocusCause::Navigated);
    }
}

/// Enter saves a one-line text and Esc cancels either kind. A multi-line text takes Enter as a new line, which
/// feathers' input does itself.
fn on_key(
    mut key: On<FocusedInput<KeyboardInput>>,
    inputs: Query<&EditableText, With<InlineInput>>,
    parents: Query<&ChildOf>,
    roots: Roots,
    mut texts: Query<&mut InlineText>,
    mut focus: ResMut<InputFocus>,
    mut commands: Commands,
) {
    // The press bubbles up from the input; act once, there.
    let start = key.original_event_target();
    if key.event_target() != start {
        return;
    }
    let Ok(editable) = inputs.get(start) else { return };
    let pressed = &key.input;
    if !pressed.state.is_pressed() || pressed.repeat {
        return;
    }
    let Some(root) = root_of(start, &parents, &roots) else { return };
    let Ok(mut text) = texts.get_mut(root) else { return };
    if text.mode != InlineMode::Editing {
        return;
    }
    let save = match pressed.key_code {
        KeyCode::Escape => false,
        KeyCode::Enter | KeyCode::NumpadEnter if !text.multiline && !editable.is_composing() => true,
        _ => return,
    };
    // Whoever is behind this text (a modal) doesn't also get the Esc.
    key.propagate(false);
    end_edit(root, &mut text, save.then(|| editable.value().to_string()), &mut focus, &mut commands);
}

/// Save or Cancel under a text being edited.
#[allow(clippy::too_many_arguments)]
fn on_action(
    pressed: On<Activate>,
    actions: Query<&EditAction>,
    parents: Query<&ChildOf>,
    roots: Roots,
    children: Query<&Children>,
    inputs: Query<&EditableText, With<InlineInput>>,
    mut texts: Query<&mut InlineText>,
    mut focus: ResMut<InputFocus>,
    mut commands: Commands,
) {
    let Ok(action) = actions.get(pressed.entity) else { return };
    let Some(root) = root_of(pressed.entity, &parents, &roots) else { return };
    let Ok(mut text) = texts.get_mut(root) else { return };
    let draft = match action {
        EditAction::Cancel => None,
        EditAction::Save => {
            children.iter_descendants(root).find_map(|e| inputs.get(e).ok()).map(|i| i.value().to_string())
        }
    };
    end_edit(root, &mut text, draft, &mut focus, &mut commands);
}

/// Ends the edit of `text` (on `root`): saves `draft`, or puts the value back if `None`, and gives the keyboard back.
fn end_edit(
    root: Entity,
    text: &mut InlineText,
    draft: Option<String>,
    focus: &mut InputFocus,
    commands: &mut Commands,
) {
    if text.mode != InlineMode::Editing {
        return;
    }
    if let Some((old, new)) = text.finish(draft) {
        commands.trigger(InlineEdited { entity: root, old, new });
    }
    focus.clear();
}

/// A pick in an inline field's select: a different option is a change, the same one is nothing.
fn on_picked(
    picked: On<Picked>,
    parents: Query<&ChildOf>,
    roots: Roots,
    mut fields: Query<&mut InlineField>,
    mut commands: Commands,
) {
    let Some(root) = root_of(picked.entity, &parents, &roots) else { return };
    let Ok(mut field) = fields.get_mut(root) else { return };
    if field.mode != InlineMode::Reading || field.selected == Some(picked.index) {
        return;
    }
    field.error.clear();
    let old = field.selected.replace(picked.index);
    field.mode = InlineMode::Saving;
    commands.trigger(InlineChosen { entity: root, old, new: picked.index });
}

/// Draws each inline widget from its state, in place: shows the value or the editor, the hint, "Saving…" and the
/// error, and a field's choice. A widget is drawn when its state changed; a text being edited is looked at every frame
/// too, since its input's text changes without it.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn sync_inline(
    texts: Query<(Entity, Ref<InlineText>)>,
    fields: Query<(Entity, Ref<InlineField>)>,
    children: Query<&Children>,
    inputs: Query<Ref<EditableText>, With<InlineInput>>,
    mut parts: Query<(&Part, &mut Node, Option<&mut Text>, Option<&TextTone>)>,
    mut notices: Query<(&Part, &mut Notice)>,
    selects: Query<(), With<crate::select::SelectOptions>>,
    mut select_edit: SelectEdit,
    mut retry: Local<Vec<Entity>>,
    mut commands: Commands,
) {
    let again = std::mem::take(&mut *retry);
    for (root, text) in &texts {
        let editing = text.mode == InlineMode::Editing;
        if !(text.is_changed() || editing) {
            continue;
        }
        let input = children.iter_descendants(root).find_map(|e| inputs.get(e).ok());
        if !text.is_changed() && !input.as_ref().is_some_and(|i| i.is_changed()) {
            continue;
        }
        let empty = text.value.is_empty();
        let typed_empty = input.as_ref().is_some_and(|i| i.value().to_string().is_empty());
        let view = View {
            mode: text.mode,
            value: &text.value,
            hint: &text.hint,
            show_value: !empty,
            show_hint: empty,
            show_edit_hint: editing && typed_empty,
            error: &text.error,
        };
        draw(root, &view, &children, &mut parts, &mut notices, &mut commands);
    }
    for (root, field) in &fields {
        if !(field.is_changed() || again.contains(&root)) {
            continue;
        }
        if let Some(select) = children.iter_descendants(root).find(|e| selects.contains(*e)) {
            // A select whose popup is open takes the options on a later frame.
            if !select_edit.set_toned(select, &field.options, &field.tones, field.selected) {
                retry.push(root);
            }
            select_edit.set_enabled(select, field.mode != InlineMode::Saving);
        }
        let view = View {
            mode: field.mode,
            value: "",
            hint: "",
            show_value: false,
            show_hint: false,
            show_edit_hint: false,
            error: &field.error,
        };
        draw(root, &view, &children, &mut parts, &mut notices, &mut commands);
    }
}

/// What [`draw`] puts on an inline widget.
struct View<'a> {
    mode: InlineMode,
    value: &'a str,
    hint: &'a str,
    show_value: bool,
    show_hint: bool,
    show_edit_hint: bool,
    error: &'a str,
}

#[allow(clippy::type_complexity)]
fn draw(
    root: Entity,
    view: &View,
    children: &Query<&Children>,
    parts: &mut Query<(&Part, &mut Node, Option<&mut Text>, Option<&TextTone>)>,
    notices: &mut Query<(&Part, &mut Notice)>,
    commands: &mut Commands,
) {
    let editing = view.mode == InlineMode::Editing;
    for part in children.iter_descendants(root) {
        if let Ok((kind, node, text, color)) = parts.get_mut(part) {
            let shown = match kind {
                Part::Shown => Some(!editing),
                Part::Value => Some(view.show_value),
                Part::Hint => Some(view.show_hint),
                Part::Editor => Some(editing),
                Part::EditHint => Some(view.show_edit_hint),
                Part::Saving => Some(view.mode == InlineMode::Saving),
                Part::Error => None,
            };
            // The notice shows itself from what it says.
            if let Some(shown) = shown {
                patch_node(node, |node| node.display = display(shown));
            }
            if let Some(text) = text {
                match kind {
                    Part::Value => set_text(text, view.value),
                    Part::Hint | Part::EditHint => set_text(text, view.hint),
                    _ => {}
                }
            }
            // A value being saved is dimmed until the caller answers.
            if *kind == Part::Value {
                let wanted = if view.mode == InlineMode::Saving { token::TEXT_DIM } else { token::TEXT };
                if color.is_none_or(|c| c.0 != wanted) {
                    commands.entity(part).insert(TextTone(wanted));
                }
            }
        }
        if let Ok((Part::Error, mut notice)) = notices.get_mut(part) {
            let wanted = Notice::new(Tone::Error, view.error);
            if *notice != wanted {
                *notice = wanted;
            }
        }
    }
}

/// The behavior of inline widgets: the click, the keys, the buttons and the picks.
pub struct InlinePlugin;

impl Plugin for InlinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputFocus>()
            .add_observer(on_key)
            .add_observer(on_action)
            .add_observer(on_picked)
            .add_systems(Update, sync_inline.in_set(crate::UiPlace));
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;
    use bevy::input::ButtonState;
    use bevy::input::keyboard::Key;
    use bevy::input_focus::FocusLost;

    use super::*;

    #[derive(Resource, Default)]
    struct Edits(Vec<InlineEdited>);

    #[derive(Resource, Default)]
    struct Choices(Vec<InlineChosen>);

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .insert_resource(crate::theme::Theme::new().resource())
            .add_plugins(InlinePlugin)
            .init_resource::<Edits>()
            .init_resource::<Choices>()
            .add_observer(|e: On<InlineEdited>, mut edits: ResMut<Edits>| edits.0.push(e.clone()))
            .add_observer(|e: On<InlineChosen>, mut choices: ResMut<Choices>| choices.0.push(e.clone()));
        app
    }

    fn settle(app: &mut App) {
        app.world_mut().flush();
        for _ in 0..3 {
            app.update();
        }
    }

    fn text(app: &mut App, value: &str, lines: f32) -> (Entity, Entity) {
        let root = app.world_mut().commands().spawn_scene(inline_text(value, "Add a description", lines)).id();
        settle(app);
        let input = app.world_mut().query_filtered::<Entity, With<InlineInput>>().single(app.world()).unwrap();
        (root, input)
    }

    fn click(app: &mut App, root: Entity) {
        app.world_mut().run_system_once(move |mut open: Opening| begin_edit(root, &mut open)).unwrap();
        settle(app);
    }

    fn type_in(app: &mut App, input: Entity, value: &str) {
        app.world_mut().get_mut::<EditableText>(input).unwrap().editor_mut().set_text(value);
    }

    fn press(app: &mut App, input: Entity, key_code: KeyCode, logical_key: Key) {
        let window = app.world_mut().spawn_empty().id();
        let keyboard =
            KeyboardInput { key_code, logical_key, state: ButtonState::Pressed, text: None, repeat: false, window };
        app.world_mut().trigger(FocusedInput::new(input, keyboard, window));
        settle(app);
    }

    fn enter(app: &mut App, input: Entity) {
        press(app, input, KeyCode::Enter, Key::Enter);
    }

    fn escape(app: &mut App, input: Entity) {
        press(app, input, KeyCode::Escape, Key::Escape);
    }

    fn blur(app: &mut App, input: Entity) {
        app.world_mut().trigger(FocusLost { entity: input });
        settle(app);
    }

    fn press_button(app: &mut App, action: EditAction) {
        let button = app.world_mut().query::<(Entity, &EditAction)>().iter(app.world()).find(|(_, a)| **a == action);
        let button = button.unwrap().0;
        app.world_mut().trigger(Activate { entity: button });
        settle(app);
    }

    fn mode(app: &App, root: Entity) -> InlineMode {
        app.world().get::<InlineText>(root).unwrap().mode()
    }

    fn display_of(app: &mut App, part: Part) -> Display {
        app.world_mut().query::<(&Part, &Node)>().iter(app.world()).find(|(p, _)| **p == part).unwrap().1.display
    }

    fn edits(app: &App) -> Vec<(String, String)> {
        app.world().resource::<Edits>().0.iter().map(|e| (e.old.clone(), e.new.clone())).collect()
    }

    #[test]
    fn a_click_turns_the_text_into_an_input_holding_it_with_the_keyboard() {
        let mut app = app();
        let (root, input) = text(&mut app, "Fix the board", 1.0);
        assert_eq!((mode(&app, root), display_of(&mut app, Part::Editor)), (InlineMode::Reading, Display::None));
        click(&mut app, root);
        assert_eq!(mode(&app, root), InlineMode::Editing);
        assert_eq!(display_of(&mut app, Part::Editor), Display::Flex);
        assert_eq!(display_of(&mut app, Part::Shown), Display::None);
        assert_eq!(app.world().get::<EditableText>(input).unwrap().value().to_string(), "Fix the board");
        assert_eq!(app.world().resource::<InputFocus>().get(), Some(input));
    }

    #[test]
    fn enter_saves_a_one_line_text_and_says_it_is_saving() {
        let mut app = app();
        let (root, input) = text(&mut app, "Fix the board", 1.0);
        assert!(!app.world().get::<EditableText>(input).unwrap().allow_newlines);
        click(&mut app, root);
        type_in(&mut app, input, "Fix the card");
        enter(&mut app, input);
        assert_eq!(edits(&app), vec![("Fix the board".to_string(), "Fix the card".to_string())]);
        assert_eq!(mode(&app, root), InlineMode::Saving);
        assert_eq!(display_of(&mut app, Part::Saving), Display::Flex);
        assert_eq!(display_of(&mut app, Part::Editor), Display::None);
        let value = app.world_mut().query::<(&Part, &Text)>().iter(app.world()).find(|(p, _)| **p == Part::Value);
        assert_eq!(value.unwrap().1.0, "Fix the card");
        // The keyboard goes back, so the blur that follows saves nothing twice.
        blur(&mut app, input);
        assert_eq!(edits(&app).len(), 1);
        // A click while it saves doesn't start another edit.
        click(&mut app, root);
        assert_eq!(mode(&app, root), InlineMode::Saving);
    }

    #[test]
    fn a_save_that_lost_to_another_change_shows_theirs_and_the_next_edit_starts_from_what_was_typed() {
        let mut app = app();
        let (root, input) = text(&mut app, "a", 1.0);
        click(&mut app, root);
        type_in(&mut app, input, "mine");
        enter(&mut app, input);
        app.world_mut().get_mut::<InlineText>(root).unwrap().fail_keeping("a", "mine", "changed elsewhere");
        // The newer value arrives and is shown.
        app.world_mut().get_mut::<InlineText>(root).unwrap().set_value("theirs");
        settle(&mut app);
        let state = app.world().get::<InlineText>(root).unwrap();
        assert_eq!((state.value(), state.error(), state.draft()), ("theirs", "changed elsewhere", Some("mine")));
        click(&mut app, root);
        assert_eq!(app.world().get::<EditableText>(input).unwrap().value().to_string(), "mine");
        enter(&mut app, input);
        assert_eq!(edits(&app).last(), Some(&("theirs".to_string(), "mine".to_string())));
    }

    #[test]
    fn save_saves_what_was_typed_and_gives_the_keyboard_back() {
        let mut app = app();
        let (root, input) = text(&mut app, "a", 1.0);
        click(&mut app, root);
        type_in(&mut app, input, "b");
        press_button(&mut app, EditAction::Save);
        assert_eq!(edits(&app), vec![("a".to_string(), "b".to_string())]);
        assert_eq!(mode(&app, root), InlineMode::Saving);
        assert_eq!(app.world().resource::<InputFocus>().get(), None);
    }

    #[test]
    fn cancel_puts_the_text_back_without_saving() {
        let mut app = app();
        let (root, input) = text(&mut app, "keep me", 4.0);
        click(&mut app, root);
        type_in(&mut app, input, "lose me");
        press_button(&mut app, EditAction::Cancel);
        assert!(edits(&app).is_empty());
        assert_eq!(app.world().get::<InlineText>(root).unwrap().value(), "keep me");
        assert_eq!((mode(&app, root), display_of(&mut app, Part::Editor)), (InlineMode::Reading, Display::None));
    }

    #[test]
    fn clicking_away_keeps_the_edit_open_with_what_was_typed() {
        let mut app = app();
        let (root, input) = text(&mut app, "a", 1.0);
        click(&mut app, root);
        type_in(&mut app, input, "b");
        blur(&mut app, input);
        assert!(edits(&app).is_empty());
        assert_eq!(mode(&app, root), InlineMode::Editing);
        assert_eq!(app.world().get::<EditableText>(input).unwrap().value().to_string(), "b");
    }

    #[test]
    fn the_input_types_at_the_size_of_the_text_it_stands_in_for() {
        let mut app = app();
        app.world_mut().commands().spawn_scene(inline_text_sized("Fix the board", "", 1.0, size::DETAIL_TITLE));
        settle(&mut app);
        let world = app.world_mut();
        let input = world.query_filtered::<&TextFont, With<InlineInput>>().single(world).unwrap().font_size;
        let shown = world.query::<(&Part, &TextFont)>().iter(world).find(|(p, _)| **p == Part::Value).unwrap().1;
        assert_eq!(input, shown.font_size);
        assert_eq!(input, FontSize::from(size::DETAIL_TITLE));
    }

    #[test]
    fn escape_puts_the_text_back_without_saving() {
        let mut app = app();
        let (root, input) = text(&mut app, "keep me", 1.0);
        click(&mut app, root);
        type_in(&mut app, input, "lose me");
        escape(&mut app, input);
        assert!(edits(&app).is_empty());
        assert_eq!(app.world().get::<InlineText>(root).unwrap().value(), "keep me");
        assert_eq!(mode(&app, root), InlineMode::Reading);
        assert_eq!(display_of(&mut app, Part::Shown), Display::Flex);
        // The blur that follows doesn't save what was thrown away.
        blur(&mut app, input);
        assert!(edits(&app).is_empty());
    }

    #[test]
    fn an_unchanged_value_emits_nothing() {
        let mut app = app();
        let (root, input) = text(&mut app, "same", 1.0);
        click(&mut app, root);
        enter(&mut app, input);
        assert_eq!(mode(&app, root), InlineMode::Reading);
        click(&mut app, root);
        press_button(&mut app, EditAction::Save);
        assert_eq!(mode(&app, root), InlineMode::Reading);
        assert!(edits(&app).is_empty());
    }

    #[test]
    fn enter_in_a_multi_line_text_adds_a_line_and_save_saves() {
        let mut app = app();
        let (root, input) = text(&mut app, "one", 4.0);
        assert!(app.world().get::<EditableText>(input).unwrap().allow_newlines);
        click(&mut app, root);
        enter(&mut app, input);
        assert_eq!(mode(&app, root), InlineMode::Editing);
        assert!(edits(&app).is_empty());
        type_in(&mut app, input, "one\ntwo");
        press_button(&mut app, EditAction::Save);
        assert_eq!(edits(&app), vec![("one".to_string(), "one\ntwo".to_string())]);
    }

    #[test]
    fn a_refused_save_restores_the_old_value_with_the_error_beside_it() {
        let mut app = app();
        let (root, input) = text(&mut app, "old", 1.0);
        click(&mut app, root);
        type_in(&mut app, input, "new");
        enter(&mut app, input);
        app.world_mut().get_mut::<InlineText>(root).unwrap().fail("old", "Refused: title too long");
        settle(&mut app);
        assert_eq!(mode(&app, root), InlineMode::Reading);
        assert_eq!(display_of(&mut app, Part::Saving), Display::None);
        let value = app.world_mut().query::<(&Part, &Text)>().iter(app.world()).find(|(p, _)| **p == Part::Value);
        assert_eq!(value.unwrap().1.0, "old");
        let notice = app.world_mut().query::<(&Part, &Notice)>().iter(app.world()).next().unwrap().1.message.clone();
        assert_eq!(notice, "Refused: title too long");
        // Editing again clears the error.
        click(&mut app, root);
        assert_eq!(app.world().get::<InlineText>(root).unwrap().error(), "");
    }

    #[test]
    fn a_confirmed_save_keeps_the_value() {
        let mut app = app();
        let (root, input) = text(&mut app, "old", 1.0);
        click(&mut app, root);
        type_in(&mut app, input, "new");
        enter(&mut app, input);
        app.world_mut().get_mut::<InlineText>(root).unwrap().confirm();
        settle(&mut app);
        assert_eq!(
            (mode(&app, root), app.world().get::<InlineText>(root).unwrap().value()),
            (InlineMode::Reading, "new")
        );
    }

    #[test]
    fn an_empty_text_shows_its_hint() {
        let mut app = app();
        let (root, _) = text(&mut app, "", 4.0);
        assert_eq!(display_of(&mut app, Part::Hint), Display::Flex);
        assert_eq!(display_of(&mut app, Part::Value), Display::None);
        let _ = root;
    }

    fn field(app: &mut App) -> Entity {
        let options = vec!["Todo".to_string(), "Doing".to_string(), "Done".to_string()];
        let root = app.world_mut().commands().spawn_scene(inline_field("Status", options, tones(), Some(0))).id();
        settle(app);
        root
    }

    fn pick(app: &mut App, root: Entity, index: usize) {
        let select =
            app.world_mut().query_filtered::<Entity, With<crate::select::SelectOptions>>().single(app.world()).unwrap();
        let _ = root;
        app.world_mut().trigger(Picked { entity: select, index });
        settle(app);
    }

    const DOING: Token = Token::new("test.doing");
    const DONE: Token = Token::new("test.done");

    fn tones() -> Vec<Option<Token>> {
        vec![None, Some(DOING), Some(DONE)]
    }

    /// The select's caption: the text inside its menu button.
    fn caption(app: &mut App) -> Entity {
        let world = app.world_mut();
        let mut buttons = world.query_filtered::<Entity, With<bevy::feathers::controls::FeathersMenuButton>>();
        let button = buttons.single(world).unwrap();
        let mut texts = world.query_filtered::<Entity, With<Text>>();
        let under = |mut e: Entity| {
            while let Some(parent) = world.get::<ChildOf>(e) {
                e = parent.parent();
                if e == button {
                    return true;
                }
            }
            false
        };
        texts.iter(world).find(|e| under(*e)).unwrap()
    }

    fn caption_tone(app: &mut App) -> Option<Token> {
        let caption = caption(app);
        app.world().get::<crate::TextTone>(caption).map(|t| t.0.clone())
    }

    #[test]
    fn a_fields_caption_is_in_the_tone_of_its_choice_and_stays_on_one_line() {
        let mut app = app();
        app.add_systems(Update, crate::select::style_select_captions);
        let root = field(&mut app);
        assert_eq!(caption_tone(&mut app), Some(crate::token::TEXT));
        pick(&mut app, root, 2);
        assert_eq!(caption_tone(&mut app), Some(DONE));
        app.world_mut().get_mut::<InlineField>(root).unwrap().fail(Some(1), "no");
        settle(&mut app);
        assert_eq!(caption_tone(&mut app), Some(DOING));
        let caption = caption(&mut app);
        assert_eq!(app.world().get::<TextLayout>(caption).unwrap().linebreak, bevy::text::LineBreak::NoWrap);
    }

    fn select_state(app: &mut App) -> (Vec<usize>, bool) {
        let select =
            app.world_mut().query_filtered::<Entity, With<crate::select::SelectOptions>>().single(app.world()).unwrap();
        app.world_mut()
            .run_system_once(move |edit: SelectEdit| (edit.selected_rows(select), edit.is_enabled(select)))
            .unwrap()
    }

    #[test]
    fn a_field_shows_its_select_from_the_start_and_a_different_pick_is_a_change() {
        let mut app = app();
        let root = field(&mut app);
        assert_eq!(select_state(&mut app), (vec![0], true));
        pick(&mut app, root, 2);
        let choices = &app.world().resource::<Choices>().0;
        assert_eq!(choices.len(), 1);
        assert_eq!((choices[0].old, choices[0].new), (Some(0), 2));
        assert_eq!(app.world().get::<InlineField>(root).unwrap().mode(), InlineMode::Saving);
        assert_eq!(display_of(&mut app, Part::Saving), Display::Flex);
        assert!(!select_state(&mut app).1, "no second pick while the first saves");
        app.world_mut().get_mut::<InlineField>(root).unwrap().fail(Some(0), "Not allowed");
        settle(&mut app);
        let f = app.world().get::<InlineField>(root).unwrap();
        assert_eq!((f.selected(), f.mode(), f.error()), (Some(0), InlineMode::Reading, "Not allowed"));
        assert_eq!(select_state(&mut app), (vec![0], true), "the select shows the old choice again");
        // The next pick clears the error.
        pick(&mut app, root, 1);
        assert_eq!(app.world().get::<InlineField>(root).unwrap().error(), "");
    }

    #[test]
    fn a_confirmed_pick_stays() {
        let mut app = app();
        let root = field(&mut app);
        pick(&mut app, root, 1);
        app.world_mut().get_mut::<InlineField>(root).unwrap().confirm();
        settle(&mut app);
        assert_eq!(app.world().get::<InlineField>(root).unwrap().selected(), Some(1));
        assert_eq!(select_state(&mut app), (vec![1], true));
        assert_eq!(display_of(&mut app, Part::Saving), Display::None);
    }

    #[test]
    fn picking_the_same_option_emits_nothing() {
        let mut app = app();
        let root = field(&mut app);
        pick(&mut app, root, 0);
        assert!(app.world().resource::<Choices>().0.is_empty());
        assert_eq!(app.world().get::<InlineField>(root).unwrap().mode(), InlineMode::Reading);
    }

    fn value_color(app: &mut App) -> Option<Token> {
        let world = app.world_mut();
        let mut q = world.query::<(&Part, Option<&TextTone>)>();
        q.iter(world).find(|(p, _)| **p == Part::Value).and_then(|(_, c)| c.map(|c| c.0.clone()))
    }

    #[test]
    fn a_value_being_saved_is_dimmed_until_the_caller_answers() {
        let mut app = app();
        let (root, input) = text(&mut app, "Fix the board", 1.0);
        assert_eq!(value_color(&mut app), Some(token::TEXT));
        click(&mut app, root);
        type_in(&mut app, input, "Fix the card");
        enter(&mut app, input);
        assert_eq!(mode(&app, root), InlineMode::Saving);
        assert_eq!(value_color(&mut app), Some(token::TEXT_DIM));
        app.world_mut().get_mut::<InlineText>(root).unwrap().confirm();
        settle(&mut app);
        assert_eq!(value_color(&mut app), Some(token::TEXT));
    }

    #[test]
    fn the_hint_shows_in_the_input_while_it_is_empty_during_an_edit() {
        let mut app = app();
        let (root, input) = text(&mut app, "", 3.0);
        assert_eq!(display_of(&mut app, Part::EditHint), Display::None, "not while reading");
        click(&mut app, root);
        assert_eq!(display_of(&mut app, Part::EditHint), Display::Flex);
        type_in(&mut app, input, "Some words");
        settle(&mut app);
        assert_eq!(display_of(&mut app, Part::EditHint), Display::None);
        type_in(&mut app, input, "");
        settle(&mut app);
        assert_eq!(display_of(&mut app, Part::EditHint), Display::Flex);
    }

    #[test]
    fn an_idle_widget_writes_nothing_from_frame_to_frame() {
        #[derive(Resource, Default)]
        struct Redraws(usize);
        let mut app = app();
        app.init_resource::<Redraws>().add_systems(
            PostUpdate,
            |changed: Query<(), (With<Part>, Changed<Node>)>,
             notices: Query<(), Changed<Notice>>,
             mut n: ResMut<Redraws>| {
                n.0 += changed.iter().count() + notices.iter().count();
            },
        );
        text(&mut app, "Fix the board", 1.0);
        app.world_mut().commands().spawn_scene(inline_field(
            "Priority",
            vec!["P1".into(), "P2".into()],
            Vec::new(),
            Some(1),
        ));
        settle(&mut app);
        settle(&mut app);
        app.world_mut().resource_mut::<Redraws>().0 = 0;
        settle(&mut app);
        settle(&mut app);
        assert_eq!(app.world().resource::<Redraws>().0, 0);
    }

    #[derive(Component)]
    struct Holder;

    impl crate::Modal for Holder {
        fn title(&self) -> String {
            "Edit".into()
        }
    }

    #[test]
    fn esc_in_an_edit_inside_a_modal_cancels_the_edit_and_leaves_the_modal_open() {
        let mut app = app();
        app.add_plugins(crate::ModalPlugin);
        let modal = crate::open_modal(&mut app.world_mut().commands(), Holder);
        settle(&mut app);
        let body = app.world().get::<crate::ModalSlots>(modal).unwrap().body;
        let root =
            app.world_mut().commands().spawn_scene(inline_text("Fix the board", "", 1.0)).insert(ChildOf(body)).id();
        settle(&mut app);
        let input = app.world_mut().query_filtered::<Entity, With<InlineInput>>().single(app.world()).unwrap();
        click(&mut app, root);
        type_in(&mut app, input, "Something else");
        escape(&mut app, input);
        assert_eq!(mode(&app, root), InlineMode::Reading);
        assert_eq!(app.world().get::<InlineText>(root).unwrap().value(), "Fix the board");
        assert!(app.world().get_entity(modal).is_ok(), "the modal is still open");
        assert!(edits(&app).is_empty());
    }

    #[test]
    fn esc_in_an_inline_fields_popup_closes_it_and_leaves_the_modal_open() {
        use bevy::feathers::controls::{FeathersMenu, FeathersMenuPopup, SelectPlugin};
        use bevy::ui_widgets::{MenuAction, MenuEvent};
        let mut app = app();
        app.add_plugins((
            crate::ModalPlugin,
            SelectPlugin,
            bevy::ui_widgets::MenuPlugin,
            bevy::ui_widgets::ModalDialogPlugin,
        ))
        .init_resource::<bevy::input_focus::InputFocusVisible>();
        let modal = crate::open_modal(&mut app.world_mut().commands(), Holder);
        settle(&mut app);
        let body = app.world().get::<crate::ModalSlots>(modal).unwrap().body;
        let options = vec!["Todo".to_string(), "Doing".to_string()];
        app.world_mut()
            .commands()
            .spawn_scene(inline_field("Status", options, Vec::new(), Some(0)))
            .insert(ChildOf(body));
        settle(&mut app);
        let world = app.world_mut();
        let menu = world.query_filtered::<Entity, With<FeathersMenu>>().single(world).unwrap();
        let popup = world.query_filtered::<Entity, With<FeathersMenuPopup>>().single(world).unwrap();
        // The popup holds the keyboard, as it does once opened (a menu that has lost it closes itself).
        world.resource_mut::<InputFocus>().set(popup, FocusCause::Navigated);
        world.trigger(MenuEvent { source: menu, action: MenuAction::Toggle });
        settle(&mut app);
        let shown = |app: &App| *app.world().get::<Visibility>(popup).unwrap() != Visibility::Hidden;
        assert!(shown(&app), "the popup is open");
        escape(&mut app, popup);
        assert!(!shown(&app), "Esc closed the popup");
        assert!(app.world().get_entity(modal).is_ok(), "the modal is still open");
    }
}
