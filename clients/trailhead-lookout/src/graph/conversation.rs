//! A spec task's conversation beside its plan (`docs/app-platform.md`, "Planning an ask"): the thread of the spec
//! task as a chat, whose turn it is, and a box to reply to the planner's question.
//!
//! The planner takes a turn, asks a question and parks the task; the person's answer reopens it for the next turn. So
//! the task's comments are the conversation: [`build_plan`] turns them (every one once [`Detail`] has fetched the
//! task, until then what the mirror holds) into a [`PlanView`], and the panel keeps its messages, turn line and reply
//! box in step with it in place. A reply is the answer to the newest open question, sent as the inspector's answer
//! form sends one ([`compose_request`]).

use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, ControlOrientation};
use trailhead_client::text::Text as Wording;
use trailhead_model::{Comment, CommentKind, Status, Task, TaskPlan};
use trailhead_shell::commands::{CommandDone, Outcome, Submit};
use trailhead_shell::connection::{Connection, ConnectionStatus};

use super::{GraphPage, GraphView};
use crate::board::Input;
use crate::detail::{Detail, Followed};
use crate::inspector::{Selection, Subject, clock, compose_request, describe};
use crate::widgets::{
    ButtonKind, Icon, Keyed, MessageEdit, Notice, Side, Tone, UiPlace, button, comment_icon, extent, field_well,
    message, notice, panel, row, scroll_body, scroll_frame, scrollbar, set_display, set_text, size, space, text_dim,
    text_input, token,
};

impl Followed for GraphPage {
    fn followed(&self) -> Option<String> {
        self.0.clone()
    }
}

/// What the plan view knows beyond the mirror: the spec task's every comment once it is fetched, and who is reading
/// (their id and name), to name what they wrote.
#[derive(Debug, Clone, Copy, Default)]
pub struct Thread<'a> {
    pub comments: Option<&'a [Comment]>,
    pub me: Option<(&'a str, &'a str)>,
}

/// A spec task's plan and conversation, as the plan view shows them.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanView {
    pub id: String,
    pub key: String,
    pub status: Status,
    /// A person has approved the plan: its tasks are graded, and the planner is finished.
    pub approved: bool,
    pub turn: Turn,
    /// The ask first, then every comment, oldest first.
    pub messages: Vec<Said>,
    /// The newest open question, its id and words: what a reply answers.
    pub question: Option<(String, String)>,
}

/// One message of the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    /// The comment's id, or [`ASK`] for the ask itself.
    pub id: String,
    pub icon: Icon,
    /// Written by the reader, so it sits on the right.
    pub mine: bool,
    /// Who wrote it and when.
    pub head: String,
    pub body: String,
    /// An open question, waiting on the reader.
    pub waiting: bool,
    /// The words are cut short: the whole is read in the inspector.
    pub clamped: bool,
}

/// The key of the ask, the conversation's first message.
pub const ASK: &str = "ask";
/// How much of the ask the conversation shows: the spec grows into the task's description, and is read whole in the
/// inspector.
const ASK_CHARS: usize = 360;

/// Whose move it is in the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    /// The planner's run is working on its turn.
    Planner,
    /// The planner asked something and waits for the reader.
    You,
    /// The planner has the next turn and hasn't started it.
    Queued,
    /// A person approved the plan, at this time (in words).
    Approved(String),
    /// None of those: the task is somewhere else (finished, set aside, waiting without a question).
    Idle(Status),
}

impl Turn {
    /// The turn line's words.
    pub fn words(&self) -> String {
        match self {
            Turn::Planner => "The planner is working on its turn.".into(),
            Turn::You => "Waiting on you: the planner asked you something.".into(),
            Turn::Queued => "Queued for the planner: it takes its turn when the factory gets to it.".into(),
            Turn::Approved(at) => format!("Approved {at}: its tasks are ordinary factory work now."),
            Turn::Idle(status) => {
                format!("{}: nobody's turn in this conversation.", crate::widgets::status_label(*status))
            }
        }
    }

