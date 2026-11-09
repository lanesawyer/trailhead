//! Creating a project from the sidebar's Projects heading, and editing one from its page: a modal with the project's
//! key, name, brief, whether its work needs review, and how long a claim lasts. Editing shows the same form filled in,
//! without the key, which can't change.
//!
//! As with the task form (`author`), the form is a resource, [`ProjectForm`], and [`request`] turns what is open into
//! the one request that saves it; it is pure, so it is tested without a window. Saving sends a [`Submit`] with the
//! modal as the source. An `Ok` closes the form (and opens a new project's page); anything else stays on the form to be
//! fixed. Nothing is applied locally: the event stream brings the change to the sidebar and the page.

use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;
use trailhead_client::http::{self as client, Call, ProjectUpdate};

use trailhead_model::{Project, ProjectStatus};

use crate::author::describe;
use crate::commands::{CommandDone, Outcome, Request, Submit};
use crate::connection::Connection;
use crate::mirror::Mirrored;
use crate::project::{OpenProject, ProjectPage};
use crate::widgets::{
    ButtonKind, Icon, InitialText, Modal, ModalClosed, ModalFocus, ModalLocked, ModalSlots, Picked, button,
    button_group, field, open_modal, size, text_dim, text_input,
};

/// Lines tall the brief's box is.
const BRIEF_LINES: f32 = 8.0;
/// The longest claim lease the server takes, in minutes (24 hours).
const MAX_CLAIM_MINUTES: u32 = 24 * 60;
/// The review choice's options, in order: not required, then required.
const REVIEW_OPTIONS: [&str; 2] = ["Not required", "Required"];

/// What is typed into the form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectDraft {
    pub key: String,
    pub name: String,
    pub brief: String,
    /// Whether finished work waits for a reviewer's approval.
    pub require_review: bool,
    /// Minutes a claim lasts before it lapses; empty for the workspace's default.
    pub claim_minutes: String,
}

/// What saving does: create a project, or change the one with `key`, as it was at `version` (sent as `If-Match`), and
/// archived or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Create,
    Edit { key: String, version: i64, archived: bool },
}

/// The form as it is open: what it saves to, what it started with, and what is typed now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectOpen {
    pub target: Target,
    /// So an edit sends only what changed.
    pub original: ProjectDraft,
    pub draft: ProjectDraft,
}

impl ProjectOpen {
    pub fn create() -> Self {
        ProjectOpen { target: Target::Create, original: ProjectDraft::default(), draft: ProjectDraft::default() }
    }

    /// An edit of `project`, as the mirror holds it.
    pub fn edit(project: &Project) -> Self {
        let draft = ProjectDraft {
            key: project.key.clone(),
            name: project.name.clone(),
            brief: project.brief.clone(),
            require_review: project.require_review,
            claim_minutes: project.claim_ttl.map(|s| (s / 60).max(1).to_string()).unwrap_or_default(),
        };
        let target = Target::Edit {
            key: project.key.clone(),
            version: project.version,
            archived: project.status == ProjectStatus::Archived,
        };
        ProjectOpen { target, original: draft.clone(), draft }
    }

    /// What the modal's heading says.
    fn title(&self) -> String {
        match &self.target {
            Target::Create => "New project".into(),
            Target::Edit { key, .. } => format!("Edit {key}"),
        }
    }
}

/// The form on screen. `generation` changes whenever it opens or closes, which is when the modal is redrawn.
#[derive(Resource, Default, Debug)]
pub struct ProjectForm {
    pub open: Option<ProjectOpen>,
    /// The last refusal, or what is in progress.
    pub message: String,
    /// The save awaiting its answer, so it can't be sent twice.
    pub saving: bool,
    pub generation: u64,
}

impl ProjectForm {
    pub fn open(&mut self, open: ProjectOpen) {
        self.open = Some(open);
        self.reset();
    }

    pub fn close(&mut self) {
        self.open = None;
        self.reset();
    }

    fn reset(&mut self) {
        self.message.clear();
        self.saving = false;
        self.generation += 1;
    }
}

