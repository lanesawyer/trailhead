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

use bevy::feathers::controls::{FeathersButton, FeathersTextInput, FeathersTextInputContainer};
use bevy::feathers::display::caption;
use bevy::prelude::*;
use bevy::text::{EditableText, LineBreak, TextLayout};
use bevy::ui_widgets::Activate;
use reqwest::Method;
use serde_json::{Value, json};

use crate::board::STATUSES;
use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::Connection;
use crate::mirror::{Data, IsTask};

/// The queue task new tasks go under unless the form says otherwise (`queue` in `.trailhead.toml`).
pub const DEFAULT_QUEUE: &str = "TRAIL-102";

/// Priorities a task can have, most urgent first.
pub const PRIORITIES: [i64; 4] = [1, 2, 3, 4];

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
    pub priority: i64,
    pub status: String,
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
            priority: 2,
            status: "open".into(),
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
    pub fn edit(task: &Value) -> Open {
        let text = |k: &str| task[k].as_str().unwrap_or_default().to_string();
        let target = Target::Edit { key: text("key"), version: task["version"].as_i64().unwrap_or(0) };
        let draft = Draft {
            title: text("title"),
            description: text("description"),
            priority: task["priority"].as_i64().unwrap_or(2),
            status: text("status"),
            ..Draft::default()
        };
        Open::new(target, draft)
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
            let body = json!({
                "project": project,
                "title": title,
                "description": d.description,
                "priority": d.priority,
                "parent": parent,
            });
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

/// The next value after `current` in `values`, wrapping.
fn step<T: PartialEq + Clone>(values: &[T], current: &T) -> T {
    let next = values.iter().position(|v| v == current).map_or(0, |i| (i + 1) % values.len());
    values[next].clone()
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
    CyclePriority,
    CycleStatus,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LabelKind {
    #[default]
    Priority,
    Status,
    Message,
}

/// Text that shows a part of the form's state.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct FormLabel(pub LabelKind);

/// The form's root node, and the source of its commands.
#[derive(Component)]
pub struct FormPanel;

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
    tasks: Query<&Data, With<IsTask>>,
    panel: Query<Entity, With<FormPanel>>,
    mut submit: MessageWriter<Submit>,
) {
    let Ok(act) = acts.get(activate.entity) else { return };
    // Opening a form is allowed offline (the board is read-only then, so its buttons are greyed out), but nothing
    // is sent until the connection is live again.
    match act {
        Act::New => form.open(Open::create(&authoring.queue)),
        Act::Edit(key) => {
            if let Some(task) = tasks.iter().find(|d| d.0["key"].as_str() == Some(key)) {
                form.open(Open::edit(&task.0));
            }
        }
        Act::Comment(key) => form.open(Open::comment(key)),
        Act::Answer { key, comment_id, question } => form.open(Open::answer(key, comment_id, question)),
        // A save in flight can't be withdrawn, so the form stays until it is answered.
        Act::Cancel if !form.saving => form.close(),
        Act::Cancel => {}
        Act::CyclePriority => {
            if let Some(open) = &mut form.open {
                open.draft.priority = step(&PRIORITIES, &open.draft.priority);
            }
        }
        Act::CycleStatus => {
            if let Some(open) = &mut form.open {
                let statuses: Vec<String> = STATUSES.iter().map(|s| s.to_string()).collect();
                open.draft.status = step(&statuses, &open.draft.status);
            }
        }
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
            .add_systems(Update, (init_text, read_fields, finish).chain());
    }
}

/// Draws the form over the board.
pub struct AuthorUiPlugin;

impl Plugin for AuthorUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(AuthorPlugin).add_systems(Update, (redraw_form, update_labels).chain());
    }
}

/// A button for `act`.
pub fn act_button(label: String, act: Act) -> impl Scene {
    bsn! {
        @FeathersButton
        act
        Children [ @caption(label) ]
    }
}

fn input(kind: FieldKind, generation: u64, initial: String, lines: f32) -> impl Scene {
    bsn! {
        @FeathersTextInputContainer
        Node { height: Val::Auto, width: percent(100), align_items: AlignItems::FlexStart, padding: UiRect::all(px(3)) }
        Children [
            @FeathersTextInput
            Field(kind, generation)
            InitialText(initial)
            EditableText { visible_lines: {Some(lines)}, allow_newlines: {lines > 1.0} }
            TextLayout { linebreak: LineBreak::WordBoundary }
        ]
    }
}

fn label_text(kind: LabelKind, text: String) -> impl Scene {
    bsn! {
        @caption(text)
        FormLabel(kind)
    }
}

fn row() -> Node {
    Node { column_gap: px(6), align_items: AlignItems::Center, ..default() }
}

