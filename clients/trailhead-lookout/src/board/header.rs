use super::*;

/// Tags a header dropdown with the filter it sets.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FilterSelect(pub FilterKey);

pub(super) fn on_pick(
    pick: On<Picked>,
    selects: Query<&FilterSelect>,
    view: Res<BoardView>,
    mut filter: ResMut<Filter>,
) {
    let Ok(&FilterSelect(key)) = selects.get(pick.entity) else { return };
    let mut next = filter.clone();
    choose(&mut next, key, view.options.of(key), pick.index);
    filter.set_if_neq(next);
}

/// Tags the header button that shows or hides the closed columns.
#[derive(Component)]
pub(super) struct ClosedToggle;

/// Tags the header button that limits the board to the factory's tasks.
#[derive(Component)]
pub(super) struct FactoryToggle;

pub(super) fn on_toggle_factory(
    activate: On<Activate>,
    toggles: Query<(), With<FactoryToggle>>,
    mut filter: ResMut<Filter>,
) {
    if toggles.contains(activate.entity) {
        filter.factory = !filter.factory;
    }
}

pub(super) fn on_toggle_closed(
    activate: On<Activate>,
    toggles: Query<(), With<ClosedToggle>>,
    mut filter: ResMut<Filter>,
) {
    if toggles.contains(activate.entity) {
        filter.closed = !filter.closed;
    }
}

/// The header bar, spawned with the panel; its widgets are spawned once and updated in place.
#[derive(Component)]
pub(super) struct HeaderBar;

/// The names a filter dropdown lists, and which is chosen. A chosen value that is no longer offered shows as "all".
pub(super) fn select_state(key: FilterKey, filter: &Filter, options: &Options) -> (Vec<String>, Option<usize>) {
    let choices = options.of(key);
    let mut names = vec!["all".to_string()];
    names.extend(choices.iter().map(|c| c.label.clone()));
    let at = filter.get(key).as_ref().and_then(|v| choices.iter().position(|c| &c.value == v)).map_or(0, |i| i + 1);
    (names, Some(at))
}

pub(super) fn factory_caption(filter: &Filter) -> String {
    format!("Factory only: {}", if filter.factory { "on" } else { "off" })
}

pub(super) fn closed_caption(filter: &Filter) -> String {
    let closed = Status::ALL.into_iter().filter(|s| s.is_closed()).map(status_label).collect::<Vec<_>>().join(" & ");
    format!("{closed}: {}", if filter.closed { "shown" } else { "hidden" })
}

/// Spawns the header bar's widgets once, when the bar appears: a dropdown per filter and the toggles.
/// [`update_header`] changes them in place afterwards.
pub(super) fn dress_header(
    bar: Query<Entity, Added<HeaderBar>>,
    view: Res<BoardView>,
    filter: Res<Filter>,
    mut commands: Commands,
) {
    for bar in &bar {
        for key in FilterKey::ALL {
            let (names, at) = select_state(key, &filter, &view.options);
            let control = bsn_list! { @select(names, at, 8) FilterSelect({ key }) };
            commands.queue_spawn_scene(field(key.name(), control)).insert(ChildOf(bar));
        }
        commands
            .queue_spawn_scene(button(closed_caption(&filter), None, ButtonKind::Normal))
            .insert((ClosedToggle, ChildOf(bar)));
        commands
            .queue_spawn_scene(button(factory_caption(&filter), None, ButtonKind::Normal))
            .insert((FactoryToggle, ChildOf(bar)));
    }
}

/// What the header was last updated from, so it is touched only when one of these changes.
#[derive(Clone, PartialEq)]
pub(super) struct HeaderDrawn {
    filter: Filter,
    options: Options,
}

/// Brings the header's widgets up to date in place: each select's rows and chosen row, and the toggles' captions. Nothing is despawned, so a dropdown just used stays the same entity. A
/// select whose popup is open is left alone and tried again once it has closed.
#[allow(clippy::too_many_arguments)]
pub(super) fn update_header(
    view: Res<BoardView>,
    filter: Res<Filter>,
    selects: Query<(Entity, &FilterSelect)>,
    toggle: Single<Entity, With<ClosedToggle>>,
    factory_toggle: Single<Entity, With<FactoryToggle>>,
    children: Query<&Children>,
    mut texts: Query<&mut Text>,
    mut edit: SelectEdit,
    mut drawn: Local<Option<HeaderDrawn>>,
) {
    let now = HeaderDrawn { filter: filter.clone(), options: view.options.clone() };
    if drawn.as_ref() == Some(&now) {
        return;
    }
    let mut settled = true;
    for (select, FilterSelect(key)) in &selects {
        let (names, at) = select_state(*key, &filter, &view.options);
        settled &= edit.set(select, &names, at);
    }
    for (toggle, caption) in [(*toggle, closed_caption(&filter)), (*factory_toggle, factory_caption(&filter))] {
        for e in children.iter_descendants(toggle) {
            if let Ok(mut text) = texts.get_mut(e)
                && text.0 != caption
            {
                text.0 = caption.clone();
            }
        }
    }
    // A select that was open is looked at again next frame.
    *drawn = settled.then_some(now);
}

#[cfg(test)]
mod tests {

    use super::super::testing::*;
    use super::*;

