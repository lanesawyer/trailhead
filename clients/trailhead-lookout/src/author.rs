//! Authoring on the task board (`docs/factory/ui.md#views`): create a task (under the factory's queue task by
//! default), edit its title, priority, status and description, comment, and answer an open question.
//!
//! The form is a [`Form`] resource: what is open, the [`Draft`] being typed and the message from the last
//! attempt. [`request`] turns it into the one REST request that saves it; it is pure, so it is tested without a
//! window. Saving sends a [`Submit`] with the form's panel as the source, and the form closes only on `Ok`; any
//! other [`Outcome`] stays on the form, where the person can fix it and save again. Nothing is applied locally: the
//! event stream brings the saved task to the board.
//!
//! [`AuthorUiPlugin`] draws the form with feathers. The description and comment boxes are feathers' text input
//! with `allow_newlines` and `visible_lines` set, which is all multiline takes.

use bevy::feathers::controls::ButtonVariant;
use bevy::feathers::theme::ThemeBackgroundColor;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;
use reqwest::Method;
use serde_json::{Value, json};
use trailhead_model::{Grade, Priority, Status, Task};

use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::Connection;
use crate::mirror::Mirrored;
use crate::widgets::{
    Modal, ModalClosed, ModalLocked, ModalSlots, Picked, button, dot, field, open_modal, priority_tone, row, section,
    select, size, space, status_label, text, text_dim, text_input, token,
};

/// The priority the form starts a new task at.
const NEW_PRIORITY: Priority = Priority::new(2).unwrap();

/// The queue task new tasks go under unless the form says otherwise (`queue` in `.trailhead.toml`).
pub const DEFAULT_QUEUE: &str = "TRAIL-102";

/// What a task's grade can be set to, in the order of the dropdown: none, then 1 to 4.
pub fn grade_choices() -> impl Iterator<Item = Option<Grade>> {
    std::iter::once(None).chain(Grade::ALL.map(Some))
}

/// The dropdown's rows for [`grade_choices`]: each grade with what it is like (`docs/factory/review.md#grades`), and where
/// Treeline falls (above grade 2 by default).
pub fn grade_options() -> Vec<String> {
    [
        "none (the factory skips it)",
        "1 · a walk on a trail",
        "2 · off-trail",
        "3 · scrambling, above Treeline",
        "4 · exposed, above Treeline",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The row of [`grade_options`] that shows `grade`.
pub fn grade_index(grade: Option<Grade>) -> Option<usize> {
    grade_choices().position(|g| g == grade)
}

#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct Authoring {
    /// The key new tasks are made under.
    pub queue: String,
}

impl Default for Authoring {
    fn default() -> Self {
        Authoring { queue: DEFAULT_QUEUE.to_string() }
    }
}

/// What the form is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Create,
    /// The task as it was when the form opened: `version` goes out as `If-Match`.
    Edit {
        key: String,
        version: i64,
    },
    Comment {
        key: String,
    },
    Answer {
        key: String,
        comment_id: String,
        question: String,
    },
}

/// The fields of the form. Which ones show depends on the [`Target`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub description: String,
    pub priority: Priority,
    pub status: Status,
    /// None for a task the factory won't pick up.
    pub grade: Option<Grade>,
    /// The parent's key when creating; empty for none.
    pub parent: String,
    /// A comment or an answer.
    pub body: String,
}

