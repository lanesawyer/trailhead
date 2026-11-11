use super::*;
/// The node the columns sit in, which scrolls sideways. The panel spawns it; [`BoardPlugin`] keeps its children.

#[derive(Component)]
pub struct BoardColumns;

/// A column's shell, one entity per status for as long as the status is on the board.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct BoardColumn {
    pub status: Status,
    /// How many cards the column holds, shown or not.
    pub count: usize,
}

/// The scrolling body of the column for this status. It is a grandchild of its [`BoardColumn`]: the body and its
/// scrollbar share a frame, which sits under the shell after the header.
#[derive(Component)]
pub struct ColumnBody(pub Status);

/// A card, one entity per task for as long as the task is on the board. It carries the view model it was drawn from,
/// so it is redrawn when, and only when, that changes.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct BoardCard(pub Card);

/// The "Nothing here" line of a column without cards.
#[derive(Component)]
pub struct ColumnEmpty;

/// The "Show more" button at the foot of a column with cards past its page. `next` is how many one press adds.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ColumnMore {
    pub status: Status,
    pub next: usize,
}

/// What sits in a column's body besides the keys of its cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Slot {
    Card(Box<Card>),
    Empty,
    More(ColumnMore),
}

/// The entities the board has made, so a change touches only what it changes.
#[derive(Resource, Default)]
pub(super) struct Tree {
    /// The [`BoardColumns`] node these belong to; a new one (the panel was rebuilt) starts them over.
    root: Option<Entity>,
    columns: Keyed<BoardColumn>,
    bodies: HashMap<Status, Entity>,
    slots: Keyed<Slot>,
}

/// How many cards each column has been asked to build, by status; a column not in it builds [`PAGE`].
#[derive(Resource, Default)]
pub(super) struct Shown(HashMap<Status, usize>);

impl Shown {
    pub(super) fn limit(&self, status: Status) -> usize {
        self.0.get(&status).copied().unwrap_or(PAGE)
    }

    pub(super) fn more(&mut self, status: Status) {
        let next = self.limit(status) + PAGE;
        self.0.insert(status, next);
    }
}

/// Brings the columns and cards under [`BoardColumns`] to [`BoardView`], touching only what changed: a card that
/// left is despawned, a new one spawned, one that moved is re-parented, and one whose view model changed is given the
/// new model. Everything else keeps its entity, so nothing flashes and the scroll positions stay put.
pub(super) fn sync_tree(
    view: Res<BoardView>,
    shown: Res<Shown>,
    root: Query<Entity, With<BoardColumns>>,
    mut tree: ResMut<Tree>,
    mut commands: Commands,
) {
    let root = root.single().ok();
    let Tree { root: drawn, columns, bodies, slots } = &mut *tree;
    if *drawn != root {
        // The old nodes went with their panel.
        *drawn = root;
        columns.forget();
        slots.forget();
        bodies.clear();
    } else if !(view.is_changed() || shown.is_changed()) {
        return;
    }
    let Some(root) = root else { return };

    let wanted = [(
        root,
        view.columns
            .iter()
            .map(|c| (c.status.as_str().to_string(), BoardColumn { status: c.status, count: c.cards.len() }))
            .collect(),
    )];
    columns.sync(
        &mut commands,
        &wanted,
        |commands, _, model| {
            let shell = commands.spawn(model.clone()).id();
            let body = commands.spawn((ColumnBody(model.status), ChildOf(shell))).id();
            bodies.insert(model.status, body);
            shell
        },
        |commands, shell, model| {
            commands.entity(shell).insert(model.clone());
        },
    );

    let wanted: Vec<(Entity, Vec<(String, Slot)>)> = view
        .columns
        .iter()
        .filter_map(|column| {
            let limit = shown.limit(column.status);
            let (cards, hidden) = visible(&column.cards, limit);
            let mut items = Vec::new();
            if column.cards.is_empty() {
                items.push((format!("empty/{}", column.status), Slot::Empty));
            }
            items.extend(cards.iter().map(|c| (c.id.clone(), Slot::Card(Box::new(c.clone())))));
            if hidden > 0 {
                let next = next_page(&column.cards, limit);
                let more = ColumnMore { status: column.status, next };
                items.push((format!("more/{}", column.status), Slot::More(more)));
            }
            Some((*bodies.get(&column.status)?, items))
        })
        .collect();
    slots.sync(
        &mut commands,
        &wanted,
        |commands, _, slot| match slot {
            Slot::Card(card) => commands.spawn(BoardCard((**card).clone())).id(),
            Slot::Empty => commands.spawn(ColumnEmpty).id(),
            Slot::More(more) => commands.spawn(more.clone()).id(),
        },
        |commands, entity, slot| match slot {
            Slot::Card(card) => {
                commands.entity(entity).insert(BoardCard((**card).clone()));
            }
            Slot::More(more) => {
                commands.entity(entity).insert(more.clone());
            }
            Slot::Empty => {}
        },
    );
}