    #[test]
    fn picking_a_dropdown_option_sets_the_filter() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(BoardPlugin);
        *app.world_mut().resource_mut::<BoardView>() = run(&Filter::default());
        let parent = app.world_mut().spawn(FilterSelect(FilterKey::Parent)).id();
        app.world_mut().trigger(Picked { entity: parent, index: 2 });
        assert_eq!(app.world().resource::<Filter>().parent.as_deref(), Some("TRAIL-103"));
        app.world_mut().trigger(Picked { entity: parent, index: 0 });
        assert_eq!(app.world().resource::<Filter>().parent, None);
    }

    #[test]
    fn the_closed_toggle_shows_and_hides_done_and_cancelled() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(BoardPlugin);
        let toggle = app.world_mut().spawn(ClosedToggle).id();
        let closed_columns = |app: &App| {
            let columns = run(app.world().resource::<Filter>()).columns;
            columns.into_iter().map(|c| c.status).filter(|s| s.is_closed()).collect::<Vec<_>>()
        };
        assert!(closed_columns(&app).is_empty());
        app.world_mut().trigger(Activate { entity: toggle });
        assert_eq!(closed_columns(&app), [Status::Done, Status::Cancelled]);
        app.world_mut().trigger(Activate { entity: toggle });
        assert!(closed_columns(&app).is_empty());
    }
}

#[cfg(test)]
mod header_tests {
    use bevy::ecs::system::RunSystemOnce;
    use bevy::state::app::StatesPlugin;

    use super::super::testing::run;
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin, AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_resource::<Filter>()
            .insert_resource(run(&Filter::default()))
            .add_systems(Update, (dress_header, update_header).chain());
        app.world_mut().spawn(HeaderBar);
        settle(&mut app);
        app
    }

    fn settle(app: &mut App) {
        for _ in 0..4 {
            app.update();
        }
    }

    /// Every entity under the header bar, in order.
    fn header(app: &mut App) -> Vec<Entity> {
        let world = app.world_mut();
        let bar = world.query_filtered::<Entity, With<HeaderBar>>().single(world).unwrap();
        let mut found = vec![bar];
        let mut at = 0;
        while at < found.len() {
            if let Some(children) = world.get::<Children>(found[at]) {
                found.extend(children.iter());
            }
            at += 1;
        }
        found
    }

    fn one<C: Component>(app: &mut App) -> Entity {
        let world = app.world_mut();
        world.query_filtered::<Entity, With<C>>().single(world).unwrap()
    }

    #[test]
    fn the_header_has_its_widgets_once() {
        let mut app = app();
        let mut q = app.world_mut().query::<&FilterSelect>();
        assert_eq!(q.iter(app.world()).count(), FilterKey::ALL.len());
        one::<ClosedToggle>(&mut app);
        one::<FactoryToggle>(&mut app);
    }

    #[test]
    fn picking_a_filter_leaves_every_widget_of_the_header_as_it_was() {
        let mut app = app();
        let before = header(&mut app);
        app.world_mut().resource_mut::<Filter>().project = Some("OPS".into());
        settle(&mut app);
        assert_eq!(header(&mut app), before, "the same entities, the selects and toggles among them");
        // The select shows the pick: its row is marked selected.
        let mut q = app.world_mut().query::<(&FilterSelect, Entity)>();
        let project = q.iter(app.world()).find(|(s, _)| s.0 == FilterKey::Project).unwrap().1;
        let chosen = app
            .world_mut()
            .run_system_once(move |edit: crate::widgets::SelectEdit| edit.selected_rows(project))
            .unwrap();
        assert_eq!(chosen.len(), 1);
        assert_eq!(
            chosen[0],
            1 + run(&Filter::default())
                .options
                .of(FilterKey::Project)
                .iter()
                .position(|c| c.value == "p2")
                .unwrap_or(0)
        );
    }

    fn is_under(world: &World, mut e: Entity, ancestor: Entity) -> bool {
        while let Some(parent) = world.get::<ChildOf>(e) {
            e = parent.parent();
            if e == ancestor {
                return true;
            }
        }
        false
    }

    #[test]
    fn toggling_closed_leaves_the_header_as_it_was() {
        let mut app = app();
        let before = header(&mut app);
        app.world_mut().resource_mut::<Filter>().closed = true;
        settle(&mut app);
        assert_eq!(header(&mut app), before);
    }

    #[test]
    fn new_options_rebuild_only_that_selects_rows() {
        let mut app = app();
        let before = header(&mut app);
        let mut q = app.world_mut().query::<(Entity, &FilterSelect)>();
        let project = q.iter(app.world()).find(|(_, s)| s.0 == FilterKey::Project).unwrap().0;
        let mine: Vec<Entity> = app
            .world()
            .entity(project)
            .get::<Children>()
            .map_or(vec![], |_| before.iter().copied().filter(|e| is_under(app.world(), *e, project)).collect());
        app.world_mut().resource_mut::<BoardView>().options.projects.pop();
        settle(&mut app);
        let after = header(&mut app);
        let gone: Vec<_> = before.iter().filter(|e| !after.contains(e)).collect();
        assert!(!gone.is_empty(), "the select's old rows are replaced");
        for e in gone {
            assert!(mine.contains(e), "only rows of the select that changed");
        }
        assert!(after.contains(&project) && after.contains(&one::<ClosedToggle>(&mut app)));
    }
}
