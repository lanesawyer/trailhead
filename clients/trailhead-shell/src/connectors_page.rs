//! The Connectors settings page ([`crate::settings::CONNECTORS`]): the connectors the server runs for the apps, from
//! the apps repo it builds. Listed in a shell with a connection, after Server.
//!
//! - A notice at the top when there is nothing to list, saying why: not connected, still reading, a server too old to
//!   know connectors, a read that failed, a server with no apps repo, or an apps repo with no connectors yet.
//! - Then a section per connector, under its name:
//!   - **On**, a switch, with how often it runs and how long a run may take.
//!   - **Status**: running now, or its last run (when, how long, ok with what each feed handed over, or failed), and
//!     when it runs next. A failed run's error is in a notice under it, its first lines with "Show all" for the rest.
//!   - What it still needs before it can run, in the server's words.
//!   - **Feeds**, each with the app collections it fills, and **Hosts**, the only ones it may reach.
//!   - Each setting it declares, drawn from its kind: a field for a string, a short one for an integer, a switch for a
//!     boolean, a list to add to and take from for a list; its label, its help as the note, and "(required)".
//!   - **Secrets**: whether each is set, never a field; while one isn't, the command that gives them on the server's
//!     machine, with a copy button.
//!   - The save's notice, then Save (only what changed) and Sync now (disabled while it runs, is off or needs
//!     something, the reason in its hint), then what the server said of the run asked for.
//!
//!   A connector whose `connector.toml` doesn't read shows that error and nothing to edit.
//!
//! The page and its list are spawned once when it opens; the connectors are kept by `Keyed` and their parts updated in
//! place as [`ConnectorSettings`] changes, which the page reads again on a slow tick while it is shown. A connector
//! whose declared shape changes (its settings, its secrets, its schedule, or whether its manifest reads) is keyed
//! anew, so its section is replaced; nothing else is ever respawned. The logic is [`crate::connector_settings`]'s.

use bevy::clipboard::Clipboard;
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::{Checked, InteractionDisabled};
use bevy::ui_widgets::{Activate, ValueChange};
use jiff::Timestamp;
use trailhead_model::connector::{ConnectorInfo, SettingInfo, SettingKind};
use trailhead_ui::{
    ButtonKind, Hint, Icon, InitialText, Keyed, Notice, Tone, UiPlace, ValueList, ValuesEdited, button, copy_block,
    extent, notice, row, set_display, set_text, setting, setting_toggle, settings_page, size, spawn_settings_section,
    text, text_input, value_list,
};

use crate::ask::offline_reason;
use crate::connection::{Connection, ConnectionStatus};
use crate::connector_settings::{
    ConnectorDraft, ConnectorSettings, Gate, Load, LoadConnectors, SaveConnector, SyncConnector, Syncing, Typed,
    error_text, feeds_text, login_command, missing_text, schedule_note, secrets_text, status_text,
};
use crate::notification_settings::Clock;

/// What the Feeds setting says under it.
pub const FEEDS_NOTE: &str = "What it hands over, and the app collections each feed fills.";
/// What the Hosts setting says under it.
pub const HOSTS_NOTE: &str = "The only hosts it may reach, over https.";
/// What the Secrets setting says under it.
pub const SECRETS_NOTE: &str =
    "Given on the server's machine and kept there; the window only says whether each is set.";
/// What the login command says under it.
pub const LOGIN_NOTE: &str = "Run this on the server's machine: it asks for each secret without showing what you type.";
/// What Sync now's hint says while it can be pressed.
pub const SYNC_HINT: &str = "Run it once now, whatever its schedule.";

/// The page's list of connectors, holding nothing else, with the sections it keeps by key.
#[derive(Component, Default)]
pub struct ConnectorList(Keyed<()>);

/// The notice at the page's top, saying why there is nothing to list.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ConnectorsStatus;

/// A part of a connector's section that shows something.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Part {
    /// The status line.
    #[default]
    Status,
    /// Why its manifest doesn't read, in place of everything else.
    Broken,
    /// The last run's error.
    Error,
    /// The button that shows the error whole, or clamped again.
    WholeError,
    /// What it still needs.
    Missing,
    Feeds,
    Hosts,
    /// Whether each secret is set.
    Secrets,
    /// The login command's setting, shown while a secret isn't set.
    Login,
    /// The login command's text.
    LoginCommand,
    SaveNotice,
    Save,
    SyncNotice,
    Sync,
}

/// On each part of a connector's section: whose it is, and which.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectorPart {
    pub id: String,
    pub part: Part,
}

