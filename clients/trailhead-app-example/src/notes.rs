//! The example app's notes: a collection it keeps in the app data store, the pattern an app that keeps data copies.
//!
//! The shape is declared in `collections.toml` beside the crate's `Cargo.toml` (an apps-repo app has it at
//! `apps/<id>/collections.toml`), and the Rust side in [`crate::Example`]'s `collections`: the `notes` collection,
//! each record read as a [`Note`]. The shell mirrors the records as entities holding a `Record<Note>`; the page lists
//! them, in the order they were made, each with a button that removes it, and a field that adds one.
//!
//! The logic ([`NotesPlugin`], headless) turns [`AddNote`] and [`RemoveNote`] into commands, and keeps what the server
//! said of the last one in [`NotesProblem`]. Nothing is applied locally: a note added or removed comes back through
//! the shell's stream, and the list follows the mirror. The drawing ([`NotesUiPlugin`]) keeps the rows in step with
//! the mirror by key, in place.

use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::Activate;
use serde::{Deserialize, Serialize};
use trailhead_shell::ActiveApp;
use trailhead_shell::commands::{CommandDone, Outcome, Submit};
use trailhead_shell::connection::Connection;
use trailhead_shell::records::{self, Record, RecordMirror};
use trailhead_ui::{
    ButtonKind, Icon, Keyed, Notice, TextEntered, Tone, Truncated, button, extent, notice, row, set_text, size, space,
    text, text_dim, text_input, text_truncated, tool_button,
};
use uuid::Uuid;

use crate::EXAMPLE;

/// The collection, as `collections.toml` names it.
pub const NOTES: &str = "notes";

/// The app's `collections.toml`, which the shell reads for what is read-only and a test declares on its server.
pub const COLLECTIONS: &str = include_str!("../collections.toml");

/// One note, as a record's data reads: the fields `collections.toml` declares.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub text: String,
}

/// Add a note saying this. The field and its button trigger it; a test can trigger it with no page.
#[derive(Event, Debug, Clone)]
pub struct AddNote(pub String);

/// Remove the note with this record id. Its row's button triggers it.
#[derive(Event, Debug, Clone, Copy)]
pub struct RemoveNote(pub Uuid);

/// Why the last write didn't happen, in the server's words, until the next one does.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct NotesProblem(pub Option<String>);

/// The entity the notes' commands are sent from, whose answers come back to it.
#[derive(Resource)]
struct NotesWriter(Entity);

/// The notes' logic: what an add and a remove send, and what came of it.
pub struct NotesPlugin;

impl Plugin for NotesPlugin {
    fn build(&self, app: &mut App) {
        let writer = app.world_mut().spawn(Name::new("example notes")).id();
        app.insert_resource(NotesWriter(writer))
            .init_resource::<NotesProblem>()
            .add_observer(add_note)
            .add_observer(remove_note)
            .add_systems(Update, hear_answers);
    }
}

fn add_note(add: On<AddNote>, writer: Res<NotesWriter>, mut submit: MessageWriter<Submit>) {
    let text = add.0.trim();
    if text.is_empty() {
        return;
    }
    let request = records::create(EXAMPLE, NOTES, &Note { text: text.to_string() });
    submit.write(Submit { source: writer.0, request });
}

fn remove_note(
    remove: On<RemoveNote>,
    notes: Query<&Record<Note>>,
    writer: Res<NotesWriter>,
    mut submit: MessageWriter<Submit>,
) {
    if let Some(note) = notes.iter().find(|n| n.id == remove.0) {
        submit.write(Submit { source: writer.0, request: note.delete() });
    }
}

fn hear_answers(mut done: MessageReader<CommandDone>, writer: Res<NotesWriter>, mut problem: ResMut<NotesProblem>) {
    for answer in done.read().filter(|d| d.source == writer.0) {
        let said = match &answer.outcome {
            Outcome::Ok { .. } => None,
            Outcome::Conflict { .. } => Some("That note changed meanwhile; try again.".to_string()),
            Outcome::Forbidden(why) => Some(format!("Not allowed: {why}")),
            Outcome::RateLimited { message, .. } => Some(format!("Slow down ({message}).")),
            Outcome::Failed(why) => Some(why.clone()),
        };
        problem.set_if_neq(NotesProblem(said));
    }
}

