//! The Server settings page ([`crate::settings::SERVER`]): what the server's user config says, edited from the window.
//! Listed in a shell with a connection, after Connection.
//!
//! - A notice when the settings can't be shown (not connected, a server too old to offer them, a read that failed),
//!   when a save found the file changed on disk, and when the file as it stands is one the server would refuse.
//! - **Waiting for a restart**, shown while the file says something the running server doesn't do: what waits, and
//!   the command that restarts the server, with a copy button.
//! - **Window builds** (`[shell]`): the apps repo, the build profile, the jobs and how often the builder looks, each a
//!   field, and what the builder is doing now. Then the section's notice and its Save.
//! - **Factory** (`[factory]`): the repos it runs (a list to add to and take from), whether it runs, and the usage
//!   pauses. Then its notice and Save.
//! - **The file**: where it is, with a copy button, and that the server reads it when it starts.
//!
//! A setting at its default says so in its note. Everything is spawned once when the page opens and kept in step with
//! [`ServerSettings`] in place; the logic is [`crate::server_settings`]'s.

use bevy::clipboard::Clipboard;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::{Checked, InteractionDisabled};
use bevy::ui_widgets::{Activate, ValueChange};
use trailhead_ui::{
    ButtonKind, InitialText, Notice, SettingNote, Tone, UiPlace, ValueList, ValuesEdited, button, copy_block, extent,
    notice, row, set_display, set_text, setting, setting_toggle, size, spawn_settings_section, text, text_input,
    value_list,
};

use crate::ask::offline_reason;
use crate::connection::{Connection, ConnectionStatus};
use crate::server_settings::{
    Gate, Load, LoadServerSettings, SaveServerSettings, Section, ServerSettings, build_text, restart_command,
    restart_how, waiting_text,
};

/// A part of the page that shows something.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerPart {
    /// Why the settings can't be shown, or that the file changed on disk.
    #[default]
    Status,
    /// What the server would say of the file as it stands.
    Problem,
    /// The restart section, shown while something waits for one.
    Restart,
    /// What waits for the restart.
    Waiting,
    /// The command that restarts the server.
    RestartCommand,
    /// A section shown once the settings are read: builds, factory or the file.
    Loaded,
    /// The line about what the builder is doing.
    BuildState,
    /// The factory's settings, hidden when there are none to show.
    FactoryRows,
    /// Why there are no factory settings to show.
    FactoryAbsent,
    /// The file's path.
    Path,
}

/// A setting that is edited, on its control's text entity (or the switch, or the list).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerField {
    #[default]
    Repo,
    Profile,
    Jobs,
    PollSecs,
    Repos,
    Runs,
    FiveHour,
    SevenDay,
}

impl ServerField {
    /// What the setting does, said under it.
    fn note(self) -> &'static str {
        match self {
            ServerField::Repo => "The apps repo's checkout the window is built from. Empty: the server builds none.",
            ServerField::Profile => "The cargo profile a build uses.",
            ServerField::Jobs => {
                "How many jobs a build runs at once (cargo's -j). It shares the machine with the factory."
            }
            ServerField::PollSecs => "How often, in seconds, the builder looks for a new commit.",
            ServerField::Repos => {
                "Directories holding a .trailhead.toml with a [factory] table. A leading ~ is your home directory."
            }
            ServerField::Runs => "Off, the factory runs nothing, whatever repos are listed.",
            ServerField::FiveHour => {
                "At this percent of the five-hour usage window the factory starts no new runs. Empty: no pause."
            }
            ServerField::SevenDay => "The same for the seven-day window.",
        }
    }

    /// The file's key the setting is, as [`trailhead_model::config::ShellSettings::defaults`] names it.
    fn key(self) -> (Section, &'static str) {
        match self {
            ServerField::Repo => (Section::Builds, "repo"),
            ServerField::Profile => (Section::Builds, "profile"),
            ServerField::Jobs => (Section::Builds, "jobs"),
            ServerField::PollSecs => (Section::Builds, "poll_secs"),
            ServerField::Repos => (Section::Factory, "repos"),
            ServerField::Runs => (Section::Factory, "disabled"),
            ServerField::FiveHour | ServerField::SevenDay => (Section::Factory, "pause_at"),
        }
    }
}

/// On a setting's root: the note under it is this field's.
#[derive(Component, Debug, Clone, Copy)]
struct NoteOf(ServerField);

/// A section's notice: what the server said, why it can't be saved, or that it was.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionNotice(pub Section);