    pub fn tone(&self) -> Tone {
        match self {
            Turn::You => Tone::Warning,
            Turn::Approved(_) => Tone::Success,
            Turn::Planner | Turn::Queued | Turn::Idle(_) => Tone::Info,
        }
    }
}

/// The plan of `task` if it is a spec task: whose turn it is, and the conversation from its comments (`thread`'s
/// when fetched, else the mirror's).
pub fn build_plan(input: &Input, task: &Task, thread: Thread) -> Option<PlanView> {
    let spec = TaskPlan::of(&task.metadata)?.spec?;
    let text = clock();
    let name_of = |id: uuid::Uuid| match thread.me {
        Some((my_id, my_name)) if my_id == id.to_string() => my_name.to_string(),
        _ => input
            .agents
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.name.clone())
            .or_else(|| input.users.iter().find(|u| u.id == id).map(|u| u.name.clone()))
            .unwrap_or_else(|| id.to_string().chars().take(8).collect()),
    };
    let mine = |id: uuid::Uuid| thread.me.is_some_and(|(my_id, _)| my_id == id.to_string());
    let mut comments: Vec<&Comment> = match thread.comments {
        Some(fetched) => fetched.iter().filter(|c| c.task_id == task.id).collect(),
        None => input.comments.iter().filter(|c| c.task_id == task.id).collect(),
    };
    comments.retain(|c| c.deleted_at.is_none());
    comments.sort_by_key(|c| (c.created_at, c.id));
    let question = comments
        .iter()
        .rev()
        .find(|c| c.kind == CommentKind::Question && c.resolved_at.is_none())
        .map(|c| (c.id.to_string(), c.body.clone()));
    let mut messages = vec![ask(task, text, name_of(task.created_by), mine(task.created_by))];
    messages.extend(comments.iter().map(|c| {
        let waiting = question.as_ref().is_some_and(|(id, _)| *id == c.id.to_string());
        let mut head = format!("{} · {}", name_of(c.author_id), text.time(c.created_at, None));
        if waiting {
            head.push_str(" · waiting on you");
        }
        Said {
            id: c.id.to_string(),
            icon: comment_icon(c.kind),
            mine: mine(c.author_id),
            head,
            body: c.body.clone(),
            waiting,
            clamped: false,
        }
    }));
    let approved = spec.approved.and_then(|s| jiff::Timestamp::from_second(s).ok());
    let turn = match (approved, task.status) {
        (Some(at), _) => Turn::Approved(text.time(at, None)),
        (None, Status::InProgress) => Turn::Planner,
        (None, Status::NeedsInput) if question.is_some() => Turn::You,
        (None, Status::Open) => Turn::Queued,
        (None, status) => Turn::Idle(status),
    };
    Some(PlanView {
        id: task.id.to_string(),
        key: task.key.0.clone(),
        status: task.status,
        approved: approved.is_some(),
        turn,
        messages,
        question,
    })
}

/// The ask: the task's description as the conversation's first message, cut to [`ASK_CHARS`].
fn ask(task: &Task, text: &Wording, author: String, mine: bool) -> Said {
    let whole = task.description.trim();
    let whole = if whole.is_empty() { task.title.trim() } else { whole };
    let clamped = whole.chars().count() > ASK_CHARS;
    let body = if clamped {
        format!("{}…", whole.chars().take(ASK_CHARS).collect::<String>().trim_end())
    } else {
        whole.to_string()
    };
    Said {
        id: ASK.into(),
        icon: Icon::Flag,
        mine,
        head: format!("{author} · asked {}", text.time(task.created_at, None)),
        body,
        waiting: false,
        clamped,
    }
}

/// Whether a reply can be sent now, or why not, the connection aside.
pub fn reply_gate(plan: &PlanView) -> Result<(), String> {
    if plan.question.is_some() {
        return Ok(());
    }
    Err(match &plan.turn {
        Turn::Planner => "The planner is working; reply once it asks you something.".into(),
        Turn::Queued => "The planner hasn't taken its turn yet.".into(),
        Turn::Approved(_) => "The plan is approved; the conversation is over.".into(),
        Turn::You | Turn::Idle(_) => "No question is waiting for an answer.".into(),
    })
}

