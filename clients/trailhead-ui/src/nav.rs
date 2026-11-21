//! An app's sidebar: a column down the left of the window with the app's name, its places to go, and a footer for
//! state that is always there (a connection chip, settings).
//!
//! [`sidebar`] is the column, a root of its own; spawn its contents as its children, and a [`sidebar_footer`] last to
//! hold the bottom. A place to go is a [`nav_item`], a plain button the width of the sidebar that shows a background
//! while it is the page shown: set that on its [`NavItem`]; [`nav_item_noted`] adds a dim note just after its caption and a dim value at its end. A group of them goes under a [`nav_heading`].
//!
//! The sidebar's right edge drags it wider or narrower, within [`extent::SIDEBAR_MIN_WIDTH`] and
//! [`extent::SIDEBAR_MAX_WIDTH`]: [`SidebarWidth`] holds where it is. The pages an app shows beside the sidebar fill
//! [`beside_sidebar`] and carry [`BesideSidebar`], so they follow its width and the two never overlap.
//!
//! A window that holds several apps has a [`rail`] down its left edge, a narrow strip with a [`rail_item`] for each
//! app, and a [`rail_foot`] at its bottom for what the window says about itself. While there is one, the sidebar and the pages beside it start where the rail ends; an app with no sidebar
//! fills [`beside_rail`] and carries [`BesideRail`].

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::system::SystemParam;
use bevy::ecs::world::DeferredWorld;
use bevy::feathers::controls::{ButtonVariant, FeathersButton};
use bevy::picking::cursor::{EntityCursor, OverrideCursor};
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::LineBreak;
use bevy::window::SystemCursorIcon;

use super::{
    BackgroundTone, BorderTone, ButtonKind, Icon, Surface, Truncated, button, button_text, extent, layer, radius,
    set_text, size, space, stroke, text_dim, token, tool_button,
};

/// How far the drag edge reaches past the sidebar's border, so it straddles the line rather than sitting inside it.
const EDGE_OVERHANG: f32 = -(extent::DRAG_EDGE + stroke::HAIRLINE) / 2.0;

/// How wide the sidebar is: [`extent::SIDEBAR_WIDTH`] until its edge is dragged, then wherever that left it, held
/// between [`extent::SIDEBAR_MIN_WIDTH`] and [`extent::SIDEBAR_MAX_WIDTH`]. Set it to restore a saved width.
#[derive(Resource, Clone, Copy, PartialEq, Debug)]
pub struct SidebarWidth(f32);

impl Default for SidebarWidth {
    fn default() -> Self {
        SidebarWidth(extent::SIDEBAR_WIDTH)
    }
}

