//! Asking for a change or a new app from the window (`docs/app-platform.md`, "Planning an ask"). The rail has two
//! ways in: its Ask button ([`crate::rail::AskButton`]) asks for a change to one of the apps the server can be asked
//! about ([`Askable`]), the app shown first, and the plus under the apps ([`crate::rail::NewAppButton`]) asks for a new
//! app. Both open a form ([`crate::ask_panel`]) whose ask becomes a task. A change goes straight to a worker unless the
//! person asks for it to be planned first; a new app is always planned. A planned ask is a spec task the planner talks
//! over with the person, by the model they chose or the repo's default.
//!
//! This is the form's logic, headless: what the server says can be asked about, read with `GET /v1/shell/ask` when
//! the connection goes live and each time the form opens; the draft ([`AskForm`]); whether it can be sent and why not
//! ([`AskForm::gate`]); sending it, `POST /v1/shell/ask` over the shell's connection as a [`Fetch`], since its answer
//! (the task and project made) is what the form shows; and what the form says of that answer ([`confirmation`]). Each
//! distinct ask carries an idempotency key of its own, so asking again after an answer was lost replays the first
//! instead of making a second task.

use bevy::prelude::*;
use trailhead_model::shell::{Ask, AskTarget, AskTargets, Asked, MAX_ASK_CHARS, PLANNER_MODELS};

use crate::connection::{Connection, ConnectionStatus, SendNet};
use crate::net::{Fetch, NetCommand};
use crate::{ActiveApp, AppId};

/// The first choice in the planner's model dropdown, which sends no model.
pub const REPO_DEFAULT: &str = "The repo's default";

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

/// Which of the rail's buttons opened the form.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AskFor {
    /// The Ask button: a change to an app.
    #[default]
    Change,
    /// The plus under the apps: a new app.
    NewApp,
}

/// What an ask is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum About {
    /// A change to the app whose project has this key; `None` while there is none to choose.
    Change(Option<String>),
    NewApp,
}

impl Default for About {
    fn default() -> About {
        About::Change(None)
    }
}

impl About {
    /// The default for a change asked over the app `active`: the project building `apps/<active>`, else the first
    /// app.
    pub fn default_for(targets: &[AskTarget], active: AppId) -> About {
        let path = format!("apps/{}", active.0);
        let shown = targets.iter().find(|t| t.path.as_deref() == Some(path.as_str()));
        About::Change(shown.or(targets.first()).map(|t| t.project.clone()))
    }

    /// Its row in [`options`], for a change to an app that is there.
    pub fn index(&self, targets: &[AskTarget]) -> Option<usize> {
        match self {
            About::Change(Some(key)) => targets.iter().position(|t| t.project == *key),
            _ => None,
        }
    }

    /// A change to the app in row `index` of [`options`].
    pub fn at(index: usize, targets: &[AskTarget]) -> About {
        About::Change(targets.get(index).map(|t| t.project.clone()))
    }

    pub fn asks_for(&self) -> AskFor {
        match self {
            About::Change(_) => AskFor::Change,
            About::NewApp => AskFor::NewApp,
        }
    }
}

/// The app dropdown's rows: each app by name.
pub fn options(targets: &[AskTarget]) -> Vec<String> {
    targets.iter().map(|t| t.name.clone()).collect()
}

/// The planner's model dropdown's rows: [`REPO_DEFAULT`], then each of [`PLANNER_MODELS`] with a capital.
pub fn model_options() -> Vec<String> {
    let capital = |m: &str| m.chars().take(1).flat_map(char::to_uppercase).chain(m.chars().skip(1)).collect();
    [REPO_DEFAULT.to_string()].into_iter().chain(PLANNER_MODELS.iter().map(|m| capital(m))).collect()
}

/// The row of [`model_options`] showing `model`.
pub fn model_index(model: Option<&str>) -> usize {
    model.and_then(|m| PLANNER_MODELS.iter().position(|p| *p == m)).map_or(0, |i| i + 1)
}

/// The model in row `index` of [`model_options`]: none for the repo's default.
pub fn model_at(index: usize) -> Option<&'static str> {
    index.checked_sub(1).and_then(|i| PLANNER_MODELS.get(i)).copied()
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
    /// Whether a change is planned first. A new app always is, whatever this says.
    pub plan: bool,
    /// The planner's model, one of [`PLANNER_MODELS`]; `None` leaves it to the repo. Sent only when planned.
    pub model: Option<&'static str>,
    pub phase: Phase,
    /// What the server said when the last ask failed.
    pub error: Option<String>,
    seq: u64,
    /// The last ask sent and its idempotency key, which an identical ask reuses.
    last: Option<(Ask, String)>,
}

