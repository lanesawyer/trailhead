//! Lookout's pages in the shell's settings (`docs/factory/ui.md#settings`), listed under Lookout after the shell's
//! own: Factory, what the factory status already says, read-only; and Logs, the control plane's log ([`crate::logs`]).
//! The window's theme, text size and connection are the shell's pages, not Lookout's.
//!
//! Each page is spawned when it is shown, and its texts are set in place as what they show changes.

use bevy::prelude::*;
use trailhead_model::Task;
use trailhead_shell::mirror::{Mirrored, rows};
use trailhead_shell::settings::{OpenSettings, PageId, SettingsPage};

use crate::LOOKOUT;
use crate::author::Authoring;
use crate::factory::FactoryStatus;
use crate::logs::{LogFilter, LogsUiPlugin, logs_page};
use crate::widgets::{set_text, setting, size, text};

/// The Factory page.
pub const FACTORY_SETTINGS: PageId = PageId::new(LOOKOUT, "factory");
/// The Logs page.
pub const LOGS_SETTINGS: PageId = PageId::new(LOOKOUT, "logs");

/// Lookout's pages, in the order they are listed.
pub fn pages() -> Vec<SettingsPage> {
    vec![
        SettingsPage::new(FACTORY_SETTINGS.key, "Factory", factory_page),
        SettingsPage::new(LOGS_SETTINGS.key, "Logs", |In(page): In<Entity>, mut commands: Commands| {
            logs_page(&mut commands, page);
        }),
    ]
}

/// A text on the Factory page, and what it shows.
#[derive(Component, Clone, Default)]
pub struct Shows(pub Value);

/// What a text on the Factory page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Value {
    #[default]
    Status,
    Repos,
    Queue,
}

/// The repos the factory serves, one per line with the state of its control plane.
pub fn repos_text(status: &FactoryStatus) -> String {
    let lines: Vec<String> = status.controls().map(|(repo, state)| format!("{repo}: {state}")).collect();
    if lines.is_empty() { "none heard of yet".into() } else { lines.join("\n") }
}

/// Every queue task the factory works, one per line, or the one new tasks go under until a status has listed them.
pub fn queues_text(authoring: &Authoring, status: &FactoryStatus, tasks: &[Task]) -> String {
    let mut queues: Vec<String> = Vec::new();
    let pinned = authoring.pinned.then(|| authoring.queue.clone());
    for queue in pinned.into_iter().chain(status.queues()) {
        if !queues.contains(&queue) {
            queues.push(queue);
        }
    }
    if queues.is_empty() {
        queues.push(authoring.queue.clone());
    }
    let lines: Vec<String> = queues.iter().map(|q| queue_text(q, tasks)).collect();
    lines.join("\n")
}

/// One queue task, and how much is queued beneath it.
pub fn queue_text(queue: &str, tasks: &[Task]) -> String {
    let Some(task) = tasks.iter().find(|t| t.key == *queue) else {
        return format!("{queue} (not loaded)");
    };
    let queued: Vec<&Task> = tasks.iter().filter(|t| t.parent_id == Some(task.id)).collect();
    let open = queued.iter().filter(|t| !t.status.is_closed()).count();
    format!("{queue}: {open} not closed, {} in all", queued.len())
}

fn value(which: Value, label: &str) -> impl Scene {
    setting(label.to_string(), "", bsn_list! { @text("", size::BODY) Shows({ which }) })
}

fn factory_page(In(page): In<Entity>, mut commands: Commands) {
    commands.spawn_scene(value(Value::Status, "Status")).insert(ChildOf(page));
    commands.spawn_scene(value(Value::Repos, "Repos it serves")).insert(ChildOf(page));
    commands.spawn_scene(value(Value::Queue, "Queue tasks")).insert(ChildOf(page));
}

/// Show the settings' Logs page filtered to a task, given by key; sent from the inspector.
#[derive(Event, Debug, Clone)]
pub struct OpenLogs(pub String);

/// Filters the log to the task and opens the shell's settings at the Logs page, or turns to it.
fn on_open_logs(open: On<OpenLogs>, mut filter: ResMut<LogFilter>, mut commands: Commands) {
    filter.task = Some(open.0.clone());
    commands.trigger(OpenSettings { page: Some(LOGS_SETTINGS) });
}

/// Keeps the Factory page's texts current, in place, while it is shown.
fn refresh(
    mut texts: Query<(&Shows, &mut Text)>,
    status: Res<FactoryStatus>,
    authoring: Res<Authoring>,
    tasks: Query<&Mirrored<Task>>,
) {
    if texts.is_empty() {
        return;
    }
    let tasks = rows(&tasks);
    for (Shows(which), text) in &mut texts {
        let wanted = match which {
            Value::Status => status.label(),
            Value::Repos => repos_text(&status),
            Value::Queue => queues_text(&authoring, &status, &tasks),
        };
        set_text(text, &wanted);
    }
}

/// What Lookout's pages draw and do once the shell shows them.
pub struct SettingsUiPlugin;