/// On a control that edits a connector: the on/off switch (`setting: None`), or a setting's field, switch or list.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectorControl {
    pub id: String,
    pub setting: Option<String>,
}

/// The login command's copy button: the connector's id.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyLogin(pub String);

/// Fills the page: run by the settings modal each time it is shown.
pub(crate) fn connectors_page(In(page): In<Entity>, mut commands: Commands) {
    commands.trigger(LoadConnectors);
    commands.spawn_scene(notice()).insert((ConnectorsStatus, ChildOf(page)));
    commands.spawn_scene(settings_page()).insert((Name::new("connectors"), ConnectorList::default(), ChildOf(page)));
}

/// What a connector's section is keyed by: its id and the shape it declares, so a connector whose shape changes is
/// drawn anew rather than having fields it no longer declares.
pub fn shape(info: &ConnectorInfo) -> String {
    let settings: Vec<String> = info
        .settings
        .iter()
        .map(|s| format!("{}:{}:{}:{}:{}", s.name, s.kind, s.label, s.help.as_deref().unwrap_or_default(), s.required))
        .collect();
    let secrets: Vec<&str> = info.secrets.iter().map(|s| s.name.as_str()).collect();
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        info.id,
        info.name,
        info.error.is_some(),
        info.every_secs,
        info.timeout_secs,
        settings.join(","),
        secrets.join(",")
    )
}

/// A setting showing a text that is kept current in place.
fn value_setting(commands: &mut Commands, rows: Entity, label: &str, note: &str, part: ConnectorPart) {
    let control = bsn_list! { @text("", size::BODY) part };
    commands.spawn_scene(setting(label.to_string(), note.to_string(), control)).insert(ChildOf(rows));
}

/// A connector's section, unparented, for the list to place.
fn spawn_connector(commands: &mut Commands, info: &ConnectorInfo, draft: Option<&ConnectorDraft>) -> Entity {
    let id = info.id.clone();
    let part = |part: Part| ConnectorPart { id: id.clone(), part };
    let section = spawn_settings_section(commands, info.name.clone());
    let rows = section.rows;
    if info.error.is_some() {
        commands.spawn_scene(notice()).insert((part(Part::Broken), ChildOf(rows)));
        return section.section;
    }
    let draft = draft.cloned().unwrap_or_else(|| ConnectorDraft::from(info));
    let on = ConnectorControl { id: id.clone(), setting: None };
    commands.spawn_scene(setting_toggle("On", schedule_note(info), draft.enabled, bsn! { on })).insert(ChildOf(rows));
    value_setting(commands, rows, "Status", "", part(Part::Status));
    commands.spawn_scene(notice()).insert((part(Part::Error), ChildOf(rows)));
    let whole = bsn! { @button("Show all", None, ButtonKind::Plain) Node { align_self: AlignSelf::FlexStart } };
    commands.spawn_scene(whole).insert((part(Part::WholeError), ChildOf(rows)));
    commands.spawn_scene(notice()).insert((part(Part::Missing), ChildOf(rows)));
    value_setting(commands, rows, "Feeds", FEEDS_NOTE, part(Part::Feeds));
    value_setting(commands, rows, "Hosts", HOSTS_NOTE, part(Part::Hosts));
    for spec in &info.settings {
        spawn_setting(commands, rows, &id, spec, draft.values.get(&spec.name));
    }
    if !info.secrets.is_empty() {
        value_setting(commands, rows, "Secrets", SECRETS_NOTE, part(Part::Secrets));
        let (words, copy) = (part(Part::LoginCommand), CopyLogin(id.clone()));
        let control = bsn_list! { @copy_block(bsn! { words }, "Copy the command", bsn! { copy }) };
        let login = setting("Give it its secrets", LOGIN_NOTE, control);
        commands.spawn_scene(login).insert((part(Part::Login), ChildOf(rows)));
    }
    commands.spawn_scene(notice()).insert((part(Part::SaveNotice), ChildOf(rows)));
    let bar = commands.spawn_scene(row()).insert(ChildOf(rows)).id();
    commands.spawn_scene(button("Save", None, ButtonKind::Primary)).insert((part(Part::Save), ChildOf(bar)));
    let sync = button("Sync now", Some(Icon::Refresh), ButtonKind::Normal);
    commands.spawn_scene(sync).insert((part(Part::Sync), Hint(SYNC_HINT.into()), ChildOf(bar)));
    commands.spawn_scene(notice()).insert((part(Part::SyncNotice), ChildOf(rows)));
    section.section
}

