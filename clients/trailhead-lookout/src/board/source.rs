use super::*;

/// What the views of tasks build from: the mirror's rows, the factory's queues, and what says any of them changed.
#[derive(SystemParam)]
pub struct Sources<'w, 's> {
    mirror: Res<'w, Mirror>,
    changed: Query<'w, 's, (), Changed<Rev>>,
    removed: RemovedComponents<'w, 's, Rev>,
    projects: Query<'w, 's, &'static Mirrored<Project>>,
    tasks: Query<'w, 's, &'static Mirrored<Task>>,
    claims: Query<'w, 's, &'static Mirrored<Claim>>,
    dependencies: Query<'w, 's, &'static Mirrored<Dependency>>,
    comments: Query<'w, 's, &'static Mirrored<Comment>>,
    agents: Query<'w, 's, &'static Mirrored<Agent>>,
    users: Query<'w, 's, &'static Mirrored<User>>,
    authoring: Res<'w, Authoring>,
    factory: Option<Res<'w, FactoryStatus>>,
}

impl Sources<'_, '_> {
    /// Whether anything changed since the system using this last ran. It reads the removals, so call it on every run.
    pub fn changed(&mut self) -> bool {
        let gone = self.removed.read().count() > 0;
        gone || self.mirror.is_changed()
            || self.authoring.is_changed()
            || self.factory.as_ref().is_some_and(|f| f.is_changed())
            || !self.changed.is_empty()
    }

    /// The rows, copied out of the mirror for [`Rows::input`].
    pub fn rows(&self) -> Rows {
        Rows {
            projects: rows(&self.projects),
            tasks: rows(&self.tasks),
            claims: rows(&self.claims),
            dependencies: rows(&self.dependencies),
            comments: rows(&self.comments),
            agents: rows(&self.agents),
            users: rows(&self.users),
            queues: queue_keys(&self.authoring, self.factory.as_deref()),
        }
    }
}

/// The mirror's rows as [`Sources::rows`] copied them.
pub struct Rows {
    pub projects: Vec<Project>,
    pub tasks: Vec<Task>,
    pub claims: Vec<Claim>,
    pub dependencies: Vec<Dependency>,
    pub comments: Vec<Comment>,
    pub agents: Vec<Agent>,
    pub users: Vec<User>,
    pub queues: Vec<String>,
}

impl Rows {
    pub fn input<'a>(&'a self, deep: &'a [DeepHit], now: Timestamp) -> Input<'a> {
        Input {
            projects: &self.projects,
            tasks: &self.tasks,
            claims: &self.claims,
            dependencies: &self.dependencies,
            comments: &self.comments,
            agents: &self.agents,
            users: &self.users,
            queues: &self.queues,
            deep,
            now,
        }
    }
}

pub(super) fn rebuild_view(
    mut filter: ResMut<Filter>,
    mut sources: Sources,
    deep: Option<Res<Deep>>,
    mut view: ResMut<BoardView>,
    mut expiry: Local<Option<Timestamp>>,
) {
    let changed = sources.changed();
    let now = Timestamp::now();
    let expired = expiry.is_some_and(|at| at <= now);
    if !(changed || filter.is_changed() || deep.as_ref().is_some_and(|d| d.is_changed()) || expired) {
        return;
    }
    let rows = sources.rows();
    *expiry = next_expiry(&rows.claims, now);
    let deep = deep.as_ref().map_or(&[][..], |d| d.for_query(filter.query.trim()));
    let mut next = build(rows.input(deep, now), &filter);
    // A parent or holder no longer offered (another project picked, its subtasks gone, its claim over) would leave the
    // board empty while its dropdown says "all", so it is dropped.
    let unoffered =
        |key: FilterKey| filter.get(key).as_ref().is_some_and(|v| !next.options.of(key).iter().any(|c| &c.value == v));
    let stale: Vec<FilterKey> = [FilterKey::Parent, FilterKey::Holder].into_iter().filter(|k| unoffered(*k)).collect();
    if !stale.is_empty() {
        for key in stale {
            *filter.get_mut(key) = None;
        }
        next = build(rows.input(deep, now), &filter);
    }
    if *view != next {
        *view = next;
    }
}

/// The first moment a claim still live at `now` runs out, if any does.
pub fn next_expiry(claims: &[Claim], now: Timestamp) -> Option<Timestamp> {
    claims.iter().filter(|c| c.is_live(now)).filter_map(|c| c.expires_at).min()
}