/// The conversation panel beside the canvas, shown for a spec task.
#[derive(Component, Clone, Default)]
pub struct ConversationPanel;

/// The line saying whose turn it is.
#[derive(Component)]
pub(super) struct TurnLine;

/// The column the messages are kept in, by [`ThreadRows`]; it holds nothing else.
#[derive(Component)]
pub(super) struct ThreadList;

/// The body that scrolls, kept at its newest message.
#[derive(Component)]
pub(super) struct ThreadBody;

/// The button under the ask that opens the whole spec in the inspector.
#[derive(Component)]
pub(super) struct ReadSpec;

/// The reply box's text entity.
#[derive(Component, Clone, Default)]
pub struct ReplyInput;

/// The Reply button, which the answer to a reply goes back to.
#[derive(Component)]
pub struct ReplyButton;

/// Why Reply can't be pressed, under the box.
#[derive(Component)]
pub(super) struct ReplyWhy;

/// How the last reply went.
#[derive(Component)]
pub(super) struct ReplyNote;

/// The messages' entities, by key.
#[derive(Resource, Default)]
pub(super) struct ThreadRows(Keyed<Said>);

/// A reply in flight, which can't be sent twice.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Replying(pub bool);

/// Spawns the panel, hidden, under `parent`: the turn line, the thread in a scrolling body, and the reply box.
pub(super) fn spawn_conversation(commands: &mut Commands, rows: &mut ThreadRows, parent: Entity) {
    // The messages of a page drawn before went with it.
    rows.0 = Keyed::default();
    let panel = commands
        .spawn_scene(bsn! {
            @panel()
            Node {
                display: Display::None,
                width: { px(extent::SIDE_PANEL_WIDTH) },
                flex_shrink: 0.0,
                min_height: { px(0) },
            }
            ConversationPanel
        })
        .insert(ChildOf(parent))
        .id();
    commands.spawn_scene(notice()).insert((TurnLine, ChildOf(panel)));
    let frame = commands.spawn(scroll_frame()).insert(ChildOf(panel)).id();
    let body = commands.spawn((scroll_body(), ThreadBody, ChildOf(frame))).id();
    commands.spawn((
        ThreadList,
        Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() },
        ChildOf(body),
    ));
    commands.queue_spawn_scene(scrollbar(body, ControlOrientation::Vertical)).insert(ChildOf(frame));
    let well = commands.spawn_scene(field_well()).insert(ChildOf(panel)).id();
    commands.spawn_scene(text_input(3.0, bsn! { ReplyInput })).insert(ChildOf(well));
    let bar = commands.spawn_scene(row()).insert(ChildOf(well)).id();
    commands.spawn_scene(text_dim("", size::SECONDARY)).insert((
        ReplyWhy,
        Node { flex_grow: 1.0, min_width: px(0), ..default() },
        ChildOf(bar),
    ));
    commands.spawn_scene(button("Reply", Some(Icon::Send), ButtonKind::Primary)).insert((
        ReplyButton,
        InteractionDisabled,
        ChildOf(bar),
    ));
    commands.spawn_scene(notice()).insert((ReplyNote, ChildOf(panel)));
}

/// Keeps the panel in step with the plan, in place. Runs while the graph is shown.
pub(super) fn conversation_systems() -> impl IntoScheduleConfigs<bevy::ecs::system::ScheduleSystem, ()> {
    (show_panel, update_turn, sync_messages, gate_reply).before(UiPlace)
}

fn show_panel(view: Res<GraphView>, panels: Query<Entity, With<ConversationPanel>>, mut nodes: Query<&mut Node>) {
    for panel in &panels {
        set_display(&mut nodes, panel, view.plan.is_some());
    }
}

fn update_turn(view: Res<GraphView>, mut lines: Query<(Entity, &Notice), With<TurnLine>>, mut commands: Commands) {
    let Some(plan) = &view.plan else { return };
    let wanted = Notice::new(plan.turn.tone(), plan.turn.words());
    for (line, now) in &mut lines {
        if *now != wanted {
            commands.entity(line).insert(wanted.clone());
        }
    }
}