/// The label a setting is shown with: its own, marked when it is required.
pub fn setting_label(spec: &SettingInfo) -> String {
    if spec.required { format!("{} (required)", spec.label) } else { spec.label.clone() }
}

/// One declared setting, drawn from its kind, showing what is typed for it.
fn spawn_setting(commands: &mut Commands, rows: Entity, id: &str, spec: &SettingInfo, typed: Option<&Typed>) {
    let control = ConnectorControl { id: id.to_string(), setting: Some(spec.name.clone()) };
    let (label, note) = (setting_label(spec), spec.help.clone().unwrap_or_default());
    let typed = typed.cloned().unwrap_or_else(|| Typed::of(spec));
    match (spec.kind, typed) {
        (SettingKind::Boolean, Typed::Switch(on)) => {
            commands.spawn_scene(setting_toggle(label, note, on, bsn! { control })).insert(ChildOf(rows));
        }
        (SettingKind::List, Typed::List(values)) => {
            let list = bsn_list! { @value_list(values, "None yet.") control };
            commands.spawn_scene(setting(label, note, list)).insert(ChildOf(rows));
        }
        (kind, typed) => {
            let initial = InitialText(match typed {
                Typed::Text(words) => words,
                _ => String::new(),
            });
            let width = if kind == SettingKind::Integer { px(extent::FIELD_SHORT) } else { Val::Auto };
            let field = bsn_list! {
                @text_input(1.0, bsn! { control initial })
                Node { max_width: { width } }
            };
            commands.spawn_scene(setting(label, note, field)).insert(ChildOf(rows));
        }
    }
}

/// The page, for a shell with a connection.
pub(crate) struct ConnectorsPageUiPlugin;

impl Plugin for ConnectorsPageUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_press).add_observer(on_switch).add_observer(on_values).add_systems(
            Update,
            (poll, read_inputs, sync_lists, show_drafts, show_texts, show_notices, show_buttons)
                .chain()
                .before(UiPlace),
        );
    }
}

/// Reads the connectors again every [`ConnectorSettings::poll_every`] while the page is shown; the page's opening
/// read is the first.
fn poll(
    lists: Query<(), With<ConnectorList>>,
    settings: Res<ConnectorSettings>,
    time: Res<Time<Real>>,
    mut asked: Local<Option<std::time::Duration>>,
    mut commands: Commands,
) {
    if lists.is_empty() {
        *asked = None;
        return;
    }
    let now = time.elapsed();
    match *asked {
        None => *asked = Some(now),
        Some(at) if now.saturating_sub(at) >= settings.poll_every() => {
            *asked = Some(now);
            commands.trigger(LoadConnectors);
        }
        Some(_) => {}
    }
}

/// Save, Sync now, Show all and the login command's copy button.
fn on_press(
    activate: On<Activate>,
    parts: Query<&ConnectorPart>,
    copies: Query<&CopyLogin>,
    mut settings: ResMut<ConnectorSettings>,
    clipboard: Option<ResMut<Clipboard>>,
    mut commands: Commands,
) {
    if let Ok(CopyLogin(id)) = copies.get(activate.entity) {
        if let Some(mut clipboard) = clipboard
            && let Err(e) = clipboard.set_text(login_command(id))
        {
            warn!(error = %e, "couldn't copy to the clipboard");
        }
        return;
    }
    let Ok(ConnectorPart { id, part }) = parts.get(activate.entity) else { return };
    match part {
        Part::Save => commands.trigger(SaveConnector(id.clone())),
        Part::Sync => commands.trigger(SyncConnector(id.clone())),
        Part::WholeError if !settings.whole_errors.remove(id) => {
            settings.whole_errors.insert(id.clone());
        }
        _ => {}
    }
}

/// The draft of the connector `id`, made from what the server said if the page has none yet.
fn draft_of<'a>(settings: &'a mut ConnectorSettings, id: &str) -> Option<&'a mut ConnectorDraft> {
    if !settings.drafts.contains_key(id) {
        let draft = ConnectorDraft::from(settings.info(id)?);
        settings.drafts.insert(id.to_string(), draft);
    }
    settings.drafts.get_mut(id)
}

/// The on/off switch and a boolean setting's.
fn on_switch(
    change: On<ValueChange<bool>>,
    controls: Query<&ConnectorControl>,
    mut settings: ResMut<ConnectorSettings>,
) {
    let Ok(ConnectorControl { id, setting }) = controls.get(change.source) else { return };
    let Some(draft) = draft_of(&mut settings, id) else { return };
    match setting {
        None => draft.enabled = change.value,
        Some(name) => {
            draft.values.insert(name.clone(), Typed::Switch(change.value));
        }
    }
}

