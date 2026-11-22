//! A list of short values a person adds to and takes from: directories, names. Each value is on a line of its own,
//! cut to the room it has, with a button that removes it; under them a field and Add, which adds what is typed (Enter
//! in the field does too). With no values a dim line says what an empty list means.
//!
//! The list doesn't change itself: an add or a remove triggers [`ValuesEdited`] on it with the values as they would
//! be, and the caller, who owns them, sets [`ValueList`] to what it keeps. The kit then brings the lines in step in
//! place. An add clears the field; an empty or repeated value adds nothing and stays in the field.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui_widgets::Activate;

use crate::buttons::{ButtonKind, button, tool_button};
use crate::containers::row;
use crate::icons::Icon;
use crate::input::{TextEntered, text_input};
use crate::keyed::Keyed;
use crate::patch::{display, set_display};
use crate::spacing::space;
use crate::text::{size, text_dim, text_truncated};
use crate::truncate::Truncated;

/// The values a list shows, on its root. Set it (with `set_if_neq`) to what the caller keeps.
#[derive(Component, Clone, Default, PartialEq, Eq, Debug)]
pub struct ValueList {
    pub values: Vec<String>,
}

/// An add or a remove asked for in a list, triggered on its root with the values as they would be after it.
#[derive(EntityEvent, Clone, Debug, PartialEq, Eq)]
pub struct ValuesEdited {
    pub entity: Entity,
    pub values: Vec<String>,
}

/// The box holding a list's lines.
#[derive(Component, Clone, Default)]
pub struct ValueLines;

/// One line of a list: its place and its value.
#[derive(Component, Clone, Default, PartialEq, Eq, Debug)]
pub struct ValueLine {
    pub index: usize,
    pub value: String,
}

/// The dim line shown while a list is empty.
#[derive(Component, Clone, Default)]
pub struct ValueListEmpty;

/// A list's field, where a value to add is typed: its text entity, with the `EditableText`.
#[derive(Component, Clone, Default)]
pub struct ValueField;

/// A line's remove button.
#[derive(Component, Clone, Default)]
pub struct RemoveValue;

/// A list's Add button.
#[derive(Component, Clone, Default)]
pub struct AddValue;

/// A list showing `values`, with `empty` dim while it has none. It fills the width it is given.
pub fn value_list(values: Vec<String>, empty: impl Into<String>) -> impl Scene {
    let empty = empty.into();
    let none = display(values.is_empty());
    bsn! {
        Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) }, width: percent(100) }
        ValueList { values: { values } }
        Children [
            Node { flex_direction: FlexDirection::Column, row_gap: { px(space::STACKED) } }
            ValueLines
            --
            @text_dim(empty, size::BODY) ValueListEmpty Node { display: { none } }
            --
            @row()
            Children [
                Node { flex_grow: 1.0, min_width: px(0) }
                Children [ @text_input(1.0, bsn! { ValueField on(on_enter) }) ]
                --
                @button("Add", Some(Icon::Plus), ButtonKind::Normal) AddValue on(on_add)
            ]
        ]
    }
}

fn line(index: usize, value: String) -> impl Scene {
    let shown = value.clone();
    bsn! {
        @row()
        ValueLine { index: { index }, value: { value } }
        Children [
            @text_truncated(shown, size::BODY)
            --
            @tool_button(Icon::X, "Remove", ButtonKind::Plain) RemoveValue on(on_remove)
        ]
    }
}

/// Keeps each changed list's lines in step with its values, and the empty line shown only while there are none.
pub(crate) fn sync_value_lists(
    lists: Query<(Entity, &ValueList, &Children), Changed<ValueList>>,
    boxes: Query<(), With<ValueLines>>,
    empties: Query<(), With<ValueListEmpty>>,
    mut nodes: Query<&mut Node>,
    mut kept: Local<HashMap<Entity, Keyed<ValueLine>>>,
    all: Query<(), With<ValueList>>,
    mut commands: Commands,
) {
    kept.retain(|list, _| all.contains(*list));
    for (list, values, children) in &lists {
        let Some(lines) = children.iter().find(|c| boxes.contains(*c)) else { continue };
        let wanted: Vec<(String, ValueLine)> = values
            .values
            .iter()
            .enumerate()
            .map(|(index, value)| (index.to_string(), ValueLine { index, value: value.clone() }))
            .collect();
        kept.entry(list).or_default().sync(
            &mut commands,
            &[(lines, wanted)],
            |commands, _, model| commands.spawn_scene(line(model.index, model.value.clone())).id(),
            |commands, entity, model| {
                commands.entity(entity).insert(model.clone());
            },
        );
        for empty in children.iter().filter(|c| empties.contains(*c)) {
            set_display(&mut nodes, empty, values.values.is_empty());
        }
    }
}

/// Shows a line's value once it changes.
pub(crate) fn paint_value_lines(
    lines: Query<(&ValueLine, &Children), Changed<ValueLine>>,
    mut texts: Query<&mut Truncated>,
) {
    for (line, children) in &lines {
        for child in children {
            if let Ok(mut text) = texts.get_mut(*child)
                && text.full != line.value
            {
                text.full = line.value.clone();
            }
        }
    }
}

/// The list `inside` is in.
fn list_of(inside: Entity, parents: &Query<&ChildOf>, lists: &Query<&ValueList>) -> Option<Entity> {
    parents.iter_ancestors(inside).find(|e| lists.contains(*e))
}

