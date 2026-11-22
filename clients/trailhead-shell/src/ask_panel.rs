//! The Ask form, in two kinds. The rail's Ask button ([`crate::rail::AskButton`]) opens "Ask for a change": a dropdown
//! of the app it is about (each app the server can be asked about, the app shown first), the ask under a dim line
//! saying what becomes of it, a "Plan this first" switch, off at first, and the planner's model while it is on. The
//! plus under the apps ([`crate::rail::NewAppButton`]) opens "Ask for a new app": its name, the ask, and the planner's
//! model, since a new app is always planned. Under them a notice says why Ask can't be pressed yet or what the server
//! said when it refused. Ask and Cancel are in the footer. Once the server has made what was asked, the form is
//! hidden and a confirmation shown in its place, with Done.
//!
//! Everything is spawned once when the modal opens and changed in place after; the logic is [`crate::ask`]'s.

use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::{Activate, RequestClose, ValueChange};
use trailhead_ui::{
    ButtonKind, InitialText, Modal, ModalFocus, ModalLocked, ModalSlots, Notice, Picked, SelectEdit, Tone, button,
    display, field, notice, select, set_display, set_text, setting_toggle, size, space, text, text_dim, text_input,
};

use crate::ask::{
    About, AskFor, AskForm, Askable, Gate, OpenAsk, Phase, SendAsk, confirmation, model_at, model_index, model_options,
    offline_reason, options,
};
use crate::connection::{Connection, ConnectionStatus};
use crate::rail::{AskButton, NewAppButton};

/// The form's modal, for a change or a new app.
#[derive(Component)]
pub struct AskPanel(pub AskFor);

impl Modal for AskPanel {
    fn title(&self) -> String {
        match self.0 {
            AskFor::Change => "Ask for a change".into(),
            AskFor::NewApp => "Ask for a new app".into(),
        }
    }
}

/// The hint over a change's text.
pub const HINT: &str = "Say what you want. It becomes a task the factory builds.";

/// The hint over a new app's text.
pub const NEW_APP_HINT: &str =
    "Say what the app is for and what it should do. The planner talks it over with you first.";

/// The "Plan this first" switch's label and the note under it.
pub const PLAN: &str = "Plan this first";
pub const PLAN_NOTE: &str =
    "A planner talks the ask over with you and proposes tasks for you to approve before any work starts.";

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
    /// The dropdown of the app a change is about.
    About,
    /// A new app's name's text entity.
    Name,
    /// The ask's text entity.
    Text,
    /// The "Plan this first" switch, for a change.
    Plan,
    /// The planner's model's field, shown while the ask is planned.
    ModelRow,
    /// The planner's model's dropdown.
    Model,
    /// Why Ask can't be pressed, or what the server said.
    Notice,
    /// The column holding the confirmation, shown once the ask is made.
    Done,
    /// "Asked: TICK-17", or "Sent to the planner: TICK-17".
    Asked,
    /// The task's title.
    Title,
    /// "Made the project TN, Trail Notes", for a new app.
    Project,
    /// What the planner does next and where its answer shows, for a planned ask.
    Next,
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
        app.add_observer(on_ask_button).add_observer(on_act).add_observer(on_pick).add_observer(on_plan).add_systems(
            Update,
            (fill_panel, read_inputs, show_choice, show_phase, show_notice, show_buttons, show_asked).chain(),
        );
    }
}