/// A list setting's add or remove.
fn on_values(edited: On<ValuesEdited>, controls: Query<&ConnectorControl>, mut settings: ResMut<ConnectorSettings>) {
    let Ok(ConnectorControl { id, setting: Some(name) }) = controls.get(edited.entity) else { return };
    if let Some(draft) = draft_of(&mut settings, id) {
        draft.values.insert(name.clone(), Typed::List(edited.values.clone()));
    }
}

/// Copies what is typed into the drafts. An input that has just appeared is empty until the kit puts its initial text
/// in, so it is read from its first change on.
fn read_inputs(fields: Query<(&ConnectorControl, Ref<EditableText>)>, mut settings: ResMut<ConnectorSettings>) {
    for (ConnectorControl { id, setting }, typed) in &fields {
        let Some(name) = setting else { continue };
        if typed.is_added() || !typed.is_changed() {
            continue;
        }
        let value = Typed::Text(typed.value().to_string());
        let held = settings.drafts.get(id).and_then(|d| d.values.get(name));
        if held != Some(&value)
            && let Some(draft) = draft_of(&mut settings, id)
        {
            draft.values.insert(name.clone(), value);
        }
    }
}

/// One section per connector, in the server's order, kept by key in each list on the page (a search puts the page up
/// a second time).
fn sync_lists(
    settings: Res<ConnectorSettings>,
    mut lists: Query<(Entity, &mut ConnectorList)>,
    mut commands: Commands,
) {
    for (body, mut list) in &mut lists {
        if !settings.is_changed() && !list.is_added() {
            continue;
        }
        let wanted = [(body, settings.list.iter().map(|info| (shape(info), ())).collect::<Vec<_>>())];
        list.0.sync(
            &mut commands,
            &wanted,
            |commands, key, _| {
                let info = settings.list.iter().find(|info| shape(info) == key).expect("a key is a listed connector's");
                spawn_connector(commands, info, settings.drafts.get(&info.id))
            },
            |_, _, _| {},
        );
    }
}

/// A connector's switches: neither a field nor a list.
type Switches = (Without<EditableText>, Without<ValueList>);

/// Shows a draft the server's answer replaced (a save, a reading) in its field, its switch and its list.
fn show_drafts(
    settings: Res<ConnectorSettings>,
    mut inputs: Query<(&ConnectorControl, &mut EditableText)>,
    mut lists: Query<(&ConnectorControl, &mut ValueList)>,
    switches: Query<(Entity, &ConnectorControl, Has<Checked>), Switches>,
    mut commands: Commands,
) {
    if !settings.is_changed() {
        return;
    }
    let typed = |control: &ConnectorControl| {
        let draft = settings.drafts.get(&control.id)?;
        match &control.setting {
            None => Some(Typed::Switch(draft.enabled)),
            Some(name) => draft.values.get(name).cloned(),
        }
    };
    for (control, mut input) in &mut inputs {
        if let Some(Typed::Text(wanted)) = typed(control)
            && input.value() != wanted.as_str()
        {
            input.editor_mut().set_text(&wanted);
            input.queue_edit(bevy::text::TextEdit::TextEnd(false));
        }
    }
    for (control, mut list) in &mut lists {
        if let Some(Typed::List(values)) = typed(control) {
            list.set_if_neq(ValueList { values });
        }
    }
    for (switch, control, checked) in &switches {
        match typed(control) {
            Some(Typed::Switch(true)) if !checked => {
                commands.entity(switch).insert(Checked);
            }
            Some(Typed::Switch(false)) if checked => {
                commands.entity(switch).remove::<Checked>();
            }
            _ => {}
        }
    }
}

/// The time now, by the shell's clock.
fn now(clock: Option<&Clock>) -> Timestamp {
    clock.map_or_else(Timestamp::now, |c| c.now().timestamp())
}

/// The texts of each section, and the page's notice.
fn show_texts(
    settings: Res<ConnectorSettings>,
    connection: Option<Res<State<Connection>>>,
    status: Option<Res<ConnectionStatus>>,
    clock: Option<Res<Clock>>,
    mut texts: Query<(&ConnectorPart, &mut Text)>,
    mut page: Query<&mut Notice, With<ConnectorsStatus>>,
) {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    let says = page_notice(&settings, live, offline_reason(status.as_deref()));
    for mut notice in &mut page {
        if *notice != says {
            *notice = says.clone();
        }
    }
    let now = now(clock.as_deref());
    for (ConnectorPart { id, part }, text) in &mut texts {
        let Some(info) = settings.info(id) else { continue };
        let words = match part {
            Part::Status => status_text(info, now),
            Part::Feeds => feeds_text(info),
            Part::Hosts => info.hosts.join(", "),
            Part::Secrets => secrets_text(info),
            Part::LoginCommand => login_command(id),
            _ => continue,
        };
        set_text(text, &words);
    }
}