impl AskForm {
    /// Whether the ask goes to the planner first: a new app always, a change when asked to.
    pub fn planned(&self) -> bool {
        self.about == About::NewApp || self.plan
    }

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
        if self.about == About::Change(None) {
            return Gate::Blocked("There is no app to ask about yet.".into());
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

    /// The request the form would send, once it says what it is about.
    pub fn ask(&self) -> Option<Ask> {
        let text = self.text.trim();
        let ask = match &self.about {
            About::Change(key) => Ask::change(text, key.clone()?),
            About::NewApp => Ask::new_app(text, self.name.trim()),
        };
        Some(if self.planned() { ask.planned(self.model) } else { ask })
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

/// What the form says once the server has answered: each line empty when there is nothing to say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Confirmation {
    /// "Asked: TICK-17", or for a planned ask "Sent to the planner: TICK-17".
    pub heading: String,
    /// The task's title.
    pub title: String,
    /// "Made the project TN, Trail Notes", for a new app.
    pub project: String,
    /// For a planned ask, what happens next and where the person will see it.
    pub next: String,
}

/// What the form says of `asked`.
pub fn confirmation(asked: &Asked) -> Confirmation {
    let task = asked.task.as_ref();
    let heading = task
        .map(|t| if asked.planned { format!("Sent to the planner: {}", t.key) } else { format!("Asked: {}", t.key) });
    let next = task.filter(|_| asked.planned).map(|t| {
        format!(
            "The planner reads the ask and comes back with questions, or with a plan of tasks for you to approve. \
             You'll find them on {} in Lookout, as a question waiting on you. Nothing is built until you approve.",
            t.key
        )
    });
    Confirmation {
        heading: heading.unwrap_or_default(),
        title: task.map(|t| t.title.clone()).unwrap_or_default(),
        project: if asked.project.created {
            format!("Made the project {}, {}", asked.project.key, asked.project.name)
        } else {
            String::new()
        },
        next: next.unwrap_or_default(),
    }
}

/// Opens the form for a change or a new app: the rail's buttons trigger it.
#[derive(Event, Debug, Clone, Copy)]
pub struct OpenAsk(pub AskFor);

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

/// Starts the form afresh after an ask was made, sets it to a new app or to a change to the app shown, and asks the
/// server again what can be asked about.
fn open(
    opened: On<OpenAsk>,
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
    form.about = match opened.0 {
        AskFor::Change => About::default_for(askable.targets(), active.get().0),
        AskFor::NewApp => About::NewApp,
    };
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
    let Some(ask) = form.ask() else { return };
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
                let gone = matches!(&form.about, About::Change(key)
                    if !key.as_ref().is_some_and(|key| targets.targets.iter().any(|t| t.project == *key)));
                if gone && form.about != About::at(0, &targets.targets) {
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

    fn tick() -> About {
        About::Change(Some("TICK".into()))
    }

    #[test]
    fn the_targets_fill_the_choices_and_the_default_is_the_app_shown() {
        let t = targets().targets;
        assert_eq!(options(&t), ["Atlas", "Ticklist"], "a change offers only the apps");
        assert_eq!(About::default_for(&t, TICKLIST), tick());
        assert_eq!(About::default_for(&t, AppId("lookout")), About::Change(Some("ATLAS".into())), "else the first");
        assert_eq!(About::default_for(&[], TICKLIST), About::Change(None));
        assert_eq!((tick().index(&t), About::NewApp.index(&t), About::Change(None).index(&t)), (Some(1), None, None));
        assert_eq!((About::at(0, &t), About::at(2, &t)), (About::Change(Some("ATLAS".into())), About::Change(None)));

        let mut app = shell();
        set_connection(&mut app, Connection::Live);
        assert_eq!(sent(&mut app).len(), 2, "read when the connection goes live, beside the notification settings");
        reply(&mut app, targets());
        assert!(app.world().resource::<Askable>().configured());
        show(&mut app, TICKLIST);
        app.world_mut().trigger(OpenAsk(AskFor::Change));
        app.update();
        assert_eq!(form(&app).about, tick());
        assert_eq!(sent(&mut app).len(), 1, "and again each time the form opens");
        app.world_mut().trigger(OpenAsk(AskFor::NewApp));
        app.update();
        assert_eq!(form(&app).about, About::NewApp, "the plus asks for a new app");
        app.world_mut().trigger(OpenAsk(AskFor::Change));
        app.update();

        let mut fewer = targets();
        fewer.targets.retain(|t| t.project != "TICK");
        reply(&mut app, fewer);
        assert_eq!(
            form(&app).about,
            About::Change(Some("ATLAS".into())),
            "an app that went is left for one still there"
        );
        reply(&mut app, AskTargets { targets: Vec::new(), ..targets() });
        assert_eq!(form(&app).about, About::Change(None), "and none, when none is");
    }

    #[test]
    fn ask_is_enabled_only_with_something_to_send_and_says_why_not() {
        let blocked = |words: &str| Gate::Blocked(words.into());
        let mut form = AskForm { about: tick(), ..AskForm::default() };
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
        form.about = About::Change(None);
        assert_eq!(form.gate(true, None), blocked("There is no app to ask about yet."));
        assert_eq!(form.ask(), None);
        form.about = About::NewApp;
        assert_eq!(form.gate(true, None), blocked("Give the new app a name."));
        form.name = "Trail Notes".into();
        assert_eq!(form.gate(true, None), Gate::Ready);
        form.phase = Phase::Sending;
        assert_eq!(form.gate(true, None), Gate::Sending);
    }

    #[test]
    fn the_planners_models_are_the_repos_default_then_each_by_name() {
        assert_eq!(model_options(), [REPO_DEFAULT, "Fable", "Opus", "Sonnet", "Haiku"]);
        assert_eq!((model_at(0), model_at(2), model_at(5)), (None, Some("opus"), None));
        assert_eq!((model_index(None), model_index(Some("haiku")), model_index(Some("gpt"))), (0, 4, 0));
    }

    #[test]
    fn a_new_app_is_always_planned_with_the_model_chosen_or_none() {
        let mut form = AskForm { about: About::NewApp, name: " Notes ".into(), text: "notes".into(), ..default() };
        assert!(form.planned());
        assert_eq!(form.ask(), Some(Ask::new_app("notes", "Notes").planned(None)));
        form.model = Some("sonnet");
        assert_eq!(form.ask(), Some(Ask::new_app("notes", "Notes").planned(Some("sonnet"))));
    }

    #[test]
    fn a_change_is_planned_only_when_asked_and_turning_it_off_keeps_what_was_typed() {
        let mut form = AskForm { about: tick(), text: "A Completed view".into(), ..AskForm::default() };
        assert!(!form.planned());
        assert_eq!(form.ask(), Some(Ask::change("A Completed view", "TICK")));
        form.plan = true;
        form.model = Some("opus");
        assert_eq!(form.ask(), Some(Ask::change("A Completed view", "TICK").planned(Some("opus"))));
        form.plan = false;
        assert_eq!(form.ask(), Some(Ask::change("A Completed view", "TICK")), "no model without a plan");
        assert_eq!((form.text.as_str(), form.model), ("A Completed view", Some("opus")), "nothing typed is cleared");
    }

    fn asked(planned: bool, created: bool) -> Asked {
        Asked {
            project: AskedProject { key: "TN".into(), name: "Trail Notes".into(), created },
            task: Some(AskedTask { key: "TN-1".into(), title: "Trail Notes".into() }),
            planned,
            notes: Vec::new(),
        }
    }

    #[test]
    fn the_answer_says_what_was_made_and_a_planned_one_where_the_planner_answers() {
        let plain = confirmation(&asked(false, false));
        assert_eq!((plain.heading.as_str(), plain.title.as_str()), ("Asked: TN-1", "Trail Notes"));
        assert_eq!((plain.project.as_str(), plain.next.as_str()), ("", ""));

        let planned = confirmation(&asked(true, true));
        assert_eq!(planned.heading, "Sent to the planner: TN-1");
        assert_eq!(planned.project, "Made the project TN, Trail Notes");
        assert!(planned.next.contains("questions, or with a plan"), "{}", planned.next);
        assert!(planned.next.contains("on TN-1 in Lookout, as a question waiting on you"), "{}", planned.next);

        let untasked = confirmation(&Asked { task: None, ..asked(true, true) });
        assert_eq!((untasked.heading, untasked.next), (String::new(), String::new()), "the notes say why");
    }

    #[test]
    fn sending_posts_a_change_or_a_new_app_once_and_a_failure_keeps_the_text() {
        let mut app = shell();
        set_connection(&mut app, Connection::Live);
        reply(&mut app, targets());
        sent(&mut app);
        edit(&mut app, |f| {
            f.about = tick();
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
            f.model = Some("fable");
        });
        app.world_mut().trigger(SendAsk);
        app.update();
        let asked = r#"{"project": {"key": "TN", "name": "Trail Notes", "created": true},
                        "task": {"key": "TN-1", "title": "Trail Notes"}, "planned": true, "notes": []}"#;
        let fetch = sent(&mut app).remove(0);
        let new = run(&mut app, &fetch, "201 Created", asked);
        assert_eq!(
            body_of(&new),
            serde_json::json!({"text": "A Completed view\nnewest first", "new_app": {"name": "Trail Notes"},
                               "plan": true, "model": "fable"})
        );
        assert_ne!(header(&new, "idempotency-key"), Some(key.as_str()), "another ask, another key");
        let done = form(&app);
        let made = Asked {
            project: AskedProject { key: "TN".into(), name: "Trail Notes".into(), created: true },
            task: Some(AskedTask { key: "TN-1".into(), title: "Trail Notes".into() }),
            planned: true,
            notes: Vec::new(),
        };
        assert_eq!((done.phase, done.text.as_str(), done.error), (Phase::Asked(made), "", None));

        app.world_mut().trigger(OpenAsk(AskFor::NewApp));
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