/// The notes in the order they were made.
pub fn in_order<'a>(notes: impl Iterator<Item = &'a Record<Note>>) -> Vec<&'a Record<Note>> {
    let mut notes: Vec<_> = notes.filter(|n| n.app == EXAMPLE).collect();
    notes.sort_by_key(|n| (n.created_at, n.id));
    notes
}

/// What the line over the list says: whether the notes have loaded, and how many there are.
pub fn notes_words(loaded: bool, count: usize) -> String {
    match (loaded, count) {
        (false, 0) => "Not loaded yet".to_string(),
        (_, 0) => "No notes yet".to_string(),
        (_, 1) => "1 note".to_string(),
        (_, n) => format!("{n} notes"),
    }
}

/// The notes' drawing: the section under the count, kept in step with the mirror.
pub struct NotesUiPlugin;

impl Plugin for NotesUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoteRows>()
            .add_observer(on_add_button)
            .add_observer(on_enter)
            .add_observer(on_remove)
            .add_systems(OnExit(ActiveApp(EXAMPLE)), forget_rows)
            .add_systems(
                Update,
                (sync_rows, show_line, show_problem, gate_writes).run_if(in_state(ActiveApp(EXAMPLE))),
            );
    }
}

/// The line that says whether the notes have loaded and how many there are.
#[derive(Component)]
struct NotesLine;

/// The node the rows are kept under: it holds nothing else.
#[derive(Component)]
struct NotesBody;

/// A row's text.
#[derive(Component, Clone, Default)]
struct NoteText;

/// A row's remove button, with the note it removes.
#[derive(Component, Clone, Default)]
struct NoteRemove(Uuid);

/// The add field's text entity.
#[derive(Component, Clone, Default)]
struct NoteField;

#[derive(Component)]
struct AddButton;

/// Where a refused write is said.
#[derive(Component)]
struct NotesNotice;

/// A row's view model, compared by `Keyed` to decide whether to update it.
#[derive(Debug, Clone, PartialEq)]
struct NoteRow {
    id: Uuid,
    text: String,
}

/// The rows' entities, by record id.
#[derive(Resource, Default)]
struct NoteRows(Keyed<NoteRow>);

/// Spawns the notes' section under `page`, empty: its rows follow the mirror from the next frame.
pub fn spawn_notes(commands: &mut Commands, page: Entity) {
    let section = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                width: percent(100),
                max_width: px(extent::SIDE_PANEL_WIDTH),
                row_gap: px(space::ROWS),
                ..default()
            },
            ChildOf(page),
        ))
        .id();
    commands.spawn_scene(text("Notes", size::HEADING)).insert(ChildOf(section));
    commands.spawn_scene(text_dim(notes_words(false, 0), size::SECONDARY)).insert((NotesLine, ChildOf(section)));
    commands.spawn((
        Node { flex_direction: FlexDirection::Column, row_gap: px(space::LIST_ITEMS), ..default() },
        NotesBody,
        ChildOf(section),
    ));
    let bar = commands.spawn_scene(bsn! { @row() Node { width: percent(100) } }).insert(ChildOf(section)).id();
    let holder = commands.spawn((Node { flex_grow: 1.0, min_width: px(0), ..default() }, ChildOf(bar))).id();
    commands.spawn_scene(text_input(1.0, bsn! { NoteField })).insert(ChildOf(holder));
    commands.spawn_scene(button("Add", Some(Icon::Plus), ButtonKind::Primary)).insert((AddButton, ChildOf(bar)));
    commands.spawn_scene(notice()).insert((NotesNotice, ChildOf(section)));
}

fn note_row(note: &NoteRow) -> impl Scene {
    let (id, words) = (note.id, note.text.clone());
    bsn! {
        @row()
        Node { width: percent(100) }
        Children [
            @text_truncated(words, size::BODY) NoteText
            --
            @tool_button(Icon::X, "Remove", ButtonKind::Plain) NoteRemove({ id })
        ]
    }
}