/// Whether `key` is one the server takes: 2 to 10 uppercase letters or digits, starting with a letter.
fn valid_key(key: &str) -> bool {
    (2..=10).contains(&key.len())
        && key.starts_with(|c: char| c.is_ascii_uppercase())
        && key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// The key as it will be saved: trimmed and in capitals, so `trail` is `TRAIL`.
fn key_of(draft: &ProjectDraft) -> String {
    draft.key.trim().to_ascii_uppercase()
}

/// The claim lease typed as minutes, in seconds: `None` when empty, for the workspace's default.
fn lease_seconds(minutes: &str) -> Result<Option<u32>, String> {
    let minutes = minutes.trim();
    if minutes.is_empty() {
        return Ok(None);
    }
    match minutes.parse::<u32>() {
        Ok(m) if (1..=MAX_CLAIM_MINUTES).contains(&m) => Ok(Some(m * 60)),
        _ => Err(format!("a claim lasts 1 to {MAX_CLAIM_MINUTES} minutes, or leave it empty")),
    }
}

/// The request that saves `open`, or why it can't be sent yet.
pub fn request(open: &ProjectOpen) -> Result<Request, String> {
    let d = &open.draft;
    match &open.target {
        Target::Create => {
            let key = key_of(d);
            if !valid_key(&key) {
                return Err("the key is 2 to 10 letters or digits, starting with a letter, e.g. TRAIL".into());
            }
            let new = client::NewProject {
                key,
                name: d.name.trim().into(),
                brief: d.brief.clone(),
                require_review: d.require_review,
                claim_ttl: lease_seconds(&d.claim_minutes)?.map(i64::from),
            };
            Ok(Request::new(Call::CreateProject(new), None))
        }
        Target::Edit { key, version, .. } => {
            let o = &open.original;
            let mut patch = ProjectUpdate::default();
            if d.name.trim() != o.name {
                patch.name = Some(d.name.trim().into());
            }
            if d.brief != o.brief {
                patch.brief = Some(d.brief.clone());
            }
            if d.require_review != o.require_review {
                patch.require_review = Some(d.require_review);
            }
            // Compared as typed: a lease the form shows rounded to minutes isn't sent back unless it was changed.
            if d.claim_minutes.trim() != o.claim_minutes {
                // `null` goes back to the workspace's default.
                patch.claim_ttl = Some(lease_seconds(&d.claim_minutes)?.map(i64::from));
            }
            if patch == ProjectUpdate::default() {
                return Err("nothing changed".into());
            }
            Ok(Request::new(Call::UpdateProject { key: key.clone(), update: patch }, Some(*version)))
        }
    }
}

/// The request that archives the project `open` edits, or brings it back when it is archived; nothing for a new one.
pub fn archive_request(open: &ProjectOpen) -> Option<Request> {
    let Target::Edit { key, version, archived } = &open.target else { return None };
    let status = if *archived { ProjectStatus::Active } else { ProjectStatus::Archived };
    let update = ProjectUpdate { status: Some(status), ..Default::default() };
    Some(Request::new(Call::UpdateProject { key: key.clone(), update }, Some(*version)))
}

/// The sidebar's button that opens the form on a new project.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct NewProject;

/// The project page's button that opens the form on the project it shows.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct EditProject;

/// What a button of the form does.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FormAct {
    #[default]
    Save,
    Cancel,
    /// Archive the project, or unarchive it.
    Archive,
}

/// Which draft field a text input holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FieldKind {
    #[default]
    Key,
    Name,
    Brief,
    ClaimMinutes,
}

/// A text input of the form, and the generation it was drawn for: a closing form's inputs go late and must not feed
/// the next one's draft.
#[derive(Component, Debug, Clone, Copy, Default)]
struct ProjectField(FieldKind, u64);

/// The review choice.
#[derive(Component, Debug, Clone, Copy, Default)]
struct ReviewPick;

/// The text showing the form's last message.
#[derive(Component, Debug, Clone, Copy, Default)]
struct ProjectFormMessage;

/// The form's modal, and the source of its commands. `title` is what its heading says.
#[derive(Component, Default)]
pub struct ProjectFormPanel {
    pub title: String,
}

impl Modal for ProjectFormPanel {
    fn title(&self) -> String {
        self.title.clone()
    }
}

fn field_value(draft: &mut ProjectDraft, kind: FieldKind) -> &mut String {
    match kind {
        FieldKind::Key => &mut draft.key,
        FieldKind::Name => &mut draft.name,
        FieldKind::Brief => &mut draft.brief,
        FieldKind::ClaimMinutes => &mut draft.claim_minutes,
    }
}

fn on_new_project(activate: On<Activate>, buttons: Query<(), With<NewProject>>, mut form: ResMut<ProjectForm>) {
    if buttons.contains(activate.entity) {
        form.open(ProjectOpen::create());
    }
}

