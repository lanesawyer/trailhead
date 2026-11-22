//! The Connectors settings page's logic, headless: every connector the server knows (`GET /v1/connectors`, with
//! whether it builds an apps repo from `GET /v1/shell/build`), read when the page opens, on a slow tick while it is
//! shown and after each action; the edits made on the page, one draft per connector ([`ConnectorDraft`]); whether a
//! connector's Save and its Sync now can be pressed and why not ([`ConnectorSettings::save_gate`],
//! [`ConnectorSettings::sync_gate`]); saving one, `PATCH /v1/connectors/{id}` with only what changed; and running one
//! now, `POST /v1/connectors/{id}/run`.
//!
//! Nothing is shown before the server says it: a save shows the connector as the server answered, never the edit. A
//! refusal keeps what was typed and says why in the server's words. The words the page shows of a connector (its
//! status, its feeds, what it needs) are the pure functions here, so they are tested without a window.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use bevy::prelude::*;
use jiff::Timestamp;
use serde_json::{Map, Value};
use trailhead_model::connector::{ConnectorInfo, ConnectorPatch, ConnectorRun, SettingInfo, SettingKind};

use crate::connection::{Connection, SendNet};
use crate::net::{Fetch, NetCommand};
use crate::server_settings::said;

/// How often the page reads the connectors again while it is shown.
pub const POLL: Duration = Duration::from_secs(10);
/// How often while one of them is running, so its end shows soon after it.
pub const POLL_RUNNING: Duration = Duration::from_secs(2);

/// How many lines of a run's error the page shows before "Show all".
pub const ERROR_LINES: usize = 4;
/// How many characters of a run's error the page shows before "Show all".
pub const ERROR_CHARS: usize = 320;

/// Where reading the connectors is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Load {
    #[default]
    Loading,
    Loaded,
    /// The server has no `/v1/connectors`: it is older than the page.
    Missing,
    /// The read failed, and why.
    Failed(String),
}

/// A setting as typed on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// A string or an integer's field.
    Text(String),
    /// A boolean's switch.
    Switch(bool),
    /// A list's values.
    List(Vec<String>),
}

impl Typed {
    /// What the page shows for `setting` as the server has it.
    pub fn of(setting: &SettingInfo) -> Typed {
        let value = setting.value.as_ref();
        match setting.kind {
            SettingKind::String => Typed::Text(value.and_then(Value::as_str).unwrap_or_default().to_string()),
            SettingKind::Integer => Typed::Text(value.map(integer_words).unwrap_or_default()),
            SettingKind::Boolean => Typed::Switch(value.and_then(Value::as_bool).unwrap_or(false)),
            SettingKind::List => Typed::List(list_of(value)),
        }
    }
}

fn integer_words(value: &Value) -> String {
    value.as_i64().map(|n| n.to_string()).unwrap_or_default()
}

fn list_of(value: Option<&Value>) -> Vec<String> {
    let items = value.and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    items.iter().filter_map(Value::as_str).map(str::to_string).collect()
}

/// One connector as edited on the page: whether it is on, and each setting it declares, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectorDraft {
    pub enabled: bool,
    pub values: BTreeMap<String, Typed>,
}

impl ConnectorDraft {
    pub fn from(info: &ConnectorInfo) -> ConnectorDraft {
        ConnectorDraft {
            enabled: info.enabled,
            values: info.settings.iter().map(|s| (s.name.clone(), Typed::of(s))).collect(),
        }
    }

    /// What saving it would send, `None` when nothing differs from what the server has; an error says what can't be
    /// sent. A field emptied, or a list left with nothing in it, unsets the setting (`null`).
    pub fn patch(&self, info: &ConnectorInfo) -> Result<Option<ConnectorPatch>, String> {
        let mut settings = Map::new();
        for setting in &info.settings {
            let Some(typed) = self.values.get(&setting.name) else { continue };
            if let Some(value) = changed(setting, typed)? {
                settings.insert(setting.name.clone(), value);
            }
        }
        let patch = ConnectorPatch {
            enabled: (self.enabled != info.enabled).then_some(self.enabled),
            settings: (!settings.is_empty()).then_some(settings),
        };
        Ok(Some(patch).filter(|p| *p != ConnectorPatch::default()))
    }
}

/// What to send for one setting: nothing when `typed` is what the server has, `null` to unset it, else the value.
fn changed(setting: &SettingInfo, typed: &Typed) -> Result<Option<Value>, String> {
    let now = setting.value.as_ref();
    let unset = || now.map(|_| Value::Null);
    Ok(match (setting.kind, typed) {
        (SettingKind::String, Typed::Text(text)) => match text.trim() {
            "" => unset(),
            text if Some(text) == now.and_then(Value::as_str) => None,
            text => Some(Value::String(text.to_string())),
        },
        (SettingKind::Integer, Typed::Text(text)) => match text.trim() {
            "" => unset(),
            text => {
                let n: i64 = text.parse().map_err(|_| format!("{} is a whole number.", setting.label))?;
                (Some(n) != now.and_then(Value::as_i64)).then(|| Value::from(n))
            }
        },
        (SettingKind::Boolean, Typed::Switch(on)) => {
            (*on != now.and_then(Value::as_bool).unwrap_or(false)).then_some(Value::Bool(*on))
        }
        (SettingKind::List, Typed::List(values)) if values.is_empty() => unset(),
        (SettingKind::List, Typed::List(values)) => {
            (*values != list_of(now)).then(|| Value::Array(values.iter().cloned().map(Value::String).collect()))
        }
        _ => None,
    })
}