/// One row per note, in the order they were made, spawned, moved, updated or despawned by key; nothing else is
/// touched. Runs only when a note changed, or the page is new.
fn sync_rows(
    notes: Query<Ref<Record<Note>>>,
    mut removed: RemovedComponents<Record<Note>>,
    body: Query<(Entity, Ref<NotesBody>)>,
    children: Query<&Children>,
    mut texts: Query<&mut Truncated, With<NoteText>>,
    mut rows: ResMut<NoteRows>,
    mut commands: Commands,
) {
    let gone = removed.read().count() > 0;
    let Ok((body, new)) = body.single() else { return };
    if !(gone || new.is_added() || notes.iter().any(|n| n.is_changed())) {
        return;
    }
    let wanted: Vec<(String, NoteRow)> = in_order(notes.iter().map(|n| n.into_inner()))
        .into_iter()
        .map(|n| (n.id.to_string(), NoteRow { id: n.id, text: n.data.text.clone() }))
        .collect();
    rows.0.sync(
        &mut commands,
        &[(body, wanted)],
        |commands, _, row| commands.spawn_scene(note_row(row)).id(),
        |_, entity, row| {
            for e in children.iter_descendants(entity) {
                if let Ok(mut text) = texts.get_mut(e) {
                    text.full.clone_from(&row.text);
                }
            }
        },
    );
}

/// The page went, and its rows with it.
fn forget_rows(mut rows: ResMut<NoteRows>) {
    rows.0.forget();
}

/// Keeps the line over the list in step, in place.
fn show_line(
    mirror: Option<Res<RecordMirror>>,
    notes: Query<&Record<Note>>,
    mut lines: Query<&mut Text, With<NotesLine>>,
) {
    let loaded = mirror.is_some_and(|m| m.loaded(EXAMPLE, NOTES));
    let words = notes_words(loaded, in_order(notes.iter()).len());
    for line in &mut lines {
        if line.0 != words {
            set_text(line, &words);
        }
    }
}

/// Says why the last write didn't happen, and nothing once one does.
fn show_problem(
    problem: Res<NotesProblem>,
    added: Query<(), Added<NotesNotice>>,
    mut notices: Query<&mut Notice, With<NotesNotice>>,
) {
    if !problem.is_changed() && added.is_empty() {
        return;
    }
    let says = Notice::new(Tone::Error, problem.0.clone().unwrap_or_default());
    for mut notice in &mut notices {
        notice.set_if_neq(says.clone());
    }
}

/// The buttons that send a write.
type WriteButtons = Or<(With<AddButton>, With<NoteRemove>)>;

/// Add and Remove send a write: they are disabled while the connection isn't live, or the shell has none.
fn gate_writes(
    connection: Option<Res<State<Connection>>>,
    buttons: Query<(Entity, Has<InteractionDisabled>), WriteButtons>,
    mut commands: Commands,
) {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    for (entity, disabled) in &buttons {
        if live && disabled {
            commands.entity(entity).remove::<InteractionDisabled>();
        } else if !live && !disabled {
            commands.entity(entity).insert(InteractionDisabled);
        }
    }
}

fn on_add_button(
    activate: On<Activate>,
    buttons: Query<(), With<AddButton>>,
    mut fields: Query<&mut EditableText, With<NoteField>>,
    mut commands: Commands,
) {
    if !buttons.contains(activate.entity) {
        return;
    }
    for mut field in &mut fields {
        commands.trigger(AddNote(field.value().to_string()));
        field.editor_mut().set_text("");
    }
}

fn on_enter(entered: On<TextEntered>, mut fields: Query<&mut EditableText, With<NoteField>>, mut commands: Commands) {
    if let Ok(mut field) = fields.get_mut(entered.entity) {
        commands.trigger(AddNote(entered.value.clone()));
        field.editor_mut().set_text("");
    }
}