impl Default for Draft {
    fn default() -> Self {
        Draft {
            title: String::new(),
            description: String::new(),
            priority: NEW_PRIORITY,
            status: Status::Open,
            grade: None,
            parent: String::new(),
            body: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    pub target: Target,
    /// What the form started with, so an edit sends only what changed.
    pub original: Draft,
    pub draft: Draft,
}

impl Open {
    fn new(target: Target, draft: Draft) -> Open {
        Open { target, original: draft.clone(), draft }
    }

    /// A new task under `queue`.
    pub fn create(queue: &str) -> Open {
        Open::new(Target::Create, Draft { parent: queue.to_string(), ..Draft::default() })
    }

    /// An edit of `task`, a task as the mirror holds it.
    pub fn edit(task: &Task) -> Open {
        let target = Target::Edit { key: task.key.clone(), version: task.version };
        let draft = Draft {
            title: task.title.clone(),
            description: task.description.clone(),
            priority: task.priority,
            status: task.status,
            grade: task.grade,
            ..Draft::default()
        };
        Open::new(target, draft)
    }

    /// What the modal's heading says.
    pub fn title(&self) -> String {
        match &self.target {
            Target::Create => "New task".to_string(),
            Target::Edit { key, .. } => format!("Edit {key}"),
            Target::Comment { key } => format!("Comment on {key}"),
            Target::Answer { key, .. } => format!("Answer on {key}"),
        }
    }

    pub fn comment(key: &str) -> Open {
        Open::new(Target::Comment { key: key.to_string() }, Draft::default())
    }

    pub fn answer(key: &str, comment_id: &str, question: &str) -> Open {
        let target =
            Target::Answer { key: key.to_string(), comment_id: comment_id.to_string(), question: question.to_string() };
        Open::new(target, Draft::default())
    }
}

/// The project a key belongs to: `TRAIL` for `TRAIL-102`.
fn project_of(key: &str) -> Option<&str> {
    key.rsplit_once('-').map(|(project, _)| project).filter(|p| !p.is_empty())
}

/// The request that saves `open`, or why it can't be sent yet.
pub fn request(open: &Open) -> Result<Request, String> {
    let d = &open.draft;
    match &open.target {
        Target::Create => {
            let title = d.title.trim();
            if title.is_empty() {
                return Err("a task needs a title".into());
            }
            let parent = d.parent.trim();
            // A task's project is its parent's, so a new one needs a parent to know where it goes.
            let Some(project) = project_of(parent) else {
                return Err("give the parent task's key, e.g. TRAIL-102".into());
            };
            let mut body = json!({
                "project": project,
                "title": title,
                "description": d.description,
                "priority": d.priority,
                "parent": parent,
            });
            if let Some(grade) = d.grade {
                body["grade"] = json!(grade);
            }
            Ok(Request::new(Method::POST, "/v1/tasks", Some(body), None))
        }
        Target::Edit { key, version } => {
            let o = &open.original;
            let mut patch = serde_json::Map::new();
            if d.title.trim().is_empty() {
                return Err("a task needs a title".into());
            }
            if d.title.trim() != o.title {
                patch.insert("title".into(), json!(d.title.trim()));
            }
            if d.description != o.description {
                patch.insert("description".into(), json!(d.description));
            }
            if d.priority != o.priority {
                patch.insert("priority".into(), json!(d.priority));
            }
            if d.status != o.status {
                patch.insert("status".into(), json!(d.status));
            }
            if d.grade != o.grade {
                // `null` clears it.
                patch.insert("grade".into(), json!(d.grade));
            }
            if patch.is_empty() {
                return Err("nothing changed".into());
            }
            Ok(Request::new(Method::PATCH, format!("/v1/tasks/{key}"), Some(Value::Object(patch)), Some(*version)))
        }
        Target::Comment { key } => {
            let body = d.body.trim();
            if body.is_empty() {
                return Err("write the comment first".into());
            }
            Ok(Request::new(
                Method::POST,
                format!("/v1/tasks/{key}/comments"),
                Some(json!({"kind": "note", "body": body})),
                None,
            ))
        }
        Target::Answer { comment_id, .. } => {
            let body = d.body.trim();
            if body.is_empty() {
                return Err("write the answer first".into());
            }
            Ok(Request::new(
                Method::POST,
                format!("/v1/comments/{comment_id}/resolve"),
                Some(json!({"answer": body})),
                None,
            ))
        }
    }
}

/// What to tell the person about an answer that wasn't `Ok`.
pub fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ok { .. } => String::new(),
        Outcome::Conflict { message, .. } => {
            format!("The task changed since you opened it ({message}). Close this and open it again.")
        }
        Outcome::Forbidden(m) => format!("Not allowed: {m}"),
        Outcome::RateLimited { retry_after, message } => match retry_after {
            Some(after) => format!("Slow down ({message}); try again in {}s.", after.as_secs()),
            None => format!("Slow down ({message})."),
        },
        Outcome::Failed(m) => m.clone(),
    }
}

/// The form on screen. `generation` changes whenever it opens or closes, which is when the panel is redrawn
/// (redrawing for every keystroke would lose the cursor).
#[derive(Resource, Default, Debug)]
pub struct Form {
    pub open: Option<Open>,
    /// The last refusal, or what is in progress.
    pub message: String,
    /// The save awaiting its answer, so it can't be sent twice.
    pub saving: bool,
    pub generation: u64,
}

impl Form {
    pub fn open(&mut self, open: Open) {
        self.open = Some(open);
        self.close_state();
    }