#[cfg(test)]
mod in_place_tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::fixture::{project_json, sid, task_json};
    use crate::mirror::MirrorPlugin;
    use crate::sync::{Snapshot, SyncItem};

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins((MirrorPlugin, BoardPlugin));
        app.world_mut().spawn(BoardColumns);
        let task = |id: &str, status: &str| {
            task_json(
                json!({"id": id, "key": id.to_uppercase(), "title": id, "project_id": "p1", "status": status, "priority": 2}),
            )
        };
        let tasks = vec![task("t1", "open"), task("t2", "open"), task("t3", "in_progress"), task("t4", "in_progress")];
        let projects =
            vec![project_json(json!({"id": "p1", "key": "TRAIL"})), project_json(json!({"id": "p2", "key": "OPS"}))];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 1,
            projects,
            tasks,
            agents: vec![],
            ..Default::default()
        }));
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..3 {
            app.update();
        }
    }

    fn update_task(app: &mut App, id: &str, status: &str) {
        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 9, "entity": "task", "entity_id": sid(id), "kind": "task.updated",
            "after": task_json(json!({"id": id, "key": id.to_uppercase(), "title": id, "project_id": "p1", "status": status,
                      "priority": 2, "version": 2})),
        })));
        settle(app);
    }

    fn cards(app: &mut App) -> HashMap<String, Entity> {
        let mut q = app.world_mut().query::<(Entity, &BoardCard)>();
        // By the task's short name: these tasks' keys are their names in capitals.
        q.iter(app.world()).map(|(e, c)| (c.0.key.to_lowercase(), e)).collect()
    }

    fn column_of(app: &mut App, card: Entity) -> Status {
        let body = app.world().get::<ChildOf>(card).expect("a card has a column").parent();
        app.world().get::<ColumnBody>(body).expect("in a column body").0
    }

    fn count_cards(app: &mut App) -> usize {
        let mut q = app.world_mut().query::<&BoardCard>();
        q.iter(app.world()).count()
    }

    #[test]
    fn a_dressed_column_is_a_header_and_a_frame_holding_the_body_and_scrollbar() {
        let mut app = app();
        // The plain test app never dresses columns. Run that system alone, on bodies it sees as new.
        app.add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_resource::<BoardScroll>()
            .add_systems(Update, dress_columns);
        let bodies: Vec<(Entity, Status)> =
            app.world_mut().query::<(Entity, &ColumnBody)>().iter(app.world()).map(|(e, b)| (e, b.0)).collect();
        for (id, status) in bodies {
            app.world_mut().entity_mut(id).remove::<ColumnBody>().insert(ColumnBody(status));
        }
        settle(&mut app);
        let mut q = app.world_mut().query::<(Entity, &BoardColumn)>();
        let shells: Vec<Entity> = q.iter(app.world()).map(|(e, _)| e).collect();
        assert!(!shells.is_empty());
        for shell in shells {
            let world = app.world();
            let kids: Vec<Entity> = world.get::<Children>(shell).expect("a dressed shell has children").to_vec();
            assert_eq!(
                kids.len(),
                2,
                "shell holds the header and the frame: {:?}",
                kids.iter().map(|k| world.get::<Name>(*k).map(Name::to_string)).collect::<Vec<_>>()
            );
            let (header, frame) = (kids[0], kids[1]);
            assert!(world.get::<ColumnBody>(header).is_none());
            assert_eq!(world.get::<Name>(frame).map(Name::as_str), Some("column frame"));
            let inner: Vec<Entity> = world.get::<Children>(frame).expect("the frame has children").to_vec();
            assert_eq!(inner.len(), 2, "frame holds the body and the scrollbar");
            assert!(world.get::<ColumnBody>(inner[0]).is_some(), "the body comes first");
            assert!(world.get::<ColumnBody>(inner[1]).is_none(), "the scrollbar comes second");
            assert_eq!(world.get::<ChildOf>(inner[0]).map(ChildOf::parent), Some(frame));
        }
    }

    #[test]
    fn changing_one_task_leaves_every_other_card_as_it_was() {
        let mut app = app();
        let before = cards(&mut app);
        assert_eq!(before.len(), 4);
        let mut q = app.world_mut().query::<(Entity, &BoardColumn)>();
        let columns: Vec<Entity> = q.iter(app.world()).map(|(e, _)| e).collect();

        app.world_mut().write_message(SyncItem::Event(json!({
            "id": 9, "entity": "task", "entity_id": sid("t1"), "kind": "task.updated",
            "after": task_json(json!({"id": "t1", "key": "T1", "title": "renamed", "project_id": "p1", "status": "open",
                      "priority": 2, "version": 2})),
        })));
        settle(&mut app);
        let after = cards(&mut app);
        assert_eq!(before, after, "the same entities, one of them redrawn");
        let renamed = app.world().get::<BoardCard>(after["t1"]).unwrap();
        assert_eq!(renamed.0.title, "renamed");
        let mut q = app.world_mut().query::<(Entity, &BoardColumn)>();
        assert_eq!(columns, q.iter(app.world()).map(|(e, _)| e).collect::<Vec<_>>());
    }

    #[test]
    fn a_card_that_changes_column_keeps_its_entity_and_leaves_no_copy() {
        let mut app = app();
        let before = cards(&mut app);
        assert_eq!(column_of(&mut app, before["t1"]), Status::Open);

        update_task(&mut app, "t1", "in_progress");
        let after = cards(&mut app);
        assert_eq!(after, before);
        assert_eq!(count_cards(&mut app), 4, "no stale copy in the old column");
        assert_eq!(column_of(&mut app, after["t1"]), Status::InProgress);
        // The columns' children are what the view says.
        let mut q = app.world_mut().query::<(&ColumnBody, &Children)>();
        let held: HashMap<Status, usize> = q.iter(app.world()).map(|(b, c)| (b.0, c.len())).collect();
        assert_eq!((held[&Status::Open], held[&Status::InProgress]), (1, 3));
    }

    /// Every card sits in a column's body, and there is one card per task shown.
    fn assert_placed(app: &mut App, step: &str) {
        let world = app.world_mut();
        let mut q = world.query::<(Entity, &BoardCard, Option<&ChildOf>)>();
        let cards: Vec<(Entity, String, Option<Entity>)> =
            q.iter(world).map(|(e, c, p)| (e, c.0.key.clone(), p.map(ChildOf::parent))).collect();
        for (e, key, parent) in &cards {
            let in_body = parent.is_some_and(|p| world.get::<ColumnBody>(p).is_some());
            assert!(in_body, "{step}: card {key} ({e}) is not in a column (parent {parent:?})");
        }
        let mut keys: Vec<&String> = cards.iter().map(|(_, k, _)| k).collect();
        keys.sort();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n, "{step}: two cards for one task: {cards:?}");
    }

    #[test]
    fn cards_stay_in_their_columns_whichever_way_they_move() {
        // A card moving back to an earlier column was left without a parent, drawn at the window's top left
        // (TRAIL-1009).
        let mut app = app();
        let mut n = 100;
        let mut version = 10;
        let mut move_to = |app: &mut App, id: &str, status: &str, title: &str, settle_after: bool| {
            n += 1;
            version += 1;
            app.world_mut().write_message(SyncItem::Event(json!({
                "id": n, "entity": "task", "entity_id": sid(id), "kind": "task.updated",
                "after": task_json(json!({"id": id, "key": id.to_uppercase(), "title": title, "project_id": "p1",
                          "status": status, "priority": 2, "version": version})),
            })));
            if settle_after {
                settle(app);
            }
        };
        let steps: &[(&str, &str)] = &[
            ("t3", "in_review"),
            ("t3", "in_progress"),
            ("t3", "open"),
            ("t1", "needs_input"),
            ("t1", "open"),
            ("t4", "in_review"),
            ("t2", "in_review"),
            ("t4", "open"),
            ("t2", "in_progress"),
            ("t2", "done"),
            ("t2", "open"),
            ("t1", "in_review"),
            ("t1", "in_progress"),
        ];
        for (i, (id, status)) in steps.iter().enumerate() {
            move_to(&mut app, id, status, &format!("{id} v{i}"), true);
            assert_placed(&mut app, &format!("step {i} {id}->{status}"));
        }
        // Several in one frame, back and forth.
        move_to(&mut app, "t3", "in_review", "a", false);
        move_to(&mut app, "t4", "needs_input", "b", false);
        move_to(&mut app, "t3", "open", "c", false);
        settle(&mut app);
        assert_placed(&mut app, "burst");
        // Closed columns shown, a card moved into one, hidden again.
        app.world_mut().resource_mut::<Filter>().closed = true;
        settle(&mut app);
        move_to(&mut app, "t1", "cancelled", "d", true);
        assert_placed(&mut app, "into cancelled");
        app.world_mut().resource_mut::<Filter>().closed = false;
        settle(&mut app);
        assert_placed(&mut app, "closed hidden");
        move_to(&mut app, "t1", "in_progress", "e", true);
        assert_placed(&mut app, "out of cancelled");
        // One frame at a time, with a move every frame.
        for (i, status) in ["in_review", "open", "needs_input", "in_progress", "in_review"].iter().enumerate() {
            move_to(&mut app, "t2", status, &format!("f{i}"), false);
            app.update();
            assert_placed(&mut app, &format!("frame {i} t2->{status}"));
        }
    }

    #[test]
    fn a_filter_change_despawns_only_the_cards_it_hides() {
        let mut app = app();
        let before = cards(&mut app);
        // Give two of the tasks to the other project.
        for (n, id, status) in [(9, "t2", "open"), (10, "t4", "in_progress")] {
            let after = task_json(json!({"id": id, "key": id.to_uppercase(), "title": id, "project_id": "p2",
                                         "status": status, "priority": 2, "version": 2}));
            app.world_mut().write_message(SyncItem::Event(json!({
                "id": n, "entity": "task", "entity_id": sid(id), "kind": "task.updated", "after": after,
            })));
        }
        settle(&mut app);
        assert_eq!(cards(&mut app), before);

        app.world_mut().resource_mut::<Filter>().project = Some("TRAIL".into());
        settle(&mut app);
        let after = cards(&mut app);
        let mut ids: Vec<_> = after.keys().cloned().collect();
        ids.sort();
        assert_eq!(ids, ["t1", "t3"]);
        for id in ids {
            assert_eq!(after[&id], before[&id]);
        }
        for id in ["t2", "t4"] {
            assert!(app.world().get_entity(before[id]).is_err(), "{id} is despawned");
        }
    }

    #[test]
    fn closed_columns_come_and_go_without_touching_the_open_ones() {
        let mut app = app();
        let before = cards(&mut app);
        update_task(&mut app, "t1", "done");
        // Done is hidden: its card is gone, the rest are untouched.
        let hidden = cards(&mut app);
        assert_eq!(hidden.len(), 3);
        assert_eq!(hidden["t2"], before["t2"]);

        app.world_mut().resource_mut::<Filter>().closed = true;
        settle(&mut app);
        let shown = cards(&mut app);
        assert_eq!(shown.len(), 4);
        assert_eq!(column_of(&mut app, shown["t1"]), Status::Done);
        for id in ["t2", "t3", "t4"] {
            assert_eq!(shown[id], before[id]);
        }
    }

    #[test]
    fn showing_more_adds_cards_and_keeps_the_ones_shown() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins((MirrorPlugin, BoardPlugin));
        app.world_mut().spawn(BoardColumns);
        let tasks: Vec<Value> = (0..PAGE + 5)
            .map(|i| {
                task_json(json!({"id": format!("t{i}"), "key": format!("TRAIL-{i}"), "title": "", "project_id": "p1",
                            "status": "open", "priority": 2}))
            })
            .collect();
        let projects = vec![project_json(json!({"id": "p1", "key": "TRAIL"}))];
        app.world_mut().write_message(SyncItem::Snapshot(Snapshot {
            at: 1,
            projects,
            tasks,
            agents: vec![],
            ..Default::default()
        }));
        settle(&mut app);
        let before = cards(&mut app);
        assert_eq!(before.len(), PAGE);
        let mut q = app.world_mut().query::<&ColumnMore>();
        assert_eq!(q.single(app.world()).unwrap().next, 5);

        app.world_mut().resource_mut::<Shown>().more(Status::Open);
        settle(&mut app);
        let after = cards(&mut app);
        assert_eq!(after.len(), PAGE + 5);
        for (id, e) in &before {
            assert_eq!(after[id], *e);
        }
        let mut q = app.world_mut().query::<&ColumnMore>();
        assert_eq!(q.iter(app.world()).count(), 0, "nothing left to show");
    }
}