/// A section's Save.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveSection(pub Section);

/// A copy button.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CopyServer {
    #[default]
    Path,
    Restart,
}

/// What the page says of the file, under its path.
pub const FILE_NOTE: &str = "Changes are written to this file. The server reads it when it starts.";

/// Fills the page: run by the settings modal each time it is shown.
pub(crate) fn server_page(In(page): In<Entity>, settings: Res<ServerSettings>, mut commands: Commands) {
    commands.trigger(LoadServerSettings);
    commands.spawn_scene(notice()).insert((ServerPart::Status, ChildOf(page)));
    commands.spawn_scene(notice()).insert((ServerPart::Problem, ChildOf(page)));

    let restart = spawn_settings_section(&mut commands, "Waiting for a restart");
    commands.entity(restart.section).insert((ServerPart::Restart, ChildOf(page)));
    commands.spawn_scene(text("", size::BODY)).insert((ServerPart::Waiting, ChildOf(restart.rows)));
    let command = copyable(ServerPart::RestartCommand, "Restart the server", restart_how(false), CopyServer::Restart);
    commands.spawn_scene(command).insert((NoteOfRestart, ChildOf(restart.rows)));

    let builds = spawn_settings_section(&mut commands, "Window builds");
    commands.entity(builds.section).insert((ServerPart::Loaded, ChildOf(page)));
    let draft = &settings.builds;
    for (field, label, typed) in [
        (ServerField::Repo, "Apps repo", &draft.repo),
        (ServerField::Profile, "Build profile", &draft.profile),
        (ServerField::Jobs, "Jobs", &draft.jobs),
        (ServerField::PollSecs, "How often it looks (seconds)", &draft.poll_secs),
    ] {
        let control = bsn_list! {
            @text_input(1.0, bsn! { field InitialText({ typed.clone() }) })
            Node { max_width: { field_width(field) } }
        };
        commands.spawn_scene(setting(label, field.note(), control)).insert((NoteOf(field), ChildOf(builds.rows)));
    }
    let state = bsn_list! { @text("", size::BODY) ServerPart::BuildState };
    commands.spawn_scene(setting("Builds now", "", state)).insert(ChildOf(builds.rows));
    save_row(&mut commands, builds.rows, Section::Builds);

    let factory = spawn_settings_section(&mut commands, "Factory");
    commands.entity(factory.section).insert((ServerPart::Loaded, ChildOf(page)));
    commands.spawn_scene(notice()).insert((ServerPart::FactoryAbsent, ChildOf(factory.rows)));
    let rows = commands
        .spawn_scene(trailhead_ui::settings_page())
        .insert((ServerPart::FactoryRows, ChildOf(factory.rows)))
        .id();
    let draft = &settings.factory;
    let repos =
        bsn_list! { @value_list(draft.repos.clone(), "No repos: the factory runs nothing.") ServerField::Repos };
    let note = ServerField::Repos.note();
    commands.spawn_scene(setting("Repos it runs", note, repos)).insert((NoteOf(ServerField::Repos), ChildOf(rows)));
    let runs = setting_toggle("Run the factory", ServerField::Runs.note(), draft.on, bsn! { ServerField::Runs });
    commands.spawn_scene(runs).insert((NoteOf(ServerField::Runs), ChildOf(rows)));
    for (field, label, typed) in [
        (ServerField::FiveHour, "Pause at five-hour usage (%)", &draft.five_hour),
        (ServerField::SevenDay, "Pause at seven-day usage (%)", &draft.seven_day),
    ] {
        let control = bsn_list! {
            @text_input(1.0, bsn! { field InitialText({ typed.clone() }) })
            Node { max_width: { field_width(field) } }
        };
        commands.spawn_scene(setting(label, field.note(), control)).insert((NoteOf(field), ChildOf(rows)));
    }
    save_row(&mut commands, rows, Section::Factory);

    let file = spawn_settings_section(&mut commands, "The file");
    commands.entity(file.section).insert((ServerPart::Loaded, ChildOf(page)));
    let path = copyable(ServerPart::Path, "User config", FILE_NOTE, CopyServer::Path);
    commands.spawn_scene(path).insert(ChildOf(file.rows));
}

/// How wide a field's input is: as wide as what is typed in it runs, a path taking the page's whole measure.
fn field_width(field: ServerField) -> Val {
    match field {
        ServerField::Profile => px(extent::FIELD_MEDIUM),
        ServerField::Jobs | ServerField::PollSecs | ServerField::FiveHour | ServerField::SevenDay => {
            px(extent::FIELD_SHORT)
        }
        ServerField::Repo | ServerField::Repos | ServerField::Runs => Val::Auto,
    }
}

