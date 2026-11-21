//! Asking for a change or a new app from the window (`docs/app-platform.md`, "Asking for an app"). The rail's Ask
//! button ([`crate::rail::AskButton`]) opens a form ([`crate::ask_panel`]) whose ask becomes a task the factory
//! builds: a change to one of the apps the server can be asked about ([`Askable`]), or a new app.
//!
//! This is the form's logic, headless: what the server says can be asked about, read with `GET /v1/shell/ask` when
//! the connection goes live and each time the form opens; the draft ([`AskForm`]); whether it can be sent and why not
//! ([`AskForm::gate`]); and sending it, `POST /v1/shell/ask` over the shell's connection as a [`Fetch`], since its
//! answer (the task and project made) is what the form shows. Each distinct ask carries an idempotency key of its own,
//! so asking again after an answer was lost replays the first instead of making a second task.

use bevy::prelude::*;
use trailhead_model::shell::{Ask, AskTarget, AskTargets, Asked, MAX_ASK_CHARS};

use crate::connection::{Connection, ConnectionStatus, SendNet};
use crate::net::{Fetch, NetCommand};
use crate::{ActiveApp, AppId};

/// The last choice in the form's "about" dropdown, after every app.
pub const NEW_APP: &str = "A new app";

/// What the server last said can be asked about; `None` until it has answered.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct Askable(pub Option<AskTargets>);

impl Askable {
    /// Whether the window offers to ask: the server has an apps repo.
    pub fn configured(&self) -> bool {
        self.0.as_ref().is_some_and(|t| t.configured)
    }

    pub fn targets(&self) -> &[AskTarget] {
        self.0.as_ref().map_or(&[], |t| &t.targets)
    }
}

/// What an ask is about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum About {
    /// A change to the app whose project has this key.
    Project(String),
    #[default]
    NewApp,
}

impl About {
    /// The default for a form opened over the app `active`: the project building `apps/<active>`, else the first
    /// app, else a new one.
    pub fn default_for(targets: &[AskTarget], active: AppId) -> About {
        let path = format!("apps/{}", active.0);
        targets
            .iter()
            .find(|t| t.path.as_deref() == Some(path.as_str()))
            .or(targets.first())
            .map_or(About::NewApp, |t| About::Project(t.project.clone()))
    }

    /// Its row in [`options`].
    pub fn index(&self, targets: &[AskTarget]) -> usize {
        match self {
            About::Project(key) => targets.iter().position(|t| t.project == *key).unwrap_or(targets.len()),
            About::NewApp => targets.len(),
        }
    }

    /// The choice in row `index` of [`options`].
    pub fn at(index: usize, targets: &[AskTarget]) -> About {
        targets.get(index).map_or(About::NewApp, |t| About::Project(t.project.clone()))
    }
}

/// The dropdown's rows: each app by name, then [`NEW_APP`].
pub fn options(targets: &[AskTarget]) -> Vec<String> {
    targets.iter().map(|t| t.name.clone()).chain([NEW_APP.to_string()]).collect()
}

/// Where the form is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Editing,
    /// The ask is with the server.
    Sending,
    /// The server made what was asked; the form shows it.
    Asked(Asked),
}

/// Whether the form's Ask can be pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Ready,
    /// An ask is with the server: the button says so.
    Sending,
    /// Not yet, and why, for the notice beside it.
    Blocked(String),
}

/// The form: what is being asked, and where the asking is.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct AskForm {
    pub about: About,
    /// A new app's name.
    pub name: String,
    pub text: String,
    pub phase: Phase,
    /// What the server said when the last ask failed.
    pub error: Option<String>,
    seq: u64,
    /// The last ask sent and its idempotency key, which an identical ask reuses.
    last: Option<(Ask, String)>,
}

