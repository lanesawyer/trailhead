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

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::feathers::controls::{ButtonVariant, FeathersButton};
use bevy::picking::cursor::{EntityCursor, OverrideCursor};
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::text::LineBreak;
use bevy::window::SystemCursorIcon;

use super::{
    BackgroundTone, BorderTone, ButtonKind, Icon, Surface, Truncated, button, button_text, extent, layer, size, space,
    stroke, text_dim, token, tool_button,
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

/// The strip along the sidebar's right edge that drags it.
#[derive(Component, Clone, Default)]
pub struct SidebarEdge;

/// The line the edge shows while it is under the pointer or being dragged.
#[derive(Component, Clone, Default)]
pub(crate) struct EdgeLine;

/// On the edge while it is dragged: holds the resize cursor for the window until the drag ends, since the pointer
/// leaves the edge as soon as it lags a fast drag. Despawning the edge mid-drag lets go too.
#[derive(Component, Clone, Copy)]
#[component(on_insert = hold_cursor, on_remove = release_cursor)]
pub(crate) struct Dragging;

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
    mut commands: Commands,
) {
    if start.button == PointerButton::Primary && edges.contains(start.entity) {
        start.propagate(false);
        commands.entity(start.entity).insert(Dragging);
    }
}

/// Sizes the sidebar to wherever its edge has been dragged.
pub(crate) fn drag_edge(
    mut drag: On<PointerDrag>,
    edges: Query<(), With<SidebarEdge>>,
    ui_scale: Res<UiScale>,
    mut width: ResMut<SidebarWidth>,
) {
    if drag.button != PointerButton::Primary || !edges.contains(drag.entity) {
        return;
    }
    drag.propagate(false);
    // The pointer is in logical pixels; the sidebar is sized in UI units.
    width.set_if_neq(SidebarWidth::new(drag.pointer.position.x / ui_scale.0));
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

/// Sizes the sidebar and moves the pages beside it to [`SidebarWidth`], when it changes or one is spawned.
#[allow(clippy::type_complexity)]
pub(crate) fn fit_to_sidebar(
    width: Res<SidebarWidth>,
    mut frames: Query<(Ref<SidebarFrame>, &mut Node), Without<BesideSidebar>>,
    mut pages: Query<(Ref<BesideSidebar>, &mut Node), Without<SidebarFrame>>,
) {
    let changed = width.is_changed();
    let at = px(width.get());
    for (frame, mut node) in &mut frames {
        if (changed || frame.is_added()) && node.width != at {
            node.width = at;
        }
    }
    for (page, mut node) in &mut pages {
        if (changed || page.is_added()) && node.left != at {
            node.left = at;
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
/// name, its key, and how many of its tasks are open. Each text carries its [`NavPart`], for updating in place. A
/// caption too long for the sidebar is cut with an ellipsis: change it through its [`Truncated`], not its `Text`.
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

/// Which text of a [`nav_item_noted`] this is.
#[derive(Component, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum NavPart {
    #[default]
    Caption,
    Note,
    End,
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
    fn dragging_the_edge_holds_the_resize_cursor_until_it_ends() {
        let mut world = World::new();
        world.init_resource::<OverrideCursor>();
        let held = |world: &World| world.resource::<OverrideCursor>().0.clone();
        let edge = world.spawn(Dragging).id();
        assert_eq!(held(&world), Some(EntityCursor::System(SystemCursorIcon::ColResize)));
        world.entity_mut(edge).remove::<Dragging>();
        assert_eq!(held(&world), None);
        world.spawn(Dragging).despawn();
        assert_eq!(held(&world), None, "an edge despawned mid-drag lets go");
    }

    #[test]
    fn a_page_beside_the_sidebar_starts_where_it_ends() {
        let page = beside_sidebar();
        assert_eq!(page.left, px(extent::SIDEBAR_WIDTH));
        assert_eq!((page.right, page.top, page.bottom), (px(0), px(0), px(0)));
    }
}
