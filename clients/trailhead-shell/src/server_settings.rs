//! The Server settings page's logic, headless: what the server's user config says (`GET /v1/config`, with the
//! builder's state from `GET /v1/shell/build`), read each time the page opens and when the connection comes back with
//! it open; the
//! edits made on the page, one draft per section ([`BuildsDraft`], [`FactoryDraft`]); whether a section can be saved
//! and why not ([`ServerSettings::gate`]); and saving it, `PATCH /v1/config` with only what changed and the version of
//! the file it was made against.
//!
//! Nothing is shown before the server says it: a save shows the server's answer, never the edit. A refusal keeps what
//! was typed and says why in the server's words; a file changed on disk since it was read (`412`) is read again and
//! the page shows what it now says. The running server reads the file only when it starts, so a saved change waits
//! for a restart, which the page says how to do ([`restart_command`]).

use std::collections::BTreeMap;

use bevy::prelude::*;
use trailhead_model::config::{ConfigChange, FactoryChange, FactorySettings, ServerConfig, ShellChange, ShellSettings};
use trailhead_model::shell::ShellBuild;

use crate::connection::{Connection, SendNet};
use crate::net::{Fetch, NetCommand};

/// The usage windows the factory can pause at, as `[factory] pause_at` names them.
pub const FIVE_HOUR: &str = "five_hour";
pub const SEVEN_DAY: &str = "seven_day";

/// A part of the page saved on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    /// `[shell]`: how the server builds the window.
    Builds,
    /// `[factory]`.
    Factory,
}

/// Where reading the settings is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Load {
    #[default]
    Loading,
    Loaded,
    /// The server has no `/v1/config`: it is older than the page, or reads no user config.
    Missing,
    /// The read failed, and why.
    Failed(String),
}

/// `[shell]` as typed on the page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildsDraft {
    pub repo: String,
    pub profile: String,
    pub jobs: String,
    pub poll_secs: String,
}

impl BuildsDraft {
    pub fn from(settings: &ShellSettings) -> BuildsDraft {
        BuildsDraft {
            repo: settings.repo.clone().unwrap_or_default(),
            profile: settings.profile.clone(),
            jobs: settings.jobs.to_string(),
            poll_secs: settings.poll_secs.to_string(),
        }
    }

    /// What saving it would change, `None` when nothing; an error says what can't be sent. An emptied field takes the
    /// key out of the file, back to its default.
    pub fn change(&self, now: &ShellSettings) -> Result<Option<ShellChange>, String> {
        let text = |typed: &str| Some(typed.trim().to_string()).filter(|t| !t.is_empty());
        let repo = text(&self.repo);
        let profile = text(&self.profile);
        let jobs = number(&self.jobs, "Jobs")?;
        let poll_secs = number(&self.poll_secs, "How often it looks")?;
        let default = |key: &str| now.defaults.iter().any(|d| d == key);
        let change = ShellChange {
            repo: (repo != now.repo).then_some(repo),
            package: None,
            profile: edited(profile, &now.profile, default("profile")),
            jobs: edited(jobs, &now.jobs, default("jobs")),
            poll_secs: edited(poll_secs, &now.poll_secs, default("poll_secs")),
        };
        Ok(Some(change).filter(|c| *c != ShellChange::default()))
    }
}

/// What to send for a key with a default: nothing when `typed` is what the file says, or empty while the key is at its
/// default; `null` for a key emptied that the file sets; else the new value.
fn edited<T: PartialEq>(typed: Option<T>, now: &T, is_default: bool) -> Option<Option<T>> {
    match typed {
        Some(value) if value == *now => None,
        None if is_default => None,
        typed => Some(typed),
    }
}

/// `[factory]` as edited on the page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactoryDraft {
    pub repos: Vec<String>,
    /// Whether the factory runs: `disabled` turned around.
    pub on: bool,
    pub five_hour: String,
    pub seven_day: String,
}

impl FactoryDraft {
    pub fn from(settings: &FactorySettings) -> FactoryDraft {
        let percent = |window: &str| settings.pause_at.get(window).map(u32::to_string).unwrap_or_default();
        FactoryDraft {
            repos: settings.repos.clone(),
            on: !settings.disabled,
            five_hour: percent(FIVE_HOUR),
            seven_day: percent(SEVEN_DAY),
        }
    }