fn redraw_form(
    form: Res<Form>,
    panel: Query<Entity, With<FormPanel>>,
    mut commands: Commands,
    mut drawn: Local<u64>,
    mut first: Local<bool>,
) {
    if *first && *drawn == form.generation {
        return;
    }
    *first = true;
    *drawn = form.generation;
    for e in &panel {
        commands.entity(e).despawn();
    }
    let Some(open) = &form.open else { return };
    let d = &open.draft;
    let generation = form.generation;
    let root = commands
        .spawn((
            Name::new("form"),
            FormPanel,
            GlobalZIndex(10),
            BackgroundColor(Color::srgb(0.13, 0.13, 0.15)),
            Node {
                position_type: PositionType::Absolute,
                left: px(40),
                top: px(60),
                width: px(640),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(12)),
                row_gap: px(8),
                ..default()
            },
        ))
        .id();
    let c = &mut commands;

    let heading = match &open.target {
        Target::Create => "New task".to_string(),
        Target::Edit { key, .. } => format!("Edit {key}"),
        Target::Comment { key } => format!("Comment on {key}"),
        Target::Answer { key, .. } => format!("Answer on {key}"),
    };
    c.queue_spawn_scene(caption(heading)).insert(ChildOf(root));

    match &open.target {
        Target::Create | Target::Edit { .. } => {
            let creating = open.target == Target::Create;
            if creating {
                c.queue_spawn_scene(caption("parent".to_string())).insert(ChildOf(root));
                c.queue_spawn_scene(input(FieldKind::Parent, generation, d.parent.clone(), 1.0)).insert(ChildOf(root));
            }
            c.queue_spawn_scene(caption("title".to_string())).insert(ChildOf(root));
            c.queue_spawn_scene(input(FieldKind::Title, generation, d.title.clone(), 1.0)).insert(ChildOf(root));
            let line = c.spawn((row(), ChildOf(root))).id();
            c.queue_spawn_scene(act_button("priority".into(), Act::CyclePriority)).insert(ChildOf(line));
            c.queue_spawn_scene(label_text(LabelKind::Priority, format!("P{}", d.priority))).insert(ChildOf(line));
            if !creating {
                c.queue_spawn_scene(act_button("status".into(), Act::CycleStatus)).insert(ChildOf(line));
                c.queue_spawn_scene(label_text(LabelKind::Status, d.status.clone())).insert(ChildOf(line));
            }
            c.queue_spawn_scene(caption("spec".to_string())).insert(ChildOf(root));
            c.queue_spawn_scene(input(FieldKind::Description, generation, d.description.clone(), 12.0))
                .insert(ChildOf(root));
        }
        Target::Comment { .. } | Target::Answer { .. } => {
            if let Target::Answer { question, .. } = &open.target {
                c.queue_spawn_scene(caption(format!("? {question}"))).insert(ChildOf(root));
            }
            c.queue_spawn_scene(input(FieldKind::Body, generation, String::new(), 6.0)).insert(ChildOf(root));
        }
    }

    let line = c.spawn((row(), ChildOf(root))).id();
    c.queue_spawn_scene(act_button("save".into(), Act::Save)).insert(ChildOf(line));
    c.queue_spawn_scene(act_button("cancel".into(), Act::Cancel)).insert(ChildOf(line));
    c.queue_spawn_scene(label_text(LabelKind::Message, form.message.clone())).insert(ChildOf(line));
}

fn update_labels(form: Res<Form>, mut labels: Query<(&FormLabel, &mut Text)>) {
    if !form.is_changed() {
        return;
    }
    let Some(open) = &form.open else { return };
    for (FormLabel(kind), mut text) in &mut labels {
        let value = match kind {
            LabelKind::Priority => format!("P{}", open.draft.priority),
            LabelKind::Status => open.draft.status.clone(),
            LabelKind::Message => form.message.clone(),
        };
        if text.0 != value {
            text.0 = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::state::app::StatesPlugin;

    use super::*;
    use crate::commands::{CommandId, CommandsPlugin, Outbox};
    use crate::mirror::MirrorPlugin;
    use crate::sync::SyncItem;

    fn task() -> Value {
        json!({"id": "t1", "key": "TRAIL-7", "title": "old", "description": "a\nb", "priority": 2,
               "status": "open", "version": 4})
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
        open.draft.priority = 1;
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
        let mut open = Open::edit(&task());
        assert_eq!(request(&open).unwrap_err(), "nothing changed");
        open.draft.description = "a\nb\nc".into();
        open.draft.status = "cancelled".into();
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

    #[test]
    fn step_wraps() {
        assert_eq!(step(&PRIORITIES, &4), 1);
        assert_eq!(step(&PRIORITIES, &2), 3);
        assert_eq!(step(&PRIORITIES, &99), 1);
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
            "id": 1, "entity": "task", "entity_id": "t1", "kind": "task.updated", "after": task(),
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
        let panel = app.world_mut().spawn(FormPanel).id();
        go_live(&mut app);

        press(&mut app, Act::Edit("TRAIL-7".into()));
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.title, "old");

        // Nothing changed: refused on the form, nothing sent.
        press(&mut app, Act::Save);
        assert_eq!(app.world().resource::<Form>().message, "nothing changed");
        assert!(app.world().resource::<Outbox>().is_empty());

        press(&mut app, Act::CycleStatus);
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.status, "in_progress");
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
        app.world_mut().spawn(FormPanel);
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

    #[test]
    fn cancel_is_ignored_while_saving() {
        let mut app = app();
        press(&mut app, Act::Comment("TRAIL-7".into()));
        app.world_mut().resource_mut::<Form>().saving = true;
        press(&mut app, Act::Cancel);
        assert!(app.world().resource::<Form>().open.is_some());
    }
}
