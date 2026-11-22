//! The Ask form. Pressing the rail's Ask button ([`crate::rail::AskButton`]) opens a modal titled "Ask": a dropdown of
//! what it is about (each app the server can be asked about, then [`NEW_APP`]), the new app's name when that is
//! chosen, the ask itself under a dim line saying what becomes of it, and a notice saying why Ask can't be pressed
//! yet or what the server said when it refused. Ask and Cancel are in the footer. Once the server has made what was
//! asked, the form is hidden and a confirmation shown in its place, with Done.
//!
//! Everything is spawned once when the modal opens and changed in place after; the logic is [`crate::ask`]'s.

use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, RequestClose};
use trailhead_ui::{
    ButtonKind, InitialText, Modal, ModalFocus, ModalLocked, ModalSlots, Notice, Picked, SelectEdit, Tone, button,
    display, field, notice, select, set_display, set_text, size, space, text, text_dim, text_input,
};

use crate::ask::{About, AskForm, Askable, Gate, NEW_APP, OpenAsk, Phase, SendAsk, offline_reason, options};
use crate::connection::{Connection, ConnectionStatus};
use crate::rail::AskButton;

/// The form's modal.
#[derive(Component)]
pub struct AskPanel;

impl Modal for AskPanel {
    fn title(&self) -> String {
        "Ask".into()
    }
}

/// The hint over the ask's text.
pub const HINT: &str = "Say what you want. It becomes a task the factory builds.";

/// How many lines the ask's text shows.
const TEXT_LINES: f32 = 6.0;

/// How many apps the dropdown shows before it scrolls.
const VISIBLE_CHOICES: usize = 8;

/// Which part of the form this is.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AskPart {
    /// The column holding the form, hidden once the ask is made.
    #[default]
    Form,
    /// The dropdown of what the ask is about.
    About,
    /// The new app's name field, shown only for a new app.
    NameRow,
    /// The name's text entity.
    Name,
    /// The ask's text entity.
    Text,
    /// Why Ask can't be pressed, or what the server said.
    Notice,
    /// The column holding the confirmation, shown once the ask is made.
    Done,
    /// "Asked: TICK-17".
    Asked,
    /// The task's title.
    Title,
    /// "Made the project TN, Trail Notes", for a new app.
    Project,
    /// What the server noted, for a part it couldn't do.
    Notes,
}

/// What a footer button does.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AskAct {
    #[default]
    Ask,
    Cancel,
    Done,
}

/// The form, for a shell with a rail.
pub(crate) struct AskPanelUiPlugin;

impl Plugin for AskPanelUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_ask_button).add_observer(on_act).add_observer(on_pick).add_systems(
            Update,
            (fill_panel, read_inputs, show_choice, show_phase, show_notice, show_buttons, show_asked).chain(),
        );
    }
}

fn on_ask_button(
    activate: On<Activate>,
    buttons: Query<(), With<AskButton>>,
    open: Query<(), With<AskPanel>>,
    mut commands: Commands,
) {
    if buttons.contains(activate.entity) && open.is_empty() {
        commands.trigger(OpenAsk);
        trailhead_ui::open_modal(&mut commands, AskPanel);
    }
}

fn on_act(activate: On<Activate>, acts: Query<&AskAct>, mut commands: Commands) {
    match acts.get(activate.entity) {
        Ok(AskAct::Ask) => commands.trigger(SendAsk),
        Ok(AskAct::Cancel | AskAct::Done) => commands.trigger(RequestClose { source: activate.entity }),
        Err(_) => {}
    }
}

fn on_pick(pick: On<Picked>, parts: Query<&AskPart>, askable: Res<Askable>, mut form: ResMut<AskForm>) {
    if matches!(parts.get(pick.entity), Ok(AskPart::About)) {
        let about = About::at(pick.index, askable.targets());
        if form.about != about {
            form.about = about;
        }
    }
}