    /// What saving it would change, `None` when nothing; an error says what can't be sent.
    pub fn change(&self, now: &FactorySettings) -> Result<Option<FactoryChange>, String> {
        let mut pause_at = BTreeMap::new();
        for (window, typed, name) in
            [(FIVE_HOUR, &self.five_hour, "The five-hour pause"), (SEVEN_DAY, &self.seven_day, "The seven-day pause")]
        {
            if let Some(percent) = number(typed, name)? {
                pause_at.insert(window.to_string(), percent);
            }
        }
        let change = FactoryChange {
            repos: (self.repos != now.repos).then(|| self.repos.clone()),
            disabled: (self.on == now.disabled).then_some(!self.on),
            pause_at: (pause_at != now.pause_at).then_some(pause_at),
        };
        Ok(Some(change).filter(|c| *c != FactoryChange::default()))
    }
}

/// A whole number typed in a field, `None` when it is empty.
fn number<T: std::str::FromStr>(typed: &str, name: &str) -> Result<Option<T>, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Ok(None);
    }
    typed.parse().map(Some).map_err(|_| format!("{name} is a whole number."))
}

/// A section's save.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionState {
    /// The change is with the server.
    pub saving: bool,
    /// What the server said when it refused the last save, in its words.
    pub refusal: Option<String>,
    /// The last save was written.
    pub saved: bool,
}

/// Whether a section's Save can be pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Ready,
    /// Nothing is different from what the file says.
    Unchanged,
    Saving,
    /// Not now, and why, for the section's notice.
    Blocked(String),
}

/// What the page knows and what is typed on it.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerSettings {
    /// What the server last said of its user config.
    pub config: Option<ServerConfig>,
    /// What its builder last said, for the line about the builds.
    pub build: Option<ShellBuild>,
    pub load: Load,
    pub builds: BuildsDraft,
    pub factory: FactoryDraft,
    pub builds_save: SectionState,
    pub factory_save: SectionState,
    /// A save found the file changed on disk, and the page was read again from it.
    pub changed_on_disk: bool,
    /// The latest read and save sent: an answer to an older one is dropped.
    read_seq: u64,
    save_seq: u64,
}

impl ServerSettings {
    pub fn save_state(&self, section: Section) -> &SectionState {
        match section {
            Section::Builds => &self.builds_save,
            Section::Factory => &self.factory_save,
        }
    }

    fn save_state_mut(&mut self, section: Section) -> &mut SectionState {
        match section {
            Section::Builds => &mut self.builds_save,
            Section::Factory => &mut self.factory_save,
        }
    }

    /// The change a section's Save would send.
    pub fn change(&self, section: Section) -> Result<Option<ConfigChange>, String> {
        let Some(config) = &self.config else { return Ok(None) };
        let change = |shell, factory| ConfigChange { version: config.version.clone(), shell, factory };
        Ok(match section {
            Section::Builds => match &config.shell {
                Some(now) => self.builds.change(now)?.map(|c| change(Some(c), None)),
                None => None,
            },
            Section::Factory => match &config.factory {
                Some(now) => self.factory.change(now)?.map(|c| change(None, Some(c))),
                None => None,
            },
        })
    }

    /// Whether a section's Save can be pressed now, and why not.
    pub fn gate(&self, section: Section, live: bool, offline: Option<String>) -> Gate {
        if !live {
            return Gate::Blocked(
                offline.map_or("Not connected to the server.".into(), |r| format!("Not connected: {r}.")),
            );
        }
        if self.save_state(section).saving {
            return Gate::Saving;
        }
        match self.change(section) {
            Err(why) => Gate::Blocked(why),
            Ok(None) => Gate::Unchanged,
            Ok(Some(_)) => Gate::Ready,
        }
    }

    /// Takes the server's answer to a read, as the page does when it arrives: what a test or a shot starts from.
    pub fn answered(&mut self, config: ServerConfig, build: Option<ShellBuild>) {
        self.read(config, true);
        self.build = build;
    }