    pub fn close(&mut self) {
        self.open = None;
        self.close_state();
    }

    fn close_state(&mut self) {
        self.message.clear();
        self.saving = false;
        self.generation += 1;
    }
}

/// What a button does.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub enum Act {
    #[default]
    New,
    Edit(String),
    Comment(String),
    Answer {
        key: String,
        comment_id: String,
        question: String,
    },
    Save,
    Cancel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FieldKind {
    #[default]
    Title,
    Description,
    Parent,
    Body,
}

/// A text input of the form, which draft field it holds, and the form generation it was drawn for
/// (inputs of a form being replaced are despawned late and must not feed the new draft).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Field(pub FieldKind, pub u64);

/// Text to put in an input when it appears.
#[derive(Component, Debug, Clone, Default)]
pub struct InitialText(pub String);

/// The dot beside the priority dropdown, tinted with the draft's priority.
#[derive(Component, Debug, Clone, Copy, Default)]
struct PriorityDot;

/// Which draft field a dropdown of the form sets.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormChoice {
    #[default]
    Priority,
    Status,
    Grade,
}

/// The text showing the form's last message.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct FormMessage;

/// Lines tall the spec's box is, and a comment's or answer's.
const SPEC_LINES: f32 = 12.0;
const BODY_LINES: f32 = 6.0;

/// The form's modal, and the source of its commands. `title` is what the modal's heading says.
#[derive(Component, Default)]
pub struct FormPanel {
    pub title: String,
}

impl Modal for FormPanel {
    fn title(&self) -> String {
        self.title.clone()
    }
}

fn field_value(draft: &mut Draft, kind: FieldKind) -> &mut String {
    match kind {
        FieldKind::Title => &mut draft.title,
        FieldKind::Description => &mut draft.description,
        FieldKind::Parent => &mut draft.parent,
        FieldKind::Body => &mut draft.body,
    }
}

#[allow(clippy::too_many_arguments)]
fn on_act(
    activate: On<Activate>,
    acts: Query<&Act>,
    mut form: ResMut<Form>,
    authoring: Res<Authoring>,
    connection: Res<State<Connection>>,
    tasks: Query<&Mirrored<Task>>,
    panel: Query<Entity, With<FormPanel>>,
    mut submit: MessageWriter<Submit>,
) {
    let Ok(act) = acts.get(activate.entity) else { return };
    // Opening a form is allowed offline (the board is read-only then, so its buttons are greyed out), but nothing
    // is sent until the connection is live again.
    match act {
        Act::New => form.open(Open::create(&authoring.queue)),
        Act::Edit(key) => {
            if let Some(task) = tasks.iter().find(|t| t.0.key == *key) {
                form.open(Open::edit(&task.0));
            }
        }
        Act::Comment(key) => form.open(Open::comment(key)),
        Act::Answer { key, comment_id, question } => form.open(Open::answer(key, comment_id, question)),
        // A save in flight can't be withdrawn, so the form stays until it is answered.
        Act::Cancel if !form.saving => form.close(),
        Act::Cancel => {}
        Act::Save => {
            let Some(open) = &form.open else { return };
            if form.saving {
                return;
            }
            if *connection.get() != Connection::Live {
                form.message = "Not connected to the server; saving waits for Live.".into();
                return;
            }
            match request(open) {
                Err(why) => form.message = why,
                Ok(request) => {
                    let Ok(source) = panel.single() else { return };
                    submit.write(Submit { source, request });
                    form.saving = true;
                    form.message = "Saving...".into();
                }
            }
        }
    }
}

/// Copies what is typed into the draft.
fn read_fields(mut form: ResMut<Form>, fields: Query<(&Field, &EditableText)>) {
    let Some(open) = &form.open else { return };
    let mut draft = open.draft.clone();
    for (Field(kind, generation), text) in &fields {
        if *generation != form.generation {
            continue;
        }
        *field_value(&mut draft, *kind) = text.value().to_string();
    }
    if draft != open.draft {
        // Typing is not a new form, so only the draft is touched.
        form.open.as_mut().expect("checked above").draft = draft;
    }
}

fn init_text(mut inputs: Query<(&InitialText, &mut EditableText), Added<InitialText>>) {
    for (InitialText(initial), mut text) in &mut inputs {
        text.editor_mut().set_text(initial);
        text.queue_edit(bevy::text::TextEdit::TextEnd(false));
    }
}