/// One message per comment, kept by key: a new one is spawned, a changed one rewritten in place, and the body scrolls
/// to the newest when one arrives.
#[allow(clippy::too_many_arguments)]
fn sync_messages(
    view: Res<GraphView>,
    list: Query<Entity, With<ThreadList>>,
    mut bodies: Query<&mut ScrollPosition, With<ThreadBody>>,
    spec: Query<Entity, With<ReadSpec>>,
    parents: Query<&ChildOf>,
    mut nodes: Query<&mut Node>,
    mut edit: MessageEdit,
    mut rows: ResMut<ThreadRows>,
    mut commands: Commands,
) {
    let (Some(plan), Ok(list)) = (&view.plan, list.single()) else { return };
    let before = rows.0.len();
    let wanted = [(list, plan.messages.iter().map(|m| (m.id.clone(), m.clone())).collect::<Vec<_>>())];
    let mut changed = Vec::new();
    rows.0.sync(
        &mut commands,
        &wanted,
        |commands, _, said| spawn_said(commands, said),
        |_, entity, said| changed.push((entity, said.clone())),
    );
    for (entity, said) in changed {
        edit.set(entity, said.icon, &said.head, &said.body, &tone_of(&said));
        for button in spec.iter().filter(|b| parents.get(*b).is_ok_and(|p| p.parent() == entity)) {
            set_display(&mut nodes, button, said.clamped);
        }
    }
    if rows.0.len() > before {
        for mut at in &mut bodies {
            at.0.y = f32::MAX;
        }
    }
}

fn tone_of(said: &Said) -> crate::widgets::Token {
    if said.waiting { token::STATUS_WARN } else { token::TEXT }
}

fn spawn_said(commands: &mut Commands, said: &Said) -> Entity {
    let side = if said.mine { Side::Mine } else { Side::Theirs };
    let item = commands.spawn_scene(message(side, said.icon, said.head.clone(), said.body.clone(), tone_of(said))).id();
    if said.id == ASK {
        let read = button("Read the whole spec", Some(Icon::Inspect), ButtonKind::Plain);
        let shown = crate::widgets::display(said.clamped);
        commands.spawn_scene(read).insert((ReadSpec, Node { display: shown, ..default() }, ChildOf(item)));
    }
    item
}

/// Reply is pressable with something typed, a question to answer, the connection live and no reply in flight; the
/// line under the box says why not.
#[allow(clippy::too_many_arguments)]
fn gate_reply(
    view: Res<GraphView>,
    connection: Res<State<Connection>>,
    status: Option<Res<ConnectionStatus>>,
    replying: Res<Replying>,
    inputs: Query<&EditableText, With<ReplyInput>>,
    buttons: Query<(Entity, Has<InteractionDisabled>), With<ReplyButton>>,
    mut whys: Query<&mut Text, With<ReplyWhy>>,
    mut commands: Commands,
) {
    let Some(plan) = &view.plan else { return };
    let typed = inputs.iter().any(|t| !t.value().to_string().trim().is_empty());
    let why = if *connection.get() != Connection::Live {
        status.and_then(|s| s.reason.as_ref().map(ToString::to_string)).unwrap_or_else(|| "Not connected.".into())
    } else if replying.0 {
        "Sending…".into()
    } else {
        reply_gate(plan).err().unwrap_or_default()
    };
    let on = why.is_empty() && typed;
    for (button, off) in &buttons {
        if off == on {
            if on {
                commands.entity(button).remove::<InteractionDisabled>();
            } else {
                commands.entity(button).insert(InteractionDisabled);
            }
        }
    }
    for text in &mut whys {
        set_text(text, &why);
    }
}

/// Reply was pressed: answer the newest open question with what is typed.
pub(super) fn on_reply(
    activate: On<Activate>,
    buttons: Query<(), (With<ReplyButton>, Without<InteractionDisabled>)>,
    inputs: Query<&EditableText, With<ReplyInput>>,
    view: Res<GraphView>,
    mut replying: ResMut<Replying>,
    mut submit: MessageWriter<Submit>,
) {
    if !buttons.contains(activate.entity) || replying.0 {
        return;
    }
    let Some(plan) = &view.plan else { return };
    let Some(question) = &plan.question else { return };
    let typed = inputs.iter().map(|t| t.value().to_string()).find(|t| !t.trim().is_empty());
    let Some(typed) = typed else { return };
    let request = compose_request(&plan.key, CommentKind::Note, Some(question), &typed);
    submit.write(Submit { source: activate.entity, request });
    replying.0 = true;
}