    /// Takes a reading of the file: each section's draft follows it, unless it holds edits not yet saved.
    fn read(&mut self, config: ServerConfig, keep_edits: bool) {
        let builds_edited = keep_edits && self.change(Section::Builds).map_or(true, |c| c.is_some());
        let factory_edited = keep_edits && self.change(Section::Factory).map_or(true, |c| c.is_some());
        if !builds_edited && let Some(shell) = &config.shell {
            self.builds = BuildsDraft::from(shell);
        }
        if !factory_edited && let Some(factory) = &config.factory {
            self.factory = FactoryDraft::from(factory);
        }
        self.config = Some(config);
        self.load = Load::Loaded;
    }
}

/// The settings named in [`ServerConfig::waiting`], in words.
pub fn waiting_text(waiting: &[String]) -> String {
    let names: Vec<&str> = waiting
        .iter()
        .map(|key| match key.as_str() {
            "shell" => "the window builds",
            "shell.repo" => "the apps repo",
            "shell.package" => "the package",
            "shell.profile" => "the build profile",
            "shell.jobs" => "the jobs",
            "shell.poll_secs" => "how often it looks",
            "factory" => "the factory",
            "factory.repos" => "the repos",
            "factory.disabled" => "whether the factory runs",
            "factory.pause_at" => "the usage pauses",
            "factory.max_sandboxes" => "the sandbox cap",
            other => other,
        })
        .collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The command that restarts the server: `trailhead server restart` for one the CLI started in the background; for
/// one started at a terminal, what to run there after Ctrl-C.
pub fn restart_command(foreground: bool) -> &'static str {
    if foreground { "trailhead factory run" } else { "trailhead server restart" }
}

/// How to restart the server, said under the command.
pub fn restart_how(foreground: bool) -> &'static str {
    if foreground {
        "The server runs in a terminal: stop it there with Ctrl-C, then start it again the way it was started, this \
         or trailhead serve."
    } else {
        "Run this on the server's machine. The factory's runs finish first, as for any restart."
    }
}

/// The line about the builder: what it is building or built, from `GET /v1/shell/build`.
pub fn build_text(build: Option<&ShellBuild>) -> String {
    let Some(build) = build else { return "Not known yet".into() };
    let short = |rev: &str| rev.get(..7).unwrap_or(rev).to_string();
    if !build.configured {
        return "No builder: the server has no apps repo".into();
    }
    match (&build.building, &build.current, &build.error) {
        (Some(rev), _, _) => format!("Building {}", short(rev)),
        (None, _, Some(error)) => format!("The build of {} failed: {}", short(&error.rev), error.message),
        (None, Some(rev), None) => format!("Built {}", short(rev)),
        (None, None, None) => "Nothing built yet".into(),
    }
}

/// Reads the settings again: the page triggers it when it opens.
#[derive(Event, Debug, Clone, Copy)]
pub struct LoadServerSettings;

/// Saves a section, if [`ServerSettings::gate`] lets it.
#[derive(Event, Debug, Clone, Copy)]
pub struct SaveServerSettings(pub Section);

/// Why a read failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// `404`: no `/v1/config` on this server.
    Missing,
    Failed(String),
}

/// Why a save failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// `412`: the file changed on disk since it was read.
    Stale,
    /// The server refused it, in its words.
    Refused(String),
}

/// The server's answer to the read sent as `seq`.
#[derive(Message, Debug, Clone)]
pub struct SettingsRead {
    pub seq: u64,
    pub result: Result<(ServerConfig, Option<ShellBuild>), ReadError>,
}

/// The server's answer to a section's save sent as `seq`.
#[derive(Message, Debug, Clone)]
pub struct SettingsSaved {
    pub seq: u64,
    pub section: Section,
    pub result: Result<ServerConfig, SaveError>,
}

/// The page's logic, in a shell with a connection.
pub(crate) struct ServerSettingsPlugin;

impl Plugin for ServerSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ServerSettings>()
            .add_message::<SettingsRead>()
            .add_message::<SettingsSaved>()
            .add_message::<SendNet>()
            .add_systems(OnEnter(Connection::Live), load.run_if(page_shown))
            .add_systems(OnExit(Connection::Live), drop_in_flight)
            .add_systems(Update, (hear_read, hear_saved).chain())
            .add_observer(on_load)
            .add_observer(on_save);
    }
}