/// What the page's top says: why there is nothing to list, or nothing.
pub fn page_notice(settings: &ConnectorSettings, live: bool, offline: Option<String>) -> Notice {
    if !live {
        let why = offline.map_or("Not connected to the server.".into(), |r| format!("Not connected: {r}."));
        return Notice::new(Tone::Info, format!("{why} The connectors show once it is."));
    }
    match &settings.load {
        Load::Loading => Notice::new(Tone::Info, "Reading the connectors…"),
        Load::Missing => Notice::new(Tone::Info, "This server doesn't know connectors: it is older than the window."),
        Load::Failed(why) => Notice::new(Tone::Error, format!("Couldn't read the connectors: {why}")),
        Load::Loaded if settings.list.is_empty() && settings.runs_connectors == Some(false) => Notice::new(
            Tone::Info,
            "This server runs no connectors: it has no apps repo to build them from. Set one on the Server page, \
             under Window builds.",
        ),
        Load::Loaded if settings.list.is_empty() => Notice::new(
            Tone::Info,
            "No connectors in the apps repo yet. A connector is a crate under connectors/<id>/ in the apps repo; the \
             server reads them after each build.",
        ),
        Load::Loaded => Notice::default(),
    }
}

/// What a connector's notice of `part` says.
pub fn connector_notice(settings: &ConnectorSettings, id: &str, part: &Part, live: bool) -> Notice {
    let Some(info) = settings.info(id) else { return Notice::default() };
    match part {
        Part::Broken => Notice::new(
            Tone::Error,
            format!(
                "Its connector.toml doesn't read, so it can't be set or run: {}",
                info.error.as_deref().unwrap_or_default()
            ),
        ),
        Part::Error => match info.last_run.as_ref().and_then(|r| r.error.as_deref()) {
            Some(error) if !info.running => {
                let (words, _) = error_text(error, settings.whole_errors.contains(id));
                Notice::new(Tone::Error, format!("The last run failed: {words}"))
            }
            _ => Notice::default(),
        },
        Part::Missing => match missing_text(info) {
            words if words.is_empty() => Notice::default(),
            words => Notice::new(Tone::Warning, words),
        },
        Part::SaveNotice => save_notice(settings, id, live),
        Part::SyncNotice => match settings.sync_state(id) {
            Syncing::Idle => Notice::default(),
            Syncing::Starting => Notice::new(Tone::Info, "Starting a run…"),
            Syncing::Started => Notice::new(Tone::Info, "Started a run. Its status above follows it."),
            Syncing::Refused(why) => Notice::new(Tone::Error, format!("The server didn't start a run: {why}")),
        },
        _ => Notice::default(),
    }
}

/// What a connector's save notice says: the server's refusal, why it can't be saved, edits not yet saved, or that it
/// was. Nothing about the connection, which the page's top says.
fn save_notice(settings: &ConnectorSettings, id: &str, live: bool) -> Notice {
    let state = settings.save_state(id);
    if let Some(refusal) = &state.refusal {
        return Notice::new(Tone::Error, format!("The server refused this: {refusal}"));
    }
    if !live {
        return Notice::default();
    }
    match settings.save_gate(id, live, None) {
        Gate::Blocked(why) => Notice::new(Tone::Info, why),
        Gate::Ready => Notice::new(Tone::Info, "Changed here, not saved yet."),
        Gate::Saving => Notice::new(Tone::Info, "Saving…"),
        Gate::Unchanged if state.saved => Notice::new(Tone::Success, "Saved. The server runs it with these now."),
        Gate::Unchanged => Notice::default(),
    }
}

/// Each section's notices.
fn show_notices(
    settings: Res<ConnectorSettings>,
    connection: Option<Res<State<Connection>>>,
    mut notices: Query<(&ConnectorPart, &mut Notice)>,
) {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    for (ConnectorPart { id, part }, mut notice) in &mut notices {
        let says = connector_notice(&settings, id, part, live);
        if *notice != says {
            *notice = says;
        }
    }
}