impl Plugin for SettingsUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_open_logs).add_plugins(LogsUiPlugin).add_systems(Update, refresh);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trailhead_shell::ShellPlugin;
    use trailhead_shell::fixture::task_json;
    use trailhead_shell::settings::{SHELL_GROUP, SettingsPages, SettingsShown, ShownPage};
    use trailhead_shell::testing::{Other, headless, settle, windowed};
    use trailhead_ui::ModalPlugin;

    use super::*;
    use crate::Lookout;

    fn app() -> App {
        let mut app = windowed(ShellPlugin::new().with(Lookout::new()).with(Other));
        app.add_plugins(ModalPlugin);
        settle(&mut app);
        app
    }

    fn shown(app: &mut App) -> Vec<PageId> {
        let world = app.world_mut();
        world.query::<&ShownPage>().iter(world).map(|p| p.0).collect()
    }

    type Texts = Vec<(Entity, Value, String)>;

    fn texts(app: &mut App) -> Texts {
        let world = app.world_mut();
        world.query::<(Entity, &Shows, &Text)>().iter(world).map(|(e, v, t)| (e, v.0, t.0.clone())).collect()
    }

    fn of(list: &Texts, which: Value) -> String {
        list.iter().find(|(_, v, _)| *v == which).unwrap().2.clone()
    }

    #[test]
    fn lookouts_pages_are_listed_under_lookout_through_the_contract_alone() {
        let app = headless(ShellPlugin::new().with(Lookout::new()));
        let pages = app.world().resource::<SettingsPages>();
        let listed: Vec<(&str, PageId)> = pages.iter().map(|p| (p.group, p.id)).collect();
        let lookouts: Vec<_> = listed.iter().filter(|(_, id)| id.app == LOOKOUT).collect();
        assert_eq!(lookouts, [&("Lookout", FACTORY_SETTINGS), &("Lookout", LOGS_SETTINGS)]);
        assert!(listed.iter().all(|(group, id)| id.app == LOOKOUT || *group == SHELL_GROUP), "{listed:?}");
        assert!(listed.iter().position(|(_, id)| id.app == LOOKOUT) > Some(0), "after the shell's own");
    }

    #[test]
    fn opening_the_logs_of_a_task_shows_the_logs_page_filtered_to_it() {
        let mut app = app();
        app.world_mut().trigger(OpenLogs("TRAIL-7".into()));
        settle(&mut app);
        assert_eq!(shown(&mut app), [LOGS_SETTINGS]);
        assert_eq!(app.world().resource::<LogFilter>().task.as_deref(), Some("TRAIL-7"));
        let world = app.world_mut();
        assert_eq!(world.query::<&crate::logs::LogsPage>().iter(world).count(), 1);
    }

    #[test]
    fn opening_the_logs_while_settings_is_open_turns_to_the_logs_page() {
        let mut app = app();
        app.world_mut().trigger(OpenSettings { page: Some(FACTORY_SETTINGS) });
        settle(&mut app);
        assert_eq!(shown(&mut app), [FACTORY_SETTINGS]);
        app.world_mut().trigger(OpenLogs("TRAIL-9".into()));
        settle(&mut app);
        assert_eq!(shown(&mut app), [LOGS_SETTINGS]);
        assert_eq!(app.world().resource::<LogFilter>().task.as_deref(), Some("TRAIL-9"));
        assert!(app.world().resource::<SettingsShown>().page().is_some());
    }

    #[test]
    fn the_factory_page_says_what_lookout_knows_and_updates_in_place() {
        let mut app = app();
        app.world_mut().trigger(OpenSettings { page: Some(FACTORY_SETTINGS) });
        settle(&mut app);
        let before = texts(&mut app);
        assert_eq!(of(&before, Value::Status), "factory: connecting");
        assert_eq!(of(&before, Value::Repos), "none heard of yet");
        assert_eq!(of(&before, Value::Queue), "TRAIL-102 (not loaded)");

        app.world_mut().resource_mut::<Authoring>().queue = "TRAIL-685".into();
        settle(&mut app);
        let after = texts(&mut app);
        assert_eq!(of(&after, Value::Queue), "TRAIL-685 (not loaded)");
        let entities = |list: &Texts| list.iter().map(|(e, ..)| *e).collect::<Vec<_>>();
        assert_eq!(entities(&before), entities(&after));
    }

    #[test]
    fn the_queue_line_counts_what_is_under_it() {
        let task = |id: &str, key: &str, status: &str, parent: Option<&str>| -> Task {
            serde_json::from_value(task_json(json!({"id": id, "key": key, "status": status, "parent_id": parent})))
                .unwrap()
        };
        let tasks = [
            task("t1", "TRAIL-102", "open", None),
            task("t2", "TRAIL-103", "open", Some("t1")),
            task("t3", "TRAIL-104", "done", Some("t1")),
            task("t4", "TRAIL-9", "open", None),
        ];
        assert_eq!(queue_text("TRAIL-102", &tasks), "TRAIL-102: 1 not closed, 2 in all");
        assert_eq!(queue_text("TRAIL-5", &tasks), "TRAIL-5 (not loaded)");

        let authoring = Authoring::default();
        let mut status = FactoryStatus::default();
        assert_eq!(queues_text(&authoring, &status, &tasks), "TRAIL-102: 1 not closed, 2 in all");
        status.repos.insert(
            "r".into(),
            crate::factory::RepoState {
                control: trailhead_factory::status::ControlState::Up,
                queue: vec!["TRAIL-102".into(), "TRAIL-9".into()],
                last_error: None,
                human_grades: None,
                holding: None,
                usage: Vec::new(),
            },
        );
        assert_eq!(
            queues_text(&authoring, &status, &tasks),
            "TRAIL-102: 1 not closed, 2 in all\nTRAIL-9: 0 not closed, 0 in all"
        );
        let pinned = Authoring { queue: "TRAIL-9".into(), pinned: true, ..Authoring::default() };
        assert_eq!(
            queues_text(&pinned, &status, &tasks),
            "TRAIL-9: 0 not closed, 0 in all\nTRAIL-102: 1 not closed, 2 in all"
        );
        let pinned = Authoring { queue: "TRAIL-7".into(), pinned: true, ..Authoring::default() };
        assert!(queues_text(&pinned, &status, &tasks).starts_with("TRAIL-7 (not loaded)\nTRAIL-102"));
    }
}