impl SidebarWidth {
    /// A width, held within the sidebar's minimum and maximum.
    pub fn new(width: f32) -> Self {
        SidebarWidth(width.clamp(extent::SIDEBAR_MIN_WIDTH, extent::SIDEBAR_MAX_WIDTH))
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

/// On the sidebar itself, whose width follows [`SidebarWidth`].
#[derive(Component, Clone, Default)]
pub(crate) struct SidebarFrame;

/// On a page beside the sidebar ([`beside_sidebar`]), whose left follows [`SidebarWidth`].
#[derive(Component, Clone, Default)]
pub struct BesideSidebar;

/// On the [`rail`] itself.
#[derive(Component, Clone, Default)]
pub(crate) struct RailFrame;

/// On a page that fills the window beside the [`rail`] ([`beside_rail`]), for an app with no sidebar.
#[derive(Component, Clone, Default)]
pub struct BesideRail;

/// The strip along the sidebar's right edge that drags it.
#[derive(Component, Clone, Default)]
pub struct SidebarEdge;

/// The line the edge shows while it is under the pointer or being dragged.
#[derive(Component, Clone, Default)]
pub(crate) struct EdgeLine;

/// On the edge while it is dragged, with the sidebar's width when the drag began: holds the resize cursor for the
/// window until the drag ends, since the pointer leaves the edge as soon as it lags a fast drag. Despawning the edge
/// mid-drag lets go too.
#[derive(Component, Clone, Copy)]
#[component(on_insert = hold_cursor, on_remove = release_cursor)]
pub(crate) struct Dragging {
    from: f32,
}

fn hold_cursor(mut world: DeferredWorld, _: HookContext) {
    if let Some(mut cursor) = world.get_resource_mut::<OverrideCursor>() {
        cursor.0 = Some(EntityCursor::System(SystemCursorIcon::ColResize));
    }
}

fn release_cursor(mut world: DeferredWorld, _: HookContext) {
    if let Some(mut cursor) = world.get_resource_mut::<OverrideCursor>() {
        cursor.0 = None;
    }
}

/// The sidebar down the left of the window, [`SidebarWidth`] wide, dragged wider or narrower by its right edge.
pub fn sidebar() -> impl Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            top: px(0),
            bottom: px(0),
            width: { px(extent::SIDEBAR_WIDTH) },
            flex_direction: FlexDirection::Column,
            row_gap: { px(space::ROWS) },
            padding: { UiRect::all(px(space::PANEL_INSET)) },
            border: { UiRect::right(px(stroke::HAIRLINE)) },
            min_height: px(0),
        }
        Surface::Panel
        BorderTone({ token::BORDER })
        SidebarFrame
        Children [
            Node {
                position_type: PositionType::Absolute,
                top: px(0),
                bottom: px(0),
                right: { px(EDGE_OVERHANG) },
                width: { px(extent::DRAG_EDGE) },
                justify_content: JustifyContent::Center,
            }
            SidebarEdge
            Hovered
            EntityCursor::System(SystemCursorIcon::ColResize)
            GlobalZIndex({ layer::SIDEBAR_EDGE })
            Children [
                Node { width: { px(stroke::GRIP) }, height: percent(100) }
                BackgroundTone({ token::FILL_ACCENT })
                Visibility::Hidden
                EdgeLine
            ]
        ]
    }
}

/// Starts dragging the sidebar's edge with the primary button.
pub(crate) fn start_edge_drag(
    mut start: On<PointerDragStart>,
    edges: Query<(), With<SidebarEdge>>,
    width: Res<SidebarWidth>,
    mut commands: Commands,
) {
    if start.button == PointerButton::Primary && edges.contains(start.entity) {
        start.propagate(false);
        commands.entity(start.entity).insert(Dragging { from: width.get() });
    }
}

/// Sizes the sidebar to wherever its edge has been dragged: its width when the drag began, moved by as far as the
/// pointer has gone since. Measured from the window's left edge instead, the edge would jump the rail's width away from
/// the pointer, and by the pointer's distance from the window's edge times the text size.
pub(crate) fn drag_edge(
    mut drag: On<PointerDrag>,
    edges: Query<Option<&Dragging>, With<SidebarEdge>>,
    ui_scale: Res<UiScale>,
    mut width: ResMut<SidebarWidth>,
) {
    if drag.button != PointerButton::Primary {
        return;
    }
    let Ok(dragging) = edges.get(drag.entity) else { return };
    drag.propagate(false);
    // Until the drag's start has landed; the distance is the whole drag's, so nothing is lost.
    let Some(dragging) = dragging else { return };
    width.set_if_neq(dragged(dragging.from, drag.distance.x, ui_scale.0));
}

/// The sidebar `from` wide dragged `distance` logical pixels to the right, under a UI drawn at `ui_scale`: the sidebar
/// is sized in UI pixels, which `ui_scale` makes larger than the pointer's logical ones.
fn dragged(from: f32, distance: f32, ui_scale: f32) -> SidebarWidth {
    SidebarWidth::new(from + distance / ui_scale.max(f32::EPSILON))
}

pub(crate) fn end_edge_drag(end: On<PointerDragEnd>, edges: Query<(), With<SidebarEdge>>, mut commands: Commands) {
    if edges.contains(end.entity) {
        commands.entity(end.entity).remove::<Dragging>();
    }
}

/// Shows the edge's line while it is under the pointer or being dragged.
pub(crate) fn show_edge_lines(
    edges: Query<(&Hovered, Has<Dragging>, &Children), With<SidebarEdge>>,
    mut lines: Query<&mut Visibility, With<EdgeLine>>,
) {
    for (hovered, dragging, children) in &edges {
        let shown = if hovered.0 || dragging { Visibility::Inherited } else { Visibility::Hidden };
        for child in children {
            if let Ok(mut line) = lines.get_mut(*child) {
                line.set_if_neq(shown);
            }
        }
    }
}