/// A connector's save.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Saving {
    /// The change is with the server.
    pub saving: bool,
    /// What the server said when it refused the last save, in its words.
    pub refusal: Option<String>,
    /// The last save was written.
    pub saved: bool,
}

/// A connector's Sync now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Syncing {
    #[default]
    Idle,
    /// Asked; the server hasn't answered.
    Starting,
    /// The server started a run, which the page follows until it ends.
    Started,
    /// The server didn't start one, in its words.
    Refused(String),
}

/// Whether a connector's Save can be pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Ready,
    /// Nothing is different from what the server has.
    Unchanged,
    Saving,
    /// Not now, and why, for the connector's notice.
    Blocked(String),
}

/// What the page knows and what is typed on it.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct ConnectorSettings {
    /// Every connector the server knows, as it last said, in id order.
    pub list: Vec<ConnectorInfo>,
    /// Whether the server builds an apps repo, and so runs connectors; `None` while it hasn't said.
    pub runs_connectors: Option<bool>,
    pub load: Load,
    pub drafts: BTreeMap<String, ConnectorDraft>,
    pub saves: BTreeMap<String, Saving>,
    pub syncs: BTreeMap<String, Syncing>,
    /// The connectors whose last run's error is shown whole rather than clamped.
    pub whole_errors: BTreeSet<String>,
    /// The latest read sent: an answer to an older one is dropped.
    read_seq: u64,
    /// The latest save and run sent for each connector, likewise.
    save_seqs: BTreeMap<String, u64>,
    sync_seqs: BTreeMap<String, u64>,
    sent: u64,
}

impl ConnectorSettings {
    pub fn info(&self, id: &str) -> Option<&ConnectorInfo> {
        self.list.iter().find(|c| c.id == id)
    }

    pub fn save_state(&self, id: &str) -> Saving {
        self.saves.get(id).cloned().unwrap_or_default()
    }

    pub fn sync_state(&self, id: &str) -> Syncing {
        self.syncs.get(id).cloned().unwrap_or_default()
    }

    /// The change a connector's Save would send.
    pub fn change(&self, id: &str) -> Result<Option<ConnectorPatch>, String> {
        match (self.info(id), self.drafts.get(id)) {
            (Some(info), Some(draft)) if info.error.is_none() => draft.patch(info),
            _ => Ok(None),
        }
    }

    /// Whether a connector's Save can be pressed now, and why not.
    pub fn save_gate(&self, id: &str, live: bool, offline: Option<String>) -> Gate {
        if !live {
            return Gate::Blocked(not_connected(offline));
        }
        if self.save_state(id).saving {
            return Gate::Saving;
        }
        match self.change(id) {
            Err(why) => Gate::Blocked(why),
            Ok(None) => Gate::Unchanged,
            Ok(Some(_)) => Gate::Ready,
        }
    }

    /// Whether a connector's Sync now can be pressed now, or why not: it runs only while on, with all it needs, and
    /// not already running.
    pub fn sync_gate(&self, id: &str, live: bool, offline: Option<String>) -> Result<(), String> {
        if !live {
            return Err(not_connected(offline));
        }
        let Some(info) = self.info(id) else { return Err("The server doesn't know this connector now.".into()) };
        if info.error.is_some() {
            return Err("Its connector.toml doesn't read.".into());
        }
        if self.sync_state(id) == Syncing::Starting {
            return Err("Starting a run…".into());
        }
        if info.running {
            return Err("It is running now.".into());
        }
        if !info.enabled {
            return Err("It is off: turn it on and save first.".into());
        }
        if !info.missing.is_empty() {
            return Err("It can't run until it has what it needs.".into());
        }
        Ok(())
    }

    /// How long until the page reads the connectors again.
    pub fn poll_every(&self) -> Duration {
        let busy = self.list.iter().any(|c| c.running) || self.syncs.values().any(|s| *s == Syncing::Starting);
        if busy { POLL_RUNNING } else { POLL }
    }

    /// Takes the server's answer to a read, as the page does when it arrives: what a test or a shot starts from.
    pub fn answered(&mut self, list: Vec<ConnectorInfo>, runs_connectors: Option<bool>) {
        self.read(list);
        self.runs_connectors = runs_connectors;
    }

    /// Takes a reading: each connector's draft follows it, unless it holds edits not yet saved or a save is under
    /// way. A run the page started is followed until the server says it isn't running.
    fn read(&mut self, list: Vec<ConnectorInfo>) {
        let edited: BTreeSet<String> = list
            .iter()
            .filter(|c| self.save_state(&c.id).saving || self.change(&c.id).map_or(true, |p| p.is_some()))
            .map(|c| c.id.clone())
            .collect();
        for info in &list {
            if !edited.contains(&info.id) {
                self.drafts.insert(info.id.clone(), ConnectorDraft::from(info));
            }
            if !info.running && self.sync_state(&info.id) == Syncing::Started {
                self.syncs.remove(&info.id);
            }
        }
        let known: BTreeSet<&str> = list.iter().map(|c| c.id.as_str()).collect();
        self.drafts.retain(|id, _| known.contains(id.as_str()));
        self.list = list;
        self.load = Load::Loaded;
    }