/// On the restart command's setting: its note says how to restart.
#[derive(Component, Debug, Clone, Copy)]
struct NoteOfRestart;

/// A section's notice, then its Save.
fn save_row(commands: &mut Commands, rows: Entity, section: Section) {
    commands.spawn_scene(notice()).insert((SectionNotice(section), ChildOf(rows)));
    let bar = commands.spawn_scene(row()).insert(ChildOf(rows)).id();
    commands.spawn_scene(button("Save", None, ButtonKind::Primary)).insert((SaveSection(section), ChildOf(bar)));
}

/// A value in a box of its own, with the button that copies it.
fn copyable(part: ServerPart, label: &str, note: &str, copy: CopyServer) -> impl Scene {
    let hint = format!("Copy: {}", label.to_lowercase());
    let control = bsn_list! { @copy_block(bsn! { part }, hint, bsn! { copy }) };
    setting(label.to_string(), note.to_string(), control)
}

/// The page, for a shell with a connection.
pub(crate) struct ServerPageUiPlugin;

impl Plugin for ServerPageUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_save).add_observer(on_copy).add_observer(on_runs).add_observer(on_repos).add_systems(
            Update,
            (read_inputs, show_drafts, show_parts, show_notes, show_sections).chain().before(UiPlace),
        );
    }
}

fn on_save(activate: On<Activate>, saves: Query<&SaveSection>, mut commands: Commands) {
    if let Ok(SaveSection(section)) = saves.get(activate.entity) {
        commands.trigger(SaveServerSettings(*section));
    }
}

fn on_runs(change: On<ValueChange<bool>>, fields: Query<&ServerField>, mut settings: ResMut<ServerSettings>) {
    if fields.get(change.source).is_ok_and(|f| *f == ServerField::Runs) && settings.factory.on != change.value {
        settings.factory.on = change.value;
    }
}

fn on_repos(edited: On<ValuesEdited>, fields: Query<&ServerField>, mut settings: ResMut<ServerSettings>) {
    if fields.get(edited.entity).is_ok_and(|f| *f == ServerField::Repos) {
        settings.factory.repos = edited.values.clone();
    }
}

fn on_copy(
    activate: On<Activate>,
    copies: Query<&CopyServer>,
    settings: Res<ServerSettings>,
    clipboard: Option<ResMut<Clipboard>>,
) {
    let Ok(copy) = copies.get(activate.entity) else { return };
    let Some(config) = &settings.config else { return };
    let words = match copy {
        CopyServer::Path => config.path.clone(),
        CopyServer::Restart => restart_command(config.foreground).to_string(),
    };
    if let Some(mut clipboard) = clipboard
        && let Err(e) = clipboard.set_text(words)
    {
        warn!(error = %e, "couldn't copy to the clipboard");
    }
}

/// Copies what is typed into the drafts. An input that has just appeared is empty until the kit puts its initial text
/// in, so it is read from its first change on.
fn read_inputs(fields: Query<(&ServerField, Ref<EditableText>)>, mut settings: ResMut<ServerSettings>) {
    for (field, typed) in &fields {
        if typed.is_added() || !typed.is_changed() {
            continue;
        }
        let value = typed.value().to_string();
        if draft(&settings, *field).is_some_and(|d| d != value)
            && let Some(draft) = draft_mut(&mut settings, *field)
        {
            *draft = value;
        }
    }
}

fn draft_mut(settings: &mut ServerSettings, field: ServerField) -> Option<&mut String> {
    match field {
        ServerField::Repo => Some(&mut settings.builds.repo),
        ServerField::Profile => Some(&mut settings.builds.profile),
        ServerField::Jobs => Some(&mut settings.builds.jobs),
        ServerField::PollSecs => Some(&mut settings.builds.poll_secs),
        ServerField::FiveHour => Some(&mut settings.factory.five_hour),
        ServerField::SevenDay => Some(&mut settings.factory.seven_day),
        ServerField::Repos | ServerField::Runs => None,
    }
}

fn draft(settings: &ServerSettings, field: ServerField) -> Option<&str> {
    Some(match field {
        ServerField::Repo => &settings.builds.repo,
        ServerField::Profile => &settings.builds.profile,
        ServerField::Jobs => &settings.builds.jobs,
        ServerField::PollSecs => &settings.builds.poll_secs,
        ServerField::FiveHour => &settings.factory.five_hour,
        ServerField::SevenDay => &settings.factory.seven_day,
        ServerField::Repos | ServerField::Runs => return None,
    })
}