/// The save's answer: `Ok` closes the form, anything else stays on it.
fn finish(mut done: MessageReader<CommandDone>, panel: Query<Entity, With<FormPanel>>, mut form: ResMut<Form>) {
    for d in done.read() {
        if !panel.contains(d.source) || !form.saving {
            continue;
        }
        if matches!(d.outcome, Outcome::Ok { .. }) {
            form.close();
        } else {
            form.saving = false;
            form.message = describe(&d.outcome);
        }
    }
}

/// The logic of authoring, without drawing.
pub struct AuthorPlugin;

impl Plugin for AuthorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Form>()
            .init_resource::<Authoring>()
            .add_observer(on_act)
            .add_observer(on_pick)
            .add_observer(on_modal_closed)
            .add_systems(Update, (init_text, read_fields, finish).chain());
    }
}

/// Draws the form over the board.
pub struct AuthorUiPlugin;

impl Plugin for AuthorUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(AuthorPlugin);
        add_drawing(app);
    }
}

/// The systems that draw the form, apart from the plugin so a test can add them to an app that has the logic.
fn add_drawing(app: &mut App) {
    app.add_systems(Update, (redraw_form, fill_form, lock_while_saving, update_message).chain());
}

/// A button for `act`: a normal one, with no icon.
pub fn act_button(label: String, act: Act) -> impl Scene {
    bsn! {
        @button(label, None, ButtonVariant::Normal)
        act
    }
}

/// A text input of the form for `kind`, `lines` high, holding `initial` when it appears.
fn input(kind: FieldKind, generation: u64, initial: String, lines: f32) -> impl SceneList {
    bsn_list! { @text_input(lines, bsn! { Field(kind, generation) InitialText(initial) }) }
}

/// A text field of the form: `label` over its input.
fn text_field(commands: &mut Commands, parent: Entity, label: &str, input: impl SceneList) {
    commands.queue_spawn_scene(field(label.to_string(), input)).insert(ChildOf(parent));
}

/// A dropdown of the form for `choice`, labelled, with `selected` showing.
fn choice_field(
    commands: &mut Commands,
    parent: Entity,
    label: &str,
    choice: FormChoice,
    options: Vec<String>,
    selected: Option<usize>,
) {
    let max_visible = options.len();
    let control: Box<dyn SceneList> = match choice {
        FormChoice::Priority => {
            let tone = selected.and_then(|i| Priority::ALL.get(i)).map_or(token::BADGE_BG, |p| priority_tone(*p));
            Box::new(bsn_list! {
                Node { align_items: AlignItems::Center, column_gap: { px(space::ICON_LABEL) } }
                Children [
                    @dot(tone) PriorityDot
                    -- @select(options, selected, max_visible) FormChoice::Priority
                ]
            })
        }
        FormChoice::Status => Box::new(bsn_list! { @select(options, selected, max_visible) FormChoice::Status }),
        FormChoice::Grade => Box::new(bsn_list! { @select(options, selected, max_visible) FormChoice::Grade }),
    };
    commands.queue_spawn_scene(field(label.to_string(), control)).insert(ChildOf(parent));
}

/// Sets the draft's priority or status from the dropdown that was used.
fn on_pick(
    picked: On<Picked>,
    choices: Query<&FormChoice>,
    dots: Query<Entity, With<PriorityDot>>,
    mut form: ResMut<Form>,
    mut commands: Commands,
) {
    let Ok(choice) = choices.get(picked.entity) else { return };
    let Some(open) = &mut form.open else { return };
    match choice {
        FormChoice::Priority => {
            if let Some(priority) = Priority::ALL.get(picked.index) {
                open.draft.priority = *priority;
                for dot in &dots {
                    commands.entity(dot).insert(ThemeBackgroundColor(priority_tone(*priority)));
                }
            }
        }
        FormChoice::Status => {
            if let Some(status) = Status::ALL.get(picked.index) {
                open.draft.status = *status;
            }
        }
        FormChoice::Grade => {
            if let Some(grade) = grade_choices().nth(picked.index) {
                open.draft.grade = grade;
            }
        }
    }
}

fn redraw_form(
    form: Res<Form>,
    forms: Query<Entity, With<FormPanel>>,
    mut commands: Commands,
    mut drawn: Local<u64>,
    mut first: Local<bool>,
) {
    if *first && *drawn == form.generation {
        return;
    }
    *first = true;
    *drawn = form.generation;
    for e in &forms {
        commands.entity(e).despawn();
    }
    let Some(open) = &form.open else { return };
    open_modal(&mut commands, FormPanel { title: open.title() });
}