/// Puts the sidebar after the [`rail`], if there is one, at [`SidebarWidth`] wide, and the pages after both. A page
/// beside the sidebar while there is none (one the window shows whatever app is shown, under an app with no sidebar)
/// starts where the rail ends. It compares every time it runs, since a rail or a sidebar coming or going moves the
/// others; a node already in place is left untouched.
#[allow(clippy::type_complexity)]
pub(crate) fn fit_to_sidebar(
    width: Res<SidebarWidth>,
    rails: Query<(), With<RailFrame>>,
    mut frames: Query<&mut Node, (With<SidebarFrame>, Without<BesideSidebar>, Without<BesideRail>)>,
    mut pages: Query<&mut Node, (With<BesideSidebar>, Without<SidebarFrame>, Without<BesideRail>)>,
    mut bare: Query<&mut Node, (With<BesideRail>, Without<SidebarFrame>, Without<BesideSidebar>)>,
) {
    let rail = if rails.is_empty() { 0.0 } else { extent::RAIL_WIDTH };
    let sidebar = if frames.is_empty() { 0.0 } else { width.get() };
    let (after_rail, wide, after_sidebar) = (px(rail), px(width.get()), px(rail + sidebar));
    for mut node in &mut frames {
        if node.left != after_rail {
            node.left = after_rail;
        }
        if node.width != wide {
            node.width = wide;
        }
    }
    for mut node in &mut pages {
        if node.left != after_sidebar {
            node.left = after_sidebar;
        }
    }
    for mut node in &mut bare {
        if node.left != after_rail {
            node.left = after_rail;
        }
    }
}

/// The strip down the left edge of a window that holds several apps: a column of [`rail_item`]s, one for each app,
/// spawned as its children. The sidebar and the pages start where it ends.
pub fn rail() -> impl Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            top: px(0),
            bottom: px(0),
            width: { px(extent::RAIL_WIDTH) },
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: { px(space::ROWS) },
            padding: { UiRect::vertical(px(space::PANEL_INSET)) },
            border: { UiRect::right(px(stroke::HAIRLINE)) },
        }
        Surface::Panel
        BorderTone({ token::BORDER })
        RailFrame
    }
}

/// An app on the [`rail`]: its `icon`, with its `name` under the pointer. Plain until it is the app shown
/// ([`NavItem::current`]).
pub fn rail_item(icon: Icon, name: impl Into<String>) -> impl Scene {
    let name = name.into();
    bsn! {
        @tool_button(icon, name, ButtonKind::Plain)
        Node { flex_shrink: { 0.0_f32 } }
        NavItem
    }
}

/// The bottom of a [`rail`], pushed down to its foot: what the window says about itself rather than about an app (an
/// update ready to restart onto). Spawn it after the [`rail_item`]s, then its contents as its children: a
/// [`tool_button`] each, hidden with [`set_display`](crate::set_display) on the foot while there is nothing to say.
pub fn rail_foot() -> impl Scene {
    bsn! {
        Node {
            margin: { UiRect::top(Val::Auto) },
            flex_shrink: { 0.0_f32 },
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: { px(space::ROWS) },
        }
    }
}

/// The bottom of a [`sidebar`]: pushed down to the sidebar's foot, its children in a row. Like the sidebar's places, it
/// keeps its height in a short window, so the part that scrolls gives way instead.
pub fn sidebar_footer() -> impl Scene {
    bsn! {
        Node {
            margin: { UiRect::top(Val::Auto) },
            flex_shrink: { 0.0_f32 },
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: { px(space::CONTROLS) },
        }
    }
}

/// A raised box in a [`sidebar`] for something to keep an eye on (how much of a subscription is used): a padded
/// column. Spawn it just before the [`sidebar_footer`], then its contents as its children.
pub fn sidebar_card() -> impl Scene {
    bsn! {
        Node {
            flex_direction: FlexDirection::Column,
            flex_shrink: { 0.0_f32 },
            row_gap: { px(space::ROWS) },
            padding: { UiRect::all(px(space::CONTROL_INSET)) },
            border_radius: { BorderRadius::all(px(radius::PANEL)) },
        }
        Surface::Raised
    }
}