/// Shows a draft the server's answer replaced (a save, a file read again) in its field, its switch and its list.
fn show_drafts(
    settings: Res<ServerSettings>,
    mut inputs: Query<(&ServerField, &mut EditableText)>,
    mut lists: Query<(&ServerField, &mut ValueList)>,
    switches: Query<(Entity, &ServerField, Has<Checked>), Without<EditableText>>,
    mut commands: Commands,
) {
    if !settings.is_changed() {
        return;
    }
    for (field, mut typed) in &mut inputs {
        if let Some(wanted) = draft(&settings, *field)
            && typed.value() != wanted
        {
            typed.editor_mut().set_text(wanted);
            typed.queue_edit(bevy::text::TextEdit::TextEnd(false));
        }
    }
    for (_, mut list) in lists.iter_mut().filter(|(f, _)| **f == ServerField::Repos) {
        list.set_if_neq(ValueList { values: settings.factory.repos.clone() });
    }
    for (switch, field, checked) in &switches {
        if *field == ServerField::Runs && checked != settings.factory.on {
            if settings.factory.on {
                commands.entity(switch).insert(Checked);
            } else {
                commands.entity(switch).remove::<Checked>();
            }
        }
    }
}

/// The page's texts and notices, and which parts show.
fn show_parts(
    settings: Res<ServerSettings>,
    connection: Option<Res<State<Connection>>>,
    status: Option<Res<ConnectionStatus>>,
    mut parts: Query<(Entity, &ServerPart, Option<&mut Text>, Option<&mut Notice>)>,
    mut nodes: Query<&mut Node>,
) {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    let config = settings.config.as_ref().filter(|_| settings.load == Load::Loaded);
    let status = status_notice(&settings, live, offline_reason(status.as_deref()));
    let absent = match config {
        Some(c) if !c.factory_built => Notice::new(Tone::Info, "This server was built without the factory."),
        Some(c) if c.factory.is_none() => {
            Notice::new(Tone::Warning, "The file's [factory] table doesn't load, so it can't be edited here.")
        }
        _ => Notice::default(),
    };
    let problem = config.and_then(|c| c.problem.as_ref()).map_or_else(Notice::default, |p| {
        Notice::new(
            Tone::Warning,
            format!("As the file stands, the server would refuse it at its next start, or turn its builder off: {p}"),
        )
    });
    let waiting = config.filter(|c| c.restart_needed).map(|c| {
        format!(
            "The file says something the server isn't doing yet: {}. The server reads the file when it starts.",
            waiting_text(&c.waiting)
        )
    });
    for (entity, part, text, notice) in &mut parts {
        if let Some(mut notice) = notice {
            let says = match part {
                ServerPart::Status => &status,
                ServerPart::Problem => &problem,
                ServerPart::FactoryAbsent => &absent,
                _ => continue,
            };
            if *notice != *says {
                *notice = says.clone();
            }
            continue;
        }
        let words = match part {
            ServerPart::Restart => {
                set_display(&mut nodes, entity, waiting.is_some());
                continue;
            }
            ServerPart::Loaded => {
                set_display(&mut nodes, entity, config.is_some());
                continue;
            }
            ServerPart::FactoryRows => {
                set_display(&mut nodes, entity, config.is_some_and(|c| c.factory.is_some()));
                continue;
            }
            ServerPart::Waiting => waiting.clone().unwrap_or_default(),
            ServerPart::RestartCommand => config.map(|c| restart_command(c.foreground).to_string()).unwrap_or_default(),
            ServerPart::BuildState => build_text(settings.build.as_ref()),
            ServerPart::Path => config.map(|c| c.path.clone()).unwrap_or_default(),
            ServerPart::Status | ServerPart::Problem | ServerPart::FactoryAbsent => continue,
        };
        if let Some(text) = text {
            set_text(text, &words);
        }
    }
}