fn on_ask_button(
    activate: On<Activate>,
    changes: Query<(), With<AskButton>>,
    new_apps: Query<(), With<NewAppButton>>,
    open: Query<(), With<AskPanel>>,
    mut commands: Commands,
) {
    let asks_for = match (changes.contains(activate.entity), new_apps.contains(activate.entity)) {
        (true, _) => AskFor::Change,
        (_, true) => AskFor::NewApp,
        _ => return,
    };
    if open.is_empty() {
        commands.trigger(OpenAsk(asks_for));
        trailhead_ui::open_modal(&mut commands, AskPanel(asks_for));
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
    match parts.get(pick.entity) {
        Ok(AskPart::About) => {
            let about = About::at(pick.index, askable.targets());
            if form.about != about {
                form.about = about;
            }
        }
        Ok(AskPart::Model) => {
            let model = model_at(pick.index);
            if form.model != model {
                form.model = model;
            }
        }
        _ => {}
    }
}

fn on_plan(change: On<ValueChange<bool>>, parts: Query<&AskPart>, mut form: ResMut<AskForm>) {
    if matches!(parts.get(change.source), Ok(AskPart::Plan)) && form.plan != change.value {
        form.plan = change.value;
    }
}

/// Fills a modal that has just spawned: the form and the confirmation (hidden) in its body, the buttons in its footer.
fn fill_panel(
    new: Query<(&ModalSlots, &AskPanel), Added<ModalSlots>>,
    form: Res<AskForm>,
    askable: Res<Askable>,
    mut commands: Commands,
) {
    for (slots, panel) in &new {
        let body = commands.spawn((column(AskPart::Form), ChildOf(slots.body))).id();
        match panel.0 {
            AskFor::Change => fill_change(&mut commands, body, &form, &askable),
            AskFor::NewApp => fill_new_app(&mut commands, body, &form),
        }
        commands.spawn_scene(notice()).insert((AskPart::Notice, ChildOf(body)));
        fill_done(&mut commands, slots.body);
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

/// A column of the form's rows, `space::ROWS` apart.
fn column(part: AskPart) -> (Node, AskPart) {
    (Node { flex_direction: FlexDirection::Column, row_gap: px(space::ROWS), ..default() }, part)
}

/// A change's rows: the app, the ask, "Plan this first" and the planner's model.
fn fill_change(commands: &mut Commands, body: Entity, form: &AskForm, askable: &Askable) {
    let targets = askable.targets();
    let (choices, chosen) = (options(targets), form.about.index(targets));
    let about = bsn_list! { @select(choices, chosen, VISIBLE_CHOICES) AskPart::About };
    commands.spawn_scene(field("App", about)).insert(ChildOf(body));
    spawn_text(commands, body, HINT, form);
    let plan = setting_toggle(PLAN, PLAN_NOTE, form.plan, bsn! { AskPart::Plan });
    commands.spawn_scene(plan).insert(ChildOf(body));
    spawn_model(commands, body, form);
}

/// A new app's rows: its name, the ask and the planner's model.
fn fill_new_app(commands: &mut Commands, body: Entity, form: &AskForm) {
    let name = bsn_list! { @text_input(1.0, bsn! { AskPart::Name InitialText({ form.name.clone() }) }) };
    commands.spawn_scene(field("The new app's name", name)).insert(ChildOf(body));
    spawn_text(commands, body, NEW_APP_HINT, form);
    spawn_model(commands, body, form);
}

fn spawn_text(commands: &mut Commands, body: Entity, hint: &str, form: &AskForm) {
    let ask =
        bsn_list! { @text_input(TEXT_LINES, bsn! { AskPart::Text InitialText({ form.text.clone() }) ModalFocus }) };
    commands.spawn_scene(field(hint, ask)).insert(ChildOf(body));
}

/// The planner's model, shown while the ask is planned.
fn spawn_model(commands: &mut Commands, body: Entity, form: &AskForm) {
    let model = bsn_list! { @select(model_options(), Some(model_index(form.model)), VISIBLE_CHOICES) AskPart::Model };
    let row =
        bsn! { @field("The planner's model", model) AskPart::ModelRow Node { display: { display(form.planned()) } } };
    commands.spawn_scene(row).insert(ChildOf(body));
}

/// The confirmation, hidden until the ask is made.
fn fill_done(commands: &mut Commands, body: Entity) {
    let (hidden, part) = column(AskPart::Done);
    let done = commands.spawn((Node { display: Display::None, ..hidden }, part, ChildOf(body))).id();
    commands.spawn_scene(text("", size::HEADING)).insert((AskPart::Asked, ChildOf(done)));
    commands.spawn_scene(text_dim("", size::BODY)).insert((AskPart::Title, ChildOf(done)));
    commands.spawn_scene(text("", size::BODY)).insert((AskPart::Project, ChildOf(done)));
    commands.spawn_scene(text("", size::BODY)).insert((AskPart::Next, ChildOf(done)));
    commands.spawn_scene(notice()).insert((AskPart::Notes, ChildOf(done)));
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

/// Keeps the app dropdown on what the form is about, with a row for each app the server last named, the model
/// dropdown on the model chosen, and the model shown while the ask is planned. A dropdown whose list is open is tried
/// again next frame.
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
    let (models, model) = (model_options(), model_index(form.model));
    *pending = false;
    for (entity, part) in &parts {
        match part {
            AskPart::About => *pending |= !edit.set(entity, &choices, chosen),
            AskPart::Model => *pending |= !edit.set(entity, &models, Some(model)),
            AskPart::ModelRow => set_display(&mut nodes, entity, form.planned()),
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

/// Fills the confirmation once the ask is made ([`confirmation`]): the task's key and title, the project for a new
/// app, what the planner does next for a planned ask, and any notes. A new app whose project was made without its task
/// shows the project and the note saying why.
fn show_asked(form: Res<AskForm>, mut parts: Query<(&AskPart, Option<&mut Text>, Option<&mut Notice>, &mut Node)>) {
    let Phase::Asked(asked) = &form.phase else { return };
    if !form.is_changed() {
        return;
    }
    let says = confirmation(asked);
    let notes = asked.notes.join(" ");
    for (part, text, notice, mut node) in &mut parts {
        let words = match part {
            AskPart::Asked => &says.heading,
            AskPart::Title => &says.title,
            AskPart::Project => &says.project,
            AskPart::Next => &says.next,
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
            set_text(text, words);
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::state::app::StatesPlugin;
    use trailhead_model::shell::{Ask, AskTarget, AskTargets, Asked, AskedProject, AskedTask};
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
        open_by::<AskButton>(app)
    }

    fn open_by<B: Component>(app: &mut App) -> Entity {
        let button = one::<With<B>>(app);
        app.world_mut().trigger(Activate { entity: button });
        settle(app);
        one::<With<AskPanel>>(app)
    }

    fn has(app: &App, wanted: AskPart) -> bool {
        let mut parts = app.world().try_query::<&AskPart>().unwrap();
        parts.iter(app.world()).any(|p| *p == wanted)
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
    fn the_change_form_opens_on_the_app_shown_with_its_rows_apart_and_ask_waiting_for_words() {
        let mut app = windowed();
        let panel = open(&mut app);
        assert!(app.world().get::<ModalRoot>(panel).is_some());
        assert_eq!(app.world().get::<AskPanel>(panel).unwrap().title(), "Ask for a change");
        assert_eq!(app.world().resource::<AskForm>().about, About::Change(Some("NOTE".into())), "notes is shown");
        let about = part(&app, AskPart::About);
        let options = app.world().get::<trailhead_ui::SelectOptions>(about).unwrap().0.clone();
        assert_eq!(options, ["Atlas", "Notes"], "no new app here");

        let form = part(&app, AskPart::Form);
        assert_eq!(app.world().get::<Node>(form).unwrap().row_gap, px(space::ROWS), "the rows stand apart");
        let rows: Vec<Entity> = app.world().get::<Children>(form).unwrap().iter().collect();
        assert_eq!(rows.len(), 5, "the app, the ask, plan this first, the model and the notice");
        for row in &rows[..4] {
            assert_eq!(app.world().get::<Node>(*row).unwrap().row_gap, px(space::STACKED), "a label over its control");
        }
        let said =
            |app: &mut App, words: &str| app.world_mut().query::<&Text>().iter(app.world()).any(|t| t.0 == words);
        assert!(said(&mut app, HINT) && said(&mut app, PLAN) && said(&mut app, PLAN_NOTE));
        assert!(!has(&app, AskPart::Name), "no name for a change to an app");
        assert!(app.world().get::<bevy::ui::Checked>(part(&app, AskPart::Plan)).is_none(), "not planned at first");
        assert!(!shown(&app, part(&app, AskPart::ModelRow)), "so no model");
        assert!(!shown(&app, part(&app, AskPart::Done)));

        let (ask, cancel, done) = (act(&app, AskAct::Ask), act(&app, AskAct::Cancel), act(&app, AskAct::Done));
        assert_eq!((shown(&app, ask), shown(&app, cancel), shown(&app, done)), (true, true, false));
        assert!(!enabled(&app, ask) && enabled(&app, cancel));
        let says = notice(&app, part(&app, AskPart::Notice));
        assert_eq!(says, (Tone::Info, "Say what you want first.".into()));
    }

    fn plan(app: &mut App, on: bool) {
        let switch = part(app, AskPart::Plan);
        app.world_mut().trigger(ValueChange { source: switch, value: on, is_final: true });
        settle(app);
    }

    #[test]
    fn plan_this_first_shows_the_planners_model_in_place_and_off_keeps_what_was_typed() {
        let mut app = windowed();
        open(&mut app);
        type_into(&mut app, AskPart::Text, "A Completed view");
        let before = entities(&mut app);
        plan(&mut app, true);
        let row = part(&app, AskPart::ModelRow);
        assert!(app.world().resource::<AskForm>().plan && shown(&app, row));
        let model = part(&app, AskPart::Model);
        let options = app.world().get::<trailhead_ui::SelectOptions>(model).unwrap().0.clone();
        assert_eq!(options, ["The repo's default", "Fable", "Opus", "Sonnet", "Haiku"]);
        app.world_mut().trigger(Picked { entity: model, index: 2 });
        settle(&mut app);
        let asked = app.world().resource::<AskForm>().ask();
        assert_eq!(asked, Some(Ask::change("A Completed view", "NOTE").planned(Some("opus"))));

        plan(&mut app, false);
        assert!(!shown(&app, row));
        let form = app.world().resource::<AskForm>();
        assert_eq!((form.text.as_str(), form.model), ("A Completed view", Some("opus")), "nothing is cleared");
        assert_eq!(form.ask(), Some(Ask::change("A Completed view", "NOTE")), "but no model is sent");
        assert!(enabled(&app, act(&app, AskAct::Ask)));
        assert_eq!(entities(&mut app), before, "nothing was spawned or despawned");
    }

    #[test]
    fn the_plus_opens_a_new_app_always_planned_and_ask_waits_for_its_name() {
        let mut app = windowed();
        let panel = open_by::<NewAppButton>(&mut app);
        assert_eq!(app.world().get::<AskPanel>(panel).unwrap().title(), "Ask for a new app");
        assert_eq!(app.world().resource::<AskForm>().about, About::NewApp);
        assert!(!has(&app, AskPart::About) && !has(&app, AskPart::Plan), "no app to choose and no switch");
        let rows: Vec<Entity> = app.world().get::<Children>(part(&app, AskPart::Form)).unwrap().iter().collect();
        assert_eq!(rows.len(), 4, "the name, the ask, the model and the notice");
        assert!(shown(&app, part(&app, AskPart::ModelRow)), "a new app is always planned");
        let hint = app.world_mut().query::<&Text>().iter(app.world()).any(|t| t.0 == NEW_APP_HINT);
        assert!(hint);

        type_into(&mut app, AskPart::Text, "Notes on routes");
        let ask = act(&app, AskAct::Ask);
        assert!(!enabled(&app, ask));
        assert_eq!(notice(&app, part(&app, AskPart::Notice)).1, "Give the new app a name.");
        type_into(&mut app, AskPart::Name, "Trail Notes");
        assert!(enabled(&app, ask));
        assert_eq!(
            app.world().resource::<AskForm>().ask(),
            Some(Ask::new_app("Notes on routes", "Trail Notes").planned(None))
        );

        set_connection(&mut app, Connection::Offline);
        assert!(!enabled(&app, ask));
        assert!(notice(&app, part(&app, AskPart::Notice)).1.starts_with("Not connected"));
    }

    fn asked() -> Asked {
        Asked {
            project: AskedProject { key: "TN".into(), name: "Trail Notes".into(), created: true },
            task: Some(AskedTask { key: "TN-1".into(), title: "Trail Notes".into() }),
            planned: true,
            notes: vec!["There is no `factory` agent yet.".into()],
        }
    }

    #[test]
    fn asking_says_so_and_success_shows_what_was_made_in_place_and_done_closes() {
        let mut app = windowed();
        let before = entities(&mut app);
        let panel = open_by::<NewAppButton>(&mut app);
        type_into(&mut app, AskPart::Text, "Notes on routes");
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
        let lines = [AskPart::Asked, AskPart::Title, AskPart::Project, AskPart::Next].map(|p| part(&app, p));
        let said = lines.map(|l| words(&app, l));
        assert_eq!(
            said[..3],
            ["Sent to the planner: TN-1".to_string(), "Trail Notes".into(), "Made the project TN, Trail Notes".into()]
        );
        assert!(said[3].contains("on TN-1 in Lookout, as a question waiting on you"), "{}", said[3]);
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