/// Whether the Server page is open, so a connection that comes back reads it again for the page.
fn page_shown(shown: Option<Res<crate::settings::SettingsShown>>) -> bool {
    shown.is_some_and(|s| s.page() == Some(crate::settings::SERVER))
}

fn load(mut settings: ResMut<ServerSettings>, mut net: MessageWriter<SendNet>) {
    settings.read_seq += 1;
    net.write(SendNet(NetCommand::Fetch(read(settings.read_seq))));
}

fn on_load(
    _: On<LoadServerSettings>,
    connection: Option<Res<State<Connection>>>,
    settings: ResMut<ServerSettings>,
    net: MessageWriter<SendNet>,
) {
    if connection.is_some_and(|c| *c.get() == Connection::Live) {
        load(settings, net);
    }
}

fn read(seq: u64) -> Fetch {
    Fetch::new(move |api| async move {
        let result = match api {
            Some(api) => match api.config().await {
                Ok(config) => Ok((config, api.shell_build(None).await.ok())),
                Err(trailhead_client::http::Error::Api { status: 404, .. }) => Err(ReadError::Missing),
                Err(e) => Err(ReadError::Failed(said(&e))),
            },
            None => Err(ReadError::Failed("not connected".into())),
        };
        SettingsRead { seq, result }
    })
}

fn on_save(
    save: On<SaveServerSettings>,
    connection: Option<Res<State<Connection>>>,
    mut settings: ResMut<ServerSettings>,
    mut net: MessageWriter<SendNet>,
) {
    let section = save.0;
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    if settings.gate(section, live, None) != Gate::Ready {
        return;
    }
    let Ok(Some(change)) = settings.change(section) else { return };
    settings.save_seq += 1;
    settings.changed_on_disk = false;
    *settings.save_state_mut(section) = SectionState { saving: true, ..default() };
    net.write(SendNet(NetCommand::Fetch(write(settings.save_seq, section, change))));
}

fn write(seq: u64, section: Section, change: ConfigChange) -> Fetch {
    Fetch::new(move |api| {
        let change = change.clone();
        async move {
            let result = match api {
                Some(api) => api.update_config(&change).await.map_err(|e| match e {
                    trailhead_client::http::Error::Api { status: 412, .. } => SaveError::Stale,
                    other => SaveError::Refused(said(&other)),
                }),
                None => Err(SaveError::Refused("not connected to the server".into())),
            };
            SettingsSaved { seq, section, result }
        }
    })
}

/// The server's own words for a refusal, or the whole chain for anything else.
pub(crate) fn said(e: &trailhead_client::http::Error) -> String {
    match e {
        trailhead_client::http::Error::Api { error, .. } => error.message.clone(),
        other => other.chain(),
    }
}

/// A reading of the file: the page shows it, keeping edits not yet saved. Only the latest read or save counts.
fn hear_read(mut replies: MessageReader<SettingsRead>, mut settings: ResMut<ServerSettings>) {
    for SettingsRead { seq, result } in replies.read() {
        if *seq != settings.read_seq {
            continue;
        }
        match result {
            Ok((config, build)) => {
                let keep = !settings.changed_on_disk;
                settings.read(config.clone(), keep);
                settings.build = build.clone();
            }
            Err(ReadError::Missing) => settings.load = Load::Missing,
            Err(ReadError::Failed(why)) => settings.load = Load::Failed(why.clone()),
        }
    }
}