/// The notice at the page's top.
fn status_notice(settings: &ServerSettings, live: bool, offline: Option<String>) -> Notice {
    if !live {
        let why = offline.map_or("Not connected to the server.".into(), |r| format!("Not connected: {r}."));
        return Notice::new(Tone::Info, format!("{why} The server's settings show once it is."));
    }
    match &settings.load {
        Load::Loading => Notice::new(Tone::Info, "Reading the server's settings…"),
        Load::Missing => Notice::new(
            Tone::Info,
            "This server doesn't offer its settings to the window: it is older than the window, or reads no user \
             config. Edit its config.toml by hand.",
        ),
        Load::Failed(why) => Notice::new(Tone::Error, format!("Couldn't read the server's settings: {why}")),
        Load::Loaded if settings.changed_on_disk => Notice::new(
            Tone::Warning,
            "The file changed on disk since the page read it, so nothing was saved. The page now shows what the file \
             says.",
        ),
        Load::Loaded => Notice::default(),
    }
}

/// Each setting's note, saying when it is at its default, and the restart command's.
fn show_notes(
    settings: Res<ServerSettings>,
    roots: Query<(&NoteOf, &Children)>,
    restart: Query<&Children, With<NoteOfRestart>>,
    mut notes: Query<&mut Text, With<SettingNote>>,
) {
    if !settings.is_changed() {
        return;
    }
    let Some(config) = &settings.config else { return };
    for (NoteOf(field), children) in &roots {
        let (section, key) = field.key();
        let defaults = match section {
            Section::Builds => config.shell.as_ref().map(|s| &s.defaults),
            Section::Factory => config.factory.as_ref().map(|f| &f.defaults),
        };
        let default = defaults.is_some_and(|d| d.iter().any(|k| k == key));
        let words = if default { format!("{} (default)", field.note()) } else { field.note().to_string() };
        for child in children {
            if let Ok(text) = notes.get_mut(*child) {
                set_text(text, &words);
            }
        }
    }
    for children in &restart {
        for child in children {
            if let Ok(text) = notes.get_mut(*child) {
                set_text(text, restart_how(config.foreground));
            }
        }
    }
}

/// Each section's notice and its Save: enabled only with something to save over a live connection.
#[expect(clippy::too_many_arguments)]
fn show_sections(
    settings: Res<ServerSettings>,
    connection: Option<Res<State<Connection>>>,
    status: Option<Res<ConnectionStatus>>,
    mut notices: Query<(&SectionNotice, &mut Notice)>,
    saves: Query<(Entity, &SaveSection, Has<InteractionDisabled>)>,
    children: Query<&Children>,
    mut texts: Query<&mut Text>,
    mut commands: Commands,
) {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    let offline = offline_reason(status.as_deref());
    for (SectionNotice(section), mut notice) in &mut notices {
        let says = section_notice(&settings, *section, live, offline.clone());
        if *notice != says {
            *notice = says;
        }
    }
    for (entity, SaveSection(section), disabled) in &saves {
        let gate = settings.gate(*section, live, offline.clone());
        match (gate == Gate::Ready, disabled) {
            (true, true) => {
                commands.entity(entity).remove::<InteractionDisabled>();
            }
            (false, false) => {
                commands.entity(entity).insert(InteractionDisabled);
            }
            _ => {}
        }
        let caption = if gate == Gate::Saving { "Saving…" } else { "Save" };
        for inside in children.iter_descendants(entity) {
            if let Ok(text) = texts.get_mut(inside) {
                set_text(text, caption);
            }
        }
    }
}

/// What a section's notice says: the server's refusal, why it can't be saved, or that it was and what that waits for.
pub fn section_notice(settings: &ServerSettings, section: Section, live: bool, offline: Option<String>) -> Notice {
    let state = settings.save_state(section);
    if let Some(refusal) = &state.refusal {
        return Notice::new(Tone::Error, format!("The server refused this: {refusal}"));
    }
    if let Gate::Blocked(why) = settings.gate(section, live, offline) {
        return Notice::new(Tone::Info, why);
    }
    if state.saved {
        let prefix = match section {
            Section::Builds => "shell",
            Section::Factory => "factory",
        };
        let waits = settings.config.as_ref().is_some_and(|c| c.waiting.iter().any(|w| w.starts_with(prefix)));
        let words = if waits {
            "Saved to the file. The server uses it once it restarts: see above."
        } else {
            "Saved to the file. It is what the server already runs."
        };
        return Notice::new(Tone::Success, words);
    }
    Notice::default()
}

#[cfg(test)]
mod tests {
    use trailhead_model::config::ServerConfig;
    use trailhead_ui::{Appearance, ModalPlugin, ThemeChoice};

    use super::*;
    use crate::ShellPlugin;
    use crate::server_settings::{SectionState, SettingsRead};
    use crate::settings::{OpenSettings, SERVER};
    use crate::testing::{Other, set_connection, settle, windowed};

