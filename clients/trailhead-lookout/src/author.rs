//! Authoring on the task board (`docs/factory/ui.md#views`): create a task in a project, under a parent or not, and
//! send it to the factory by putting it under the project's queue task; and send comments and answers to the task
//! modal's comment box. An existing task is edited in place, in the task modal.
//!
//! The form is a [`Form`] resource: what is open, the [`Draft`] being typed and the message from the last
//! attempt. [`request`] turns it into the one REST request that saves it; it is pure, so it is tested without a
//! window. Saving sends a [`Submit`] with the form's panel as the source, and the form closes only on `Ok`; any
//! other [`Outcome`] stays on the form, where the person can fix it and save again. Nothing is applied locally: the
//! event stream brings the saved task to the board.
//!
//! [`AuthorUiPlugin`] draws the form with feathers. The description and comment boxes are feathers' text input
//! with `allow_newlines` and `visible_lines` set, which is all multiline takes.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::text::{EditableText, LineBreak};
use bevy::ui_widgets::{Activate, ValueChange};
use reqwest::Method;
use serde_json::json;
use trailhead_model::{Grade, Priority, Project, ProjectStatus, Status, Task};

use crate::board::View;
use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::Connection;
use crate::factory::FactoryStatus;
use crate::inspector::{OpenComposer, Selection, Subject};
use crate::mirror::Mirrored;
use crate::project::ProjectPage;
use crate::widgets::{
    ButtonKind, Icon, InitialText, Keyed, Modal, ModalClosed, ModalFocus, ModalLocked, ModalSlots, Picked, SelectEdit,
    Token, Truncated, button, dropdown_item, dropdown_list, field, grade_tone, matches_search, open_modal,
    priority_tone, row, select_toned, size, space, status_tone, text, text_dim, text_input, toggle_switch, token,
    tool_button,
};

/// The priority the form starts a new task at.
const NEW_PRIORITY: Priority = Priority::new(2).unwrap();

/// The queue task new tasks go under until the factory has said what its queue is (`queue` in `.trailhead.toml`),
/// unless `LOOKOUT_QUEUE` pins one.
pub const DEFAULT_QUEUE: &str = "TRAIL-102";

/// What a task's grade can be set to, in the order of the dropdown: none, then 1 to 4.
pub fn grade_choices() -> impl Iterator<Item = Option<Grade>> {
    std::iter::once(None).chain(Grade::ALL.map(Some))
}

/// The dropdown's rows for [`grade_choices`]: each grade with what it is like (`docs/factory/review.md#grades`), and where
/// Treeline falls: the grades in `human` are the ones whose gate wants a person (`FactoryStatus::human_grades`), and
/// those are above it.
pub fn grade_options(human: &[Grade]) -> Vec<String> {
    let like = ["a walk on a trail", "off-trail", "scrambling", "exposed"];
    let rows = Grade::ALL.iter().zip(like).map(|(g, like)| {
        let above = if human.contains(g) { ", above Treeline" } else { "" };
        format!("{g} · {like}{above}")
    });
    std::iter::once("none (the factory skips it)".to_string()).chain(rows).collect()
}

/// The row of [`grade_options`] that shows `grade`.
pub fn grade_index(grade: Option<Grade>) -> Option<usize> {
    grade_choices().position(|g| g == grade)
}

/// The tone of each row of [`grade_options`]: none is dim, each grade its own color.
pub fn grade_tones() -> Vec<Option<Token>> {
    grade_choices().map(|g| Some(g.map_or(token::TEXT_DIM, grade_tone))).collect()
}

/// The priority dropdown's rows, P1 to P4.
pub fn priority_options() -> Vec<String> {
    Priority::ALL.iter().map(|p| format!("P{p}")).collect()
}

/// The tone of each row of [`priority_options`].
pub fn priority_tones() -> Vec<Option<Token>> {
    Priority::ALL.iter().map(|p| Some(priority_tone(*p))).collect()
}

/// The tone of each of `statuses`, for a status dropdown's rows.
pub fn status_tones(statuses: &[Status]) -> Vec<Option<Token>> {
    statuses.iter().map(|s| Some(status_tone(*s))).collect()
}

#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct Authoring {
    /// The queue task new tasks go to the factory under, until the factory reports its queues.
    pub queue: String,
    /// `queue` was set by the person (`LOOKOUT_QUEUE`), so the factory's queue doesn't replace it.
    pub pinned: bool,
    /// The project the last new task went to, which the next one starts in when no project page is showing.
    pub last_project: Option<String>,
}

impl Default for Authoring {
    fn default() -> Self {
        Authoring { queue: DEFAULT_QUEUE.to_string(), pinned: false, last_project: None }
    }
}

/// How many of a project's open tasks the parent search offers at once.
const PARENT_SUGGESTIONS: usize = 6;

/// The fields of the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub description: String,
    pub priority: Priority,
    /// The project's key.
    pub project: String,
    /// The parent's key; empty for a task at the top of its project. Not used while `queued`.
    pub parent: String,
    /// What is typed to find a parent.
    pub parent_query: String,
    /// The task goes to the factory, under `queue`.
    pub queued: bool,
    /// The queue task it goes under when queued; empty when the project has none.
    pub queue: String,
    /// Sent only when queued: the factory skips a task without one.
    pub grade: Option<Grade>,
}

impl Default for Draft {
    fn default() -> Self {
        Draft {
            title: String::new(),
            description: String::new(),
            priority: NEW_PRIORITY,
            project: String::new(),
            parent: String::new(),
            parent_query: String::new(),
            queued: false,
            queue: String::new(),
            grade: None,
        }
    }
}

/// The form as it is open: a new task, being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    pub draft: Draft,
}

impl Open {
    /// A new task in `project`, under `parent` (empty for none), going to `queue` if it is queued.
    pub fn create(project: &str, parent: &str, queue: Option<String>) -> Open {
        let draft = Draft {
            project: project.to_string(),
            parent: parent.to_string(),
            queue: queue.unwrap_or_default(),
            ..Draft::default()
        };
        Open { draft }
    }
}