/// A save's answer: the file as written, which the section then shows; the server's refusal, with what was typed
/// kept; or a file changed on disk, which is read again and shown as it is.
fn hear_saved(
    mut replies: MessageReader<SettingsSaved>,
    mut settings: ResMut<ServerSettings>,
    mut net: MessageWriter<SendNet>,
) {
    for SettingsSaved { seq, section, result } in replies.read() {
        if *seq != settings.save_seq || !settings.save_state(*section).saving {
            continue;
        }
        match result {
            Ok(config) => {
                // A read sent before this save may answer after it, with the file as it was.
                settings.read_seq += 1;
                *settings.save_state_mut(*section) = SectionState { saved: true, ..default() };
                let edits = settings.clone();
                settings.read(config.clone(), false);
                let other = match section {
                    Section::Builds => Section::Factory,
                    Section::Factory => Section::Builds,
                };
                if edits.change(other).map_or(true, |c| c.is_some()) {
                    match other {
                        Section::Builds => settings.builds = edits.builds,
                        Section::Factory => settings.factory = edits.factory,
                    }
                }
            }
            Err(SaveError::Refused(why)) => {
                *settings.save_state_mut(*section) = SectionState { refusal: Some(why.clone()), ..default() };
            }
            Err(SaveError::Stale) => {
                *settings.save_state_mut(*section) = SectionState::default();
                settings.changed_on_disk = true;
                settings.read_seq += 1;
                net.write(SendNet(NetCommand::Fetch(read(settings.read_seq))));
            }
        }
    }
}