    fn config() -> ServerConfig {
        serde_json::from_value(crate::server_settings::tests::reply()).unwrap()
    }

    fn app() -> App {
        let mut app = windowed(ShellPlugin::new().with(Other));
        app.insert_resource(Appearance::new(ThemeChoice::Desktop)).add_plugins(ModalPlugin);
        set_connection(&mut app, Connection::Live);
        app.world_mut().trigger(OpenSettings { page: Some(SERVER) });
        settle(&mut app);
        app.world_mut().write_message(SettingsRead { seq: 1, result: Ok((config(), None)) });
        settle(&mut app);
        app
    }

    fn field(app: &mut App, wanted: ServerField) -> Entity {
        let world = app.world_mut();
        world.query::<(Entity, &ServerField)>().iter(world).find(|(_, f)| **f == wanted).unwrap().0
    }

    fn typed(app: &App, entity: Entity) -> String {
        app.world().get::<EditableText>(entity).unwrap().value().to_string()
    }

    fn section(app: &mut App, wanted: Section) -> (Notice, bool) {
        let world = app.world_mut();
        let notice = world.query::<(&SectionNotice, &Notice)>().iter(world).find(|(s, _)| s.0 == wanted).unwrap().1;
        let notice = notice.clone();
        let save = world.query::<(&SaveSection, Has<InteractionDisabled>)>().iter(world).find(|(s, _)| s.0 == wanted);
        (notice, !save.unwrap().1)
    }

    #[test]
    fn the_page_shows_the_file_and_saves_only_an_edit() {
        let mut app = app();
        let jobs = field(&mut app, ServerField::Jobs);
        assert_eq!(typed(&app, jobs), "3");
        assert_eq!(section(&mut app, Section::Builds), (Notice::default(), false), "nothing changed, nothing to save");
        let mut notes = app.world_mut().query_filtered::<&Text, With<SettingNote>>();
        let words: Vec<String> = notes.iter(app.world()).map(|t| t.0.clone()).collect();
        assert!(words.iter().any(|w| w.starts_with("How many jobs") && w.ends_with("(default)")), "{words:?}");
        assert!(words.iter().any(|w| w == ServerField::Repo.note()), "the repo is set in the file: {words:?}");

        app.world_mut().get_mut::<EditableText>(jobs).unwrap().editor_mut().set_text("4");
        settle(&mut app);
        assert_eq!(app.world().resource::<ServerSettings>().builds.jobs, "4");
        assert!(section(&mut app, Section::Builds).1, "an edit can be saved");

        app.world_mut().resource_mut::<ServerSettings>().builds_save =
            SectionState { refusal: Some("[shell] jobs must be at least 1".into()), ..Default::default() };
        settle(&mut app);
        let (notice, _) = section(&mut app, Section::Builds);
        assert_eq!(notice, Notice::new(Tone::Error, "The server refused this: [shell] jobs must be at least 1"));
        assert_eq!(typed(&app, jobs), "4", "what was typed stays");
    }

    #[test]
    fn a_save_that_waits_for_a_restart_says_how_to_restart() {
        let mut app = app();
        let mut saved = config();
        saved.restart_needed = true;
        saved.waiting = vec!["shell.jobs".into()];
        saved.shell.as_mut().unwrap().jobs = 4;
        {
            let mut settings = app.world_mut().resource_mut::<ServerSettings>();
            settings.builds.jobs = "4".into();
            settings.config = Some(saved);
            settings.builds_save = SectionState { saved: true, ..Default::default() };
        }
        settle(&mut app);
        let world = app.world_mut();
        let texts: Vec<(ServerPart, String)> =
            world.query::<(&ServerPart, &Text)>().iter(world).map(|(p, t)| (*p, t.0.clone())).collect();
        let shown = |part: ServerPart| texts.iter().find(|(p, _)| *p == part).map(|(_, t)| t.clone()).unwrap();
        assert_eq!(shown(ServerPart::RestartCommand), "trailhead server restart");
        assert!(shown(ServerPart::Waiting).contains("the jobs"), "{}", shown(ServerPart::Waiting));
        let restart = world.query::<(Entity, &ServerPart)>().iter(world).find(|(_, p)| **p == ServerPart::Restart);
        assert_ne!(world.get::<Node>(restart.unwrap().0).unwrap().display, Display::None);
        let (notice, can_save) = section(&mut app, Section::Builds);
        assert_eq!(notice.tone, Tone::Success);
        assert!(!can_save, "what was saved is what the file says");
    }
}