fn on_edit_project(
    activate: On<Activate>,
    buttons: Query<(), With<EditProject>>,
    page: Res<ProjectPage>,
    projects: Query<&Mirrored<Project>>,
    mut form: ResMut<ProjectForm>,
) {
    if !buttons.contains(activate.entity) {
        return;
    }
    let Some(key) = &page.0 else { return };
    if let Some(project) = projects.iter().find(|p| p.0.key == *key) {
        form.open(ProjectOpen::edit(&project.0));
    }
}

fn on_form_act(
    activate: On<Activate>,
    acts: Query<&FormAct>,
    mut form: ResMut<ProjectForm>,
    connection: Res<State<Connection>>,
    panel: Query<Entity, With<ProjectFormPanel>>,
    mut submit: MessageWriter<Submit>,
) {
    let Ok(act) = acts.get(activate.entity) else { return };
    match act {
        // A save in flight can't be withdrawn, so the form stays until it is answered.
        FormAct::Cancel if !form.saving => form.close(),
        FormAct::Cancel => {}
        FormAct::Save => {
            let Some(open) = &form.open else { return };
            if form.saving {
                return;
            }
            if *connection.get() != Connection::Live {
                form.message = "Not connected to the server; saving waits for Live.".into();
                return;
            }
            let request = request(open);
            send(&mut form, request, &panel, &mut submit);
        }
        FormAct::Archive => {
            let Some(open) = &form.open else { return };
            if form.saving {
                return;
            }
            if *connection.get() != Connection::Live {
                form.message = "Not connected to the server; archiving waits for Live.".into();
                return;
            }
            let Some(request) = archive_request(open) else { return };
            send(&mut form, Ok(request), &panel, &mut submit);
        }
    }
}

/// Sends `request` from the form's modal, or shows why it can't be sent.
fn send(
    form: &mut ProjectForm,
    request: Result<Request, String>,
    panel: &Query<Entity, With<ProjectFormPanel>>,
    submit: &mut MessageWriter<Submit>,
) {
    match request {
        Err(why) => form.message = why,
        Ok(request) => {
            let Ok(source) = panel.single() else { return };
            submit.write(Submit { source, request });
            form.saving = true;
            form.message = "Saving...".into();
        }
    }
}

fn on_review_pick(picked: On<Picked>, picks: Query<(), With<ReviewPick>>, mut form: ResMut<ProjectForm>) {
    if !picks.contains(picked.entity) {
        return;
    }
    if let Some(open) = &mut form.open {
        open.draft.require_review = picked.index == 1;
    }
}

/// A close request (the X, Escape, a click outside) closes the form too; the modal is already going.
fn on_modal_closed(closed: On<ModalClosed>, panels: Query<(), With<ProjectFormPanel>>, mut form: ResMut<ProjectForm>) {
    if panels.contains(closed.modal) {
        form.close();
    }
}

/// Copies what is typed into the draft.
fn read_fields(mut form: ResMut<ProjectForm>, fields: Query<(&ProjectField, &EditableText)>) {
    let Some(open) = &form.open else { return };
    let mut typed = open.draft.clone();
    for (ProjectField(kind, generation), text) in &fields {
        if *generation == form.generation {
            *field_value(&mut typed, *kind) = text.value().to_string();
        }
    }
    if typed != open.draft {
        form.open.as_mut().expect("checked above").draft = typed;
    }
}

/// The save's answer: `Ok` closes the form (and opens a new project), anything else stays on it.
fn finish(
    mut done: MessageReader<CommandDone>,
    panel: Query<Entity, With<ProjectFormPanel>>,
    mut form: ResMut<ProjectForm>,
    mut commands: Commands,
) {
    for d in done.read() {
        if !panel.contains(d.source) || !form.saving {
            continue;
        }
        if matches!(d.outcome, Outcome::Ok { .. }) {
            if let Some(open) = &form.open
                && open.target == Target::Create
            {
                commands.trigger(OpenProject(key_of(&open.draft)));
            }
            form.close();
        } else {
            form.saving = false;
            form.message = describe(&d.outcome);
        }
    }
}

/// The form's logic, without drawing.
pub struct ProjectFormPlugin;

impl Plugin for ProjectFormPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProjectForm>()
            .init_resource::<ProjectPage>()
            .add_observer(on_new_project)
            .add_observer(on_edit_project)
            .add_observer(on_form_act)
            .add_observer(on_review_pick)
            .add_observer(on_modal_closed)
            .add_systems(Update, (read_fields, finish).chain());
    }
}