/// Fills a modal that has just spawned: the form and the confirmation (hidden) in its body, the buttons in its footer.
fn fill_panel(
    new: Query<&ModalSlots, (With<AskPanel>, Added<ModalSlots>)>,
    form: Res<AskForm>,
    askable: Res<Askable>,
    mut commands: Commands,
) {
    for slots in &new {
        let column = |part: AskPart| {
            (Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() }, part)
        };
        let body = commands.spawn((column(AskPart::Form), ChildOf(slots.body))).id();
        let targets = askable.targets();
        let (choices, chosen) = (options(targets), form.about.index(targets));
        let about = bsn_list! { @select(choices, Some(chosen), VISIBLE_CHOICES) AskPart::About };
        commands.spawn_scene(field("About", about)).insert(ChildOf(body));
        let name = bsn_list! { @text_input(1.0, bsn! { AskPart::Name InitialText({ form.name.clone() }) }) };
        let (label, new_app) = (format!("{NEW_APP}: its name"), form.about == About::NewApp);
        let name_row = bsn! { @field(label, name) AskPart::NameRow Node { display: { display(new_app) } } };
        commands.spawn_scene(name_row).insert(ChildOf(body));
        let ask =
            bsn_list! { @text_input(TEXT_LINES, bsn! { AskPart::Text InitialText({ form.text.clone() }) ModalFocus }) };
        commands.spawn_scene(field(HINT, ask)).insert(ChildOf(body));
        commands.spawn_scene(notice()).insert((AskPart::Notice, ChildOf(body)));

        let (hidden, part) = column(AskPart::Done);
        let done = commands.spawn((Node { display: Display::None, ..hidden }, part, ChildOf(slots.body))).id();
        commands.spawn_scene(text("", size::HEADING)).insert((AskPart::Asked, ChildOf(done)));
        commands.spawn_scene(text_dim("", size::BODY)).insert((AskPart::Title, ChildOf(done)));
        commands.spawn_scene(text("", size::BODY)).insert((AskPart::Project, ChildOf(done)));
        commands.spawn_scene(notice()).insert((AskPart::Notes, ChildOf(done)));

        for (caption, kind, act, shown) in [
            ("Ask", ButtonKind::Primary, AskAct::Ask, true),
            ("Cancel", ButtonKind::Normal, AskAct::Cancel, true),
            ("Done", ButtonKind::Primary, AskAct::Done, false),
        ] {
            let footed = bsn! { @button(caption, None, kind) Node { display: { display(shown) } } };
            commands.spawn_scene(footed).insert((act, ChildOf(slots.footer)));
        }
    }
}

/// Copies what is typed into the form while it is being edited. An input that has just appeared is empty until the
/// kit puts its initial text in, so it is read from its first change on.
fn read_inputs(fields: Query<(&AskPart, Ref<EditableText>)>, mut form: ResMut<AskForm>) {
    if form.phase != Phase::Editing {
        return;
    }
    for (part, typed) in &fields {
        if typed.is_added() || !typed.is_changed() {
            continue;
        }
        let value = typed.value().to_string();
        match part {
            AskPart::Name if form.name != value => form.name = value,
            AskPart::Text if form.text != value => form.text = value,
            _ => {}
        }
    }
}

/// Keeps the dropdown on what the form is about, with a row for each app the server last named, and the name field
/// shown for a new app. A dropdown whose list is open is tried again next frame.
fn show_choice(
    form: Res<AskForm>,
    askable: Res<Askable>,
    parts: Query<(Entity, &AskPart)>,
    added: Query<(), Added<ModalSlots>>,
    mut nodes: Query<&mut Node>,
    mut edit: SelectEdit,
    mut pending: Local<bool>,
) {
    if !form.is_changed() && !askable.is_changed() && added.is_empty() && !*pending {
        return;
    }
    let targets = askable.targets();
    let (choices, chosen) = (options(targets), form.about.index(targets));
    *pending = false;
    for (entity, part) in &parts {
        match part {
            AskPart::About => *pending |= !edit.set(entity, &choices, Some(chosen)),
            AskPart::NameRow => set_display(&mut nodes, entity, form.about == About::NewApp),
            _ => {}
        }
    }
}