/// A save in flight when the link drops is never answered: the page says so, and the edits stay to save again.
fn drop_in_flight(mut settings: ResMut<ServerSettings>) {
    for section in [Section::Builds, Section::Factory] {
        if settings.save_state(section).saving {
            *settings.save_state_mut(section) = SectionState {
                refusal: Some(
                    "The connection dropped before the server answered. Open the page again once it is back to see \
                     whether the change was written."
                        .into(),
                ),
                ..default()
            };
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;
    use crate::ShellPlugin;
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
        app
    }

    fn sent(app: &mut App) -> Vec<Fetch> {
        std::mem::take(&mut app.world_mut().resource_mut::<Sent>().0)
    }

    fn settings(app: &App) -> ServerSettings {
        app.world().resource::<ServerSettings>().clone()
    }

    fn edit(app: &mut App, edit: impl FnOnce(&mut ServerSettings)) {
        edit(&mut app.world_mut().resource_mut::<ServerSettings>());
    }

    /// A reply as a server with an apps repo and a factory sends it.
    pub(crate) fn reply() -> serde_json::Value {
        json!({
            "path": "/home/someone/.config/trailhead/config.toml",
            "version": "v1",
            "factory": {"repos": ["~/Dev/trailhead"], "disabled": false, "pause_at": {"five_hour": 80},
                        "max_sandboxes": null, "defaults": ["disabled", "max_sandboxes"]},
            "factory_built": true,
            "shell": {"repo": "~/Dev/trailhead-apps", "package": null, "profile": "release", "jobs": 3,
                      "poll_secs": 60, "defaults": ["package", "profile", "jobs", "poll_secs"]},
            "problem": null,
            "running": {},
            "restart_needed": false,
            "waiting": [],
            "foreground": false
        })
    }

    /// Runs `fetch` against a server answering each request with the next of `answers`, and says what was asked.
    pub(crate) fn run(app: &mut App, fetch: &Fetch, answers: &[(&str, String)]) -> Vec<String> {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let answers: Vec<(String, String)> = answers.iter().map(|(s, b)| (s.to_string(), b.clone())).collect();
        let served = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for (status, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = vec![0u8; 16384];
                let n = stream.read(&mut request).unwrap();
                let head = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
                    body.len()
                );
                write!(stream, "{head}connection: close\r\n\r\n{body}").unwrap();
                asked.push(String::from_utf8_lossy(&request[..n]).into_owned());
            }
            asked
        });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let fetched = runtime.block_on(fetch.run(Some(trailhead_client::http::Client::new(base, "token"))));
        fetched.deliver(app.world_mut());
        app.update();
        served.join().unwrap()
    }

    pub(crate) fn body_of(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    fn build_reply() -> String {
        json!({"configured": true, "current": "0123456789", "previous": null, "building": null, "error": null,
               "bad": [], "changes": [], "changes_total": 0})
        .to_string()
    }

    /// A shell that has read [`reply`].
    fn loaded() -> App {
        let mut app = shell();
        set_connection(&mut app, Connection::Live);
        sent(&mut app);
        app.world_mut().trigger(LoadServerSettings);
        app.update();
        let fetch = sent(&mut app).remove(0);
        let asked = run(&mut app, &fetch, &[("200 OK", reply().to_string()), ("200 OK", build_reply())]);
        assert!(asked[0].starts_with("GET /v1/config HTTP/1.1"), "{}", asked[0]);
        assert!(asked[1].starts_with("GET /v1/shell/build HTTP/1.1"), "{}", asked[1]);
        app
    }

    #[test]
    fn the_page_reads_the_settings_into_its_drafts() {
        let app = loaded();
        let read = settings(&app);
        assert_eq!(read.load, Load::Loaded);
        let builds = BuildsDraft {
            repo: "~/Dev/trailhead-apps".into(),
            profile: "release".into(),
            jobs: "3".into(),
            poll_secs: "60".into(),
        };
        assert_eq!(read.builds, builds);
        let factory = FactoryDraft {
            repos: vec!["~/Dev/trailhead".into()],
            on: true,
            five_hour: "80".into(),
            seven_day: String::new(),
        };
        assert_eq!(read.factory, factory);
        assert_eq!(build_text(read.build.as_ref()), "Built 0123456");
        assert_eq!(read.gate(Section::Builds, true, None), Gate::Unchanged, "nothing to save until something changes");
        assert_eq!(
            read.gate(Section::Factory, false, Some("the server isn't answering".into())),
            Gate::Blocked("Not connected: the server isn't answering.".into())
        );
    }

    #[test]
    fn an_older_server_without_the_route_is_said_so() {
        let mut app = shell();
        set_connection(&mut app, Connection::Live);
        sent(&mut app);
        app.world_mut().trigger(LoadServerSettings);
        app.update();
        let fetch = sent(&mut app).remove(0);
        let missing = json!({"error": {"code": "not_found", "message": "no such route"}}).to_string();
        run(&mut app, &fetch, &[("404 Not Found", missing)]);
        assert_eq!(settings(&app).load, Load::Missing);
    }

    #[test]
    fn each_save_sends_only_what_changed_with_the_version_it_was_made_against() {
        let mut app = loaded();
        edit(&mut app, |s| {
            s.builds.jobs = " 4 ".into();
            s.builds.profile = String::new();
            s.factory.repos.push("~/Dev/other".into());
            s.factory.on = false;
            s.factory.five_hour = String::new();
            s.factory.seven_day = "90".into();
        });
        let s = settings(&app);
        let builds = s.change(Section::Builds).unwrap().unwrap();
        assert_eq!(serde_json::to_value(&builds).unwrap(), json!({"version": "v1", "shell": {"jobs": 4}}));
        let factory = s.change(Section::Factory).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&factory).unwrap(),
            json!({"version": "v1", "factory": {"repos": ["~/Dev/trailhead", "~/Dev/other"], "disabled": true,
                   "pause_at": {"seven_day": 90}}})
        );
        edit(&mut app, |s| s.builds.repo = String::new());
        let cleared = settings(&app).change(Section::Builds).unwrap().unwrap();
        assert_eq!(serde_json::to_value(&cleared.shell).unwrap(), json!({"repo": null, "jobs": 4}));
        edit(&mut app, |s| s.builds.jobs = "four".into());
        assert_eq!(settings(&app).gate(Section::Builds, true, None), Gate::Blocked("Jobs is a whole number.".into()));

        edit(&mut app, |s| s.builds.jobs = "4".into());
        app.world_mut().trigger(SaveServerSettings(Section::Builds));
        app.world_mut().trigger(SaveServerSettings(Section::Builds));
        app.update();
        let fetches = sent(&mut app);
        assert_eq!(fetches.len(), 1, "a second press while saving sends nothing");
        assert_eq!(settings(&app).gate(Section::Builds, true, None), Gate::Saving);
        let mut saved = reply();
        saved["version"] = json!("v2");
        saved["shell"]["repo"] = json!(null);
        saved["shell"]["jobs"] = json!(4);
        saved["restart_needed"] = json!(true);
        saved["waiting"] = json!(["shell.repo", "shell.jobs"]);
        let asked = run(&mut app, &fetches[0], &[("200 OK", saved.to_string())]);
        assert!(asked[0].starts_with("PATCH /v1/config HTTP/1.1"), "{}", asked[0]);
        assert_eq!(body_of(&asked[0]), json!({"version": "v1", "shell": {"repo": null, "jobs": 4}}));
        let after = settings(&app);
        assert_eq!(after.builds_save, SectionState { saved: true, ..Default::default() });
        assert_eq!((after.builds.repo.as_str(), after.builds.jobs.as_str()), ("", "4"), "what the server answered");
        assert_eq!(after.config.as_ref().unwrap().version, "v2");
        assert!(after.factory.repos.contains(&"~/Dev/other".to_string()), "the other section's edits stay");
        assert_eq!(waiting_text(&after.config.unwrap().waiting), "the apps repo and the jobs");
    }

    #[test]
    fn a_refusal_keeps_the_edits_and_says_why_in_the_servers_words() {
        let mut app = loaded();
        edit(&mut app, |s| s.factory.five_hour = "101".into());
        app.world_mut().trigger(SaveServerSettings(Section::Factory));
        app.update();
        let fetch = sent(&mut app).remove(0);
        let words = "/c/config.toml: [factory] pause_at for five_hour is 101; it's a percent, 0 to 100";
        let refused = json!({"error": {"code": "invalid", "message": words}}).to_string();
        run(&mut app, &fetch, &[("400 Bad Request", refused)]);
        let after = settings(&app);
        assert_eq!(after.factory_save, SectionState { refusal: Some(words.into()), ..Default::default() });
        assert_eq!(after.factory.five_hour, "101", "nothing typed is lost");
        assert_eq!(after.config.unwrap().version, "v1");
    }

    #[test]
    fn a_file_changed_on_disk_is_read_again_and_shown_as_it_is() {
        let mut app = loaded();
        edit(&mut app, |s| s.builds.jobs = "5".into());
        app.world_mut().trigger(SaveServerSettings(Section::Builds));
        app.update();
        let fetch = sent(&mut app).remove(0);
        let stale = json!({"error": {"code": "version_conflict", "message": "changed"}}).to_string();
        run(&mut app, &fetch, &[("412 Precondition Failed", stale)]);
        assert!(settings(&app).changed_on_disk);
        let reread = sent(&mut app).remove(0);
        let mut by_hand = reply();
        by_hand["version"] = json!("v3");
        by_hand["shell"]["jobs"] = json!(2);
        run(&mut app, &reread, &[("200 OK", by_hand.to_string()), ("200 OK", build_reply())]);
        let after = settings(&app);
        assert_eq!((after.builds.jobs.as_str(), after.config.unwrap().version.as_str()), ("2", "v3"));
        assert!(after.changed_on_disk && after.builds_save == SectionState::default());
    }

    #[test]
    fn a_read_while_saving_neither_loses_the_save_nor_undoes_it() {
        let mut app = loaded();
        edit(&mut app, |s| s.builds.jobs = "4".into());
        app.world_mut().trigger(SaveServerSettings(Section::Builds));
        app.update();
        let save = sent(&mut app).remove(0);
        app.world_mut().trigger(LoadServerSettings);
        app.update();
        let reread = sent(&mut app).remove(0);
        let mut saved = reply();
        saved["version"] = json!("v2");
        saved["shell"]["jobs"] = json!(4);
        run(&mut app, &save, &[("200 OK", saved.to_string())]);
        assert_eq!(settings(&app).builds_save, SectionState { saved: true, ..Default::default() });
        run(&mut app, &reread, &[("200 OK", reply().to_string()), ("200 OK", build_reply())]);
        let after = settings(&app);
        assert_eq!((after.config.unwrap().version.as_str(), after.builds.jobs.as_str()), ("v2", "4"));
    }

    #[test]
    fn the_restart_is_said_for_how_the_server_runs() {
        assert_eq!(restart_command(false), "trailhead server restart");
        assert_eq!(restart_command(true), "trailhead factory run");
        assert!(restart_how(true).contains("Ctrl-C"));
        assert_eq!(waiting_text(&["factory.repos".into()]), "the repos");
        let three = ["shell.jobs", "factory.pause_at", "factory.disabled"].map(String::from);
        assert_eq!(waiting_text(&three), "the jobs, the usage pauses and whether the factory runs");
    }
}