impl AskForm {
    /// Whether Ask can be pressed now, and why not.
    pub fn gate(&self, live: bool, offline: Option<String>) -> Gate {
        if !live {
            return Gate::Blocked(
                offline.map_or("Not connected to the server.".into(), |r| format!("Not connected: {r}.")),
            );
        }
        if self.phase == Phase::Sending {
            return Gate::Sending;
        }
        if self.text.trim().is_empty() {
            return Gate::Blocked("Say what you want first.".into());
        }
        if self.text.trim().chars().count() > MAX_ASK_CHARS {
            return Gate::Blocked(format!("An ask is at most {MAX_ASK_CHARS} characters."));
        }
        if self.about == About::NewApp && self.name.trim().is_empty() {
            return Gate::Blocked("Give the new app a name.".into());
        }
        Gate::Ready
    }

    /// The request the form would send.
    pub fn ask(&self) -> Ask {
        let text = self.text.trim();
        match &self.about {
            About::Project(key) => Ask::change(text, key.clone()),
            About::NewApp => Ask::new_app(text, self.name.trim()),
        }
    }

    /// The idempotency key for `ask`: the last one's when it is the same ask, else a new one.
    fn key_for(&mut self, ask: &Ask) -> String {
        match &self.last {
            Some((last, key)) if last == ask => key.clone(),
            _ => {
                let key = crate::commands::new_key();
                self.last = Some((ask.clone(), key.clone()));
                key
            }
        }
    }
}

/// Opens the form: the rail's Ask button triggers it.
#[derive(Event, Debug, Clone, Copy)]
pub struct OpenAsk;

/// Sends the form's ask, if [`AskForm::gate`] lets it.
#[derive(Event, Debug, Clone, Copy)]
pub struct SendAsk;

/// The server's answer to `GET /v1/shell/ask`.
#[derive(Message, Debug, Clone)]
pub struct TargetsReply(pub Result<AskTargets, String>);

/// The server's answer to the ask sent as `seq`.
#[derive(Message, Debug, Clone)]
pub struct AskReply {
    pub seq: u64,
    pub result: Result<Asked, String>,
}

/// The form's logic.
pub(crate) struct AskPlugin;

impl Plugin for AskPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Askable>()
            .init_resource::<AskForm>()
            .add_message::<TargetsReply>()
            .add_message::<AskReply>()
            .add_message::<SendNet>()
            .add_systems(OnEnter(Connection::Live), ask_targets)
            .add_systems(OnExit(Connection::Live), drop_in_flight)
            .add_systems(Update, (hear_targets, hear_ask).chain())
            .add_observer(open)
            .add_observer(send);
    }
}

fn ask_targets(mut net: MessageWriter<SendNet>) {
    net.write(SendNet(NetCommand::Fetch(read_targets())));
}

fn read_targets() -> Fetch {
    Fetch::new(|api| async move {
        TargetsReply(match api {
            Some(api) => api.shell_ask_targets().await.map_err(|e| said(&e)),
            None => Err("not connected".into()),
        })
    })
}

/// The server's own words for a refusal, or the whole chain for anything else.
fn said(e: &trailhead_client::http::Error) -> String {
    match e {
        trailhead_client::http::Error::Api { error, .. } => error.message.clone(),
        other => other.chain(),
    }
}

/// Starts the form afresh after an ask was made, picks what it is about from the app shown, and asks the server
/// again what can be asked about.
fn open(
    _: On<OpenAsk>,
    askable: Res<Askable>,
    active: Res<State<ActiveApp>>,
    connection: Option<Res<State<Connection>>>,
    mut form: ResMut<AskForm>,
    mut net: MessageWriter<SendNet>,
) {
    if matches!(form.phase, Phase::Asked(_)) {
        *form = AskForm { last: form.last.take(), seq: form.seq, ..AskForm::default() };
    }
    form.error = None;
    form.about = About::default_for(askable.targets(), active.get().0);
    if connection.is_some_and(|c| *c.get() == Connection::Live) {
        net.write(SendNet(NetCommand::Fetch(read_targets())));
    }
}