/// Shows the form while it is edited or sent and the confirmation once the ask is made, with the footer to match;
/// the modal stays open while the ask is with the server.
fn show_phase(
    form: Res<AskForm>,
    parts: Query<(Entity, &AskPart)>,
    acts: Query<(Entity, &AskAct)>,
    panels: Query<(Entity, Has<ModalLocked>), With<AskPanel>>,
    mut nodes: Query<&mut Node>,
    mut commands: Commands,
) {
    let asked = matches!(form.phase, Phase::Asked(_));
    for (entity, part) in &parts {
        match part {
            AskPart::Form => set_display(&mut nodes, entity, !asked),
            AskPart::Done => set_display(&mut nodes, entity, asked),
            _ => {}
        }
    }
    for (entity, act) in &acts {
        set_display(&mut nodes, entity, (*act == AskAct::Done) == asked);
    }
    let sending = form.phase == Phase::Sending;
    for (panel, locked) in &panels {
        match (sending, locked) {
            (true, false) => {
                commands.entity(panel).insert(ModalLocked);
            }
            (false, true) => {
                commands.entity(panel).remove::<ModalLocked>();
            }
            _ => {}
        }
    }
}

/// Whether Ask can be pressed, from the form and the connection.
fn gate(form: &AskForm, connection: Option<&State<Connection>>, status: Option<&ConnectionStatus>) -> Gate {
    let live = connection.is_some_and(|c| *c.get() == Connection::Live);
    form.gate(live, offline_reason(status))
}

/// Says in the notice why Ask can't be pressed, or what the server said of the last ask.
fn show_notice(
    form: Res<AskForm>,
    connection: Option<Res<State<Connection>>>,
    status: Option<Res<ConnectionStatus>>,
    mut parts: Query<(&AskPart, &mut Notice)>,
) {
    let says = match (&form.error, gate(&form, connection.as_deref(), status.as_deref())) {
        (Some(error), _) => Notice::new(Tone::Error, error.clone()),
        (None, Gate::Blocked(why)) => Notice::new(Tone::Info, why),
        _ => Notice::default(),
    };
    for (part, mut notice) in &mut parts {
        if *part == AskPart::Notice && (notice.tone != says.tone || notice.message != says.message) {
            *notice = says.clone();
        }
    }
}