/// Draws the form in a modal.
pub struct ProjectFormUiPlugin;

impl Plugin for ProjectFormUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ProjectFormPlugin)
            .add_systems(Update, (redraw_form, fill_form, lock_while_saving, update_message).chain());
    }
}

fn redraw_form(
    form: Res<ProjectForm>,
    panels: Query<Entity, With<ProjectFormPanel>>,
    mut commands: Commands,
    mut drawn: Local<u64>,
) {
    if *drawn == form.generation {
        return;
    }
    *drawn = form.generation;
    for e in &panels {
        commands.entity(e).despawn();
    }
    if let Some(open) = &form.open {
        open_modal(&mut commands, ProjectFormPanel { title: open.title() });
    }
}

/// A text input for `kind`, `lines` high, holding `initial` when it appears, with the keyboard in it when `focus`.
fn input(kind: FieldKind, generation: u64, initial: String, lines: f32, focus: bool) -> Box<dyn SceneList> {
    if focus {
        Box::new(
            bsn_list! { @text_input(lines, bsn! { ProjectField(kind, generation) InitialText(initial) ModalFocus }) },
        )
    } else {
        Box::new(bsn_list! { @text_input(lines, bsn! { ProjectField(kind, generation) InitialText(initial) }) })
    }
}

/// Fills a new form's modal: the fields in its body, the buttons and message in its footer.
fn fill_form(
    new: Query<&ModalSlots, (With<ProjectFormPanel>, Added<ModalSlots>)>,
    form: Res<ProjectForm>,
    mut commands: Commands,
) {
    let Some(open) = &form.open else { return };
    let (d, generation) = (&open.draft, form.generation);
    let creating = open.target == Target::Create;
    for slots in &new {
        let body = slots.body;
        let text_field = |commands: &mut Commands, label: &str, kind: FieldKind, initial: &str, lines: f32| {
            // The keyboard starts in the first field: the key for a new project, the name for an edit, whose key
            // can't change.
            let focus = kind == if creating { FieldKind::Key } else { FieldKind::Name };
            let input = input(kind, generation, initial.to_string(), lines, focus);
            commands.queue_spawn_scene(field(label.to_string(), input)).insert(ChildOf(body));
        };
        if creating {
            text_field(&mut commands, "Key (e.g. TRAIL: its tasks are TRAIL-1, TRAIL-2…)", FieldKind::Key, &d.key, 1.0);
        }
        text_field(&mut commands, "Name", FieldKind::Name, &d.name, 1.0);
        let brief = "Brief: what the project is for, for agents to read";
        text_field(&mut commands, brief, FieldKind::Brief, &d.brief, BRIEF_LINES);
        let options = REVIEW_OPTIONS.iter().map(|o| (o.to_string(), None)).collect();
        let review = Some(usize::from(d.require_review));
        commands
            .queue_spawn_scene(field(
                "Review of finished work",
                bsn_list! { @button_group(options, review) ReviewPick },
            ))
            .insert(ChildOf(body));
        let lease = "Claim lease in minutes (empty for the default)";
        text_field(&mut commands, lease, FieldKind::ClaimMinutes, &d.claim_minutes, 1.0);
        let footer = slots.footer;
        let save = if creating { "Create" } else { "Save" };
        commands.spawn_scene(bsn! { @button(save, None, ButtonKind::Primary) FormAct::Save }).insert(ChildOf(footer));
        commands
            .spawn_scene(bsn! { @button("Cancel", None, ButtonKind::Normal) FormAct::Cancel })
            .insert(ChildOf(footer));
        commands
            .spawn_scene(text_dim(form.message.clone(), size::SECONDARY))
            .insert((ProjectFormMessage, ChildOf(footer)));
        // In the footer's far corner, apart from saving. It is undone with the same button, so it asks for no
        // confirmation.
        if let Target::Edit { archived, .. } = &open.target {
            let (caption, icon) =
                if *archived { ("Unarchive", Icon::ArchiveRestore) } else { ("Archive", Icon::Archive) };
            commands
                .spawn_scene(bsn! {
                    @button(caption, Some(icon), ButtonKind::Normal)
                    Node { margin: { UiRect::left(Val::Auto) } }
                    FormAct::Archive
                })
                .insert(ChildOf(footer));
        }
    }
}