/// The cards before and after the task `id` in its column, in the board's order (a subtask under its parent counts
/// where it sits). A task is in one column at most, since its status picks it.
pub fn neighbours(view: &BoardView, id: &str) -> Neighbours {
    for column in &view.columns {
        if let Some(i) = column.cards.iter().position(|c| c.id == id) {
            return Neighbours {
                previous: i.checked_sub(1).map(|p| column.cards[p].id.clone()),
                next: column.cards.get(i + 1).map(|c| c.id.clone()),
            };
        }
    }
    Neighbours::default()
}

/// Keeps [`Neighbours`] for the selected task as the selection and the board change.
pub(super) fn follow_neighbours(selection: Res<Selection>, view: Res<BoardView>, mut neighbours: ResMut<Neighbours>) {
    if !(selection.is_changed() || view.is_changed()) {
        return;
    }
    let next = match &selection.0 {
        Some(Subject::Task(id)) => self::neighbours(&view, id),
        _ => Neighbours::default(),
    };
    if *neighbours != next {
        *neighbours = next;
    }
}

/// The factory's queue task keys: what the control plane reports for its repos, or else the one parent Lookout knows.
pub(super) fn queue_keys(authoring: &Authoring, factory: Option<&FactoryStatus>) -> Vec<String> {
    let mut keys: Vec<String> =
        factory.into_iter().flat_map(|f| f.repos.values()).flat_map(|r| r.queue.clone()).collect();
    if keys.is_empty() {
        keys.push(authoring.queue.clone());
    }
    keys
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::testing::*;
    use super::*;
    use crate::fixture::{claim_json, sid, task_json};
    use crate::sync::{Snapshot, SyncItem};

    #[test]
    fn queue_keys_prefer_the_reported_queues_and_fall_back_to_the_authoring_parent() {
        use crate::factory::RepoState;
        let state = |queue: &[&str]| RepoState {
            control: trailhead_factory::status::ControlState::Up,
            queue: queue.iter().map(ToString::to_string).collect(),
            last_error: None,
            human_grades: None,
            holding: None,
            usage: Vec::new(),
        };
        let authoring = Authoring { queue: "TRAIL-1".into(), ..Default::default() };
        let mut factory = FactoryStatus::default();
        assert_eq!(queue_keys(&authoring, None), ["TRAIL-1"], "no factory: the authoring parent");
        assert_eq!(queue_keys(&authoring, Some(&factory)), ["TRAIL-1"], "no repos: the authoring parent");
        factory.repos.insert("o/a".into(), state(&[]));
        assert_eq!(queue_keys(&authoring, Some(&factory)), ["TRAIL-1"], "no reported queue: the authoring parent");
        factory.repos.insert("o/b".into(), state(&["TRAIL-102", "TRAIL-685"]));
        factory.repos.insert("o/c".into(), state(&["OPS-3"]));
        assert_eq!(queue_keys(&authoring, Some(&factory)), ["TRAIL-102", "TRAIL-685", "OPS-3"]);
    }

    #[test]
    fn an_expired_claim_is_not_a_holder() {
        let live = typed::<Claim>(vec![claim_json(json!({"task_id": "t103", "expires_at": "2026-10-16T00:00:00Z"}))]);
        let gone = typed::<Claim>(vec![claim_json(json!({"task_id": "t103", "expires_at": "2026-10-14T00:00:00Z"}))]);
        let (projects, tasks, _, comments, agents) = input_json();
        let (projects, tasks, comments, agents) =
            (typed::<Project>(projects), typed::<Task>(tasks), typed::<Comment>(comments), typed::<Agent>(agents));
        let holders = |claims: &[Claim], filter: &Filter| {
            let input = Input {
                projects: &projects,
                tasks: &tasks,
                claims,
                dependencies: &[],
                comments: &comments,
                agents: &agents,
                users: &[],
                queues: &[],
                deep: &[],
                now: at_now(),
            };
            build(input, filter)
        };
        assert_eq!(holders(&live, &Filter::default()).options.holders.len(), 1);
        let view = holders(&gone, &Filter::default());
        assert!(view.options.holders.is_empty(), "an expired claim isn't offered as a holder");
        let by_holder = Filter { holder: Some(sid("a1")), ..Default::default() };
        let view = holders(&gone, &by_holder);
        assert!(view.columns.iter().all(|c| c.cards.is_empty()), "nor does it match the filter");
        assert_eq!(next_expiry(&live, at_now()), Some("2026-10-16T00:00:00Z".parse().unwrap()));
        assert_eq!(next_expiry(&gone, at_now()), None);
    }

    #[test]
    fn neighbours_are_the_cards_either_side_in_the_column_and_none_past_its_ends() {
        let view = run(&Filter::default());
        let id = |key: &str| card(&view, key).id.clone();
        let around = |key: &str| {
            let n = neighbours(&view, &id(key));
            let key_of = |id: Option<String>| {
                id.map(|id| view.columns.iter().flat_map(|c| &c.cards).find(|c| c.id == id).unwrap().key.clone())
            };
            (key_of(n.previous), key_of(n.next))
        };
        // In progress is OPS-1, TRAIL-103, then TRAIL-104 nested under it.
        assert_eq!(around("OPS-1"), (None, Some("TRAIL-103".into())));
        assert_eq!(around("TRAIL-103"), (Some("OPS-1".into()), Some("TRAIL-104".into())));
        assert_eq!(around("TRAIL-104"), (Some("TRAIL-103".into()), None));
        // Never across into another column.
        assert_eq!(around("TRAIL-102"), (None, Some("TRAIL-105".into())));
        assert_eq!(neighbours(&view, "not on the board"), Neighbours::default());
    }

    #[test]
    fn picking_another_project_drops_a_parent_it_doesnt_have() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(BoardPlugin);
        let (projects, tasks, _, _, agents) = input_json();
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects,
            tasks,
            agents,
            ..Default::default()
        }));
        app.update();
        app.update();
        app.world_mut().resource_mut::<Filter>().parent = Some("TRAIL-102".into());
        app.update();
        let shown =
            |app: &App| app.world().resource::<BoardView>().columns.iter().map(|c| c.cards.len()).sum::<usize>();
        assert_eq!(shown(&app), 2);
        app.world_mut().resource_mut::<Filter>().project = Some("TRAIL".into());
        app.update();
        assert_eq!(app.world().resource::<Filter>().parent.as_deref(), Some("TRAIL-102"), "TRAIL has it");
        app.world_mut().resource_mut::<Filter>().project = Some("OPS".into());
        app.update();
        assert_eq!(app.world().resource::<Filter>().parent, None);
        assert_eq!(shown(&app), 1, "OPS-1 shows, not an empty board");
    }

    #[test]
    fn picking_another_project_drops_a_holder_with_nothing_there() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(BoardPlugin);
        let (projects, mut tasks, claims, _, agents) = input_json();
        // The view runs on the clock, so the claims last past it.
        for mut claim in claims {
            claim["expires_at"] = json!("2099-01-01T00:00:00Z");
            let task = tasks.iter_mut().find(|t| t["id"] == claim["task_id"]).unwrap();
            task["claim"] = claim;
        }
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 5,
            projects,
            tasks,
            agents,
            ..Default::default()
        }));
        app.update();
        app.update();
        app.world_mut().resource_mut::<Filter>().holder = Some(sid("a1"));
        app.update();
        app.world_mut().resource_mut::<Filter>().project = Some("TRAIL".into());
        app.update();
        assert_eq!(app.world().resource::<Filter>().holder, Some(sid("a1")), "a1 holds TRAIL-103");
        app.world_mut().resource_mut::<Filter>().project = Some("OPS".into());
        app.update();
        assert_eq!(app.world().resource::<Filter>().holder, None);
    }

    #[test]
    fn the_view_follows_the_mirror() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(crate::mirror::MirrorPlugin).add_plugins(BoardPlugin);
        let (projects, tasks, claims, comments, agents) = input_json();
        let snapshot = Snapshot { at: 5, projects, tasks, agents, ..Default::default() };
        let _ = (claims, comments);
        app.world_mut().write_message(SyncItem::Snapshot(snapshot));
        app.update();
        app.update();
        let view = app.world().resource::<BoardView>();
        assert_eq!(view.columns.iter().map(|c| c.cards.len()).sum::<usize>(), 5);

        app.world_mut().resource_mut::<Filter>().project = Some("OPS".into());
        app.update();
        let view = app.world().resource::<BoardView>();
        assert_eq!(view.columns.iter().map(|c| c.cards.len()).sum::<usize>(), 1);

        // An event moves the task to another column.
        app.world_mut().resource_mut::<Filter>().closed = true;
        app.world_mut().write_message(crate::fixture::sync_event(json!({
            "id": 6, "entity": "task", "entity_id": sid("t200"), "kind": "task.updated",
            "after": task_json(json!({"id": "t200", "key": "OPS-1", "project_id": "p2", "status": "done", "priority": 2, "version": 2})),
        })));
        app.update();
        app.update();
        let view = app.world().resource::<BoardView>();
        assert_eq!(keys(view, Status::Done), [("OPS-1".into(), 0)]);
    }
}