/// Enables Ask only when there is something to send over a live connection, and says on it while an ask is with the
/// server. Cancel waits for the answer too.
fn show_buttons(
    form: Res<AskForm>,
    connection: Option<Res<State<Connection>>>,
    status: Option<Res<ConnectionStatus>>,
    acts: Query<(Entity, &AskAct, Has<InteractionDisabled>)>,
    children: Query<&Children>,
    mut texts: Query<&mut Text>,
    mut commands: Commands,
) {
    let gate = gate(&form, connection.as_deref(), status.as_deref());
    for (entity, act, disabled) in &acts {
        let enabled = match act {
            AskAct::Ask => gate == Gate::Ready,
            AskAct::Cancel => gate != Gate::Sending,
            AskAct::Done => true,
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
        if *act == AskAct::Ask {
            let caption = if gate == Gate::Sending { "Asking…" } else { "Ask" };
            for inside in children.iter_descendants(entity) {
                if let Ok(text) = texts.get_mut(inside) {
                    set_text(text, caption);
                }
            }
        }
    }
}

/// Fills the confirmation once the ask is made: the task's key and title, the project for a new app, and any notes. A
/// new app whose project was made without its task shows the project and the note saying why.
fn show_asked(form: Res<AskForm>, mut parts: Query<(&AskPart, Option<&mut Text>, Option<&mut Notice>, &mut Node)>) {
    let Phase::Asked(asked) = &form.phase else { return };
    if !form.is_changed() {
        return;
    }
    let asked_line = asked.task.as_ref().map(|t| format!("Asked: {}", t.key)).unwrap_or_default();
    let title = asked.task.as_ref().map_or("", |t| t.title.as_str());
    let made = asked.project.created.then(|| format!("Made the project {}, {}", asked.project.key, asked.project.name));
    let notes = asked.notes.join(" ");
    for (part, text, notice, mut node) in &mut parts {
        let words = match part {
            AskPart::Asked => asked_line.clone(),
            AskPart::Title => title.to_string(),
            AskPart::Project => made.clone().unwrap_or_default(),
            AskPart::Notes => {
                if let Some(mut notice) = notice
                    && notice.message != notes
                {
                    *notice = Notice::new(Tone::Warning, notes.clone());
                }
                continue;
            }
            _ => continue,
        };
        let shown = !words.is_empty();
        if node.display != display(shown) {
            node.display = display(shown);
        }
        if let Some(text) = text {
            set_text(text, &words);
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use trailhead_model::shell::{AskTarget, AskTargets, Asked, AskedProject, AskedTask};
    use trailhead_ui::{ModalPlugin, ModalRoot};

    use super::*;
    use crate::ShellPlugin;
    use crate::ask::AskReply;
    use crate::testing::{TestConnection, set_connection};
    use crate::tests::{Fake, MAPS, NOTES};

    fn windowed() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .add_plugins(TestConnection)
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_plugins(ModalPlugin)
            .add_plugins(ShellPlugin::new().with(Fake(NOTES)).with(Fake(MAPS)).windowed());
        set_connection(&mut app, Connection::Live);
        let target = |project: &str, name: &str, path: &str| AskTarget {
            project: project.into(),
            name: name.into(),
            path: Some(path.into()),
        };
        let targets = vec![target("ATLAS", "Atlas", "apps/atlas"), target("NOTE", "Notes", "apps/notes")];
        app.world_mut().resource_mut::<Askable>().0 =
            Some(AskTargets { configured: true, repo: Some("lane/apps".into()), targets });
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    fn one<F: bevy::ecs::query::QueryFilter>(app: &mut App) -> Entity {
        app.world_mut().query_filtered::<Entity, F>().single(app.world()).unwrap()
    }

    fn open(app: &mut App) -> Entity {
        let button = one::<With<AskButton>>(app);
        app.world_mut().trigger(Activate { entity: button });
        settle(app);
        one::<With<AskPanel>>(app)
    }

    fn part(app: &App, wanted: AskPart) -> Entity {
        let mut parts = app.world().try_query::<(Entity, &AskPart)>().unwrap();
        parts.iter(app.world()).find(|(_, p)| **p == wanted).map(|(e, _)| e).unwrap()
    }

    fn act(app: &App, wanted: AskAct) -> Entity {
        let mut acts = app.world().try_query::<(Entity, &AskAct)>().unwrap();
        acts.iter(app.world()).find(|(_, a)| **a == wanted).map(|(e, _)| e).unwrap()
    }

    fn shown(app: &App, entity: Entity) -> bool {
        app.world().get::<Node>(entity).unwrap().display != Display::None
    }

    fn words(app: &App, entity: Entity) -> String {
        app.world().get::<Text>(entity).unwrap().0.clone()
    }

    fn notice(app: &App, entity: Entity) -> (Tone, String) {
        let notice = app.world().get::<Notice>(entity).unwrap();
        (notice.tone, notice.message.clone())
    }

    fn enabled(app: &App, entity: Entity) -> bool {
        app.world().get::<InteractionDisabled>(entity).is_none()
    }

    fn caption(app: &App, button: Entity) -> String {
        let world = app.world();
        let mut stack = vec![button];
        while let Some(e) = stack.pop() {
            if let Some(text) = world.get::<Text>(e) {
                return text.0.clone();
            }
            stack.extend(world.get::<Children>(e).into_iter().flat_map(|c| c.iter()));
        }
        panic!("no caption")
    }

    fn type_into(app: &mut App, wanted: AskPart, words: &str) {
        let field = part(app, wanted);
        app.world_mut().get_mut::<EditableText>(field).unwrap().editor_mut().set_text(words);
        settle(app);
    }

    fn entities(app: &mut App) -> Vec<Entity> {
        let mut all: Vec<Entity> = app.world_mut().query::<Entity>().iter(app.world()).collect();
        all.sort();
        all
    }

    fn press(app: &mut App, entity: Entity) {
        app.world_mut().trigger(Activate { entity });
        settle(app);
    }

    #[test]
    fn the_form_opens_on_the_app_shown_with_its_rows_apart_and_ask_waiting_for_words() {
        let mut app = windowed();
        let panel = open(&mut app);
        assert!(app.world().get::<ModalRoot>(panel).is_some());
        assert_eq!(app.world().resource::<AskForm>().about, About::Project("NOTE".into()), "notes is shown");
        let about = part(&app, AskPart::About);
        let options = app.world().get::<trailhead_ui::SelectOptions>(about).unwrap().0.clone();
        assert_eq!(options, ["Atlas", "Notes", NEW_APP]);

        let form = part(&app, AskPart::Form);
        assert_eq!(app.world().get::<Node>(form).unwrap().row_gap, px(space::ROWS), "the rows stand apart");
        let rows: Vec<Entity> = app.world().get::<Children>(form).unwrap().iter().collect();
        assert_eq!(rows.len(), 4, "about, the name, the ask and the notice");
        for field in &rows[..3] {
            assert_eq!(
                app.world().get::<Node>(*field).unwrap().row_gap,
                px(space::STACKED),
                "a label over its control"
            );
        }
        let hint = app.world_mut().query::<&Text>().iter(app.world()).any(|t| t.0 == HINT);
        assert!(hint, "the hint over the ask");
        let name_row = part(&app, AskPart::NameRow);
        assert!(!shown(&app, name_row), "no name for a change to an app");
        assert!(!shown(&app, part(&app, AskPart::Done)));

        let (ask, cancel, done) = (act(&app, AskAct::Ask), act(&app, AskAct::Cancel), act(&app, AskAct::Done));
        assert_eq!((shown(&app, ask), shown(&app, cancel), shown(&app, done)), (true, true, false));
        assert!(!enabled(&app, ask) && enabled(&app, cancel));
        let says = notice(&app, part(&app, AskPart::Notice));
        assert_eq!(says, (Tone::Info, "Say what you want first.".into()));
    }

    #[test]
    fn choosing_a_new_app_shows_its_name_in_place_and_ask_waits_for_it() {
        let mut app = windowed();
        open(&mut app);
        type_into(&mut app, AskPart::Text, "Notes on routes");
        let ask = act(&app, AskAct::Ask);
        assert!(enabled(&app, ask));
        assert_eq!(notice(&app, part(&app, AskPart::Notice)).1, "");
        let before = entities(&mut app);
        let about = part(&app, AskPart::About);
        app.world_mut().trigger(Picked { entity: about, index: 2 });
        settle(&mut app);
        assert_eq!(app.world().resource::<AskForm>().about, About::NewApp);
        assert!(shown(&app, part(&app, AskPart::NameRow)));
        assert!(!enabled(&app, ask));
        assert_eq!(notice(&app, part(&app, AskPart::Notice)).1, "Give the new app a name.");
        type_into(&mut app, AskPart::Name, "Trail Notes");
        assert!(enabled(&app, ask));
        assert_eq!(entities(&mut app), before, "nothing was spawned or despawned");

        set_connection(&mut app, Connection::Offline);
        assert!(!enabled(&app, ask));
        assert!(notice(&app, part(&app, AskPart::Notice)).1.starts_with("Not connected"));
    }

    fn asked() -> Asked {
        Asked {
            project: AskedProject { key: "TN".into(), name: "Trail Notes".into(), created: true },
            task: Some(AskedTask { key: "TN-1".into(), title: "Create the Trail Notes app".into() }),
            planned: false,
            notes: vec!["There is no `factory` agent yet.".into()],
        }
    }

    #[test]
    fn asking_says_so_and_success_shows_what_was_made_in_place_and_done_closes() {
        let mut app = windowed();
        let before = entities(&mut app);
        let panel = open(&mut app);
        type_into(&mut app, AskPart::Text, "Notes on routes");
        let about = part(&app, AskPart::About);
        app.world_mut().trigger(Picked { entity: about, index: 2 });
        settle(&mut app);
        type_into(&mut app, AskPart::Name, "Trail Notes");
        let (ask, cancel, done) = (act(&app, AskAct::Ask), act(&app, AskAct::Cancel), act(&app, AskAct::Done));
        press(&mut app, ask);
        press(&mut app, ask);
        assert_eq!(app.world().resource::<AskForm>().phase, Phase::Sending);
        assert_eq!(caption(&app, ask), "Asking…");
        assert!(!enabled(&app, ask) && !enabled(&app, cancel));
        assert!(app.world().get::<ModalLocked>(panel).is_some(), "it stays open while asking");

        let in_form = entities(&mut app);
        app.world_mut().write_message(AskReply { seq: 1, result: Ok(asked()) });
        settle(&mut app);
        assert_eq!(entities(&mut app), in_form, "the confirmation replaces the form in place");
        assert!(!shown(&app, part(&app, AskPart::Form)) && shown(&app, part(&app, AskPart::Done)));
        let lines = [AskPart::Asked, AskPart::Title, AskPart::Project].map(|p| part(&app, p));
        assert_eq!(
            lines.map(|l| words(&app, l)),
            ["Asked: TN-1".to_string(), "Create the Trail Notes app".into(), "Made the project TN, Trail Notes".into()]
        );
        assert!(lines.iter().all(|l| shown(&app, *l)));
        let notes = notice(&app, part(&app, AskPart::Notes));
        assert_eq!(notes, (Tone::Warning, "There is no `factory` agent yet.".into()));
        assert_eq!((shown(&app, ask), shown(&app, cancel), shown(&app, done)), (false, false, true));
        assert!(app.world().get::<ModalLocked>(panel).is_none());

        press(&mut app, done);
        assert_eq!(app.world_mut().query::<&AskPanel>().iter(app.world()).count(), 0);
        assert_eq!(entities(&mut app), before, "the window is as it was");
    }

    #[test]
    fn a_failure_keeps_the_text_and_shows_the_servers_message() {
        let mut app = windowed();
        open(&mut app);
        type_into(&mut app, AskPart::Text, "A Completed view");
        let ask = act(&app, AskAct::Ask);
        press(&mut app, ask);
        app.world_mut().write_message(AskReply { seq: 1, result: Err("NOTE isn't an app in lane/apps".into()) });
        settle(&mut app);
        assert_eq!(notice(&app, part(&app, AskPart::Notice)), (Tone::Error, "NOTE isn't an app in lane/apps".into()));
        let field = part(&app, AskPart::Text);
        assert_eq!(app.world().get::<EditableText>(field).unwrap().value().to_string(), "A Completed view");
        assert!(shown(&app, part(&app, AskPart::Form)));
        assert!(enabled(&app, ask), "it can be asked again");
        assert_eq!(caption(&app, ask), "Ask");
    }

    #[test]
    fn cancel_closes_and_keeps_the_draft_for_next_time() {
        let mut app = windowed();
        open(&mut app);
        type_into(&mut app, AskPart::Text, "half a thought");
        let cancel = act(&app, AskAct::Cancel);
        press(&mut app, cancel);
        assert_eq!(app.world_mut().query::<&AskPanel>().iter(app.world()).count(), 0);
        assert_eq!(app.world().resource::<AskForm>().text, "half a thought");
        open(&mut app);
        let field = part(&app, AskPart::Text);
        assert_eq!(app.world().get::<InitialText>(field).map(|t| t.0.as_str()), Some("half a thought"));
    }
}