/// The dim words over a group of [`nav_item`]s.
pub fn nav_heading(words: impl Into<String>) -> impl Scene {
    let words = words.into();
    bsn! {
        @text_dim(words, size::SECONDARY)
        Node { margin: { UiRect::top(px(space::GROUPS)) }, flex_shrink: { 0.0_f32 } }
    }
}

/// A [`nav_heading`] whose words are a place to go (the page listing the group's items), with a plain tool button at
/// its end for something done to the group (adding to it): `icon`, with `hint` under the pointer. `heading` goes on the
/// words' button and `action` on the tool button, where the caller puts what each press does. Like a [`nav_item`], the
/// words show a background while their page is shown: set that on their [`NavItem`].
pub fn nav_heading_action(
    words: impl Into<String>,
    heading: impl Scene,
    icon: Icon,
    hint: impl Into<String>,
    action: impl Scene,
) -> impl Scene {
    let (words, hint) = (words.into(), hint.into());
    bsn! {
        Node {
            margin: { UiRect::top(px(space::GROUPS)) },
            flex_shrink: { 0.0_f32 },
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: { px(space::CONTROLS) },
        }
        Children [
            @FeathersButton {
                @caption: { bsn_list! { @text_dim(words, size::SECONDARY) } },
                @variant: { ButtonVariant::Plain },
            }
            Node { flex_grow: 1.0, flex_shrink: 0.0, justify_content: JustifyContent::Start }
            NavItem
            @{heading}
            --
            @tool_button(icon, hint, ButtonKind::Plain)
            @{action}
        ]
    }
}

/// A place to go in a [`sidebar`]: `caption` after `icon`, the width of the sidebar. Plain until it is the page shown
/// ([`NavItem::current`]), and like every button it shows a background under the pointer. It keeps its height in a
/// sidebar too short for its places, which scroll instead.
pub fn nav_item(caption: impl Into<String>, icon: Option<Icon>) -> impl Scene {
    let caption = caption.into();
    bsn! {
        @button(caption, icon, ButtonKind::Plain)
        Node { justify_content: JustifyContent::Start, flex_shrink: { 0.0_f32 } }
        NavItem
    }
}

/// A [`nav_item`] with `note` just after `caption` and `end` at the far right, both smaller and dimmer: a project's
/// name, its key, and how many of its tasks are open. A caption too long for the sidebar is cut with an ellipsis.
/// Change it in place with [`NavEdit::set_noted`].
pub fn nav_item_noted(caption: impl Into<String>, note: impl Into<String>, end: impl Into<String>) -> impl Scene {
    let (caption, note, end) = (caption.into(), note.into(), end.into());
    let full = caption.clone();
    bsn! {
        @FeathersButton {
            @caption: { bsn_list! {
                // The caption and its note take what the end leaves, so the caption has a width to be cut to.
                Node {
                    flex_grow: 1.0,
                    min_width: px(0),
                    overflow: Overflow::clip_x(),
                    align_items: AlignItems::Center,
                    column_gap: { px(space::ICON_LABEL) },
                }
                Children [
                    @button_text(caption)
                    Truncated { full: { full }, size: { size::BODY } }
                    TextLayout { linebreak: LineBreak::NoWrap }
                    NavPart::Caption
                    --
                    @text_dim(note, size::SMALL)
                    Node { flex_shrink: 0.0 }
                    NavPart::Note
                ]
                --
                @text_dim(end, size::SMALL)
                Node { flex_shrink: 0.0 }
                NavPart::End
            } },
            @variant: { ButtonVariant::Plain },
        }
        Node { justify_content: JustifyContent::Start, column_gap: { px(space::ICON_LABEL) }, flex_shrink: { 0.0_f32 } }
        NavItem
    }
}

/// Which text of a [`nav_item_noted`] this is, which [`NavEdit`] finds them by.
#[derive(Component, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum NavPart {
    #[default]
    Caption,
    Note,
    End,
}