fn on_remove(
    activate: On<Activate>,
    parents: Query<&ChildOf>,
    lines: Query<&ValueLine>,
    lists: Query<&ValueList>,
    mut commands: Commands,
) {
    let Some(line) = parents.iter_ancestors(activate.entity).find_map(|e| lines.get(e).ok()) else { return };
    let Some(list) = list_of(activate.entity, &parents, &lists) else { return };
    let mut values = lists.get(list).map(|l| l.values.clone()).unwrap_or_default();
    if line.index < values.len() {
        values.remove(line.index);
        commands.trigger(ValuesEdited { entity: list, values });
    }
}

fn on_add(
    activate: On<Activate>,
    parents: Query<&ChildOf>,
    lists: Query<&ValueList>,
    children: Query<&Children>,
    mut fields: Query<&mut EditableText, With<ValueField>>,
    mut commands: Commands,
) {
    let Some(list) = list_of(activate.entity, &parents, &lists) else { return };
    let Some(field) = children.iter_descendants(list).find(|e| fields.contains(*e)) else { return };
    add(list, field, &lists, &mut fields, &mut commands);
}

fn on_enter(
    entered: On<TextEntered>,
    parents: Query<&ChildOf>,
    lists: Query<&ValueList>,
    mut fields: Query<&mut EditableText, With<ValueField>>,
    mut commands: Commands,
) {
    let Some(list) = list_of(entered.entity, &parents, &lists) else { return };
    add(list, entered.entity, &lists, &mut fields, &mut commands);
}

/// Asks for what is typed in `field` to be added to `list`, and clears the field.
fn add(
    list: Entity,
    field: Entity,
    lists: &Query<&ValueList>,
    fields: &mut Query<&mut EditableText, With<ValueField>>,
    commands: &mut Commands,
) {
    let (Ok(shown), Ok(mut typed)) = (lists.get(list), fields.get_mut(field)) else { return };
    let value = typed.value().to_string().trim().to_string();
    if value.is_empty() || shown.values.contains(&value) {
        return;
    }
    typed.editor_mut().set_text("");
    let mut values = shown.values.clone();
    values.push(value);
    commands.trigger(ValuesEdited { entity: list, values });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::settle;

    #[derive(Resource, Default)]
    struct Edits(Vec<Vec<String>>);

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .init_resource::<Edits>()
            .add_observer(|edited: On<ValuesEdited>, mut edits: ResMut<Edits>| edits.0.push(edited.values.clone()))
            .add_systems(Update, (sync_value_lists, paint_value_lines).chain());
        app
    }

    fn lines(app: &mut App) -> Vec<(Entity, String)> {
        let world = app.world_mut();
        let mut found: Vec<(Entity, ValueLine)> =
            world.query::<(Entity, &ValueLine)>().iter(world).map(|(e, l)| (e, l.clone())).collect();
        found.sort_by_key(|(_, l)| l.index);
        found.into_iter().map(|(e, l)| (e, l.value)).collect()
    }

    fn one<F: bevy::ecs::query::QueryFilter>(app: &mut App) -> Entity {
        app.world_mut().query_filtered::<Entity, F>().single(app.world()).unwrap()
    }

    fn empty_shown(app: &mut App) -> bool {
        let empty = one::<With<ValueListEmpty>>(app);
        app.world().get::<Node>(empty).unwrap().display != Display::None
    }

    #[test]
    fn lines_follow_the_values_in_place_and_edits_are_asked_for() {
        let mut app = app();
        let values = vec!["~/Dev/a".to_string(), "~/Dev/b".to_string()];
        let list = app.world_mut().commands().spawn_scene(value_list(values, "No repos")).id();
        settle(&mut app);
        let before = lines(&mut app);
        assert_eq!(before.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>(), ["~/Dev/a", "~/Dev/b"]);
        assert!(!empty_shown(&mut app));

        let remove = app.world().get::<Children>(before[0].0).unwrap()[1];
        app.world_mut().trigger(Activate { entity: remove });
        settle(&mut app);
        assert_eq!(app.world().resource::<Edits>().0, [vec!["~/Dev/b".to_string()]], "asked for, not done");
        assert_eq!(lines(&mut app).len(), 2);

        let field = one::<With<ValueField>>(&mut app);
        app.world_mut().get_mut::<EditableText>(field).unwrap().editor_mut().set_text("  ~/Dev/c ");
        let add = one::<With<AddValue>>(&mut app);
        app.world_mut().trigger(Activate { entity: add });
        settle(&mut app);
        let asked = &app.world().resource::<Edits>().0[1];
        assert_eq!(asked, &["~/Dev/a", "~/Dev/b", "~/Dev/c"]);
        assert_eq!(app.world().get::<EditableText>(field).unwrap().value().to_string(), "", "the field is cleared");
        app.world_mut().get_mut::<EditableText>(field).unwrap().editor_mut().set_text("~/Dev/a");
        app.world_mut().trigger(TextEntered { entity: field, value: "~/Dev/a".into() });
        settle(&mut app);
        assert_eq!(app.world().resource::<Edits>().0.len(), 2, "a value already listed adds nothing");

        app.world_mut().get_mut::<ValueList>(list).unwrap().values = vec!["~/Dev/b".into()];
        settle(&mut app);
        let after = lines(&mut app);
        assert_eq!(after, [(before[0].0, "~/Dev/b".to_string())], "the first line kept, showing the new value");
        let text = app.world().get::<Children>(after[0].0).unwrap()[0];
        assert_eq!(app.world().get::<Truncated>(text).unwrap().full, "~/Dev/b");
        app.world_mut().get_mut::<ValueList>(list).unwrap().values.clear();
        settle(&mut app);
        assert!(lines(&mut app).is_empty() && empty_shown(&mut app));
    }
}