    /// Takes one connector as the server answered a save or a run.
    fn take(&mut self, info: ConnectorInfo, keep_draft: bool) {
        if !keep_draft {
            self.drafts.insert(info.id.clone(), ConnectorDraft::from(&info));
        }
        match self.list.iter_mut().find(|c| c.id == info.id) {
            Some(held) => *held = info,
            None => {
                self.list.push(info);
                self.list.sort_by(|a, b| a.id.cmp(&b.id));
            }
        }
    }
}

fn not_connected(offline: Option<String>) -> String {
    offline.map_or("Not connected to the server.".into(), |r| format!("Not connected: {r}."))
}

/// How long `secs` seconds is, in the largest unit that fits: `45 s`, `5 min`, `3 h`, `2 d`.
pub fn span(secs: i64) -> String {
    match secs.max(0) {
        s @ 0..60 => format!("{s} s"),
        s @ 60..3_600 => format!("{} min", s / 60),
        s @ 3_600..86_400 => format!("{} h", s / 3_600),
        s => format!("{} d", s / 86_400),
    }
}

/// How long a run took: `850 ms`, `2.1 s`, `1 min 5 s`.
pub fn took(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms} ms"),
        1_000..60_000 => format!("{:.1} s", ms as f64 / 1_000.0),
        _ => format!("{} min {} s", ms / 60_000, ms % 60_000 / 1_000),
    }
}

/// How long ago `then` was: `just now` under a minute, so the line doesn't change every second.
pub fn ago(then: Timestamp, now: Timestamp) -> String {
    let secs = now.as_second() - then.as_second();
    if secs < 60 { "just now".into() } else { format!("{} ago", span(secs)) }
}

/// What the switch's note says: how often it runs and how long a run may take.
pub fn schedule_note(info: &ConnectorInfo) -> String {
    format!(
        "Runs every {}, each run stopped after {}. Off, it never runs.",
        span(info.every_secs as i64),
        span(info.timeout_secs as i64)
    )
}

/// A connector's status: running now, or its last run (when, how long, and whether it went well, with what each feed
/// handed over); then when it runs next, or why it doesn't. The error itself is [`error_text`]'s.
pub fn status_text(info: &ConnectorInfo, now: Timestamp) -> String {
    let mut lines = Vec::new();
    match (&info.last_run, info.running) {
        (_, true) => lines.push("Running now.".to_string()),
        (None, false) => lines.push("Never run.".to_string()),
        (Some(run), false) => {
            let how = if run.ok { "ok" } else { "failed" };
            lines.push(format!("Last run {}, took {}: {how}.", ago(run.started_at, now), took(run.duration_ms)));
            lines.extend(feed_lines(run));
        }
    }
    if let Some(next) = next_text(info, now) {
        lines.push(next);
    }
    lines.join("\n")
}

/// What each feed handed over in a run: its records, and what that did to the apps' collections.
pub fn feed_lines(run: &ConnectorRun) -> Vec<String> {
    if run.ok && run.feeds.is_empty() {
        return vec!["It handed nothing over: nothing had changed.".into()];
    }
    run.feeds
        .iter()
        .map(|(feed, f)| {
            let mut line = format!(
                "{feed}: {} {}, {} new, {} changed, {} removed",
                f.records,
                if f.records == 1 { "record" } else { "records" },
                f.created,
                f.changed,
                f.removed
            );
            if !f.errors.is_empty() {
                line.push_str(&format!("; refused by {}", f.errors.join("; ")));
            }
            line
        })
        .collect()
}

/// When it runs next, or why it won't; nothing while it runs.
pub fn next_text(info: &ConnectorInfo, now: Timestamp) -> Option<String> {
    if info.running || info.error.is_some() {
        return None;
    }
    Some(if !info.enabled {
        "It is off, so it doesn't run.".into()
    } else if !info.missing.is_empty() {
        "It doesn't run until it has what it needs.".into()
    } else {
        match info.next_run {
            Some(at) if at <= now => "Next run: due now.".into(),
            Some(at) => format!("Next run in {}.", span(at.as_second() - now.as_second())),
            None => "Not scheduled: this server doesn't run connectors on a schedule.".into(),
        }
    })
}

/// The last run's error as the page shows it: whole, or its first [`ERROR_LINES`] lines and [`ERROR_CHARS`]
/// characters with an ellipsis; and whether there is more than the clamp shows.
pub fn error_text(error: &str, whole: bool) -> (String, bool) {
    let error = error.trim();
    let lines: Vec<&str> = error.lines().collect();
    let mut clamped = lines[..lines.len().min(ERROR_LINES)].join("\n");
    if clamped.chars().count() > ERROR_CHARS {
        clamped = clamped.chars().take(ERROR_CHARS).collect();
    }
    let more = clamped.len() < error.len();
    if whole || !more {
        return (error.to_string(), more);
    }
    (format!("{}…", clamped.trim_end()), more)
}