/// Changes a spawned [`nav_item_noted`] in place. Writes only what differs.
#[derive(SystemParam)]
pub struct NavEdit<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    parts: Query<'w, 's, &'static NavPart>,
    texts: Query<'w, 's, &'static mut Text>,
    truncated: Query<'w, 's, &'static mut Truncated>,
}

impl NavEdit<'_, '_> {
    /// Gives a [`nav_item_noted`] this caption, note and end, the caption cut to fit as it was at spawn.
    pub fn set_noted(&mut self, item: Entity, caption: &str, note: &str, end: &str) {
        let parts: Vec<Entity> = self.children.iter_descendants(item).collect();
        for part in parts {
            match self.parts.get(part) {
                Ok(NavPart::Caption) => {
                    if let Ok(mut shown) = self.truncated.get_mut(part)
                        && shown.full != caption
                    {
                        shown.full = caption.to_string();
                    }
                }
                Ok(NavPart::Note) => {
                    if let Ok(text) = self.texts.get_mut(part) {
                        set_text(text, note);
                    }
                }
                Ok(NavPart::End) => {
                    if let Ok(text) = self.texts.get_mut(part) {
                        set_text(text, end);
                    }
                }
                Err(_) => {}
            }
        }
    }
}

/// A [`nav_item`], and whether it is the page shown, which gives it a background. Set `current` with `set_if_neq`.
#[derive(Component, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct NavItem {
    pub current: bool,
}

/// Give each nav item whose `current` changed the background that says so.
pub(crate) fn paint_nav_items(mut items: Query<(&NavItem, &mut ButtonVariant), Changed<NavItem>>) {
    for (item, mut variant) in &mut items {
        variant.set_if_neq(if item.current { ButtonVariant::Normal } else { ButtonVariant::Plain });
    }
}

/// The area right of a [`sidebar`], for a page to fill. Put [`BesideSidebar`] on the page too, so it follows the
/// sidebar's width.
pub fn beside_sidebar() -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: px(extent::SIDEBAR_WIDTH),
        right: px(0),
        top: px(0),
        bottom: px(0),
        ..default()
    }
}