/// The project a key belongs to: `TRAIL` for `TRAIL-102`.
fn project_of(key: &str) -> Option<&str> {
    key.rsplit_once('-').map(|(project, _)| project).filter(|p| !p.is_empty())
}

/// The factory's queue tasks in `project`: the one `LOOKOUT_QUEUE` pins first, then those the factory reports.
pub fn queues_of(project: &str, authoring: &Authoring, factory: &FactoryStatus) -> Vec<String> {
    let pinned = authoring.pinned.then(|| authoring.queue.clone());
    let mut keys: Vec<String> = Vec::new();
    for key in pinned.into_iter().chain(factory.queues()) {
        if project_of(&key) == Some(project) && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// The request that creates the task in `open`, or why it can't be sent yet.
pub fn request(open: &Open) -> Result<Request, String> {
    let d = &open.draft;
    let title = d.title.trim();
    if title.is_empty() {
        return Err("a task needs a title".into());
    }
    let project = d.project.trim();
    if project.is_empty() {
        return Err("pick a project".into());
    }
    let parent = if d.queued {
        if d.queue.is_empty() {
            return Err(format!("{project} has no factory queue"));
        }
        d.queue.as_str()
    } else {
        d.parent.trim()
    };
    if !parent.is_empty() && project_of(parent) != Some(project) {
        return Err(format!("the parent has to be a task in {project}"));
    }
    let mut body = json!({
        "project": project,
        "title": title,
        "description": d.description,
        "priority": d.priority,
    });
    if !parent.is_empty() {
        body["parent"] = json!(parent);
    }
    if d.queued
        && let Some(grade) = d.grade
    {
        body["grade"] = json!(grade);
    }
    Ok(Request::new(Method::POST, "/v1/tasks", Some(body), None))
}

/// The open tasks of `project` whose key or title has every word of `query`, by key, at most `limit` of them.
pub fn parent_suggestions<'a>(
    tasks: impl IntoIterator<Item = &'a Task>,
    project: &str,
    query: &str,
    limit: usize,
) -> Vec<(String, String)> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let mut found: Vec<&Task> = tasks
        .into_iter()
        .filter(|t| !t.status.is_closed() && project_of(&t.key.0) == Some(project))
        .filter(|t| matches_search(query, &[&t.key.0, &t.title]))
        .collect();
    found.sort_by(|a, b| a.key.cmp(&b.key));
    found.into_iter().take(limit).map(|t| (t.key.0.clone(), t.title.clone())).collect()
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
    /// The new-task form, under the task with this key.
    NewSubtask(String),
    /// Opens the task in the modal, where it is edited in place.
    Edit(String),
    Comment(String),
    Answer {
        key: String,
        comment_id: String,
        question: String,
    },
    Save,
    Cancel,
    /// Makes the task with this key the new task's parent.
    PickParent(String),
    /// Leaves the new task without a parent.
    ClearParent,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FieldKind {
    #[default]
    Title,
    Description,
    ParentQuery,
}

/// A text input of the form, which draft field it holds, and the form generation it was drawn for
/// (inputs of a form being replaced are despawned late and must not feed the new draft).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Field(pub FieldKind, pub u64);

/// Which draft field a dropdown of the form sets.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormChoice {
    #[default]
    Priority,
    Grade,
    Project,
    /// The queue task a queued task goes under.
    Queue,
}

/// The keys a project or queue dropdown's rows stand for, in order.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
struct ChoiceKeys(Vec<String>);

/// The switch that sends the task to the factory.
#[derive(Component, Debug, Clone, Copy, Default)]
struct QueueToggle;

/// A part of the form shown only some of the time; [`dress_form`] shows and hides it as the draft changes.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FormPart {
    /// The queue switch: while the project has a factory queue.
    #[default]
    Queueing,
    /// The queue dropdown: while queued, in a project with more than one queue.
    QueuePick,
    /// The parent search and its suggestions: while not queued.
    Parent,
    /// The search box: while no parent is picked. The pick takes its place, so the form below doesn't move.
    Search,
    /// The parent picked, with the X that clears it: while there is one.
    Chosen,
    /// The suggestions floating under the search: while nothing is picked and something matches.
    Suggestions,
    /// The grade: while queued.
    Grade,
}

/// The words saying which parent is picked.
#[derive(Component, Debug, Clone, Copy, Default)]
struct ChosenParent;

/// The parent suggestions' buttons, by key.
#[derive(Resource, Default)]
struct Suggestions(Keyed<(String, String)>);

/// Follows the factory's queue: until the person pins one, new tasks go under the first queue it reports.
fn follow_factory_queue(factory: Res<FactoryStatus>, mut authoring: ResMut<Authoring>) {
    if authoring.pinned || !factory.is_changed() {
        return;
    }
    if let Some(first) = factory.queues().into_iter().next()
        && authoring.queue != first
    {
        authoring.queue = first;
    }
}

/// The text showing the form's last message.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct FormMessage;

/// Lines tall the spec's box is, and a comment's or answer's.
const SPEC_LINES: f32 = 12.0;

/// The form's modal, and the source of its commands.
#[derive(Component, Default)]
pub struct FormPanel;

impl Modal for FormPanel {
    fn title(&self) -> String {
        "New task".into()
    }
}

fn field_value(draft: &mut Draft, kind: FieldKind) -> &mut String {
    match kind {
        FieldKind::Title => &mut draft.title,
        FieldKind::Description => &mut draft.description,
        FieldKind::ParentQuery => &mut draft.parent_query,
    }
}

/// What a new task starts in: the project page showing, else the project the last one went to, else the first.
#[derive(SystemParam)]
struct Starting<'w, 's> {
    view: Option<Res<'w, State<View>>>,
    page: Option<Res<'w, ProjectPage>>,
    projects: Query<'w, 's, &'static Mirrored<Project>>,
    authoring: Res<'w, Authoring>,
    factory: Res<'w, FactoryStatus>,
}