/// What each feed fills: `pull_requests → cairn.pull_requests`, a line each.
pub fn feeds_text(info: &ConnectorInfo) -> String {
    let lines: Vec<String> = info
        .feeds
        .iter()
        .map(|feed| match feed.collections.as_slice() {
            [] => format!("{} → no app's collection takes it yet", feed.name),
            fills => format!("{} → {}", feed.name, fills.join(", ")),
        })
        .collect();
    if lines.is_empty() { "None".into() } else { lines.join("\n") }
}

/// What it needs before it can run, in the server's words.
pub fn missing_text(info: &ConnectorInfo) -> String {
    match info.missing.as_slice() {
        [] => String::new(),
        [one] => format!("Before it can run, it needs {one}."),
        many => format!(
            "Before it can run, it needs:\n{}",
            many.iter().map(|m| format!("• {m}")).collect::<Vec<_>>().join("\n")
        ),
    }
}

/// Whether each secret is set, a line each: `token: set`.
pub fn secrets_text(info: &ConnectorInfo) -> String {
    let lines: Vec<String> =
        info.secrets.iter().map(|s| format!("{}: {}", s.name, if s.set { "set" } else { "not set" })).collect();
    lines.join("\n")
}

/// The command that gives a connector its secrets, run on the server's machine.
pub fn login_command(id: &str) -> String {
    format!("trailhead connector login {id}")
}

/// Reads the connectors again: the page triggers it when it opens and on its tick.
#[derive(Event, Debug, Clone, Copy)]
pub struct LoadConnectors;

/// Saves a connector's edits, if [`ConnectorSettings::save_gate`] lets it.
#[derive(Event, Debug, Clone)]
pub struct SaveConnector(pub String);

/// Runs a connector now, if [`ConnectorSettings::sync_gate`] lets it.
#[derive(Event, Debug, Clone)]
pub struct SyncConnector(pub String);

/// Why a read failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// `404`: no `/v1/connectors` on this server.
    Missing,
    Failed(String),
}

/// The server's answer to the read sent as `seq`: the connectors, and whether it builds an apps repo.
#[derive(Message, Debug, Clone)]
pub struct ConnectorsRead {
    pub seq: u64,
    pub result: Result<(Vec<ConnectorInfo>, Option<bool>), ReadError>,
}

/// The server's answer to a connector's save sent as `seq`: the connector as now set, or its refusal in its words.
#[derive(Message, Debug, Clone)]
pub struct ConnectorSaved {
    pub id: String,
    pub seq: u64,
    pub result: Result<ConnectorInfo, String>,
}

/// The server's answer to a connector's Sync now sent as `seq`: the connector with its run started, or why not.
#[derive(Message, Debug, Clone)]
pub struct ConnectorSynced {
    pub id: String,
    pub seq: u64,
    pub result: Result<ConnectorInfo, String>,
}

/// The page's logic, in a shell with a connection.
pub(crate) struct ConnectorSettingsPlugin;

impl Plugin for ConnectorSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ConnectorSettings>()
            .add_message::<ConnectorsRead>()
            .add_message::<ConnectorSaved>()
            .add_message::<ConnectorSynced>()
            .add_message::<SendNet>()
            .add_systems(OnEnter(Connection::Live), load.run_if(page_shown))
            .add_systems(OnExit(Connection::Live), drop_in_flight)
            .add_systems(Update, (hear_read, hear_saved, hear_synced).chain())
            .add_observer(on_load)
            .add_observer(on_save)
            .add_observer(on_sync);
    }
}

/// Whether the Connectors page is open, so a connection that comes back reads them again for the page.
fn page_shown(shown: Option<Res<crate::settings::SettingsShown>>) -> bool {
    shown.is_some_and(|s| s.page() == Some(crate::settings::CONNECTORS))
}

fn load(mut settings: ResMut<ConnectorSettings>, mut net: MessageWriter<SendNet>) {
    settings.read_seq += 1;
    net.write(SendNet(NetCommand::Fetch(read(settings.read_seq))));
}

fn on_load(
    _: On<LoadConnectors>,
    connection: Option<Res<State<Connection>>>,
    settings: ResMut<ConnectorSettings>,
    net: MessageWriter<SendNet>,
) {
    if connection.is_some_and(|c| *c.get() == Connection::Live) {
        load(settings, net);
    }
}

fn read(seq: u64) -> Fetch {
    Fetch::new(move |api| async move {
        let result = match api {
            Some(api) => match api.list_connectors().await {
                Ok(list) => Ok((list, api.shell_build(None).await.ok().map(|b| b.configured))),
                Err(trailhead_client::http::Error::Api { status: 404, .. }) => Err(ReadError::Missing),
                Err(e) => Err(ReadError::Failed(said(&e))),
            },
            None => Err(ReadError::Failed("not connected".into())),
        };
        ConnectorsRead { seq, result }
    })
}

fn live(connection: Option<Res<State<Connection>>>) -> bool {
    connection.is_some_and(|c| *c.get() == Connection::Live)
}