/// The whole window right of the [`rail`], for the page of an app with no sidebar. Put [`BesideRail`] on the page
/// too, so it starts where the rail ends.
pub fn beside_rail() -> Node {
    Node { left: px(extent::RAIL_WIDTH), ..beside_sidebar() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidebar_has_room_for_a_scrollbar_in_its_padding() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        let id = app.world_mut().commands().spawn_scene(sidebar()).id();
        app.world_mut().flush();
        app.update();
        assert!(crate::has_scrollbar_room(app.world().get::<Node>(id).unwrap()));
    }

    #[test]
    fn a_nav_item_is_plain_until_it_is_the_page_shown() {
        let mut app = App::new();
        app.add_systems(Update, paint_nav_items);
        let item = app.world_mut().spawn((NavItem::default(), ButtonVariant::Plain)).id();
        let set = |app: &mut App, current| {
            app.world_mut().get_mut::<NavItem>(item).unwrap().set_if_neq(NavItem { current });
            app.update();
            app.world().get::<ButtonVariant>(item).unwrap().clone()
        };
        assert_eq!(set(&mut app, true), ButtonVariant::Normal);
        assert_eq!(set(&mut app, false), ButtonVariant::Plain);
    }

    #[test]
    fn a_noted_nav_item_is_changed_in_place() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>();
        let item = app.world_mut().commands().spawn_scene(nav_item_noted("Home", "HOME", "3")).id();
        app.world_mut().flush();
        app.update();
        let before: Vec<Entity> = app.world_mut().query::<Entity>().iter(app.world()).collect();
        app.world_mut().run_system_once(move |mut edit: NavEdit| edit.set_noted(item, "Crag", "CRAG", "4")).unwrap();
        let world = app.world_mut();
        let mut parts: Vec<(NavPart, String)> = world
            .query::<(&NavPart, &Text, Option<&Truncated>)>()
            .iter(world)
            .map(|(part, text, cut)| (*part, cut.map_or_else(|| text.0.clone(), |t| t.full.clone())))
            .collect();
        parts.sort_by_key(|(part, _)| *part as u8);
        let words = |w: &str| w.to_string();
        assert_eq!(
            parts,
            [(NavPart::Caption, words("Crag")), (NavPart::Note, words("CRAG")), (NavPart::End, words("4"))]
        );
        assert_eq!(world.query::<Entity>().iter(world).collect::<Vec<_>>(), before, "nothing respawned");
    }

    #[test]
    fn the_sidebar_is_held_between_its_narrowest_and_widest() {
        assert_eq!(SidebarWidth::default().get(), extent::SIDEBAR_WIDTH);
        assert_eq!(SidebarWidth::new(10.0).get(), extent::SIDEBAR_MIN_WIDTH);
        assert_eq!(SidebarWidth::new(300.0).get(), 300.0);
        assert_eq!(SidebarWidth::new(5000.0).get(), extent::SIDEBAR_MAX_WIDTH);
    }

    #[test]
    fn the_sidebar_and_the_pages_beside_it_follow_its_width() {
        let mut app = App::new();
        app.init_resource::<SidebarWidth>().add_systems(Update, fit_to_sidebar);
        let frame = app.world_mut().spawn((SidebarFrame, Node::default())).id();
        let page = app.world_mut().spawn((BesideSidebar, beside_sidebar())).id();
        app.update();
        app.world_mut().insert_resource(SidebarWidth::new(300.0));
        app.update();
        assert_eq!(app.world().get::<Node>(frame).unwrap().width, px(300));
        assert_eq!(app.world().get::<Node>(page).unwrap().left, px(300));
        // A page spawned later starts where the sidebar ends now.
        let late = app.world_mut().spawn((BesideSidebar, beside_sidebar())).id();
        app.update();
        assert_eq!(app.world().get::<Node>(late).unwrap().left, px(300));
    }

    #[test]
    fn a_rail_moves_the_sidebar_and_the_pages_over_and_gives_the_room_back_when_it_goes() {
        let mut app = App::new();
        app.init_resource::<SidebarWidth>().add_systems(Update, fit_to_sidebar);
        let frame = app.world_mut().spawn((SidebarFrame, Node::default())).id();
        let page = app.world_mut().spawn((BesideSidebar, beside_sidebar())).id();
        let bare = app.world_mut().spawn((BesideRail, beside_rail())).id();
        let left = |app: &App, e| app.world().get::<Node>(e).unwrap().left;
        app.update();
        assert_eq!(left(&app, frame), px(0));
        assert_eq!(left(&app, page), px(extent::SIDEBAR_WIDTH));
        assert_eq!(left(&app, bare), px(0), "with no rail, a page beside it has the whole window");

        let rail = app.world_mut().spawn(RailFrame).id();
        app.update();
        assert_eq!(left(&app, frame), px(extent::RAIL_WIDTH));
        assert_eq!(left(&app, page), px(extent::RAIL_WIDTH + extent::SIDEBAR_WIDTH));
        assert_eq!(left(&app, bare), px(extent::RAIL_WIDTH));

        app.world_mut().despawn(rail);
        app.update();
        assert_eq!(left(&app, frame), px(0));
        assert_eq!(left(&app, page), px(extent::SIDEBAR_WIDTH));
    }

    #[test]
    fn a_page_beside_the_sidebar_with_no_sidebar_starts_where_the_rail_ends() {
        let mut app = App::new();
        app.init_resource::<SidebarWidth>().add_systems(Update, fit_to_sidebar);
        let page = app.world_mut().spawn((BesideSidebar, beside_sidebar())).id();
        let left = |app: &App| app.world().get::<Node>(page).unwrap().left;
        app.update();
        assert_eq!(left(&app), px(0), "with no rail either, it starts at the window's edge");

        let rail = app.world_mut().spawn(RailFrame).id();
        app.update();
        assert_eq!(left(&app), px(extent::RAIL_WIDTH));

        let frame = app.world_mut().spawn((SidebarFrame, Node::default())).id();
        app.update();
        assert_eq!(left(&app), px(extent::RAIL_WIDTH + extent::SIDEBAR_WIDTH), "a sidebar coming moves it over");

        app.world_mut().despawn(frame);
        app.world_mut().despawn(rail);
        app.update();
        assert_eq!(left(&app), px(0));
    }

    #[test]
    fn a_rail_item_is_a_nav_item_so_the_app_shown_is_marked_like_a_page() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((AssetPlugin::default(), bevy::scene::ScenePlugin))
            .init_asset::<bevy::text::Font>()
            .init_asset::<Image>()
            .add_systems(Update, paint_nav_items);
        let item = app.world_mut().commands().spawn_scene(rail_item(Icon::Board, "Lookout")).id();
        app.world_mut().flush();
        app.update();
        assert_eq!(app.world().get::<ButtonVariant>(item), Some(&ButtonVariant::Plain));
        app.world_mut().get_mut::<NavItem>(item).unwrap().current = true;
        app.update();
        assert_eq!(app.world().get::<ButtonVariant>(item), Some(&ButtonVariant::Normal));
    }

    #[test]
    fn dragging_the_edge_holds_the_resize_cursor_until_it_ends() {
        let mut world = World::new();
        world.init_resource::<OverrideCursor>();
        let held = |world: &World| world.resource::<OverrideCursor>().0.clone();
        let edge = world.spawn(Dragging { from: 0.0 }).id();
        assert_eq!(held(&world), Some(EntityCursor::System(SystemCursorIcon::ColResize)));
        world.entity_mut(edge).remove::<Dragging>();
        assert_eq!(held(&world), None);
        world.spawn(Dragging { from: 0.0 }).despawn();
        assert_eq!(held(&world), None, "an edge despawned mid-drag lets go");
    }

    /// Drags the edge of a sidebar `from` wide, beside a rail, `distance` logical pixels under a UI drawn at
    /// `ui_scale`, and returns the width it is left at.
    fn drag_by(from: f32, distance: f32, ui_scale: f32) -> f32 {
        use bevy::picking::backend::HitData;
        use bevy::picking::events::Pointer;
        use bevy::picking::pointer::{Location, PointerId};
        let mut app = App::new();
        app.insert_resource(UiScale(ui_scale))
            .insert_resource(SidebarWidth::new(from))
            .init_resource::<OverrideCursor>()
            .add_observer(start_edge_drag)
            .add_observer(drag_edge)
            .add_observer(end_edge_drag);
        let edge = app.world_mut().spawn(SidebarEdge).id();
        // Where the pointer is on screen: the rail, then the sidebar, drawn at the scale.
        let start = (extent::RAIL_WIDTH + from) * ui_scale;
        let at = |x: f32| {
            let target = bevy::camera::NormalizedRenderTarget::None { width: 1, height: 1 };
            Pointer::new(PointerId::Mouse, Location { target, position: Vec2::new(x, 300.0) })
        };
        let hit = HitData::new(Entity::PLACEHOLDER, 0.0, None, None);
        let world = app.world_mut();
        let button = PointerButton::Primary;
        world.trigger(PointerDragStart { entity: edge, pointer: at(start), button, hit: hit.clone() });
        world.flush();
        let (distance, delta) = (Vec2::new(distance, 0.0), Vec2::new(distance, 0.0));
        world.trigger(PointerDrag { entity: edge, pointer: at(start + distance.x), button, distance, delta });
        world.flush();
        world.resource::<SidebarWidth>().get()
    }

    #[test]
    fn the_edge_moves_with_the_pointer_beside_a_rail_and_under_a_larger_text_size() {
        assert_eq!(drag_by(220.0, 30.0, 1.0), 250.0);
        assert_eq!(drag_by(300.0, -60.0, 1.0), 240.0);
        // At 150% the pointer's 30 logical pixels are 20 of the sidebar's.
        assert_eq!(drag_by(220.0, 30.0, 1.5), 240.0);
        assert_eq!(drag_by(220.0, 0.0, 2.0), 220.0, "a press that hasn't moved leaves it where it was");
    }

    #[test]
    fn a_page_beside_the_sidebar_starts_where_it_ends() {
        let page = beside_sidebar();
        assert_eq!(page.left, px(extent::SIDEBAR_WIDTH));
        assert_eq!((page.right, page.top, page.bottom), (px(0), px(0), px(0)));
    }
}