/// Fills a new form's modal: the fields in its body, the buttons and message in its footer.
fn fill_form(new: Query<&ModalSlots, (With<FormPanel>, Added<ModalSlots>)>, form: Res<Form>, mut commands: Commands) {
    let Some(open) = &form.open else { return };
    let d = &open.draft;
    let generation = form.generation;
    let c = &mut commands;
    for slots in &new {
        let body = slots.body;
        match &open.target {
            Target::Create | Target::Edit { .. } => {
                let creating = open.target == Target::Create;
                if creating {
                    text_field(c, body, "Parent", input(FieldKind::Parent, generation, d.parent.clone(), 1.0));
                }
                text_field(c, body, "Title", input(FieldKind::Title, generation, d.title.clone(), 1.0));
                let line = c.spawn_scene(bsn! { @row() }).insert(ChildOf(body)).id();
                let priorities = Priority::ALL.iter().map(|p| format!("P{p}")).collect();
                let priority = Priority::ALL.iter().position(|p| *p == d.priority);
                choice_field(c, line, "Priority", FormChoice::Priority, priorities, priority);
                if !creating {
                    let statuses = Status::ALL.iter().map(|s| status_label(*s).to_string()).collect();
                    let status = Status::ALL.iter().position(|s| *s == d.status);
                    choice_field(c, line, "Status", FormChoice::Status, statuses, status);
                }
                choice_field(c, line, "Grade", FormChoice::Grade, grade_options(), grade_index(d.grade));
                let spec = input(FieldKind::Description, generation, d.description.clone(), SPEC_LINES);
                text_field(c, body, "Spec", spec);
            }
            Target::Comment { .. } | Target::Answer { .. } => {
                if let Target::Answer { question, .. } = &open.target {
                    // The question wraps to the dialog's width and is styled like the rest of the kit's text.
                    let quoted = c.spawn_scene(bsn! { @section("Question") }).insert(ChildOf(body)).id();
                    c.spawn_scene(text(question.clone(), size::BODY)).insert(ChildOf(quoted));
                }
                let label = if matches!(open.target, Target::Answer { .. }) { "Answer" } else { "Comment" };
                text_field(c, body, label, input(FieldKind::Body, generation, String::new(), BODY_LINES));
            }
        }
        let footer = slots.footer;
        c.spawn_scene(bsn! { @button("Save", None, ButtonVariant::Primary) Act::Save }).insert(ChildOf(footer));
        c.spawn_scene(bsn! { @button("Cancel", None, ButtonVariant::Normal) Act::Cancel }).insert(ChildOf(footer));
        c.spawn_scene(text_dim(form.message.clone(), size::SECONDARY)).insert((FormMessage, ChildOf(footer)));
    }
}

/// A close request (the X, Escape, a click outside) closes the form too; the modal is already going.
fn on_modal_closed(closed: On<ModalClosed>, forms: Query<(), With<FormPanel>>, mut form: ResMut<Form>) {
    if forms.contains(closed.modal) {
        form.close();
    }
}

/// A save in flight can't be withdrawn, so the form's modal stays open until it is answered.
fn lock_while_saving(form: Res<Form>, forms: Query<Entity, With<FormPanel>>, mut commands: Commands) {
    if !form.is_changed() {
        return;
    }
    for e in &forms {
        if form.saving {
            commands.entity(e).insert(ModalLocked);
        } else {
            commands.entity(e).remove::<ModalLocked>();
        }
    }
}