fn on_save(
    save: On<SaveConnector>,
    connection: Option<Res<State<Connection>>>,
    mut settings: ResMut<ConnectorSettings>,
    mut net: MessageWriter<SendNet>,
) {
    let id = save.0.clone();
    if settings.save_gate(&id, live(connection), None) != Gate::Ready {
        return;
    }
    let Ok(Some(patch)) = settings.change(&id) else { return };
    settings.sent += 1;
    let seq = settings.sent;
    settings.save_seqs.insert(id.clone(), seq);
    settings.saves.insert(id.clone(), Saving { saving: true, ..default() });
    let fetch = Fetch::new(move |api| {
        let (id, patch) = (id.clone(), patch.clone());
        async move {
            let result = match api {
                Some(api) => api.update_connector(&id, patch).await.map_err(|e| said(&e)),
                None => Err("not connected to the server".into()),
            };
            ConnectorSaved { id, seq, result }
        }
    });
    net.write(SendNet(NetCommand::Fetch(fetch)));
}

fn on_sync(
    sync: On<SyncConnector>,
    connection: Option<Res<State<Connection>>>,
    mut settings: ResMut<ConnectorSettings>,
    mut net: MessageWriter<SendNet>,
) {
    let id = sync.0.clone();
    if settings.sync_gate(&id, live(connection), None).is_err() {
        return;
    }
    settings.sent += 1;
    let seq = settings.sent;
    settings.sync_seqs.insert(id.clone(), seq);
    settings.syncs.insert(id.clone(), Syncing::Starting);
    let fetch = Fetch::new(move |api| {
        let id = id.clone();
        async move {
            let result = match api {
                Some(api) => api.run_connector(&id).await.map_err(|e| said(&e)),
                None => Err("not connected to the server".into()),
            };
            ConnectorSynced { id, seq, result }
        }
    });
    net.write(SendNet(NetCommand::Fetch(fetch)));
}

/// A reading: the page shows it, keeping edits not yet saved. Only the latest read counts.
fn hear_read(mut replies: MessageReader<ConnectorsRead>, mut settings: ResMut<ConnectorSettings>) {
    for ConnectorsRead { seq, result } in replies.read() {
        if *seq != settings.read_seq {
            continue;
        }
        match result {
            Ok((list, runs)) => settings.answered(list.clone(), *runs),
            Err(ReadError::Missing) => settings.load = Load::Missing,
            Err(ReadError::Failed(why)) => settings.load = Load::Failed(why.clone()),
        }
    }
}

/// A save's answer: the connector as written, which its section then shows; or the server's refusal, with what was
/// typed kept. Either way the list is read again, for whatever else changed.
fn hear_saved(
    mut replies: MessageReader<ConnectorSaved>,
    mut settings: ResMut<ConnectorSettings>,
    mut net: MessageWriter<SendNet>,
) {
    let mut reread = false;
    for ConnectorSaved { id, seq, result } in replies.read() {
        if settings.save_seqs.get(id) != Some(seq) || !settings.save_state(id).saving {
            continue;
        }
        reread = true;
        match result {
            Ok(info) => {
                settings.saves.insert(id.clone(), Saving { saved: true, ..default() });
                settings.take(info.clone(), false);
            }
            Err(why) => {
                settings.saves.insert(id.clone(), Saving { refusal: Some(why.clone()), ..default() });
            }
        }
    }
    if reread {
        settings.read_seq += 1;
        net.write(SendNet(NetCommand::Fetch(read(settings.read_seq))));
    }
}

/// A Sync now's answer: the run started, and the connector shows it running; or why not, in the server's words.
fn hear_synced(
    mut replies: MessageReader<ConnectorSynced>,
    mut settings: ResMut<ConnectorSettings>,
    mut net: MessageWriter<SendNet>,
) {
    let mut reread = false;
    for ConnectorSynced { id, seq, result } in replies.read() {
        if settings.sync_seqs.get(id) != Some(seq) || settings.sync_state(id) != Syncing::Starting {
            continue;
        }
        reread = true;
        match result {
            Ok(info) => {
                let edited = settings.change(id).map_or(true, |p| p.is_some());
                settings.syncs.insert(id.clone(), if info.running { Syncing::Started } else { Syncing::Idle });
                settings.take(info.clone(), edited);
            }
            Err(why) => {
                settings.syncs.insert(id.clone(), Syncing::Refused(why.clone()));
            }
        }
    }
    if reread {
        settings.read_seq += 1;
        net.write(SendNet(NetCommand::Fetch(read(settings.read_seq))));
    }
}