impl Starting<'_, '_> {
    fn project(&self) -> String {
        let active: Vec<&str> =
            self.projects.iter().filter(|p| p.0.status == ProjectStatus::Active).map(|p| p.0.key.as_str()).collect();
        let shown = self.view.as_ref().is_some_and(|v| *v.get() == View::Project);
        let page = self.page.as_ref().and_then(|p| p.0.clone()).filter(|_| shown);
        let known = |key: &String| active.contains(&key.as_str());
        page.filter(known)
            .or_else(|| self.authoring.last_project.clone().filter(known))
            .or_else(|| active.iter().min().map(|k| k.to_string()))
            .unwrap_or_default()
    }

    /// A new task in `project` under `parent`.
    fn open(&self, project: &str, parent: &str) -> Open {
        Open::create(project, parent, queues_of(project, &self.authoring, &self.factory).into_iter().next())
    }
}

#[allow(clippy::too_many_arguments)]
fn on_act(
    activate: On<Activate>,
    acts: Query<&Act>,
    mut form: ResMut<Form>,
    starting: Starting,
    connection: Res<State<Connection>>,
    tasks: Query<&Mirrored<Task>>,
    panel: Query<Entity, With<FormPanel>>,
    mut submit: MessageWriter<Submit>,
    mut selection: ResMut<Selection>,
    mut commands: Commands,
) {
    let Ok(act) = acts.get(activate.entity) else { return };
    // Opening a form is allowed offline (the board is read-only then, so its buttons are greyed out), but nothing
    // is sent until the connection is live again.
    match act {
        Act::New => {
            let project = starting.project();
            form.open(starting.open(&project, ""));
        }
        Act::NewSubtask(key) => form.open(starting.open(project_of(key).unwrap_or_default(), key)),
        // An existing task is edited in place, in the task modal.
        Act::Edit(key) => {
            if let Some(task) = tasks.iter().find(|t| t.0.key == key.as_str()) {
                selection.0 = Some(Subject::Task(task.0.id.to_string()));
            }
        }
        // Comments and answers are written in the task modal's comment box.
        Act::Comment(key) => commands.trigger(OpenComposer { key: key.clone(), answer: None }),
        Act::Answer { key, comment_id, question } => {
            commands.trigger(OpenComposer { key: key.clone(), answer: Some((comment_id.clone(), question.clone())) })
        }
        Act::PickParent(key) => {
            if let Some(open) = &mut form.open {
                open.draft.parent = key.clone();
            }
        }
        Act::ClearParent => {
            if let Some(open) = &mut form.open {
                open.draft.parent.clear();
            }
        }
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

/// The queue switch: a queued task goes to the factory.
fn on_queue_toggle(change: On<ValueChange<bool>>, toggles: Query<(), With<QueueToggle>>, mut form: ResMut<Form>) {
    if !toggles.contains(change.source) {
        return;
    }
    if let Some(open) = &mut form.open
        && open.draft.queued != change.value
    {
        open.draft.queued = change.value;
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

/// The save's answer: `Ok` closes the form, and the next new task starts in the same project; anything else stays on
/// the form.
fn finish(
    mut done: MessageReader<CommandDone>,
    panel: Query<Entity, With<FormPanel>>,
    mut form: ResMut<Form>,
    mut authoring: ResMut<Authoring>,
) {
    for d in done.read() {
        if !panel.contains(d.source) || !form.saving {
            continue;
        }
        if matches!(d.outcome, Outcome::Ok { .. }) {
            if let Some(open) = &form.open {
                authoring.last_project = Some(open.draft.project.clone());
            }
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
            .init_resource::<FactoryStatus>()
            .init_resource::<Selection>()
            .add_observer(on_act)
            .add_observer(on_pick)
            .add_observer(on_queue_toggle)
            .add_observer(on_modal_closed)
            .add_systems(Update, (follow_factory_queue, read_fields, finish).chain());
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
    app.init_resource::<FactoryStatus>().init_resource::<Suggestions>();
    app.add_systems(Update, (redraw_form, fill_form, dress_form, lock_while_saving, update_message).chain());
}

/// A button for `act`: a normal one, with no icon.
pub fn act_button(label: String, act: Act) -> impl Scene {
    bsn! {
        @button(label, None, ButtonKind::Normal)
        act
    }
}

/// A text input of the form for `kind`, `lines` high, holding `initial` when it appears.
fn input(kind: FieldKind, generation: u64, initial: String, lines: f32) -> Box<dyn SceneList> {
    // The title is the first thing to type, so the keyboard starts there.
    if kind == FieldKind::Title {
        Box::new(bsn_list! { @text_input(lines, bsn! { Field(kind, generation) InitialText(initial) ModalFocus }) })
    } else {
        Box::new(bsn_list! { @text_input(lines, bsn! { Field(kind, generation) InitialText(initial) }) })
    }
}

/// A text field of the form: `label` over its input.
fn text_field(commands: &mut Commands, parent: Entity, label: &str, input: impl SceneList) {
    commands.queue_spawn_scene(field(label.to_string(), input)).insert(ChildOf(parent));
}

/// A dropdown of the form for `choice`, labelled, with `selected` showing and each row in its tone (none for a plain
/// row), as the task's own dropdowns are. `keys` are what its rows stand for, when they stand for projects or tasks.
/// Returns the field, for a part shown only some of the time.
fn choice_field(
    commands: &mut Commands,
    parent: Entity,
    label: &str,
    choice: FormChoice,
    (options, tones): (Vec<String>, Vec<Option<Token>>),
    selected: Option<usize>,
    keys: Vec<String>,
) -> Entity {
    let max_visible = options.len().max(1);
    let control: Box<dyn SceneList> = match choice {
        FormChoice::Priority => {
            Box::new(bsn_list! { @select_toned(options, tones, selected, max_visible) FormChoice::Priority })
        }
        FormChoice::Grade => {
            Box::new(bsn_list! { @select_toned(options, tones, selected, max_visible) FormChoice::Grade })
        }
        FormChoice::Project => Box::new(bsn_list! {
            @select_toned(options, tones, selected, max_visible) FormChoice::Project ChoiceKeys({ keys })
        }),
        FormChoice::Queue => Box::new(bsn_list! {
            @select_toned(options, tones, selected, max_visible) FormChoice::Queue ChoiceKeys({ keys })
        }),
    };
    commands.queue_spawn_scene(field(label.to_string(), control)).insert(ChildOf(parent)).id()
}

/// Sets the draft from the dropdown that was used. A different project starts the parent and queue over.
fn on_pick(
    picked: On<Picked>,
    choices: Query<(&FormChoice, Option<&ChoiceKeys>)>,
    mut form: ResMut<Form>,
    authoring: Res<Authoring>,
    factory: Res<FactoryStatus>,
) {
    let Ok((choice, keys)) = choices.get(picked.entity) else { return };
    let Some(open) = &mut form.open else { return };
    let d = &mut open.draft;
    let key = keys.and_then(|k| k.0.get(picked.index)).cloned();
    match choice {
        FormChoice::Priority => {
            if let Some(priority) = Priority::ALL.get(picked.index) {
                d.priority = *priority;
            }
        }
        FormChoice::Grade => {
            if let Some(grade) = grade_choices().nth(picked.index) {
                d.grade = grade;
            }
        }
        FormChoice::Project => {
            if let Some(project) = key
                && d.project != project
            {
                d.queue = queues_of(&project, &authoring, &factory).into_iter().next().unwrap_or_default();
                d.queued &= !d.queue.is_empty();
                d.parent.clear();
                d.project = project;
            }
        }
        FormChoice::Queue => {
            if let Some(queue) = key {
                d.queue = queue;
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
    mut suggestions: ResMut<Suggestions>,
) {
    if *first && *drawn == form.generation {
        return;
    }
    *first = true;
    *drawn = form.generation;
    for e in &forms {
        commands.entity(e).despawn();
    }
    // The last form's suggestions went with it.
    suggestions.0 = Keyed::default();
    if form.open.is_some() {
        open_modal(&mut commands, FormPanel);
    }
}

/// The active projects, by key, as the project dropdown lists them: "KEY · Name".
fn project_rows(projects: &Query<&Mirrored<Project>>) -> (Vec<String>, Vec<String>) {
    let mut active: Vec<&Project> =
        projects.iter().map(|p| &p.0).filter(|p| p.status == ProjectStatus::Active).collect();
    active.sort_by(|a, b| a.key.cmp(&b.key));
    let labels = active
        .iter()
        .map(|p| if p.name.is_empty() { p.key.clone() } else { format!("{} · {}", p.key, p.name) })
        .collect();
    (active.iter().map(|p| p.key.clone()).collect(), labels)
}

/// Fills a new form's modal: the fields in its body, the buttons and message in its footer. What shows depends on the
/// draft; [`dress_form`] keeps that in step afterwards.
fn fill_form(
    new: Query<&ModalSlots, (With<FormPanel>, Added<ModalSlots>)>,
    form: Res<Form>,
    authoring: Res<Authoring>,
    factory: Res<FactoryStatus>,
    projects: Query<&Mirrored<Project>>,
    mut commands: Commands,
) {
    let Some(open) = &form.open else { return };
    let d = &open.draft;
    let generation = form.generation;
    let c = &mut commands;
    for slots in &new {
        let body = slots.body;
        let (keys, labels) = project_rows(&projects);
        let at = keys.iter().position(|k| *k == d.project);
        let plain = vec![None; labels.len()];
        choice_field(c, body, "Project", FormChoice::Project, (labels, plain), at, keys);

        c.spawn_scene(toggle_switch("Queue for the factory", d.queued, bsn! { QueueToggle }))
            .insert((FormPart::Queueing, ChildOf(body)));
        let queues = queues_of(&d.project, &authoring, &factory);
        let at = queues.iter().position(|q| *q == d.queue);
        let plain = vec![None; queues.len()];
        let queue = choice_field(c, body, "Queue task", FormChoice::Queue, (queues.clone(), plain), at, queues);
        c.entity(queue).insert(FormPart::QueuePick);

        // The parent: a search over the project's open tasks with what it found floating under it, or in its place
        // the one picked.
        let parent = c
            .spawn((Node { flex_direction: FlexDirection::Column, row_gap: px(space::STACKED), ..default() },))
            .insert((FormPart::Parent, ChildOf(body)))
            .id();
        c.spawn_scene(text_dim("Parent (optional): search by key or title", size::SECONDARY)).insert(ChildOf(parent));
        let anchor = c.spawn((Node { flex_direction: FlexDirection::Column, ..default() }, ChildOf(parent))).id();
        let search = c
            .spawn((Node { flex_direction: FlexDirection::Column, ..default() }, FormPart::Search, ChildOf(anchor)))
            .id();
        let query = d.parent_query.clone();
        c.spawn_scene(bsn! {
            @text_input(1.0, bsn! { Field(FieldKind::ParentQuery, generation) InitialText(query) })
        })
        .insert(ChildOf(search));
        let chosen = c
            .spawn((Node {
                align_items: AlignItems::Center,
                column_gap: px(space::CONTROLS),
                min_width: px(0),
                ..default()
            },))
            .insert((FormPart::Chosen, ChildOf(anchor)))
            .id();
        // Cut to the room the X leaves it, so a long title stays on the search box's one line.
        let (picked, full) = (d.parent.clone(), d.parent.clone());
        c.spawn_scene(bsn! {
            @text(picked, size::BODY)
            Truncated { full: { full }, size: { size::BODY } }
            TextLayout { linebreak: LineBreak::NoWrap }
            Node { flex_grow: 1.0, min_width: px(0) }
        })
        .insert((ChosenParent, ChildOf(chosen)));
        c.spawn_scene(bsn! { @tool_button(Icon::X, "No parent", ButtonKind::Plain) Act::ClearParent })
            .insert(ChildOf(chosen));
        c.queue_spawn_scene(dropdown_list()).insert((FormPart::Suggestions, ChildOf(anchor)));

        text_field(c, body, "Title", input(FieldKind::Title, generation, d.title.clone(), 1.0));
        let line = c.spawn_scene(bsn! { @row() }).insert(ChildOf(body)).id();
        let priority = Priority::ALL.iter().position(|p| *p == d.priority);
        let priorities = (priority_options(), priority_tones());
        choice_field(c, line, "Priority", FormChoice::Priority, priorities, priority, Vec::new());
        let grades = (grade_options(&factory.human_grades()), grade_tones());
        let grade = choice_field(c, line, "Grade", FormChoice::Grade, grades, grade_index(d.grade), Vec::new());
        c.entity(grade).insert(FormPart::Grade);
        let spec = input(FieldKind::Description, generation, d.description.clone(), SPEC_LINES);
        text_field(c, body, "Spec", spec);
        let footer = slots.footer;
        c.spawn_scene(bsn! { @button("Save", None, ButtonKind::Primary) Act::Save }).insert(ChildOf(footer));
        c.spawn_scene(bsn! { @button("Cancel", None, ButtonKind::Normal) Act::Cancel }).insert(ChildOf(footer));
        c.spawn_scene(text_dim(form.message.clone(), size::SECONDARY)).insert((FormMessage, ChildOf(footer)));
    }
}

/// Keeps the form in step with its draft, in place: which parts show, the parent picked, the suggestions under the
/// search, and the queue dropdown's rows for the project picked.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn dress_form(
    form: Res<Form>,
    authoring: Res<Authoring>,
    factory: Res<FactoryStatus>,
    tasks: Query<&Mirrored<Task>>,
    mut parts: Query<(Entity, &FormPart, &mut Node)>,
    mut chosen: Query<&mut Truncated, With<ChosenParent>>,
    mut queue_keys: Query<(Entity, &mut ChoiceKeys, &FormChoice)>,
    mut edit: SelectEdit,
    mut suggestions: ResMut<Suggestions>,
    mut commands: Commands,
) {
    let Some(open) = &form.open else { return };
    // A part's node arrives from a queued scene a frame or two after the form opens, with the scene's own display.
    let drawn = parts.iter_mut().any(|(_, _, node)| node.is_changed());
    if !form.is_changed() && !factory.is_changed() && !drawn {
        return;
    }
    let d = &open.draft;
    let queues = queues_of(&d.project, &authoring, &factory);
    let picked = (!d.parent.is_empty()).then(|| {
        let title = tasks.iter().find(|t| t.0.key == d.parent.as_str()).map(|t| t.0.title.clone());
        title.map_or_else(|| d.parent.clone(), |title| format!("{} · {title}", d.parent))
    });
    let found = parent_suggestions(tasks.iter().map(|t| &t.0), &d.project, &d.parent_query, PARENT_SUGGESTIONS);
    let mut list = None;
    for (entity, part, mut node) in &mut parts {
        let shown = match part {
            FormPart::Queueing => !queues.is_empty(),
            FormPart::QueuePick => d.queued && queues.len() > 1,
            FormPart::Parent => !d.queued,
            FormPart::Search => picked.is_none(),
            FormPart::Chosen => picked.is_some(),
            FormPart::Suggestions => {
                list = Some(entity);
                picked.is_none() && !found.is_empty()
            }
            FormPart::Grade => d.queued,
        };
        let display = if shown { Display::Flex } else { Display::None };
        if node.display != display {
            node.display = display;
        }
    }
    for mut shown in &mut chosen {
        let words = picked.clone().unwrap_or_default();
        if shown.full != words {
            shown.full = words;
        }
    }
    // The queue dropdown lists the project's queues; it is rebuilt in place when the project changes.
    for (select, mut keys, _) in queue_keys.iter_mut().filter(|(_, _, c)| **c == FormChoice::Queue) {
        if keys.0 == queues {
            continue;
        }
        let at = queues.iter().position(|q| *q == d.queue);
        if edit.set(select, &queues, at) {
            keys.0 = queues.clone();
        }
    }
    let Some(list) = list else { return };
    let wanted = [(list, found.into_iter().map(|(k, title)| (k.clone(), (k, title))).collect::<Vec<_>>())];
    suggestions.0.sync(
        &mut commands,
        &wanted,
        |commands, key, (_, title)| {
            let words = format!("{key} · {title}");
            commands.spawn_scene(dropdown_item(words)).insert(Act::PickParent(key.to_string())).id()
        },
        |_, _, _| {},
    );
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

    use serde_json::Value;

    use super::*;
    use crate::commands::{CommandId, CommandsPlugin, Outbox};
    use crate::fixture::{sid, task_json};
    use crate::mirror::MirrorPlugin;
    use crate::sync::SyncItem;

    fn task() -> Value {
        task_json(json!({"id": "t1", "key": "TRAIL-7", "title": "old", "description": "a\nb", "priority": 2,
               "status": "open", "version": 4}))
    }

    fn body(r: &Request) -> &Value {
        r.body.as_ref().unwrap()
    }

    #[test]
    fn a_new_task_goes_to_its_project_with_or_without_a_parent() {
        let mut open = Open::create("TRAIL", "", None);
        assert_eq!(request(&open).unwrap_err(), "a task needs a title");
        open.draft.title = " Do it ".into();
        open.draft.description = "line 1\nline 2".into();
        open.draft.priority = Priority::ALL[0];
        let r = request(&open).unwrap();
        assert_eq!((r.method.clone(), r.path.as_str(), r.if_match), (Method::POST, "/v1/tasks", None));
        assert_eq!(
            body(&r),
            &json!({"project": "TRAIL", "title": "Do it", "description": "line 1\nline 2", "priority": 1})
        );
        open.draft.parent = "TRAIL-7".into();
        assert_eq!(body(&request(&open).unwrap())["parent"], json!("TRAIL-7"));
        open.draft.parent = "OPS-1".into();
        assert!(request(&open).unwrap_err().contains("in TRAIL"), "a parent from another project");
        open.draft.project = String::new();
        assert_eq!(request(&open).unwrap_err(), "pick a project");
    }

    #[test]
    fn a_queued_task_goes_under_the_queue_with_its_grade() {
        let mut open = Open::create("TRAIL", "TRAIL-7", Some("TRAIL-102".into()));
        open.draft.title = "Do it".into();
        open.draft.grade = Grade::new(2);
        let unqueued = request(&open).unwrap();
        assert_eq!(body(&unqueued)["parent"], json!("TRAIL-7"));
        assert!(body(&unqueued).get("grade").is_none(), "a grade goes only with a queued task");
        open.draft.queued = true;
        let queued = request(&open).unwrap();
        assert_eq!((body(&queued)["parent"].clone(), body(&queued)["grade"].clone()), (json!("TRAIL-102"), json!(2)));
        let mut nowhere = Open::create("OPS", "", None);
        nowhere.draft.title = "Do it".into();
        nowhere.draft.queued = true;
        assert_eq!(request(&nowhere).unwrap_err(), "OPS has no factory queue");
    }

    #[test]
    fn a_projects_queues_are_its_own_with_a_pinned_one_first() {
        let mut factory = FactoryStatus::default();
        let state = crate::factory::RepoState {
            control: trailhead_factory::status::ControlState::Up,
            queue: vec!["TRAIL-102".into(), "TRAIL-685".into(), "OPS-3".into()],
            last_error: None,
            holding: None,
            human_grades: Vec::new(),
            seq: 1,
        };
        factory.repos.insert("o/r".into(), state);
        let authoring = Authoring::default();
        assert_eq!(queues_of("TRAIL", &authoring, &factory), ["TRAIL-102", "TRAIL-685"]);
        assert_eq!(queues_of("OPS", &authoring, &factory), ["OPS-3"]);
        assert!(queues_of("ZED", &authoring, &factory).is_empty());
        let pinned = Authoring { queue: "TRAIL-685".into(), pinned: true, ..default() };
        assert_eq!(queues_of("TRAIL", &pinned, &factory), ["TRAIL-685", "TRAIL-102"]);
    }

    #[test]
    fn parent_suggestions_are_the_projects_open_tasks_that_match() {
        let tasks = [
            crate::fixture::task(json!({"id": "a", "key": "TRAIL-1", "title": "Build the board"})),
            crate::fixture::task(json!({"id": "b", "key": "TRAIL-2", "title": "Board filters", "status": "done"})),
            crate::fixture::task(json!({"id": "c", "key": "OPS-1", "title": "Board the ship"})),
            crate::fixture::task(json!({"id": "d", "key": "TRAIL-3", "title": "Lookout board header"})),
        ];
        let keys = |q: &str, limit| -> Vec<String> {
            parent_suggestions(&tasks, "TRAIL", q, limit).into_iter().map(|(k, _)| k).collect()
        };
        assert_eq!(keys("board", 6), ["TRAIL-1", "TRAIL-3"], "open, in the project, by key");
        assert_eq!(keys("trail-3", 6), ["TRAIL-3"]);
        assert_eq!(keys("board", 1), ["TRAIL-1"]);
        assert!(keys("  ", 6).is_empty(), "nothing typed, nothing offered");
    }

    #[test]
    fn comments_and_answers_post_their_body() {
        use crate::inspector::compose_request;
        use trailhead_model::CommentKind;
        let r = compose_request("TRAIL-7", CommentKind::Handoff, None, "hello\nthere");
        assert_eq!(r.path, "/v1/tasks/TRAIL-7/comments");
        assert_eq!(body(&r), &json!({"kind": "handoff", "body": "hello\nthere"}));

        let question = ("c9".to_string(), "which db?".to_string());
        let r = compose_request("TRAIL-7", CommentKind::Note, Some(&question), "sqlite");
        assert_eq!((r.method.clone(), r.path.as_str()), (Method::POST, "/v1/comments/c9/resolve"));
        assert_eq!(body(&r), &json!({"answer": "sqlite"}));
    }

    #[test]
    fn comment_and_answer_open_the_tasks_comment_box_instead_of_a_form() {
        let mut app = app();
        app.init_resource::<crate::inspector::Selection>().init_resource::<crate::inspector::Compose>();
        #[derive(Resource, Default)]
        struct Opened(Vec<crate::inspector::OpenComposer>);
        app.init_resource::<Opened>()
            .add_observer(|o: On<crate::inspector::OpenComposer>, mut opened: ResMut<Opened>| opened.0.push(o.clone()));
        press(&mut app, Act::Comment("TRAIL-7".into()));
        press(&mut app, Act::Answer { key: "TRAIL-7".into(), comment_id: "c1".into(), question: "which db?".into() });
        assert!(app.world().resource::<Form>().open.is_none(), "no form");
        let opened = &app.world().resource::<Opened>().0;
        assert_eq!(opened[0], crate::inspector::OpenComposer { key: "TRAIL-7".into(), answer: None });
        assert_eq!(opened[1].answer, Some(("c1".to_string(), "which db?".to_string())));
    }

    fn open_new_form(app: &mut App) {
        app.world_mut().resource_mut::<Form>().open(Open::create("TRAIL", "", Some("TRAIL-102".into())));
        app.update();
    }

    fn draft(app: &App) -> Draft {
        app.world().resource::<Form>().open.as_ref().unwrap().draft.clone()
    }

    #[test]
    fn edit_opens_the_task_in_the_modal_and_add_sub_task_opens_a_new_task_under_it() {
        let mut app = app();
        press(&mut app, Act::Edit("TRAIL-7".into()));
        assert!(app.world().resource::<Form>().open.is_none(), "no form");
        let selected = app.world().resource::<Selection>().0.clone();
        assert!(matches!(selected, Some(Subject::Task(_))), "{selected:?}");
        press(&mut app, Act::NewSubtask("TRAIL-7".into()));
        assert_eq!((draft(&app).project.as_str(), draft(&app).parent.as_str()), ("TRAIL", "TRAIL-7"));
    }

    #[test]
    fn a_parent_is_picked_from_the_suggestions_and_cleared_with_the_x() {
        let mut app = app();
        open_new_form(&mut app);
        press(&mut app, Act::PickParent("TRAIL-7".into()));
        assert_eq!(draft(&app).parent, "TRAIL-7");
        press(&mut app, Act::ClearParent);
        assert_eq!(draft(&app).parent, "");
    }

    #[test]
    fn the_switch_queues_the_task_for_the_factory() {
        let mut app = app();
        open_new_form(&mut app);
        assert!(!draft(&app).queued, "a new task isn't queued until asked");
        let switch = app.world_mut().spawn(QueueToggle).id();
        app.world_mut().trigger(ValueChange { source: switch, value: true, is_final: true });
        assert!(draft(&app).queued);
    }

    #[test]
    fn another_project_starts_the_parent_and_queue_over() {
        let mut app = app();
        report_queues(&mut app, &["TRAIL-102", "OPS-3"]);
        open_new_form(&mut app);
        press(&mut app, Act::PickParent("TRAIL-7".into()));
        app.world_mut().resource_mut::<Form>().open.as_mut().unwrap().draft.queued = true;
        let keys = vec!["OPS".to_string(), "TRAIL".to_string(), "ZED".to_string()];
        let select = app.world_mut().spawn((FormChoice::Project, ChoiceKeys(keys))).id();
        app.world_mut().trigger(Picked { entity: select, index: 0 });
        let d = draft(&app);
        assert_eq!((d.project.as_str(), d.parent.as_str(), d.queue.as_str(), d.queued), ("OPS", "", "OPS-3", true));
        app.world_mut().trigger(Picked { entity: select, index: 2 });
        let d = draft(&app);
        assert_eq!((d.queue.as_str(), d.queued), ("", false), "ZED has no queue, so it can't stay queued");
    }

    /// Picks `index` in the form's dropdown for `choice`, as a click on its row does.
    fn pick(app: &mut App, choice: FormChoice, index: usize) {
        let select = app.world_mut().spawn(choice).id();
        app.world_mut().trigger(Picked { entity: select, index });
        app.update();
    }

    #[test]
    fn the_priority_dropdown_sets_the_priority() {
        let mut app = app();
        open_new_form(&mut app);
        pick(&mut app, FormChoice::Priority, 0);
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.priority, Priority::ALL[0]);
        // A row past the end of the options changes nothing.
        pick(&mut app, FormChoice::Priority, 99);
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.priority, Priority::ALL[0]);
    }

    #[test]
    fn the_grade_dropdown_sets_the_draft() {
        let mut app = app();
        open_new_form(&mut app);
        pick(&mut app, FormChoice::Grade, 3);
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.grade, Grade::new(3));
        pick(&mut app, FormChoice::Grade, 0);
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.grade, None);
        assert_eq!(grade_options(&[]).len(), grade_choices().count());
    }

    #[test]
    fn treeline_is_labelled_where_the_factory_gates_want_a_person() {
        let g = |n| Grade::new(n).unwrap();
        let above: Vec<_> = grade_options(&[g(2), g(4)]).iter().map(|r| r.contains("above Treeline")).collect();
        assert_eq!(above, [false, false, true, false, true]);
        assert!(grade_options(&[]).iter().all(|r| !r.contains("Treeline")));
    }

    #[test]
    fn every_priority_status_and_grade_row_has_its_tone() {
        assert_eq!(priority_tones().len(), priority_options().len());
        assert_eq!(priority_tones()[0], Some(token::PRIORITY_1));
        assert_eq!(grade_tones().len(), grade_options(&[]).len());
        assert_eq!(grade_tones()[0], Some(token::TEXT_DIM), "none is dim");
        assert_eq!(status_tones(&[Status::Done]), [Some(status_tone(Status::Done))]);
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin)).init_state::<Connection>().init_state::<View>().add_plugins((
            MirrorPlugin,
            CommandsPlugin,
            AuthorPlugin,
        ));
        app.world_mut().resource_mut::<Outbox>().backoff = Duration::ZERO;
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 1, "entity": "task", "entity_id": sid("t1"), "kind": "task.updated", "after": task(),
        })));
        for (id, key) in [(2, "TRAIL"), (3, "ZED")] {
            let project = crate::fixture::project_json(json!({"id": key, "key": key, "name": key}));
            app.world_mut().write_message(SyncItem::Event(json!({
                "id": id, "entity": "project", "entity_id": sid(key), "kind": "project.created", "after": project,
            })));
        }
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

        open_new_form(&mut app);

        // No title: refused on the form, nothing sent.
        press(&mut app, Act::Save);
        assert_eq!(app.world().resource::<Form>().message, "a task needs a title");
        assert!(app.world().resource::<Outbox>().is_empty());

        app.world_mut().resource_mut::<Form>().open.as_mut().unwrap().draft.title = "Do it".into();
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
        assert_eq!(app.world().resource::<Authoring>().last_project.as_deref(), Some("TRAIL"), "the next starts here");
    }

    #[test]
    fn saving_while_offline_says_so_and_sends_nothing() {
        let mut app = app();
        app.world_mut().spawn(FormPanel);
        open_new_form(&mut app);
        app.world_mut().resource_mut::<Form>().open.as_mut().unwrap().draft.title = "hi".into();
        press(&mut app, Act::Save);
        let form = app.world().resource::<Form>();
        assert!(form.message.contains("Not connected") && !form.saving);
        assert!(app.world().resource::<Outbox>().is_empty());
    }

    #[test]
    fn new_starts_in_the_project_page_shown_else_the_last_one_else_the_first() {
        let mut app = app();
        report_queues(&mut app, &["TRAIL-102"]);
        press(&mut app, Act::New);
        let d = draft(&app);
        assert_eq!((d.project.as_str(), d.parent.as_str(), d.queue.as_str()), ("TRAIL", "", "TRAIL-102"));
        press(&mut app, Act::Cancel);
        assert!(app.world().resource::<Form>().open.is_none());

        app.world_mut().resource_mut::<Authoring>().last_project = Some("ZED".into());
        press(&mut app, Act::New);
        assert_eq!((draft(&app).project.as_str(), draft(&app).queue.as_str()), ("ZED", ""));
        press(&mut app, Act::Cancel);

        app.insert_resource(crate::project::ProjectPage(Some("TRAIL".into())));
        app.world_mut().resource_mut::<NextState<View>>().set(View::Project);
        app.update();
        press(&mut app, Act::New);
        assert_eq!(draft(&app).project, "TRAIL", "the project page shown wins");
    }

    fn report_queues(app: &mut App, queues: &[&str]) {
        let state = crate::factory::RepoState {
            control: trailhead_factory::status::ControlState::Up,
            queue: queues.iter().map(|q| q.to_string()).collect(),
            last_error: None,
            holding: None,
            human_grades: Vec::new(),
            seq: 1,
        };
        app.world_mut().resource_mut::<FactoryStatus>().repos.insert("o/r".into(), state);
        app.update();
    }

    #[test]
    fn new_follows_the_factory_queue_unless_pinned() {
        let mut app = app();
        report_queues(&mut app, &["TRAIL-102", "TRAIL-685"]);
        assert_eq!(app.world().resource::<FactoryStatus>().queues(), ["TRAIL-102", "TRAIL-685"]);
        report_queues(&mut app, &["TRAIL-685"]);
        assert_eq!(app.world().resource::<Authoring>().queue, "TRAIL-685");
        {
            let mut authoring = app.world_mut().resource_mut::<Authoring>();
            authoring.queue = "TRAIL-9".into();
            authoring.pinned = true;
        }
        report_queues(&mut app, &["TRAIL-102"]);
        assert_eq!(app.world().resource::<Authoring>().queue, "TRAIL-9");
    }

    #[test]
    fn typed_text_reaches_the_draft() {
        let mut app = app();
        open_new_form(&mut app);
        let generation = app.world().resource::<Form>().generation;
        let mut text = EditableText::new("one\ntwo");
        text.allow_newlines = true;
        app.world_mut().spawn((Field(FieldKind::Description, generation), text));
        app.update();
        assert_eq!(app.world().resource::<Form>().open.as_ref().unwrap().draft.description, "one\ntwo");
    }

    #[test]
    fn fields_of_a_replaced_form_do_not_feed_the_new_draft() {
        let mut app = app();
        open_new_form(&mut app);
        let old = app.world().resource::<Form>().generation;
        let mut text = EditableText::new("old form text");
        text.allow_newlines = true;
        app.world_mut().spawn((Field(FieldKind::Description, old), text));
        // Switch forms while the old panel's inputs still exist.
        press(&mut app, Act::New);
        let open = app.world().resource::<Form>().open.clone().unwrap();
        assert_eq!(open.draft.description, "");
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
    fn the_form_opens_one_modal_and_closing_it_closes_the_form() {
        // Cancel, then the modal's own ways out: Escape and a click outside both send `RequestClose` for the
        // backdrop, and the X sends it from inside.
        for from_backdrop in [None, Some(true), Some(false)] {
            let mut app = ui_app();
            settle(&mut app);
            press(&mut app, Act::New);
            settle(&mut app);
            assert_eq!(modals(&mut app), 1);
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
            assert_eq!(modals(&mut app), 0, "{from_backdrop:?}");
            assert!(app.world().resource::<Form>().open.is_none());
        }
    }

    #[test]
    fn a_form_opens_with_the_keyboard_on_its_title() {
        // Edit opens the inspector, not a form, so the forms are a new task and a new sub-task.
        for subtask in [false, true] {
            let mut app = ui_app();
            settle(&mut app);
            if subtask {
                press(&mut app, Act::NewSubtask("TRAIL-7".into()));
            } else {
                press(&mut app, Act::New);
            }
            settle(&mut app);
            let focused = app.world().resource::<bevy::input_focus::InputFocus>().get().unwrap();
            let field = app.world().get::<Field>(focused);
            assert_eq!(field.map(|f| f.0), Some(FieldKind::Title), "subtask: {subtask}");
        }
    }

    #[test]
    fn the_form_shows_what_the_draft_needs_and_offers_parents_as_you_type() {
        let mut app = ui_app();
        report_queues(&mut app, &["TRAIL-102"]);
        open_new_form(&mut app);
        settle(&mut app);
        let shown = |app: &mut App, part: FormPart| {
            let world = app.world_mut();
            let mut parts = world.query::<(&FormPart, &Node)>();
            parts.iter(world).find(|(p, _)| **p == part).map(|(_, n)| n.display != Display::None).unwrap()
        };
        assert!(shown(&mut app, FormPart::Queueing), "TRAIL has a queue, so the switch shows");
        assert!(shown(&mut app, FormPart::Parent) && !shown(&mut app, FormPart::Grade));
        assert!(!shown(&mut app, FormPart::QueuePick), "one queue needs no dropdown");
        assert!(!shown(&mut app, FormPart::Suggestions), "nothing typed, no dropdown");

        let search = {
            let world = app.world_mut();
            let mut fields = world.query::<(Entity, &Field)>();
            fields.iter(world).find(|(_, f)| f.0 == FieldKind::ParentQuery).unwrap().0
        };
        app.world_mut().get_mut::<EditableText>(search).unwrap().editor_mut().set_text("old");
        settle(&mut app);
        let offered = |app: &mut App| {
            let world = app.world_mut();
            world.query::<&Act>().iter(world).filter(|a| matches!(a, Act::PickParent(_))).cloned().collect::<Vec<_>>()
        };
        assert_eq!(offered(&mut app), [Act::PickParent("TRAIL-7".into())], "TRAIL-7 is titled \"old\"");
        assert!(shown(&mut app, FormPart::Suggestions));
        let floats = {
            let world = app.world_mut();
            let mut parts = world.query::<(&FormPart, &Node)>();
            parts.iter(world).find(|(p, _)| **p == FormPart::Suggestions).unwrap().1.position_type
        };
        assert_eq!(floats, PositionType::Absolute, "the suggestions float over the form rather than push it down");
        press(&mut app, Act::PickParent("TRAIL-7".into()));
        settle(&mut app);
        assert!(shown(&mut app, FormPart::Chosen) && !shown(&mut app, FormPart::Suggestions));
        assert!(!shown(&mut app, FormPart::Search), "the pick takes the search box's place");

        app.world_mut().resource_mut::<Form>().open.as_mut().unwrap().draft.queued = true;
        settle(&mut app);
        assert!(!shown(&mut app, FormPart::Parent) && shown(&mut app, FormPart::Grade));
    }

    #[test]
    fn a_form_with_a_save_in_flight_ignores_a_click_outside() {
        let mut app = ui_app();
        open_new_form(&mut app);
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
        open_new_form(&mut app);
        app.world_mut().resource_mut::<Form>().saving = true;
        press(&mut app, Act::Cancel);
        assert!(app.world().resource::<Form>().open.is_some());
    }
}