fn send(
    _: On<SendAsk>,
    connection: Option<Res<State<Connection>>>,
    mut form: ResMut<AskForm>,
    mut net: MessageWriter<SendNet>,
) {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    if form.gate(live, None) != Gate::Ready {
        return;
    }
    let ask = form.ask();
    let key = form.key_for(&ask);
    form.seq += 1;
    form.phase = Phase::Sending;
    form.error = None;
    net.write(SendNet(NetCommand::Fetch(post(form.seq, ask, key))));
}

fn post(seq: u64, ask: Ask, key: String) -> Fetch {
    Fetch::new(move |api| {
        let (ask, key) = (ask.clone(), key.clone());
        async move {
            let result = match api {
                Some(api) => api.shell_ask(&ask, Some(&key)).await.map_err(|e| said(&e)),
                None => Err("not connected to the server".into()),
            };
            AskReply { seq, result }
        }
    })
}

/// Keeps what can be asked about, and keeps the form on an app that is still there.
fn hear_targets(mut replies: MessageReader<TargetsReply>, mut askable: ResMut<Askable>, mut form: ResMut<AskForm>) {
    for TargetsReply(reply) in replies.read() {
        match reply {
            Ok(targets) => {
                askable.set_if_neq(Askable(Some(targets.clone())));
                let gone =
                    matches!(&form.about, About::Project(key) if !targets.targets.iter().any(|t| t.project == *key));
                if gone {
                    form.about = About::at(0, &targets.targets);
                }
            }
            Err(e) => debug!(error = %e, "couldn't ask the server what can be asked for"),
        }
    }
}

/// The answer to the ask in flight: what was made, or why not, with the text kept.
fn hear_ask(mut replies: MessageReader<AskReply>, mut form: ResMut<AskForm>) {
    for AskReply { seq, result } in replies.read() {
        if *seq != form.seq || form.phase != Phase::Sending {
            continue;
        }
        match result {
            Ok(asked) => {
                form.phase = Phase::Asked(asked.clone());
                form.text.clear();
                form.name.clear();
                form.last = None;
            }
            Err(e) => {
                form.phase = Phase::Editing;
                form.error = Some(e.clone());
            }
        }
    }
}

/// An ask in flight when the link drops is never answered. Asking again sends the same key, so the server answers
/// once whether or not the first one landed.
fn drop_in_flight(mut form: ResMut<AskForm>) {
    if form.phase == Phase::Sending {
        form.phase = Phase::Editing;
        form.error = Some(
            "The connection dropped before the server answered. Ask again once it is back: if the first ask landed, \
             the server answers with it rather than asking twice."
                .into(),
        );
    }
}

/// Why the connection isn't live, in words, for [`AskForm::gate`].
pub fn offline_reason(status: Option<&ConnectionStatus>) -> Option<String> {
    status.and_then(|s| s.reason.as_ref()).map(|r| r.cause().to_string())
}

#[cfg(test)]
mod tests {
    use trailhead_model::shell::{AskedProject, AskedTask};

    use super::*;
    use crate::ShellPlugin;
    use crate::testing::{Other, headless, set_connection, show};

    const TICKLIST: AppId = AppId("ticklist");

    struct Ticklist;

    impl crate::ShellApp for Ticklist {
        fn manifest(&self) -> crate::Manifest {
            crate::Manifest { id: TICKLIST, name: "Ticklist", icon: trailhead_ui::Icon::Check }
        }

        fn build(&self, _: &mut App) {}
    }