/// The reply's answer: on `Ok` the box is emptied, anything else is said under it and what was typed stays.
pub(super) fn finish_reply(
    mut done: MessageReader<CommandDone>,
    buttons: Query<(), With<ReplyButton>>,
    mut inputs: Query<&mut EditableText, With<ReplyInput>>,
    notes: Query<Entity, With<ReplyNote>>,
    mut replying: ResMut<Replying>,
    mut commands: Commands,
) {
    for d in done.read() {
        if !buttons.contains(d.source) {
            continue;
        }
        replying.0 = false;
        let says = match describe(&d.outcome) {
            None => Notice::default(),
            Some(why) => Notice::new(Tone::Error, format!("The reply wasn't sent: {why}")),
        };
        if matches!(d.outcome, Outcome::Ok { .. }) {
            for mut text in &mut inputs {
                text.editor_mut().set_text("");
            }
        }
        for note in &notes {
            commands.entity(note).insert(says.clone());
        }
    }
}

/// "Read the whole spec": the spec task in the inspector.
pub(super) fn on_read_spec(
    activate: On<Activate>,
    buttons: Query<(), With<ReadSpec>>,
    view: Res<GraphView>,
    mut selection: ResMut<Selection>,
) {
    if buttons.contains(activate.entity)
        && let Some(plan) = &view.plan
    {
        selection.0 = Some(Subject::Task(plan.id.clone()));
    }
}