/// A save in flight can't be withdrawn, so the modal stays open until it is answered.
fn lock_while_saving(form: Res<ProjectForm>, panels: Query<Entity, With<ProjectFormPanel>>, mut commands: Commands) {
    if !form.is_changed() {
        return;
    }
    for e in &panels {
        if form.saving {
            commands.entity(e).insert(ModalLocked);
        } else {
            commands.entity(e).remove::<ModalLocked>();
        }
    }
}

fn update_message(form: Res<ProjectForm>, mut messages: Query<&mut Text, With<ProjectFormMessage>>) {
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
    use bevy::state::app::StatesPlugin;
    use reqwest::Method;
    use serde_json::json;

    use super::*;
    use crate::commands::{CommandId, CommandsPlugin, Outbox};

    use crate::fixture::project;

    fn draft(key: &str) -> ProjectDraft {
        ProjectDraft { key: key.into(), name: " Trailhead ".into(), brief: "the task engine".into(), ..default() }
    }

    fn create_request(draft: &ProjectDraft) -> Result<Request, String> {
        request(&ProjectOpen { draft: draft.clone(), ..ProjectOpen::create() })
    }

    fn creating(key: &str) -> ProjectOpen {
        ProjectOpen { draft: draft(key), ..ProjectOpen::create() }
    }

    fn trail() -> Project {
        project(serde_json::json!({"key": "TRAIL", "name": "Trailhead", "brief": "engine", "claim_ttl": 1800,
                                   "version": 7}))
    }

    #[test]
    fn edit_starts_filled_in_and_sends_only_what_changed_with_the_version_it_saw() {
        let mut open = ProjectOpen::edit(&trail());
        assert_eq!((open.draft.name.as_str(), open.draft.claim_minutes.as_str()), ("Trailhead", "30"));
        assert_eq!(request(&open).unwrap_err(), "nothing changed");
        open.draft.name = "Trailhead engine ".into();
        open.draft.require_review = true;
        let r = request(&open).unwrap();
        assert_eq!(
            (r.call.method(), r.call.path().as_str(), r.if_match),
            (Method::PATCH, "/v1/projects/TRAIL", Some(7))
        );
        assert_eq!(r.call.body(), Some(json!({"name": "Trailhead engine", "require_review": true})));
    }

    #[test]
    fn archive_flips_the_status_with_the_version_it_saw() {
        assert!(archive_request(&ProjectOpen::create()).is_none(), "a new project has nothing to archive");
        let r = archive_request(&ProjectOpen::edit(&trail())).unwrap();
        assert_eq!(
            (r.call.method(), r.call.path().as_str(), r.if_match),
            (Method::PATCH, "/v1/projects/TRAIL", Some(7))
        );
        assert_eq!(r.call.body(), Some(json!({"status": "archived"})));
        let mut shelved = trail();
        shelved.status = ProjectStatus::Archived;
        assert_eq!(
            archive_request(&ProjectOpen::edit(&shelved)).unwrap().call.body(),
            Some(json!({"status": "active"}))
        );
    }

    #[test]
    fn emptying_the_lease_goes_back_to_the_default() {
        let mut open = ProjectOpen::edit(&trail());
        open.draft.claim_minutes = "".into();
        assert_eq!(request(&open).unwrap().call.body(), Some(json!({"claim_ttl": null})));
        open.draft.claim_minutes = "60".into();
        assert_eq!(request(&open).unwrap().call.body(), Some(json!({"claim_ttl": 3600})));
        open.draft.claim_minutes = "lots".into();
        assert!(request(&open).unwrap_err().contains("minutes"));
    }

    #[test]
    fn a_lease_shown_rounded_is_not_sent_back_untouched() {
        let mut odd = trail();
        odd.claim_ttl = Some(90);
        let open = ProjectOpen::edit(&odd);
        assert_eq!(open.draft.claim_minutes, "1");
        assert_eq!(request(&open).unwrap_err(), "nothing changed");
    }

    #[test]
    fn create_sends_the_key_in_capitals_and_the_rest_as_typed() {
        let r = create_request(&draft(" trail ")).unwrap();
        assert_eq!((r.call.method(), r.call.path().as_str(), r.if_match), (Method::POST, "/v1/projects", None));
        assert_eq!(
            r.call.body(),
            Some(json!({"key": "TRAIL", "name": "Trailhead", "brief": "the task engine", "require_review": false}))
        );
    }

    #[test]
    fn a_key_the_server_would_refuse_is_caught_first() {
        for bad in ["", "T", "1ABC", "TRAIL-1", "ELEVENCHARS"] {
            assert!(create_request(&draft(bad)).unwrap_err().contains("key"), "{bad:?}");
        }
        assert!(create_request(&draft("OPS2")).is_ok());
    }

    #[test]
    fn the_claim_lease_goes_in_seconds_and_empty_leaves_the_default() {
        let mut d = draft("OPS");
        d.claim_minutes = " 45 ".into();
        d.require_review = true;
        let body = create_request(&d).unwrap().call.body().unwrap();
        assert_eq!((body["claim_ttl"].clone(), body["require_review"].clone()), (json!(2700), json!(true)));
        for bad in ["0", "abc", "1441", "-5"] {
            d.claim_minutes = bad.into();
            assert!(create_request(&d).unwrap_err().contains("minutes"), "{bad:?}");
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .init_state::<Connection>()
            .add_plugins((CommandsPlugin, ProjectFormPlugin));
        app.add_observer(|open: On<OpenProject>, mut opened: ResMut<Opened>| opened.0 = Some(open.0.clone()));
        app.init_resource::<Opened>();
        app.update();
        app
    }

    #[derive(Resource, Default)]
    struct Opened(Option<String>);

    fn press(app: &mut App, act: FormAct) {
        let button = app.world_mut().spawn(act).id();
        app.world_mut().trigger(Activate { entity: button });
    }

    #[test]
    fn the_button_opens_the_form_and_a_saved_project_opens_its_page() {
        let mut app = app();
        let button = app.world_mut().spawn(NewProject).id();
        app.world_mut().trigger(Activate { entity: button });
        assert_eq!(app.world().resource::<ProjectForm>().open.as_ref().unwrap().target, Target::Create);
        app.world_mut().resource_mut::<ProjectForm>().open = Some(creating("ops"));
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.update();
        let panel = app.world_mut().spawn(ProjectFormPanel::default()).id();
        press(&mut app, FormAct::Save);
        let form = app.world().resource::<ProjectForm>();
        assert!(form.saving, "{}", form.message);
        app.update();
        assert_eq!(app.world().resource::<Outbox>().len(), 1);
        press(&mut app, FormAct::Save);
        app.update();
        assert_eq!(app.world().resource::<Outbox>().len(), 1, "a save out isn't sent twice");
        app.world_mut().write_message(CommandDone {
            id: CommandId(0),
            source: panel,
            outcome: Outcome::Ok { version: Some(1) },
        });
        app.update();
        assert!(app.world().resource::<ProjectForm>().open.is_none(), "an Ok closes the form");
        assert_eq!(app.world().resource::<Opened>().0.as_deref(), Some("OPS"));
    }

    #[test]
    fn a_refusal_stays_on_the_form_and_says_why() {
        let mut app = app();
        app.world_mut().resource_mut::<ProjectForm>().open(creating("OPS"));
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.update();
        let panel = app.world_mut().spawn(ProjectFormPanel::default()).id();
        press(&mut app, FormAct::Save);
        app.world_mut().write_message(CommandDone {
            id: CommandId(0),
            source: panel,
            outcome: Outcome::Failed("project key OPS is taken".into()),
        });
        app.update();
        let form = app.world().resource::<ProjectForm>();
        assert!(form.open.is_some() && !form.saving);
        assert_eq!(form.message, "project key OPS is taken");
    }

    #[test]
    fn edit_opens_the_form_on_the_project_the_page_shows() {
        let mut app = app();
        app.insert_resource(ProjectPage(Some("TRAIL".into())));
        app.world_mut().spawn(Mirrored(trail()));
        let button = app.world_mut().spawn(EditProject).id();
        app.world_mut().trigger(Activate { entity: button });
        let form = app.world().resource::<ProjectForm>();
        let open = form.open.as_ref().unwrap();
        assert_eq!(open.target, Target::Edit { key: "TRAIL".into(), version: 7, archived: false });
        assert_eq!(open.title(), "Edit TRAIL");
    }

    #[test]
    fn picking_required_makes_review_required() {
        let mut app = app();
        app.world_mut().resource_mut::<ProjectForm>().open(ProjectOpen::create());
        let pick = app.world_mut().spawn(ReviewPick).id();
        app.world_mut().trigger(Picked { entity: pick, index: 1 });
        assert!(app.world().resource::<ProjectForm>().open.as_ref().unwrap().draft.require_review);
    }
}