fn on_remove(activate: On<Activate>, buttons: Query<&NoteRemove>, mut commands: Commands) {
    if let Ok(remove) = buttons.get(activate.entity) {
        commands.trigger(RemoveNote(remove.0));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trailhead_shell::ShellPlugin;
    use trailhead_shell::commands::Method;
    use trailhead_shell::fixture::{app_event, id, record_json};
    use trailhead_shell::testing::{
        Other, answer, feed_record, fill_records, headless, sent, set_connection, windowed,
    };

    use super::*;
    use crate::Example;

    fn note(name: &str, text: &str, version: i64) -> serde_json::Value {
        record_json(json!({"id": name, "collection": NOTES, "data": {"text": text}, "version": version}))
    }

    fn problem(app: &App) -> Option<String> {
        app.world().resource::<NotesProblem>().0.clone()
    }

    #[test]
    fn an_add_and_a_remove_are_sent_as_commands_and_a_refusal_is_kept_to_show() {
        let mut app = headless(ShellPlugin::new().with(Example));
        set_connection(&mut app, Connection::Live);
        fill_records(&mut app, EXAMPLE, vec![(NOTES, vec![note("n1", "milk", 3)])]);

        app.world_mut().trigger(AddNote("  eggs ".into()));
        app.world_mut().trigger(AddNote("   ".into()));
        app.update();
        let out = sent(&mut app);
        assert_eq!(out.len(), 1, "an empty note sends nothing");
        let request = &out[0].request;
        assert_eq!((request.method(), request.call.path().as_str()), (Method::Post, "/v1/apps/example/records/notes"));
        assert_eq!(request.call.body().unwrap()["data"], json!({"text": "eggs"}));

        app.world_mut().trigger(RemoveNote(id("n1")));
        app.update();
        let out = sent(&mut app);
        let request = &out[0].request;
        assert_eq!(
            (request.method(), request.call.path()),
            (Method::Delete, format!("/v1/apps/example/records/notes/{}", id("n1")))
        );
        assert_eq!(request.if_match, Some(3), "removed only if it is still the note shown");

        answer(&mut app, out[0].source, Outcome::Forbidden("no".into()));
        assert_eq!(problem(&app).as_deref(), Some("Not allowed: no"));
        answer(&mut app, out[0].source, Outcome::Ok { version: None });
        assert_eq!(problem(&app), None);
    }

    #[test]
    fn the_line_tells_not_loaded_from_none() {
        assert_eq!(notes_words(false, 0), "Not loaded yet");
        assert_eq!(notes_words(true, 0), "No notes yet");
        assert_eq!(notes_words(true, 1), "1 note");
        assert_eq!(notes_words(true, 3), "3 notes");
    }

    /// Each row's text, and the entity holding it, in the order shown.
    fn rows(app: &mut App) -> Vec<(Entity, String)> {
        let world = app.world_mut();
        let body = world.query_filtered::<&Children, With<NotesBody>>().single(world).unwrap().to_vec();
        let mut texts = world.query::<(&ChildOf, &Truncated)>();
        body.iter()
            .map(|row| {
                let text = texts.iter(world).find(|(parent, _)| parent.parent() == *row).unwrap().1.full.clone();
                (*row, text)
            })
            .collect()
    }

    fn line(app: &mut App) -> String {
        let world = app.world_mut();
        world.query_filtered::<&Text, With<NotesLine>>().single(world).unwrap().0.clone()
    }

    #[test]
    fn a_note_changed_added_or_removed_elsewhere_is_shown_in_place() {
        let mut app = windowed(ShellPlugin::new().with(Example).with(Other));
        assert_eq!(line(&mut app), "Not loaded yet");
        fill_records(&mut app, EXAMPLE, vec![(NOTES, vec![note("n1", "milk", 1), note("n2", "bread", 1)])]);
        assert_eq!(line(&mut app), "2 notes");
        let before = rows(&mut app);
        assert_eq!(before.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(), ["milk", "bread"]);
        let all = |app: &mut App| {
            let world = app.world_mut();
            let mut all: Vec<Entity> = world.query::<Entity>().iter(world).collect();
            all.sort();
            all
        };
        let everything = all(&mut app);

        let changed = note("n1", "oat milk", 2);
        feed_record(
            &mut app,
            EXAMPLE,
            app_event(
                json!({"id": 1, "collection": NOTES, "record_id": "n1", "before": note("n1", "milk", 1), "after": changed}),
            ),
        );
        let after = rows(&mut app);
        assert_eq!(after, [(before[0].0, "oat milk".to_string()), before[1].clone()], "the same rows");
        assert_eq!(all(&mut app), everything, "nothing spawned or despawned");

        let mut added = note("n3", "jam", 1);
        added["created_at"] = json!("2026-10-02T00:00:00Z");
        feed_record(
            &mut app,
            EXAMPLE,
            app_event(json!({"id": 2, "collection": NOTES, "record_id": "n3", "after": added})),
        );
        feed_record(
            &mut app,
            EXAMPLE,
            app_event(json!({"id": 3, "collection": NOTES, "record_id": "n2", "before": note("n2", "bread", 1)})),
        );
        let last = rows(&mut app);
        assert_eq!(last.len(), 2);
        assert_eq!(last[0], (before[0].0, "oat milk".to_string()));
        assert_eq!(last[1].1, "jam");
        assert_eq!(line(&mut app), "2 notes");
    }
}
