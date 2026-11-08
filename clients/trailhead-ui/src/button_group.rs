//! A few choices side by side, drawn as one control: a row of buttons joined at a seam, only the outer ends rounded,
//! the chosen one primary. For a choice of two to four short options that should all show at once (a theme's light,
//! dark or the desktop's); a longer list is a [`super::select`].
//!
//! Pressing an option chooses it and triggers [`Picked`] on the group with its index, as a select does. Set
//! [`ButtonGroup::chosen`] to show a choice made elsewhere.
//!
//! Ported from bevy-data-explorer's theme and interface size buttons (`ui/settings.rs`).

use bevy::feathers::controls::{ButtonVariant, FeathersButton};
use bevy::feathers::rounded_corners::RoundedCorners;
use bevy::prelude::*;
use bevy::ui_widgets::Activate;

use super::{Icon, Picked, button_icon, button_text, space};

/// A group's state, on its row: which option is chosen, if any.
#[derive(Component, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ButtonGroup {
    pub chosen: Option<usize>,
}

/// An option of a group, by its index.
#[derive(Component, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct GroupOption(usize);

/// A group of `options` (a caption, after an icon when there is one), `chosen` showing as chosen. It fills the width
/// it is given, the options sharing it evenly.
pub fn button_group(options: Vec<(String, Option<Icon>)>, chosen: Option<usize>) -> impl Scene {
    let last = options.len().saturating_sub(1);
    let buttons: Vec<Box<dyn SceneList>> = options
        .into_iter()
        .enumerate()
        .map(|(i, (caption, icon))| -> Box<dyn SceneList> {
            let corners = match i {
                0 if last == 0 => RoundedCorners::All,
                0 => RoundedCorners::Left,
                i if i == last => RoundedCorners::Right,
                _ => RoundedCorners::None,
            };
            let inside: Box<dyn SceneList> = match icon {
                Some(icon) => Box::new(bsn_list! { @button_icon(icon) -- @button_text(caption) }),
                None => Box::new(bsn_list! { @button_text(caption) }),
            };
            let variant = if Some(i) == chosen { ButtonVariant::Primary } else { ButtonVariant::Normal };
            Box::new(bsn_list! {
                @FeathersButton { @caption: { inside }, @corners: { corners }, @variant: { variant } }
                Node { column_gap: { px(space::ICON_LABEL) }, flex_grow: 1.0 }
                GroupOption({ i })
            })
        })
        .collect();
    bsn! {
        Node { width: percent(100), column_gap: { px(space::SEAM) } }
        ButtonGroup { chosen: { chosen } }
        Children [ { buttons } ]
    }
}

/// A press on an option: choose it, and say so.
pub(crate) fn on_option(
    activate: On<Activate>,
    options: Query<(&GroupOption, &ChildOf)>,
    mut groups: Query<&mut ButtonGroup>,
    mut commands: Commands,
) {
    let Ok((GroupOption(index), parent)) = options.get(activate.entity) else { return };
    let Ok(mut group) = groups.get_mut(parent.parent()) else { return };
    if group.chosen != Some(*index) {
        group.chosen = Some(*index);
        commands.trigger(Picked { entity: parent.parent(), index: *index });
    }
}

/// Shows each changed group's choice: the chosen option primary, the rest normal.
pub(crate) fn paint_groups(
    groups: Query<(&ButtonGroup, &Children), Changed<ButtonGroup>>,
    mut options: Query<(&GroupOption, &mut ButtonVariant)>,
) {
    for (group, children) in &groups {
        for &child in children {
            if let Ok((GroupOption(index), mut variant)) = options.get_mut(child) {
                let wanted = if group.chosen == Some(*index) { ButtonVariant::Primary } else { ButtonVariant::Normal };
                variant.set_if_neq(wanted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Resource, Default)]
    struct Picks(Vec<usize>);

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .insert_resource(crate::theme::Theme::new().resource())
            .init_resource::<Picks>()
            .add_observer(on_option)
            .add_observer(|picked: On<Picked>, mut picks: ResMut<Picks>| picks.0.push(picked.index))
            .add_systems(Update, paint_groups);
        app
    }

    fn settle(app: &mut App) {
        app.world_mut().flush();
        for _ in 0..3 {
            app.update();
        }
    }

    fn variants(app: &mut App) -> Vec<ButtonVariant> {
        let world = app.world_mut();
        let mut options: Vec<_> = world.query::<(&GroupOption, &ButtonVariant)>().iter(world).collect();
        options.sort_by_key(|(o, _)| o.0);
        options.into_iter().map(|(_, v)| v.clone()).collect()
    }

    fn press(app: &mut App, index: usize) {
        let world = app.world_mut();
        let option = world.query::<(Entity, &GroupOption)>().iter(world).find(|(_, o)| o.0 == index).unwrap().0;
        app.world_mut().trigger(Activate { entity: option });
        settle(app);
    }

    #[test]
    fn pressing_an_option_chooses_it_and_says_so_once() {
        let mut app = app();
        let options = ["Light", "Dark", "Desktop"].map(|c| (c.to_string(), None)).to_vec();
        let group = app.world_mut().commands().spawn_scene(button_group(options, Some(2))).id();
        settle(&mut app);
        use ButtonVariant::{Normal, Primary};
        assert_eq!(variants(&mut app), [Normal, Normal, Primary]);
        press(&mut app, 0);
        assert_eq!(variants(&mut app), [Primary, Normal, Normal]);
        assert_eq!(app.world().resource::<Picks>().0, [0]);
        press(&mut app, 0);
        assert_eq!(app.world().resource::<Picks>().0, [0], "choosing the chosen one again is nothing");
        // A choice made elsewhere shows.
        app.world_mut().get_mut::<ButtonGroup>(group).unwrap().chosen = Some(1);
        settle(&mut app);
        assert_eq!(variants(&mut app), [Normal, Primary, Normal]);
    }
}