/// Save, Sync now and Show all: enabled, captioned and hinted as the connector stands; the login command shown while
/// a secret isn't set.
#[expect(clippy::too_many_arguments)]
fn show_buttons(
    settings: Res<ConnectorSettings>,
    connection: Option<Res<State<Connection>>>,
    status: Option<Res<ConnectionStatus>>,
    parts: Query<(Entity, &ConnectorPart, Has<InteractionDisabled>)>,
    mut hints: Query<&mut Hint>,
    children: Query<&Children>,
    mut texts: Query<&mut Text>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    let offline = offline_reason(status.as_deref());
    for (entity, ConnectorPart { id, part }, disabled) in &parts {
        let Some(info) = settings.info(id) else { continue };
        let (enabled, caption) = match part {
            Part::Save => {
                let gate = settings.save_gate(id, live, offline.clone());
                (gate == Gate::Ready, Some(if gate == Gate::Saving { "Saving…" } else { "Save" }))
            }
            Part::Sync => {
                let gate = settings.sync_gate(id, live, offline.clone());
                if let Ok(mut hint) = hints.get_mut(entity) {
                    let words = gate.as_ref().err().map_or(SYNC_HINT, String::as_str);
                    if hint.0 != words {
                        hint.0 = words.to_string();
                    }
                }
                // Its caption is left alone: the icon beside it is a text too.
                (gate.is_ok(), None)
            }
            Part::WholeError => {
                let error = info.last_run.as_ref().and_then(|r| r.error.as_deref()).filter(|_| !info.running);
                let more = error.is_some_and(|e| error_text(e, false).1);
                set_display(&mut nodes, entity, more);
                (true, Some(if settings.whole_errors.contains(id) { "Show less" } else { "Show all" }))
            }
            Part::Login => {
                set_display(&mut nodes, entity, info.secrets.iter().any(|s| !s.set));
                continue;
            }
            _ => continue,
        };
        match (enabled, disabled) {
            (true, true) => {
                commands.entity(entity).remove::<InteractionDisabled>();
            }
            (false, false) => {
                commands.entity(entity).insert(InteractionDisabled);
            }
            _ => {}
        }
        let Some(caption) = caption else { continue };
        for inside in children.iter_descendants(entity) {
            if let Ok(text) = texts.get_mut(inside) {
                set_text(text, caption);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trailhead_ui::{Appearance, ModalPlugin, ThemeChoice};

    use super::*;
    use crate::ShellPlugin;
    use crate::connector_settings::tests::{github, info};
    use crate::connector_settings::{ConnectorsRead, Saving};
    use crate::settings::{CONNECTORS, OpenSettings};
    use crate::testing::{Other, set_connection, settle, windowed};

    fn app(list: Vec<serde_json::Value>) -> App {
        let mut app = windowed(ShellPlugin::new().with(Other));
        app.insert_resource(Appearance::new(ThemeChoice::Desktop)).add_plugins(ModalPlugin);
        let now: Timestamp = "2026-11-22T12:01:30Z".parse().unwrap();
        app.insert_resource(Clock::new(move || now.to_zoned(jiff::tz::TimeZone::UTC)));
        set_connection(&mut app, Connection::Live);
        app.world_mut().trigger(OpenSettings { page: Some(CONNECTORS) });
        settle(&mut app);
        let list = list.into_iter().map(info).collect();
        app.world_mut().write_message(ConnectorsRead { seq: 1, result: Ok((list, Some(true))) });
        settle(&mut app);
        app
    }

    fn edit(app: &mut App, edit: impl FnOnce(&mut ConnectorSettings)) {
        edit(&mut app.world_mut().resource_mut::<ConnectorSettings>());
        settle(app);
    }

    fn part(app: &mut App, id: &str, wanted: Part) -> Entity {
        let world = app.world_mut();
        let mut parts = world.query::<(Entity, &ConnectorPart)>();
        parts.iter(world).find(|(_, p)| p.id == id && p.part == wanted).unwrap().0
    }

    fn text_of(app: &mut App, id: &str, wanted: Part) -> String {
        let entity = part(app, id, wanted);
        app.world().get::<Text>(entity).unwrap().0.clone()
    }

    fn notice_of(app: &mut App, id: &str, wanted: Part) -> Notice {
        let entity = part(app, id, wanted);
        app.world().get::<Notice>(entity).unwrap().clone()
    }

    fn shown(app: &App, entity: Entity) -> bool {
        app.world().get::<Node>(entity).unwrap().display != Display::None
    }

    fn enabled(app: &App, entity: Entity) -> bool {
        !app.world().entity(entity).contains::<InteractionDisabled>()
    }

    fn control(app: &mut App, id: &str, setting: Option<&str>) -> Vec<Entity> {
        let world = app.world_mut();
        let mut controls = world.query::<(Entity, &ConnectorControl)>();
        let wanted = ConnectorControl { id: id.into(), setting: setting.map(str::to_string) };
        controls.iter(world).filter(|(_, c)| **c == wanted).map(|(e, _)| e).collect()
    }

    fn entities(app: &mut App) -> Vec<Entity> {
        let world = app.world_mut();
        let mut all: Vec<Entity> = world.query::<Entity>().iter(world).collect();
        all.sort();
        all
    }

    #[test]
    fn each_connector_is_drawn_from_what_it_declares() {
        let mut app = app(vec![github()]);
        let world = app.world_mut();
        let headings: Vec<String> =
            world.query::<&trailhead_ui::SettingsHeading>().iter(world).map(|h| h.0.clone()).collect();
        assert_eq!(headings, ["GitHub"]);
        assert_eq!(
            text_of(&mut app, "github", Part::Status),
            "Last run 1 min ago, took 2.1 s: ok.\npull_requests: 12 records, 1 new, 2 changed, 0 removed\nNext run \
             in 3 min."
        );
        assert_eq!(text_of(&mut app, "github", Part::Feeds), "pull_requests → cairn.pull_requests");
        assert_eq!(text_of(&mut app, "github", Part::Hosts), "api.github.com");
        assert_eq!(text_of(&mut app, "github", Part::Secrets), "token: set");

        let me = control(&mut app, "github", Some("me"))[0];
        assert_eq!(app.world().get::<EditableText>(me).unwrap().value(), "lanesawyer", "a string is a field");
        let pages = control(&mut app, "github", Some("max_pages"))[0];
        assert_eq!(app.world().get::<EditableText>(pages).unwrap().value(), "", "an integer too, empty while unset");
        let repos = control(&mut app, "github", Some("repos"))[0];
        assert_eq!(app.world().get::<ValueList>(repos).unwrap().values, ["lanesawyer/trailhead"], "a list");
        let drafts = control(&mut app, "github", Some("drafts"))[0];
        assert!(app.world().entity(drafts).contains::<Checked>(), "a boolean is a switch");
        let on = control(&mut app, "github", None)[0];
        assert!(app.world().entity(on).contains::<Checked>());
        let world = app.world_mut();
        let labels: Vec<String> =
            world.query::<&trailhead_ui::SettingWords>().iter(world).map(|w| w.label.clone()).collect();
        assert!(labels.contains(&"Repositories (required)".to_string()), "{labels:?}");

        let login = part(&mut app, "github", Part::Login);
        assert!(!shown(&app, login), "every secret is set");
        let (save, sync) = (part(&mut app, "github", Part::Save), part(&mut app, "github", Part::Sync));
        assert!(!enabled(&app, save), "nothing to save");
        assert!(enabled(&app, sync));
        assert_eq!(app.world().get::<Hint>(sync).unwrap().0, SYNC_HINT);
        assert_eq!(notice_of(&mut app, "github", Part::Missing), Notice::default());
    }

    #[test]
    fn a_change_from_the_server_is_shown_in_place_and_nothing_is_respawned() {
        let mut app = app(vec![github()]);
        let before = entities(&mut app);
        let status = part(&mut app, "github", Part::Status);
        let mut changed = github();
        changed["running"] = json!(true);
        changed["secrets"] = json!([{"name": "token", "set": false}]);
        changed["missing"] = json!(["the secret `token` (`trailhead connector login github token` on the server's \
                                     machine)"]);
        changed["settings"][1]["value"] = json!("lane");
        edit(&mut app, |s| s.answered(vec![info(changed)], Some(true)));
        assert_eq!(entities(&mut app), before, "the same entities, changed in place");
        assert_eq!(part(&mut app, "github", Part::Status), status);
        assert_eq!(text_of(&mut app, "github", Part::Status), "Running now.");
        assert_eq!(text_of(&mut app, "github", Part::Secrets), "token: not set");
        assert_eq!(text_of(&mut app, "github", Part::LoginCommand), "trailhead connector login github");
        let login = part(&mut app, "github", Part::Login);
        assert!(shown(&app, login), "the command shows while a secret isn't set");
        assert_eq!(notice_of(&mut app, "github", Part::Missing).tone, Tone::Warning);
        let me = control(&mut app, "github", Some("me"))[0];
        assert_eq!(app.world().get::<EditableText>(me).unwrap().value(), "lane", "the field shows it");
        let sync = part(&mut app, "github", Part::Sync);
        assert!(!enabled(&app, sync));
        assert_eq!(app.world().get::<Hint>(sync).unwrap().0, "It is running now.");
    }

    #[test]
    fn typing_makes_a_save_and_a_refusal_keeps_what_was_typed() {
        let mut app = app(vec![github()]);
        let me = control(&mut app, "github", Some("me"))[0];
        app.world_mut().get_mut::<EditableText>(me).unwrap().editor_mut().set_text("lane");
        settle(&mut app);
        let save = part(&mut app, "github", Part::Save);
        assert!(enabled(&app, save), "an edit can be saved");
        assert_eq!(
            notice_of(&mut app, "github", Part::SaveNotice),
            Notice::new(Tone::Info, "Changed here, not saved yet.")
        );
        let words = "github: setting `me` must be a string";
        edit(&mut app, |s| {
            s.saves.insert("github".into(), Saving { refusal: Some(words.into()), ..Default::default() });
        });
        assert_eq!(
            notice_of(&mut app, "github", Part::SaveNotice),
            Notice::new(Tone::Error, format!("The server refused this: {words}"))
        );
        assert_eq!(app.world().get::<EditableText>(me).unwrap().value(), "lane", "what was typed stays");
    }

    #[test]
    fn a_failed_run_shows_its_error_clamped_with_the_rest_on_asking() {
        let mut failed = github();
        let error: String = (1..=8).map(|n| format!("line {n}\n")).collect();
        failed["last_run"] = json!({"started_at": "2026-11-22T12:00:00Z", "duration_ms": 900, "ok": false,
                                    "error": error});
        let mut app = app(vec![failed]);
        let notice = notice_of(&mut app, "github", Part::Error);
        assert_eq!(notice, Notice::new(Tone::Error, "The last run failed: line 1\nline 2\nline 3\nline 4…"));
        let whole = part(&mut app, "github", Part::WholeError);
        assert!(shown(&app, whole));
        app.world_mut().trigger(Activate { entity: whole });
        settle(&mut app);
        assert!(notice_of(&mut app, "github", Part::Error).message.ends_with("line 8"));
    }

    #[test]
    fn a_broken_manifest_shows_its_error_and_nothing_to_edit() {
        let broken = json!({"id": "jira", "name": "jira", "error": "connectors/jira/connector.toml: hosts is empty",
                            "every_secs": 0, "timeout_secs": 0, "hosts": [], "settings": [], "secrets": [],
                            "feeds": [], "enabled": false, "missing": ["a connector.toml that reads"],
                            "running": false});
        let mut app = app(vec![github(), broken]);
        assert_eq!(
            notice_of(&mut app, "jira", Part::Broken).message,
            "Its connector.toml doesn't read, so it can't be set or run: connectors/jira/connector.toml: hosts is \
             empty"
        );
        assert!(control(&mut app, "jira", None).is_empty(), "no switch");
        let world = app.world_mut();
        let jira = world.query::<&ConnectorPart>().iter(world).filter(|p| p.id == "jira").count();
        assert_eq!(jira, 1, "nothing but the error");
    }

    #[test]
    fn the_page_says_why_it_lists_nothing() {
        let mut s = ConnectorSettings::default();
        assert_eq!(
            page_notice(&s, false, None).message,
            "Not connected to the server. The connectors show once it is."
        );
        assert_eq!(page_notice(&s, true, None).message, "Reading the connectors…");
        s.answered(Vec::new(), Some(false));
        assert!(page_notice(&s, true, None).message.starts_with("This server runs no connectors: it has no apps repo"));
        s.answered(Vec::new(), Some(true));
        assert!(page_notice(&s, true, None).message.starts_with("No connectors in the apps repo yet."));
        s.answered(vec![info(github())], Some(true));
        assert_eq!(page_notice(&s, true, None), Notice::default());
    }

    #[test]
    fn a_connector_added_or_gone_is_spawned_or_despawned_alone() {
        let mut app = app(vec![github()]);
        let github_status = part(&mut app, "github", Part::Status);
        let mut todo = github();
        todo["id"] = json!("todo-list");
        todo["name"] = json!("Todo list");
        edit(&mut app, |s| s.answered(vec![info(github()), info(todo)], Some(true)));
        assert_eq!(part(&mut app, "github", Part::Status), github_status, "the other kept its entities");
        part(&mut app, "todo-list", Part::Status);
        edit(&mut app, |s| s.answered(vec![info(github())], Some(true)));
        let world = app.world_mut();
        assert!(world.query::<&ConnectorPart>().iter(world).all(|p| p.id == "github"));
        assert_eq!(part(&mut app, "github", Part::Status), github_status);
    }
}
