//! Text and choices edited where they are read: click, change, and Enter or blur saves, Esc puts it back.
//!
//! [`inline_text`] is a text (a task's title, its description) that turns into a text input when clicked. One line
//! saves on Enter; several lines take Enter as a new line. Both save on blur and cancel on Esc, and a value that
//! didn't change saves nothing. [`inline_field`] is a sidebar row, a label and a value that becomes the kit's
//! [`select`] when clicked (status, priority, grade).
//!
//! Neither saves anything itself. A change triggers [`InlineEdited`] (or [`InlineChosen`]) on the widget with the old
//! and the new value, and the widget shows the new value dimmed with "Saving…" until the caller answers on its
//! component: [`InlineText::confirm`] keeps the value, [`InlineText::fail`] puts the old one back and shows the error
//! beside it, and [`InlineText::fail_keeping`] does too but keeps what was typed for the next edit (a save that lost
//! to someone else's change, to try again over theirs). The caller decides what saving means, and may refuse.
//!
//! The kit's modal rules hold: an Esc that cancels an edit is not passed on, so it doesn't also close the modal the
//! text sits in. The hint shows while the value is empty, and in the input while it is empty during an edit (feathers'
//! input has no placeholder of its own; the hint is laid over it, as the kit's search box does).

use bevy::ecs::system::SystemParam;
use bevy::feathers::theme::ThemeTextColor;
use bevy::feathers::tokens;
use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::tab_navigation::NavAction;
use bevy::input_focus::{FocusCause, FocusLost, FocusedInput, InputFocus};
use bevy::picking::events::PointerClick;
use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::{MenuAction, MenuEvent};

use crate::input::text_input;
use crate::notice::{Notice, Tone, notice};
use crate::patch::{display, patch_node, set_text};
use crate::select::{Picked, SelectEdit, select};
use crate::spacing::{extent, space};
use crate::text::{size, text, text_dim};

/// Where an inline widget is in its edit.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum InlineMode {
    /// Showing the value.
    #[default]
    Reading,
    /// Showing the input (or the select).
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
    selected: Option<usize>,
    mode: InlineMode,
    error: String,
    /// The popup has been seen open, so it closing without a pick means the edit is over.
    opened: bool,
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

    /// Shows a choice that came from elsewhere, and new options. Ignored while it is being edited or saved.
    pub fn set(&mut self, options: Vec<String>, selected: Option<usize>) {
        if self.mode == InlineMode::Reading && (self.options != options || self.selected != selected) {
            self.options = options;
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

    fn shown(&self) -> &str {
        self.selected.and_then(|i| self.options.get(i)).map_or(NO_CHOICE, String::as_str)
    }
}

/// How far the hint over an empty input sits from its left edge, where the input's text starts (as in the search box).
const HINT_LEFT: f32 = 6.0;
/// What a field with nothing chosen reads.
const NO_CHOICE: &str = "None";
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

/// A text that becomes an input when clicked, `lines` high while editing (more than one allows newlines). `hint`
/// shows while it is empty.
pub fn inline_text(value: impl Into<String>, hint: impl Into<String>, lines: f32) -> impl Scene {
    inline_text_sized(value, hint, lines, size::BODY)
}

/// An [`inline_text`] read at `font_size` (one of [`size`]'s), such as a detail view's title.
pub fn inline_text_sized(value: impl Into<String>, hint: impl Into<String>, lines: f32, font_size: f32) -> impl Scene {
    let (value, hint) = (value.into(), hint.into());
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) } }
        InlineText { value: { value.clone() }, hint: { hint.clone() }, multiline: { lines > 1.0 } }
        Children [
            @shown(value, hint.clone(), font_size)
            --
            Node { display: { Display::None }, width: percent(100) }
            Part::Editor
            Children [
                @text_input(lines, bsn! { InlineInput })
                --
                @text_dim(hint, size::BODY)
                Part::EditHint
                Node { display: { Display::None }, position_type: { PositionType::Absolute }, left: { px(HINT_LEFT) } }
                Pickable::IGNORE
            ]
            --
            @status()
        ]
    }
}

/// A sidebar row: `label` and the chosen option, which becomes a [`select`] when clicked.
pub fn inline_field(label: impl Into<String>, options: Vec<String>, selected: Option<usize>) -> impl Scene {
    let label = label.into();
    let shown_text = selected.and_then(|i| options.get(i).cloned()).unwrap_or_else(|| NO_CHOICE.to_string());
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) } }
        InlineField { options: { options.clone() }, selected: { selected } }
        Children [
            Node { column_gap: { px(space::CONTROLS) }, align_items: AlignItems::Center }
            Children [
                @text_dim(label, size::SECONDARY)
                Node { width: { px(extent::FIELD_LABEL_WIDTH) }, flex_shrink: { 0.0_f32 } }
                --
                @shown(shown_text, String::new(), size::BODY)
                --
                Node { display: { Display::None } }
                Part::Editor
                Children [ @select(options, selected, FIELD_ROWS) ]
            ]
            --
            @status()
        ]
    }
}