/// The detail fetched for the graph's task, for the conversation.
pub(super) type PlanDetail = Detail<GraphPage>;

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use serde_json::{Value, json};
    use trailhead_model::{Agent, User};
    use trailhead_shell::fixture::{agent, at, comment, comment_json, sid, sync_event, task, task_json, user};
    use trailhead_shell::mirror::MirrorPlugin;
    use trailhead_shell::sync::{Snapshot, SyncItem};

    use super::*;
    use crate::board::{BoardPlugin, View};
    use crate::graph::{GraphUiPlugin, OpenGraph};

    fn spec(status: &str, plan: Value) -> Task {
        task(json!({"id": "s", "key": "T-1", "title": "a todo app", "description": "Make me a todo app.",
                    "status": status, "created_by": "u1", "metadata": {"plan": plan}}))
    }

    fn people() -> (Vec<Agent>, Vec<User>) {
        (vec![agent(json!({"id": "a1", "name": "factory/planner"}))], vec![user(json!({"id": "u1", "name": "lane"}))])
    }

    fn said(id: &str, kind: &str, by: &str, n: u8, resolved: bool) -> Comment {
        let mut c = json!({"id": id, "task_id": "s", "kind": kind, "author_id": by, "body": format!("{kind} {id}"),
                           "created_at": at(n)});
        if resolved {
            c["resolved_at"] = json!(at(n + 1));
        }
        comment(c)
    }

    fn plan_of(task: &Task, comments: &[Comment], thread: Thread) -> PlanView {
        let (agents, users) = people();
        let tasks = [task.clone()];
        let input = Input { tasks: &tasks, comments, agents: &agents, users: &users, ..Input::default() };
        build_plan(&input, task, thread).expect("a spec task")
    }

    #[test]
    fn the_thread_is_the_ask_then_every_comment_oldest_first_naming_who_said_it() {
        let comments = [
            said("c3", "note", "u1", 3, false),
            said("c1", "question", "a1", 1, true),
            said("c4", "question", "a1", 4, false),
        ];
        let me = (sid("u1"), "lane".to_string());
        let thread = Thread { comments: None, me: Some((&me.0, &me.1)) };
        let plan = plan_of(&spec("needs_input", json!({"spec": {}})), &comments, thread);
        let ids: Vec<&str> = plan.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, [ASK.to_string(), sid("c1"), sid("c3"), sid("c4")]);
        assert_eq!(plan.messages[0].body, "Make me a todo app.");
        assert!(plan.messages[0].mine && plan.messages[0].head.starts_with("lane · asked"));
        assert!(plan.messages[1].head.starts_with("factory/planner · "));
        let sides: Vec<bool> = plan.messages.iter().map(|m| m.mine).collect();
        assert_eq!(sides, [true, false, true, false], "what the reader wrote sits on their side");
        assert!(plan.messages[3].waiting && plan.messages[3].head.ends_with("waiting on you"));
        assert!(!plan.messages[1].waiting, "an answered question waits on nobody");
        assert_eq!(plan.question, Some((sid("c4"), "question c4".into())));
        assert_eq!(plan.turn, Turn::You);
        assert_eq!(reply_gate(&plan), Ok(()));
    }

    #[test]
    fn a_fetched_thread_replaces_what_the_mirror_holds() {
        let mirrored = [said("c4", "question", "a1", 4, false)];
        let fetched = [said("c1", "note", "a1", 1, false), said("c4", "question", "a1", 4, false)];
        let thread = Thread { comments: Some(&fetched), me: None };
        let plan = plan_of(&spec("needs_input", json!({"spec": {}})), &mirrored, thread);
        assert_eq!(plan.messages.len(), 3);
    }

    #[test]
    fn whose_turn_it_is_follows_the_status_and_the_approval() {
        let open = [said("c4", "question", "a1", 4, false)];
        let turn = |status: &str, plan: Value, comments: &[Comment]| {
            plan_of(&spec(status, plan), comments, Thread::default()).turn
        };
        assert_eq!(turn("in_progress", json!({"spec": {}}), &[]), Turn::Planner);
        assert_eq!(turn("open", json!({"spec": {}}), &[]), Turn::Queued);
        assert_eq!(turn("needs_input", json!({"spec": {}}), &open), Turn::You);
        assert_eq!(turn("needs_input", json!({"spec": {}}), &[]), Turn::Idle(Status::NeedsInput));
        let approved = turn("open", json!({"spec": {"approved": 1_790_000_000}}), &[]);
        assert!(matches!(approved, Turn::Approved(_)), "{approved:?}");
        assert_eq!(approved.tone(), Tone::Success);
        let working = plan_of(&spec("in_progress", json!({"spec": {}})), &[], Thread::default());
        assert!(reply_gate(&working).unwrap_err().contains("working"));
    }

    #[test]
    fn a_long_ask_is_cut_short_and_read_whole_in_the_inspector() {
        let mut task = spec("open", json!({"spec": {}}));
        task.description = "word ".repeat(200);
        let ask = &plan_of(&task, &[], Thread::default()).messages[0];
        assert!(ask.clamped && ask.body.ends_with('…'));
        assert!(ask.body.chars().count() <= ASK_CHARS + 1);
    }

    #[test]
    fn a_spec_task_offers_its_graph_on_its_card_before_it_has_subtasks() {
        let tasks = [spec("open", json!({"spec": {}})), task(json!({"id": "x", "key": "T-2"}))];
        let board = crate::board::build(Input { tasks: &tasks, ..Input::default() }, &crate::board::Filter::default());
        let graph = |key: &str| board.columns.iter().flat_map(|c| &c.cards).find(|c| c.key == key).unwrap().graph;
        assert!(graph("T-1") && !graph("T-2"));
    }

    #[test]
    fn a_task_that_is_not_a_spec_has_no_plan() {
        let plain = task(json!({"id": "s", "key": "T-1"}));
        assert_eq!(
            build_plan(&Input { tasks: std::slice::from_ref(&plain), ..Input::default() }, &plain, Thread::default()),
            None
        );
    }

    /// The graph page drawn headless, open on the spec task `s`, which waits on the reader's answer to `c1`.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_state::<Connection>()
            .init_resource::<ConnectionStatus>()
            .init_resource::<Selection>()
            .add_message::<Submit>()
            .add_message::<CommandDone>()
            .add_plugins((MirrorPlugin, BoardPlugin, GraphUiPlugin, crate::widgets::CanvasPlugin));
        app.insert_state(trailhead_shell::ActiveApp(crate::LOOKOUT)).add_sub_state::<View>();
        let question = comment_json(
            json!({"id": "c1", "task_id": "s", "kind": "question", "body": "which db?", "created_at": at(1)}),
        );
        let tasks = vec![task_json(json!({"id": "s", "key": "T-1", "title": "a todo app", "status": "needs_input",
                                          "metadata": {"plan": {"spec": {}}}, "open_questions": [question]}))];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot { at: 1, tasks, ..Default::default() }));
        app.world_mut().resource_mut::<NextState<Connection>>().set(Connection::Live);
        app.world_mut().trigger(OpenGraph(sid("s")));
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..5 {
            app.update();
        }
    }

    fn one<C: Component>(app: &mut App) -> Entity {
        app.world_mut().query_filtered::<Entity, With<C>>().single(app.world()).unwrap()
    }

    fn everything(app: &mut App) -> Vec<Entity> {
        let mut all: Vec<Entity> = app.world_mut().query::<Entity>().iter(app.world()).collect();
        all.sort();
        all
    }

    fn type_reply(app: &mut App, words: &str) {
        let input = one::<ReplyInput>(app);
        app.world_mut().get_mut::<EditableText>(input).unwrap().editor_mut().set_text(words);
        settle(app);
    }

    #[test]
    fn a_spec_task_with_no_subtasks_shows_its_conversation_beside_an_empty_canvas() {
        let mut app = app();
        assert_eq!(*app.world().resource::<State<View>>().get(), View::Graph);
        let panel = one::<ConversationPanel>(&mut app);
        assert_eq!(app.world().get::<Node>(panel).unwrap().display, Display::Flex);
        let texts: Vec<String> = app.world_mut().query::<&Text>().iter(app.world()).map(|t| t.0.clone()).collect();
        assert!(texts.iter().any(|t| t == "No tasks are planned yet."));
        assert!(texts.iter().any(|t| t == "which db?"));
        let line = one::<TurnLine>(&mut app);
        let turn = app.world().get::<Notice>(line).unwrap().clone();
        assert_eq!(turn, Notice::new(Tone::Warning, Turn::You.words()));
    }

    #[test]
    fn a_reply_answers_the_newest_question_and_a_new_comment_lands_in_place_keeping_the_draft() {
        let mut app = app();
        let reply = one::<ReplyButton>(&mut app);
        assert!(app.world().get::<InteractionDisabled>(reply).is_some(), "nothing typed yet");
        type_reply(&mut app, "sqlite");
        assert!(app.world().get::<InteractionDisabled>(reply).is_none());
        let before = everything(&mut app);
        // A note lands while the reply is being written: one message more, and nothing else respawned.
        app.world_mut().write_message(sync_event(json!({
            "id": 20, "entity": "comment", "entity_id": sid("c2"), "kind": "comment.created",
            "after": comment_json(json!({"id": "c2", "task_id": "s", "kind": "note", "author_id": "a1",
                                         "body": "one more thing", "created_at": at(2)})),
        })));
        settle(&mut app);
        let after = everything(&mut app);
        assert!(before.iter().all(|e| after.contains(e)), "nothing was despawned");
        assert!(after.len() > before.len());
        let input = one::<ReplyInput>(&mut app);
        assert_eq!(app.world().get::<EditableText>(input).unwrap().value().to_string(), "sqlite", "the draft held");

        app.world_mut().trigger(Activate { entity: reply });
        let sent: Vec<Submit> =
            app.world_mut().resource_mut::<bevy::ecs::message::Messages<Submit>>().drain().collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].request.call.path(), format!("/v1/comments/{}/resolve", sid("c1")));
        assert_eq!(sent[0].request.call.body(), Some(json!({"answer": "sqlite"})));
        app.world_mut().write_message(CommandDone {
            source: reply,
            id: trailhead_shell::commands::CommandId(1),
            outcome: Outcome::Ok { version: None },
        });
        settle(&mut app);
        assert_eq!(app.world().get::<EditableText>(input).unwrap().value().to_string(), "", "sent, so emptied");
    }
}