    /// The reads and asks sent over the connection, in order.
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
        let mut app = headless(ShellPlugin::new().with(Other).with(Ticklist));
        app.init_resource::<Sent>().add_systems(PostUpdate, count);
        app
    }

    fn sent(app: &mut App) -> Vec<Fetch> {
        std::mem::take(&mut app.world_mut().resource_mut::<Sent>().0)
    }

    pub(crate) fn targets() -> AskTargets {
        let target = |project: &str, name: &str, path: &str| AskTarget {
            project: project.into(),
            name: name.into(),
            path: Some(path.into()),
        };
        AskTargets {
            configured: true,
            repo: Some("lane/apps".into()),
            targets: vec![target("ATLAS", "Atlas", "apps/atlas"), target("TICK", "Ticklist", "apps/ticklist")],
        }
    }

    fn reply(app: &mut App, targets: AskTargets) {
        app.world_mut().write_message(TargetsReply(Ok(targets)));
        app.update();
    }

    fn form(app: &App) -> AskForm {
        app.world().resource::<AskForm>().clone()
    }

    fn edit(app: &mut App, edit: impl FnOnce(&mut AskForm)) {
        edit(&mut app.world_mut().resource_mut::<AskForm>());
    }

    /// Runs `fetch` against a server answering once with `status` and `body`, and says what was asked.
    fn run(app: &mut App, fetch: &Fetch, status: &str, body: &str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (status, body) = (status.to_string(), body.to_string());
        let served = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0u8; 16384];
            let n = stream.read(&mut request).unwrap();
            let head =
                format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n", body.len());
            write!(stream, "{head}connection: close\r\n\r\n{body}").unwrap();
            String::from_utf8_lossy(&request[..n]).into_owned()
        });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let fetched = runtime.block_on(fetch.run(Some(trailhead_client::http::Client::new(base, "token"))));
        fetched.deliver(app.world_mut());
        app.update();
        served.join().unwrap()
    }

    fn body_of(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
        request.lines().find_map(|l| l.split_once(": ").filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v))
    }

    #[test]
    fn the_targets_fill_the_choices_and_the_default_is_the_app_shown() {
        let t = targets().targets;
        assert_eq!(options(&t), ["Atlas", "Ticklist", NEW_APP]);
        assert_eq!(About::default_for(&t, TICKLIST), About::Project("TICK".into()));
        assert_eq!(About::default_for(&t, AppId("lookout")), About::Project("ATLAS".into()), "else the first");
        assert_eq!(About::default_for(&[], TICKLIST), About::NewApp);
        assert_eq!((About::Project("TICK".into()).index(&t), About::NewApp.index(&t)), (1, 2));
        assert_eq!((About::at(0, &t), About::at(2, &t)), (About::Project("ATLAS".into()), About::NewApp));

        let mut app = shell();
        set_connection(&mut app, Connection::Live);
        assert_eq!(sent(&mut app).len(), 1, "read when the connection goes live");
        reply(&mut app, targets());
        assert!(app.world().resource::<Askable>().configured());
        show(&mut app, TICKLIST);
        app.world_mut().trigger(OpenAsk);
        app.update();
        assert_eq!(form(&app).about, About::Project("TICK".into()));
        assert_eq!(sent(&mut app).len(), 1, "and again each time the form opens");

        let mut fewer = targets();
        fewer.targets.retain(|t| t.project != "TICK");
        reply(&mut app, fewer);
        assert_eq!(form(&app).about, About::Project("ATLAS".into()), "an app that went is left for one still there");
    }

    #[test]
    fn ask_is_enabled_only_with_something_to_send_and_says_why_not() {
        let blocked = |words: &str| Gate::Blocked(words.into());
        let mut form = AskForm { about: About::Project("TICK".into()), ..AskForm::default() };
        assert_eq!(
            form.gate(false, Some("the server isn't answering".into())),
            blocked("Not connected: the server isn't answering.")
        );
        assert_eq!(form.gate(false, None), blocked("Not connected to the server."));
        assert_eq!(form.gate(true, None), blocked("Say what you want first."));
        form.text = "   \n".into();
        assert_eq!(form.gate(true, None), blocked("Say what you want first."));
        form.text = "x".repeat(MAX_ASK_CHARS + 1);
        assert_eq!(form.gate(true, None), blocked("An ask is at most 8000 characters."));
        form.text = "A Completed view".into();
        assert_eq!(form.gate(true, None), Gate::Ready);
        form.about = About::NewApp;
        assert_eq!(form.gate(true, None), blocked("Give the new app a name."));
        form.name = "Trail Notes".into();
        assert_eq!(form.gate(true, None), Gate::Ready);
        form.phase = Phase::Sending;
        assert_eq!(form.gate(true, None), Gate::Sending);
    }

    #[test]
    fn sending_posts_a_change_or_a_new_app_once_and_a_failure_keeps_the_text() {
        let mut app = shell();
        set_connection(&mut app, Connection::Live);
        reply(&mut app, targets());
        sent(&mut app);
        edit(&mut app, |f| {
            f.about = About::Project("TICK".into());
            f.text = "  A Completed view\nnewest first ".into();
        });
        app.world_mut().trigger(SendAsk);
        app.world_mut().trigger(SendAsk);
        app.update();
        let fetches = sent(&mut app);
        assert_eq!(fetches.len(), 1, "a second press while asking sends nothing");
        assert_eq!(form(&app).phase, Phase::Sending);
        let error = r#"{"error": {"code": "invalid", "message": "TICK isn't an app in lane/apps"}}"#;
        let request = run(&mut app, &fetches[0], "400 Bad Request", error);
        assert!(request.starts_with("POST /v1/shell/ask HTTP/1.1"), "{request}");
        assert_eq!(body_of(&request), serde_json::json!({"text": "A Completed view\nnewest first", "project": "TICK"}));
        let key = header(&request, "idempotency-key").expect("a key").to_string();
        let failed = form(&app);
        assert_eq!((failed.phase, failed.error.as_deref()), (Phase::Editing, Some("TICK isn't an app in lane/apps")));
        assert_eq!(failed.text, "  A Completed view\nnewest first ", "nothing typed is lost");

        app.world_mut().trigger(SendAsk);
        app.update();
        let fetch = sent(&mut app).remove(0);
        let again = run(&mut app, &fetch, "400 Bad Request", error);
        assert_eq!(header(&again, "idempotency-key"), Some(key.as_str()), "the same ask, the same key");

        edit(&mut app, |f| {
            f.about = About::NewApp;
            f.name = " Trail Notes ".into();
        });
        app.world_mut().trigger(SendAsk);
        app.update();
        let asked = r#"{"project": {"key": "TN", "name": "Trail Notes", "created": true},
                        "task": {"key": "TN-1", "title": "Create the Trail Notes app"}, "notes": []}"#;
        let fetch = sent(&mut app).remove(0);
        let new = run(&mut app, &fetch, "201 Created", asked);
        assert_eq!(
            body_of(&new),
            serde_json::json!({"text": "A Completed view\nnewest first", "new_app": {"name": "Trail Notes"}})
        );
        assert_ne!(header(&new, "idempotency-key"), Some(key.as_str()), "another ask, another key");
        let done = form(&app);
        let made = Asked {
            project: AskedProject { key: "TN".into(), name: "Trail Notes".into(), created: true },
            task: Some(AskedTask { key: "TN-1".into(), title: "Create the Trail Notes app".into() }),
            notes: Vec::new(),
        };
        assert_eq!((done.phase, done.text.as_str(), done.error), (Phase::Asked(made), "", None));

        app.world_mut().trigger(OpenAsk);
        app.update();
        assert_eq!(form(&app).phase, Phase::Editing, "opening again starts afresh");
    }

    #[test]
    fn a_link_that_drops_mid_ask_says_so_and_lets_it_be_asked_again() {
        let mut app = shell();
        set_connection(&mut app, Connection::Live);
        edit(&mut app, |f| {
            f.about = About::NewApp;
            f.name = "Notes".into();
            f.text = "notes".into();
        });
        app.world_mut().trigger(SendAsk);
        app.update();
        set_connection(&mut app, Connection::Offline);
        let dropped = form(&app);
        assert_eq!(dropped.phase, Phase::Editing);
        assert!(dropped.error.unwrap().contains("dropped"));
        app.world_mut().write_message(AskReply { seq: 1, result: Err("late".into()) });
        app.update();
        assert!(!form(&app).error.unwrap().contains("late"), "an answer to an ask no longer in flight is dropped");
    }
}