/// The clickable value, and its hint while the value is empty.
fn shown(value: String, hint: String, font_size: f32) -> impl Scene {
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

/// A click on the value starts the edit: the input takes the text and the keyboard, or the select opens.
#[derive(SystemParam)]
struct Opening<'w, 's> {
    parents: Query<'w, 's, &'static ChildOf>,
    roots: Roots<'w, 's>,
    texts: Query<'w, 's, &'static mut InlineText>,
    fields: Query<'w, 's, &'static mut InlineField>,
    children: Query<'w, 's, &'static Children>,
    inputs: Query<'w, 's, &'static mut EditableText, With<InlineInput>>,
    selects: Query<'w, 's, (), With<crate::select::SelectOptions>>,
    focus: ResMut<'w, InputFocus>,
    commands: Commands<'w, 's>,
}

fn open_edit(click: On<PointerClick>, mut open: Opening) {
    if click.button != PointerButton::Primary {
        return;
    }
    begin_edit(click.entity, &mut open);
}

/// Starts the edit of the inline widget `start` is in.
fn begin_edit(start: Entity, open: &mut Opening) {
    let Some(root) = root_of(start, &open.parents, &open.roots) else { return };
    if let Ok(mut text) = open.texts.get_mut(root) {
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
    } else if let Ok(mut field) = open.fields.get_mut(root) {
        if field.mode != InlineMode::Reading {
            return;
        }
        field.mode = InlineMode::Editing;
        field.opened = false;
        field.error.clear();
        if let Some(select) = open.children.iter_descendants(root).find(|e| open.selects.contains(*e)) {
            open.commands.trigger(MenuEvent { source: select, action: MenuAction::Open(NavAction::First) });
        }
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
    let draft = save.then(|| editable.value().to_string());
    if let Some((old, new)) = text.finish(draft) {
        commands.trigger(InlineEdited { entity: root, old, new });
    }
    focus.clear();
}

/// Losing the keyboard saves a text being edited.
fn on_blur(
    lost: On<FocusLost>,
    inputs: Query<&EditableText, With<InlineInput>>,
    parents: Query<&ChildOf>,
    roots: Roots,
    mut texts: Query<&mut InlineText>,
    mut commands: Commands,
) {
    let start = lost.original_event_target();
    if lost.event_target() != start {
        return;
    }
    let Ok(editable) = inputs.get(start) else { return };
    let Some(root) = root_of(start, &parents, &roots) else { return };
    let Ok(mut text) = texts.get_mut(root) else { return };
    if let Some((old, new)) = text.finish(Some(editable.value().to_string())) {
        commands.trigger(InlineEdited { entity: root, old, new });
    }
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
    if field.mode != InlineMode::Editing {
        return;
    }
    if field.selected == Some(picked.index) {
        field.mode = InlineMode::Reading;
        return;
    }
    let old = field.selected.replace(picked.index);
    field.mode = InlineMode::Saving;
    commands.trigger(InlineChosen { entity: root, old, new: picked.index });
}

/// Draws each inline widget from its state, in place: shows the value or the editor, the hint, "Saving…" and the
/// error, and ends a field's edit when its popup closed without a pick. A widget is drawn when its state changed; one
/// being edited is looked at every frame too, since its input's text and its select's popup change without it.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn sync_inline(
    texts: Query<(Entity, Ref<InlineText>)>,
    mut fields: Query<(Entity, &mut InlineField)>,
    children: Query<&Children>,
    inputs: Query<Ref<EditableText>, With<InlineInput>>,
    mut parts: Query<(&Part, &mut Node, Option<&mut Text>, Option<&ThemeTextColor>)>,
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
    for (root, mut field) in &mut fields {
        let editing = field.mode == InlineMode::Editing;
        if !(field.is_changed() || editing || again.contains(&root)) {
            continue;
        }
        let select = children.iter_descendants(root).find(|e| selects.contains(*e));
        if let Some(select) = select {
            let open = select_edit.is_open(select);
            if editing {
                if open && !field.opened {
                    field.opened = true;
                } else if !open && field.opened {
                    field.mode = InlineMode::Reading;
                    field.opened = false;
                }
            }
            if field.mode != InlineMode::Editing {
                let (options, selected) = (field.options.clone(), field.selected);
                // A select that isn't ready yet takes the options on a later frame.
                if !select_edit.set(select, &options, selected) {
                    retry.push(root);
                }
            }
        }
        let view = View {
            mode: field.mode,
            value: field.shown(),
            hint: "",
            show_value: true,
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
    parts: &mut Query<(&Part, &mut Node, Option<&mut Text>, Option<&ThemeTextColor>)>,
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
                let wanted = if view.mode == InlineMode::Saving { tokens::TEXT_DIM } else { tokens::TEXT_MAIN };
                if color.is_none_or(|c| c.0 != wanted) {
                    commands.entity(part).insert(ThemeTextColor(wanted));
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

/// The behavior of inline widgets: the click, the keys, the blur and the picks.
pub struct InlinePlugin;

impl Plugin for InlinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputFocus>()
            .add_observer(on_key)
            .add_observer(on_blur)
            .add_observer(on_picked)
            .add_systems(Update, sync_inline.in_set(crate::UiPlace));
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;
    use bevy::input::ButtonState;
    use bevy::input::keyboard::Key;

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
    fn blur_saves_too() {
        let mut app = app();
        let (root, input) = text(&mut app, "a", 1.0);
        click(&mut app, root);
        type_in(&mut app, input, "b");
        blur(&mut app, input);
        assert_eq!(edits(&app), vec![("a".to_string(), "b".to_string())]);
        assert_eq!(mode(&app, root), InlineMode::Saving);
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
        blur(&mut app, input);
        assert_eq!(mode(&app, root), InlineMode::Reading);
        assert!(edits(&app).is_empty());
    }

    #[test]
    fn enter_in_a_multi_line_text_adds_a_line_and_blur_saves() {
        let mut app = app();
        let (root, input) = text(&mut app, "one", 4.0);
        assert!(app.world().get::<EditableText>(input).unwrap().allow_newlines);
        click(&mut app, root);
        enter(&mut app, input);
        assert_eq!(mode(&app, root), InlineMode::Editing);
        assert!(edits(&app).is_empty());
        type_in(&mut app, input, "one\ntwo");
        blur(&mut app, input);
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
        let root = app.world_mut().commands().spawn_scene(inline_field("Status", options, Some(0))).id();
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

    #[test]
    fn a_click_on_a_field_opens_its_select_and_a_different_pick_is_a_change() {
        let mut app = app();
        let root = field(&mut app);
        app.world_mut().run_system_once(move |mut open: Opening| begin_edit(root, &mut open)).unwrap();
        settle(&mut app);
        assert_eq!(app.world().get::<InlineField>(root).unwrap().mode(), InlineMode::Editing);
        assert_eq!(display_of(&mut app, Part::Editor), Display::Flex);
        pick(&mut app, root, 2);
        let choices = &app.world().resource::<Choices>().0;
        assert_eq!(choices.len(), 1);
        assert_eq!((choices[0].old, choices[0].new), (Some(0), 2));
        assert_eq!(app.world().get::<InlineField>(root).unwrap().mode(), InlineMode::Saving);
        app.world_mut().get_mut::<InlineField>(root).unwrap().fail(Some(0), "Not allowed");
        settle(&mut app);
        assert_eq!(app.world().get::<InlineField>(root).unwrap().selected(), Some(0));
    }

    #[test]
    fn picking_the_same_option_emits_nothing() {
        let mut app = app();
        let root = field(&mut app);
        app.world_mut().run_system_once(move |mut open: Opening| begin_edit(root, &mut open)).unwrap();
        pick(&mut app, root, 0);
        assert!(app.world().resource::<Choices>().0.is_empty());
        assert_eq!(app.world().get::<InlineField>(root).unwrap().mode(), InlineMode::Reading);
    }

    fn value_color(app: &mut App) -> Option<bevy::feathers::theme::ThemeToken> {
        let world = app.world_mut();
        let mut q = world.query::<(&Part, Option<&ThemeTextColor>)>();
        q.iter(world).find(|(p, _)| **p == Part::Value).and_then(|(_, c)| c.map(|c| c.0.clone()))
    }

    #[test]
    fn a_value_being_saved_is_dimmed_until_the_caller_answers() {
        let mut app = app();
        let (root, input) = text(&mut app, "Fix the board", 1.0);
        assert_eq!(value_color(&mut app), Some(tokens::TEXT_MAIN));
        click(&mut app, root);
        type_in(&mut app, input, "Fix the card");
        enter(&mut app, input);
        assert_eq!(mode(&app, root), InlineMode::Saving);
        assert_eq!(value_color(&mut app), Some(tokens::TEXT_DIM));
        app.world_mut().get_mut::<InlineText>(root).unwrap().confirm();
        settle(&mut app);
        assert_eq!(value_color(&mut app), Some(tokens::TEXT_MAIN));
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
        app.world_mut().commands().spawn_scene(inline_field("Priority", vec!["P1".into(), "P2".into()], Some(1)));
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
}