fn update_message(form: Res<Form>, mut messages: Query<&mut Text, With<FormMessage>>) {
    if !form.is_changed() {
        return;
    }
    for mut text in &mut messages {
        if text.0 != form.message {
            text.0.clone_from(&form.message);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::state::app::StatesPlugin;

    use super::*;
    use crate::commands::{CommandId, CommandsPlugin, Outbox};
    use crate::fixture::{sid, task_json};
    use crate::mirror::MirrorPlugin;
    use crate::sync::SyncItem;

    fn task() -> Value {
        task_json(json!({"id": "t1", "key": "TRAIL-7", "title": "old", "description": "a\nb", "priority": 2,
               "status": "open", "version": 4}))
    }

    fn edit(task: &Value) -> Open {
        Open::edit(&serde_json::from_value(task.clone()).unwrap())
    }

    fn body(r: &Request) -> &Value {
        r.body.as_ref().unwrap()
    }

    #[test]
    fn create_goes_under_the_parent_and_takes_its_project() {
        let mut open = Open::create("TRAIL-102");
        assert_eq!(request(&open).unwrap_err(), "a task needs a title");
        open.draft.title = " Do it ".into();
        open.draft.description = "line 1\nline 2".into();
        open.draft.priority = Priority::ALL[0];
        let r = request(&open).unwrap();
        assert_eq!((r.method.clone(), r.path.as_str(), r.if_match), (Method::POST, "/v1/tasks", None));
        assert_eq!(
            body(&r),
            &json!({"project": "TRAIL", "title": "Do it", "description": "line 1\nline 2", "priority": 1,
                    "parent": "TRAIL-102"})
        );
        open.draft.parent = "nope".into();
        assert!(request(&open).unwrap_err().contains("parent"));
    }

    #[test]
    fn edit_sends_only_what_changed_with_the_version_it_saw() {
        let mut open = edit(&task());
        assert_eq!(request(&open).unwrap_err(), "nothing changed");
        open.draft.description = "a\nb\nc".into();
        open.draft.status = Status::Cancelled;
        let r = request(&open).unwrap();
        assert_eq!((r.method.clone(), r.path.as_str(), r.if_match), (Method::PATCH, "/v1/tasks/TRAIL-7", Some(4)));
        assert_eq!(body(&r), &json!({"description": "a\nb\nc", "status": "cancelled"}));
        open.draft.title = "  ".into();
        assert_eq!(request(&open).unwrap_err(), "a task needs a title");
    }

    #[test]
    fn comments_and_answers_need_a_body() {
        let mut open = Open::comment("TRAIL-7");
        assert!(request(&open).is_err());
        open.draft.body = "hello\nthere".into();
        let r = request(&open).unwrap();
        assert_eq!(r.path, "/v1/tasks/TRAIL-7/comments");
        assert_eq!(body(&r), &json!({"kind": "note", "body": "hello\nthere"}));

        let mut open = Open::answer("TRAIL-7", "c9", "which db?");
        assert!(request(&open).is_err());
        open.draft.body = "sqlite".into();
        let r = request(&open).unwrap();
        assert_eq!((r.method.clone(), r.path.as_str()), (Method::POST, "/v1/comments/c9/resolve"));
        assert_eq!(body(&r), &json!({"answer": "sqlite"}));
    }

    /// Picks `index` in the form's dropdown for `choice`, as a click on its row does.
    fn pick(app: &mut App, choice: FormChoice, index: usize) {
        let select = app.world_mut().spawn(choice).id();
        app.world_mut().trigger(Picked { entity: select, index });
        app.update();
    }

    #[test]
    fn the_dropdowns_set_the_priority_and_the_status() {
        let mut app = app();
        press(&mut app, Act::Edit("TRAIL-7".into()));
        pick(&mut app, FormChoice::Priority, 0);
        pick(&mut app, FormChoice::Status, 1);
        let draft = app.world().resource::<Form>().open.as_ref().unwrap().draft.clone();
        assert_eq!((draft.priority, draft.status), (Priority::ALL[0], Status::InProgress));
        // A row past the end of the options changes nothing.
        pick(&mut app, FormChoice::Priority, 99);
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.priority, Priority::ALL[0]);
    }

    #[test]
    fn the_grade_is_sent_on_create_and_edit_and_null_clears_it() {
        let mut open = Open::create("TRAIL-102");
        open.draft.title = "Do it".into();
        assert!(body(&request(&open).unwrap()).get("grade").is_none());
        open.draft.grade = Grade::new(2);
        assert_eq!(body(&request(&open).unwrap())["grade"], json!(2));

        let mut graded = task();
        graded["grade"] = json!(3);
        let mut open = edit(&graded);
        assert_eq!(request(&open).unwrap_err(), "nothing changed");
        open.draft.grade = Grade::new(4);
        let r = request(&open).unwrap();
        assert_eq!((r.path.as_str(), r.if_match), ("/v1/tasks/TRAIL-7", Some(4)));
        assert_eq!(body(&r), &json!({"grade": 4}));
        open.draft.grade = None;
        assert_eq!(body(&request(&open).unwrap()), &json!({"grade": null}));
    }

    #[test]
    fn the_grade_dropdown_sets_the_draft() {
        let mut app = app();
        press(&mut app, Act::Edit("TRAIL-7".into()));
        pick(&mut app, FormChoice::Grade, 3);
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.grade, Grade::new(3));
        pick(&mut app, FormChoice::Grade, 0);
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.grade, None);
        assert_eq!(grade_options().len(), grade_choices().count());
    }

    #[test]
    fn the_priority_dot_follows_the_picked_priority() {
        let mut app = app();
        press(&mut app, Act::Edit("TRAIL-7".into()));
        let dot = app.world_mut().spawn((PriorityDot, ThemeBackgroundColor(token::BADGE_BG))).id();
        pick(&mut app, FormChoice::Priority, 0);
        assert_eq!(app.world().get::<ThemeBackgroundColor>(dot).unwrap().0, token::PRIORITY_1);
        pick(&mut app, FormChoice::Priority, 3);
        assert_eq!(app.world().get::<ThemeBackgroundColor>(dot).unwrap().0, token::PRIORITY_4);
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin)).init_state::<Connection>().add_plugins((
            MirrorPlugin,
            CommandsPlugin,
            AuthorPlugin,
        ));
        app.world_mut().resource_mut::<Outbox>().backoff = Duration::ZERO;
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 1, "entity": "task", "entity_id": sid("t1"), "kind": "task.updated", "after": task(),
        })));
        app.update();
        app
    }

    fn press(app: &mut App, act: Act) {
        let button = app.world_mut().spawn(act).id();
        app.world_mut().trigger(Activate { entity: button });
        app.update();
    }

    fn go_live(app: &mut App) {
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.update();
    }

    #[test]
    fn a_form_opens_saves_and_closes_on_ok_but_stays_on_a_refusal() {
        let mut app = app();
        let panel = app.world_mut().spawn(FormPanel::default()).id();
        go_live(&mut app);

        press(&mut app, Act::Edit("TRAIL-7".into()));
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.title, "old");

        // Nothing changed: refused on the form, nothing sent.
        press(&mut app, Act::Save);
        assert_eq!(app.world().resource::<Form>().message, "nothing changed");
        assert!(app.world().resource::<Outbox>().is_empty());

        pick(&mut app, FormChoice::Status, 1);
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.status, Status::InProgress);
        press(&mut app, Act::Save);
        assert!(app.world().resource::<Form>().saving);
        assert_eq!(app.world().resource::<Outbox>().len(), 1);
        // Pressing save again while it is out does not send twice.
        press(&mut app, Act::Save);
        assert_eq!(app.world().resource::<Outbox>().len(), 1);

        let done = |outcome| CommandDone { source: panel, id: CommandId(1), outcome };
        app.world_mut().write_message(done(Outcome::Conflict { current_version: Some(5), message: "stale".into() }));
        app.update();
        let form = app.world().resource::<Form>();
        assert!(form.open.is_some() && !form.saving);
        assert!(form.message.contains("stale"));

        app.world_mut().write_message(done(Outcome::Ok { version: Some(5) }));
        app.update();
        // Not saving any more, so this answer is ignored: the form stays until a save is out again.
        assert!(app.world().resource::<Form>().open.is_some());
        press(&mut app, Act::Save);
        app.world_mut().write_message(done(Outcome::Ok { version: Some(5) }));
        app.update();
        assert!(app.world().resource::<Form>().open.is_none());
    }

    #[test]
    fn saving_while_offline_says_so_and_sends_nothing() {
        let mut app = app();
        app.world_mut().spawn(FormPanel::default());
        press(&mut app, Act::Comment("TRAIL-7".into()));
        app.world_mut().resource_mut::<Form>().open.as_mut().unwrap().draft.body = "hi".into();
        press(&mut app, Act::Save);
        let form = app.world().resource::<Form>();
        assert!(form.message.contains("Not connected") && !form.saving);
        assert!(app.world().resource::<Outbox>().is_empty());
    }

    #[test]
    fn new_opens_under_the_queue_task() {
        let mut app = app();
        app.world_mut().resource_mut::<Authoring>().queue = "TRAIL-685".into();
        press(&mut app, Act::New);
        let open = app.world().resource::<Form>().open.clone().unwrap();
        assert_eq!((open.target, open.draft.parent.as_str()), (Target::Create, "TRAIL-685"));
        press(&mut app, Act::Cancel);
        assert!(app.world().resource::<Form>().open.is_none());
    }

    #[test]
    fn typed_text_reaches_the_draft() {
        let mut app = app();
        press(&mut app, Act::Comment("TRAIL-7".into()));
        let generation = app.world().resource::<Form>().generation;
        let mut text = EditableText::new("one\ntwo");
        text.allow_newlines = true;
        app.world_mut().spawn((Field(FieldKind::Body, generation), text));
        app.update();
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.body, "one\ntwo");
    }

    #[test]
    fn fields_of_a_replaced_form_do_not_feed_the_new_draft() {
        let mut app = app();
        press(&mut app, Act::Comment("TRAIL-1".into()));
        let old = app.world().resource::<Form>().generation;
        let mut text = EditableText::new("old form text");
        text.allow_newlines = true;
        app.world_mut().spawn((Field(FieldKind::Body, old), text));
        // Switch forms while the old panel's inputs still exist.
        press(&mut app, Act::Comment("TRAIL-2".into()));
        let open = app.world().resource::<Form>().open.clone().unwrap();
        assert_eq!(open.target, Target::Comment { key: "TRAIL-2".into() });
        assert_eq!(open.draft.body, "");
    }

    /// The form with its drawing, headless: scenes need the asset and scene plugins, and the kit's modal its observer.
    fn ui_app() -> App {
        let mut app = app();
        app.add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_plugins(crate::widgets::ModalPlugin);
        add_drawing(&mut app);
        app
    }

    fn modals(app: &mut App) -> usize {
        app.world_mut().query_filtered::<(), With<crate::widgets::ModalRoot>>().iter(app.world()).count()
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    #[test]
    fn the_forms_status_dropdown_lists_every_status_in_words() {
        let mut app = ui_app();
        press(&mut app, Act::Edit("TRAIL-7".into()));
        settle(&mut app);
        let world = app.world_mut();
        let shown: Vec<String> = world.query::<&Text>().iter(world).map(|t| t.0.clone()).collect();
        for status in Status::ALL {
            let word = status_label(status).to_string();
            assert!(shown.contains(&word), "{word} in {shown:?}");
        }
        assert!(!shown.iter().any(|s| Status::ALL.iter().any(|st| st.as_str() == s) && s.contains('_')), "{shown:?}");
    }

    #[test]
    fn every_form_opens_one_modal_and_closing_it_closes_the_form() {
        let acts = [
            Act::New,
            Act::Edit("TRAIL-7".into()),
            Act::Comment("TRAIL-7".into()),
            Act::Answer { key: "TRAIL-7".into(), comment_id: "c1".into(), question: "which db?".into() },
        ];
        for act in acts {
            // Cancel, then the modal's own ways out: Escape and a click outside both send `RequestClose` for the
            // backdrop, and the X sends it from inside.
            for from_backdrop in [None, Some(true), Some(false)] {
                let mut app = ui_app();
                settle(&mut app);
                press(&mut app, act.clone());
                settle(&mut app);
                assert_eq!(modals(&mut app), 1, "{act:?}");
                let root = app.world_mut().query_filtered::<Entity, With<FormPanel>>().single(app.world()).unwrap();
                match from_backdrop {
                    None => press(&mut app, Act::Cancel),
                    Some(backdrop) => {
                        let slots = *app.world().get::<ModalSlots>(root).unwrap();
                        let source = if backdrop { root } else { slots.footer };
                        app.world_mut().trigger(bevy::ui_widgets::RequestClose { source });
                    }
                }
                settle(&mut app);
                assert_eq!(modals(&mut app), 0, "{act:?} {from_backdrop:?}");
                assert!(app.world().resource::<Form>().open.is_none());
            }
        }
    }

    #[test]
    fn a_form_with_a_save_in_flight_ignores_a_click_outside() {
        let mut app = ui_app();
        press(&mut app, Act::Comment("TRAIL-7".into()));
        settle(&mut app);
        app.world_mut().resource_mut::<Form>().saving = true;
        settle(&mut app);
        let root = app.world_mut().query_filtered::<Entity, With<FormPanel>>().single(app.world()).unwrap();
        app.world_mut().trigger(bevy::ui_widgets::RequestClose { source: root });
        settle(&mut app);
        assert_eq!(modals(&mut app), 1);
        assert!(app.world().resource::<Form>().open.is_some());
    }

    #[test]
    fn cancel_is_ignored_while_saving() {
        let mut app = app();
        press(&mut app, Act::Comment("TRAIL-7".into()));
        app.world_mut().resource_mut::<Form>().saving = true;
        press(&mut app, Act::Cancel);
        assert!(app.world().resource::<Form>().open.is_some());
    }
}