/// A save or a run asked for when the link drops is never answered: the page says so, and the edits stay.
fn drop_in_flight(mut settings: ResMut<ConnectorSettings>) {
    let dropped = "The connection dropped before the server answered. Open the page again once it is back to see \
                   whether it went through.";
    for save in settings.saves.values_mut().filter(|s| s.saving) {
        *save = Saving { refusal: Some(dropped.into()), ..default() };
    }
    for sync in settings.syncs.values_mut().filter(|s| **s == Syncing::Starting) {
        *sync = Syncing::Refused(dropped.into());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;
    use crate::ShellPlugin;
    use crate::server_settings::tests::{body_of, run};
    use crate::testing::{Other, headless, set_connection};

    #[derive(Resource, Default)]
    struct Sent(Vec<Fetch>);

    fn count(mut sent: MessageReader<SendNet>, mut fetches: ResMut<Sent>) {
        for SendNet(command) in sent.read() {
            if let NetCommand::Fetch(fetch) = command {
                fetches.0.push(fetch.clone());
            }
        }
    }

    fn shell() -> App {
        let mut app = headless(ShellPlugin::new().with(Other));
        app.init_resource::<Sent>().add_systems(PostUpdate, count);
        set_connection(&mut app, Connection::Live);
        app
    }

    fn sent(app: &mut App) -> Vec<Fetch> {
        std::mem::take(&mut app.world_mut().resource_mut::<Sent>().0)
    }

    fn settings(app: &App) -> ConnectorSettings {
        app.world().resource::<ConnectorSettings>().clone()
    }

    fn edit(app: &mut App, edit: impl FnOnce(&mut ConnectorSettings)) {
        edit(&mut app.world_mut().resource_mut::<ConnectorSettings>());
    }

    /// A GitHub connector as the server shows one that is on, set, and ran a minute ago.
    pub(crate) fn github() -> serde_json::Value {
        json!({
            "id": "github", "name": "GitHub", "every_secs": 300, "timeout_secs": 120,
            "hosts": ["api.github.com"],
            "settings": [
                {"name": "repos", "kind": "list", "label": "Repositories", "help": "owner/name, one per line",
                 "required": true, "value": ["lanesawyer/trailhead"]},
                {"name": "me", "kind": "string", "label": "Who you are", "required": false, "value": "lanesawyer"},
                {"name": "max_pages", "kind": "integer", "label": "Pages at most", "required": false},
                {"name": "drafts", "kind": "boolean", "label": "Include drafts", "required": false, "value": true}
            ],
            "secrets": [{"name": "token", "set": true}],
            "feeds": [{"name": "pull_requests", "fields": {"title": "string"},
                       "collections": ["cairn.pull_requests"]}],
            "enabled": true, "missing": [], "running": false,
            "last_run": {"started_at": "2026-11-22T12:00:00Z", "duration_ms": 2140, "ok": true,
                         "feeds": {"pull_requests": {"records": 12, "created": 1, "changed": 2, "removed": 0,
                                                     "unchanged": 9}}},
            "next_run": "2026-11-22T12:05:00Z"
        })
    }

    pub(crate) fn info(value: serde_json::Value) -> ConnectorInfo {
        serde_json::from_value(value).unwrap()
    }

    fn at(time: &str) -> Timestamp {
        time.parse().unwrap()
    }

    /// A shell that has read `list`.
    fn loaded(list: serde_json::Value) -> App {
        let mut app = shell();
        sent(&mut app);
        app.world_mut().trigger(LoadConnectors);
        app.update();
        let fetch = sent(&mut app).remove(0);
        let build = json!({"configured": true, "current": "0123456789", "previous": null, "building": null,
                           "error": null, "bad": [], "changes": [], "changes_total": 0});
        let asked = run(&mut app, &fetch, &[("200 OK", list.to_string()), ("200 OK", build.to_string())]);
        assert!(asked[0].starts_with("GET /v1/connectors HTTP/1.1"), "{}", asked[0]);
        assert!(asked[1].starts_with("GET /v1/shell/build HTTP/1.1"), "{}", asked[1]);
        app
    }

    #[test]
    fn the_status_says_how_the_last_run_went_and_when_the_next_is() {
        let now = at("2026-11-22T12:01:30Z");
        let ok = info(github());
        assert_eq!(
            status_text(&ok, now),
            "Last run 1 min ago, took 2.1 s: ok.\npull_requests: 12 records, 1 new, 2 changed, 0 removed\nNext run \
             in 3 min."
        );

        let mut failed = github();
        failed["last_run"] = json!({"started_at": "2026-11-22T09:00:00Z", "duration_ms": 120000, "ok": false,
                                    "error": "it ran longer than its time limit of 120s and was stopped"});
        failed["next_run"] = json!("2026-11-22T12:00:00Z");
        assert_eq!(status_text(&info(failed), now), "Last run 3 h ago, took 2 min 0 s: failed.\nNext run: due now.");

        let mut running = github();
        running["running"] = json!(true);
        running["next_run"] = json!(null);
        assert_eq!(status_text(&info(running), now), "Running now.");

        let mut new = github();
        new["last_run"] = json!(null);
        new["enabled"] = json!(false);
        new["next_run"] = json!(null);
        assert_eq!(status_text(&info(new), now), "Never run.\nIt is off, so it doesn't run.");

        let mut needs = github();
        needs["missing"] =
            json!(["the secret `token` (`trailhead connector login github token` on the server's machine)"]);
        needs["last_run"]["feeds"] = json!({});
        let needs = info(needs);
        assert_eq!(
            status_text(&needs, now),
            "Last run 1 min ago, took 2.1 s: ok.\nIt handed nothing over: nothing had changed.\nIt doesn't run until it \
             has what it needs."
        );
        assert_eq!(
            missing_text(&needs),
            "Before it can run, it needs the secret `token` (`trailhead connector login github token` on the server's \
             machine)."
        );

        let mut unscheduled = github();
        unscheduled["next_run"] = json!(null);
        unscheduled["last_run"]["feeds"]["pull_requests"]["errors"] = json!(["cairn: no such collection"]);
        let unscheduled = info(unscheduled);
        assert!(status_text(&unscheduled, now).ends_with(
            "0 removed; refused by cairn: no such collection\nNot scheduled: this server doesn't run connectors on a \
             schedule."
        ));
        assert_eq!(feeds_text(&unscheduled), "pull_requests → cairn.pull_requests");
        assert_eq!(schedule_note(&unscheduled), "Runs every 5 min, each run stopped after 2 min. Off, it never runs.");
        assert_eq!(secrets_text(&unscheduled), "token: set");
        assert_eq!(login_command("github"), "trailhead connector login github");
    }

    #[test]
    fn a_long_error_is_clamped_with_the_rest_shown_on_asking() {
        assert_eq!(error_text("exit 1: bad token\n", false), ("exit 1: bad token".into(), false));
        let long: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        let (clamped, more) = error_text(&long, false);
        assert!(more);
        assert_eq!(clamped, "line 1\nline 2\nline 3\nline 4…");
        assert_eq!(error_text(&long, true), (long.trim().to_string(), true));
        let wide = "x".repeat(ERROR_CHARS + 10);
        assert_eq!(error_text(&wide, false).0.chars().count(), ERROR_CHARS + 1);
    }

    #[test]
    fn save_sends_only_what_changed_for_each_kind_of_setting() {
        let github = info(github());
        let draft = ConnectorDraft::from(&github);
        assert_eq!(draft.patch(&github), Ok(None), "nothing changed, nothing to send");
        let typed = |name: &str, typed: Typed| {
            let mut draft = draft.clone();
            draft.values.insert(name.into(), typed);
            draft.patch(&github).map(|p| p.map(|p| serde_json::to_value(p).unwrap()))
        };
        assert_eq!(typed("me", Typed::Text(" lane ".into())), Ok(Some(json!({"settings": {"me": "lane"}}))));
        assert_eq!(typed("me", Typed::Text(" lanesawyer".into())), Ok(None), "the same once trimmed");
        assert_eq!(typed("me", Typed::Text(String::new())), Ok(Some(json!({"settings": {"me": null}}))));
        assert_eq!(typed("max_pages", Typed::Text("10".into())), Ok(Some(json!({"settings": {"max_pages": 10}}))));
        assert_eq!(typed("max_pages", Typed::Text(String::new())), Ok(None), "unset and left empty");
        assert_eq!(typed("max_pages", Typed::Text("ten".into())), Err("Pages at most is a whole number.".into()));
        assert_eq!(typed("drafts", Typed::Switch(false)), Ok(Some(json!({"settings": {"drafts": false}}))));
        let repos = Typed::List(vec!["lanesawyer/trailhead".into(), "lanesawyer/cairn".into()]);
        assert_eq!(
            typed("repos", repos),
            Ok(Some(json!({"settings": {"repos": ["lanesawyer/trailhead", "lanesawyer/cairn"]}})))
        );
        assert_eq!(typed("repos", Typed::List(Vec::new())), Ok(Some(json!({"settings": {"repos": null}}))));

        let mut off = draft.clone();
        off.enabled = false;
        off.values.insert("me".into(), Typed::Text("lane".into()));
        let patch = serde_json::to_value(off.patch(&github).unwrap().unwrap()).unwrap();
        assert_eq!(patch, json!({"enabled": false, "settings": {"me": "lane"}}));

        let mut unset_switch = github.clone();
        unset_switch.settings[3].value = None;
        assert_eq!(ConnectorDraft::from(&unset_switch).values["drafts"], Typed::Switch(false));
        assert_eq!(ConnectorDraft::from(&unset_switch).patch(&unset_switch), Ok(None), "unset reads as off");
    }

    #[test]
    fn sync_now_is_pressed_only_when_it_can_run_and_says_why_not() {
        let mut s = ConnectorSettings::default();
        s.answered(vec![info(github())], Some(true));
        assert_eq!(s.sync_gate("github", true, None), Ok(()));
        assert_eq!(
            s.sync_gate("github", false, Some("the server isn't answering".into())),
            Err("Not connected: the server isn't answering.".into())
        );
        let gate = |change: &dyn Fn(&mut serde_json::Value)| {
            let mut value = github();
            change(&mut value);
            let mut s = ConnectorSettings::default();
            s.answered(vec![info(value)], Some(true));
            s.sync_gate("github", true, None).unwrap_err()
        };
        assert_eq!(gate(&|v| v["running"] = json!(true)), "It is running now.");
        assert_eq!(gate(&|v| v["enabled"] = json!(false)), "It is off: turn it on and save first.");
        assert_eq!(gate(&|v| v["missing"] = json!(["a build"])), "It can't run until it has what it needs.");
        assert_eq!(gate(&|v| v["error"] = json!("bad toml")), "Its connector.toml doesn't read.");
        s.syncs.insert("github".into(), Syncing::Starting);
        assert_eq!(s.sync_gate("github", true, None), Err("Starting a run…".into()));
        assert_eq!(s.poll_every(), POLL_RUNNING);
    }

    #[test]
    fn a_save_sends_only_what_changed_and_shows_what_the_server_answered() {
        let mut app = loaded(json!([github()]));
        let read = settings(&app);
        assert_eq!((read.load.clone(), read.runs_connectors), (Load::Loaded, Some(true)));
        assert_eq!(read.save_gate("github", true, None), Gate::Unchanged);
        edit(&mut app, |s| {
            let draft = s.drafts.get_mut("github").unwrap();
            draft.values.insert("me".into(), Typed::Text("lane".into()));
        });
        assert_eq!(settings(&app).save_gate("github", true, None), Gate::Ready);
        app.world_mut().trigger(SaveConnector("github".into()));
        app.world_mut().trigger(SaveConnector("github".into()));
        app.update();
        let fetches = sent(&mut app);
        assert_eq!(fetches.len(), 1, "a second press while saving sends nothing");
        let mut saved = github();
        saved["settings"][1]["value"] = json!("lane");
        let asked = run(&mut app, &fetches[0], &[("200 OK", saved.to_string())]);
        assert!(asked[0].starts_with("PATCH /v1/connectors/github HTTP/1.1"), "{}", asked[0]);
        assert_eq!(body_of(&asked[0]), json!({"settings": {"me": "lane"}}));
        let after = settings(&app);
        assert_eq!(after.save_state("github"), Saving { saved: true, ..Default::default() });
        assert_eq!(after.info("github").unwrap().settings[1].value, Some(json!("lane")), "what the server answered");
        assert_eq!(after.save_gate("github", true, None), Gate::Unchanged);
        assert_eq!(sent(&mut app).len(), 1, "and the list is read again");
    }

    #[test]
    fn a_refusal_keeps_what_was_typed_and_says_why_in_the_servers_words() {
        let mut app = loaded(json!([github()]));
        edit(&mut app, |s| s.drafts.get_mut("github").unwrap().enabled = false);
        app.world_mut().trigger(SaveConnector("github".into()));
        app.update();
        let fetch = sent(&mut app).remove(0);
        let words = "github: setting `repos` must be a list";
        let refused = json!({"error": {"code": "invalid", "message": words}}).to_string();
        let asked = run(&mut app, &fetch, &[("400 Bad Request", refused)]);
        assert_eq!(body_of(&asked[0]), json!({"enabled": false}));
        let after = settings(&app);
        assert_eq!(after.save_state("github"), Saving { refusal: Some(words.into()), ..Default::default() });
        assert!(!after.drafts["github"].enabled, "nothing typed is lost");

        let reread = sent(&mut app).remove(0);
        let build = json!({"configured": true}).to_string();
        run(&mut app, &reread, &[("200 OK", json!([github()]).to_string()), ("200 OK", build)]);
        assert!(!settings(&app).drafts["github"].enabled, "a reading keeps edits not yet saved");
    }

    #[test]
    fn sync_now_starts_a_run_and_a_refusal_is_said_in_the_servers_words() {
        let mut app = loaded(json!([github()]));
        app.world_mut().trigger(SyncConnector("github".into()));
        app.update();
        assert_eq!(settings(&app).sync_state("github"), Syncing::Starting);
        let fetch = sent(&mut app).remove(0);
        let mut running = github();
        running["running"] = json!(true);
        let asked = run(&mut app, &fetch, &[("202 Accepted", running.to_string())]);
        assert!(asked[0].starts_with("POST /v1/connectors/github/run HTTP/1.1"), "{}", asked[0]);
        let after = settings(&app);
        assert_eq!(after.sync_state("github"), Syncing::Started);
        assert_eq!(after.sync_gate("github", true, None), Err("It is running now.".into()));

        let reread = sent(&mut app).remove(0);
        let build = json!({"configured": true}).to_string();
        run(&mut app, &reread, &[("200 OK", json!([github()]).to_string()), ("200 OK", build)]);
        assert_eq!(settings(&app).sync_state("github"), Syncing::Idle, "followed until it ended");

        app.world_mut().trigger(SyncConnector("github".into()));
        app.update();
        let fetch = sent(&mut app).remove(0);
        let words = "a run of connector github: it is running now";
        let busy = json!({"error": {"code": "already_exists", "message": words}}).to_string();
        run(&mut app, &fetch, &[("409 Conflict", busy)]);
        assert_eq!(settings(&app).sync_state("github"), Syncing::Refused(words.into()));
    }

    #[test]
    fn a_server_without_connectors_or_an_apps_repo_is_told_apart() {
        let mut app = shell();
        sent(&mut app);
        app.world_mut().trigger(LoadConnectors);
        app.update();
        let fetch = sent(&mut app).remove(0);
        let missing = json!({"error": {"code": "not_found", "message": "no such route"}}).to_string();
        run(&mut app, &fetch, &[("404 Not Found", missing)]);
        assert_eq!(settings(&app).load, Load::Missing);

        let app = {
            let mut app = shell();
            sent(&mut app);
            app.world_mut().trigger(LoadConnectors);
            app.update();
            let fetch = sent(&mut app).remove(0);
            run(&mut app, &fetch, &[("200 OK", "[]".into()), ("200 OK", json!({"configured": false}).to_string())]);
            app
        };
        let read = settings(&app);
        assert_eq!((read.load, read.list.len(), read.runs_connectors), (Load::Loaded, 0, Some(false)));
    }
}
