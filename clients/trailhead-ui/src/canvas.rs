//! A canvas: content of any size, with no edge, that a drag pans and the wheel zooms.
//!
//! [`spawn_canvas`] makes the canvas's root, which clips what is on it to its own box, and its content, which the
//! caller fills with nodes placed absolutely (`left` and `top` in UI pixels from the content's origin). The view is
//! the root's [`Canvas`]: how far the content is panned, and how far zoomed. Clicks on what lies on the canvas reach
//! it as they would anywhere; a press that moves further than [`DRAG_THRESHOLD`] pans instead, and is no longer a press,
//! so the release that ends it activates nothing (the rule `reorder.rs` set, TRAIL-1621).
//!
//! The wheel zooms about the pointer, between [`CANVAS_MIN_ZOOM`] and [`CANVAS_MAX_ZOOM`]; a sideways swipe pans.
//! The kit's one wheel observer (`scroll.rs`) routes it: it walks up from what the pointer is over to the nearest of
//! a [`ScrollDown`](crate::ScrollDown), a [`ScrollBoth`](crate::ScrollBoth) and a canvas, and the nearest takes the
//! whole turn. So a list that scrolls inside a canvas scrolls under the pointer, and a canvas inside a page that
//! scrolls zooms rather than scrolling the page.
//!
//! The view is kept apart from the content, so changing the content never moves it. A new canvas fits its content as
//! soon as it has some ([`Canvas::fit`] asks again): the caller says how big the content is with [`CanvasExtent`].

use bevy::picking::cursor::{EntityCursor, OverrideCursor};
use bevy::prelude::*;
use bevy::ui::Pressed;
use bevy::window::{RequestRedraw, SystemCursorIcon};

use crate::{DRAG_THRESHOLD, space};

/// The least a canvas zooms out to: a quarter of the content's size.
pub const CANVAS_MIN_ZOOM: f32 = 0.25;
/// The most a canvas zooms in to: twice the content's size.
pub const CANVAS_MAX_ZOOM: f32 = 2.0;
/// How far the wheel turns, in UI pixels, to double or halve the zoom: about seven notches of a wheel.
const ZOOM_DOUBLING: f32 = 140.0;

/// A canvas's view of its content: where the content's origin sits in the canvas's box (UI pixels from its top left)
/// and how far it is zoomed. Read it, or set it with [`Canvas::set`]; [`Canvas::fit`] fits the content to the box.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct Canvas {
    pan: Vec2,
    zoom: f32,
    fit_pending: bool,
    drag: Option<Drag>,
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas { pan: Vec2::ZERO, zoom: 1.0, fit_pending: true, drag: None }
    }
}

/// A press on the canvas, from the moment it starts to move.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    /// The pan when it began.
    from: Vec2,
    /// Past the threshold: a pan, not a press.
    panning: bool,
}

impl Canvas {
    /// Where the content's origin sits in the canvas's box, in UI pixels.
    pub fn pan(&self) -> Vec2 {
        self.pan
    }

    /// How far the content is zoomed: 1 is its own size.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Moves the view: the content's origin to `pan`, at `zoom` (held between the least and most a canvas zooms).
    pub fn set(&mut self, pan: Vec2, zoom: f32) {
        self.pan = pan;
        self.zoom = zoom.clamp(CANVAS_MIN_ZOOM, CANVAS_MAX_ZOOM);
        self.fit_pending = false;
    }

    /// Fits the content into the canvas's box, centered, zoomed out as far as it must be and never in past its own
    /// size: "fit all". It happens once the canvas has a size and its content an extent.
    pub fn fit(&mut self) {
        self.fit_pending = true;
    }

    /// Whether the view waits to be fitted.
    pub fn fitting(&self) -> bool {
        self.fit_pending
    }

    /// Whether a drag is panning it now.
    pub fn panning(&self) -> bool {
        self.drag.is_some_and(|d| d.panning)
    }
}

/// How big a canvas's content is, in UI pixels from its origin, which fitting it goes by. The caller sets it on the
/// canvas's root when the content changes (a graph sets its own).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct CanvasExtent(pub Vec2);

/// The node a canvas moves: its content's origin, holding what is on the canvas.
#[derive(Component, Clone, Copy, Debug)]
pub struct CanvasContent {
    /// The canvas it belongs to.
    pub canvas: Entity,
}

/// A canvas's root and its content, from [`spawn_canvas`].
#[derive(Clone, Copy, Debug)]
pub struct CanvasParts {
    /// The canvas's box: put it where it goes and size it (it clips to itself). It carries [`Canvas`] and
    /// [`CanvasExtent`].
    pub root: Entity,
    /// Where the content goes: children placed absolutely, `left` and `top` from the content's origin.
    pub content: Entity,
}

/// A canvas that pans and zooms its content, spawned unparented. The root fills what holds it (`flex_grow`); give it
/// a parent with room. See the module's docs for the input it takes.
pub fn spawn_canvas(commands: &mut Commands) -> CanvasParts {
    let root = commands
        .spawn((
            Name::new("canvas"),
            Canvas::default(),
            CanvasExtent::default(),
            Node { flex_grow: 1.0, min_width: px(0), min_height: px(0), overflow: Overflow::clip(), ..default() },
        ))
        .id();
    let content = commands
        .spawn((
            Name::new("canvas content"),
            CanvasContent { canvas: root },
            // No size, at the box's top left: a node scales about its middle, so this one scales about its origin.
            Node { position_type: PositionType::Absolute, left: px(0), top: px(0), ..default() },
            UiTransform::IDENTITY,
            ChildOf(root),
        ))
        .id();
    CanvasParts { root, content }
}

/// Panning, zooming and fitting canvases, and laying graphs out on them. Part of `UiPlugin`; add it on its own only in
/// a headless test without `UiPlugin`.
pub struct CanvasPlugin;

impl Plugin for CanvasPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(start_pan).add_observer(pan).add_observer(end_pan).add_systems(
            Update,
            (crate::graph::lay_out_graphs, fit_canvases, place_canvases).chain().in_set(crate::UiPlace),
        );
    }
}

/// The view that zooms `canvas` by `factor` about `pointer` (UI pixels in its box), keeping what is under the pointer
/// there.
pub(crate) fn zoomed(pan: Vec2, zoom: f32, pointer: Vec2, factor: f32) -> (Vec2, f32) {
    let next = (zoom * factor).clamp(CANVAS_MIN_ZOOM, CANVAS_MAX_ZOOM);
    let under = (pointer - pan) / zoom;
    (pointer - under * next, next)
}

/// The view that fits content `extent` big into a box `size` big, a panel's inset all round.
pub(crate) fn fitted(size: Vec2, extent: Vec2) -> (Vec2, f32) {
    let room = (size - Vec2::splat(space::PANEL_INSET * 2.0)).max(Vec2::ONE);
    let zoom = (room / extent.max(Vec2::ONE)).min_element().clamp(CANVAS_MIN_ZOOM, 1.0);
    ((size - extent * zoom) / 2.0, zoom)
}

/// The factor a wheel turn of `wheel` UI pixels (up is positive, as a wheel reports it) zooms by.
pub(crate) fn wheel_factor(wheel: f32) -> f32 {
    2f32.powf(wheel / ZOOM_DOUBLING)
}

/// Zooms `canvas` by a wheel turn: `wheel` in UI pixels, down by `y` and across by `x`, the pointer at `pointer`
/// (UI pixels in the canvas's box). The vertical part zooms about the pointer; the sideways one pans.
pub(crate) fn wheel_canvas(canvas: &mut Canvas, wheel: Vec2, pointer: Vec2) {
    let (mut pan, zoom) = zoomed(canvas.pan, canvas.zoom, pointer, wheel_factor(wheel.y));
    pan.x += wheel.x;
    canvas.set(pan, zoom);
}

/// The canvas `entity` lies on, if any: it or its nearest ancestor with a [`Canvas`].
fn canvas_of(entity: Entity, parents: &Query<&ChildOf>, canvases: &Query<&mut Canvas>) -> Option<Entity> {
    std::iter::once(entity).chain(parents.iter_ancestors(entity)).find(|e| canvases.contains(*e))
}

pub(crate) fn start_pan(mut start: On<PointerDragStart>, parents: Query<&ChildOf>, mut canvases: Query<&mut Canvas>) {
    if start.button != PointerButton::Primary {
        return;
    }
    let Some(root) = canvas_of(start.entity, &parents, &canvases) else { return };
    start.propagate(false);
    if let Ok(mut canvas) = canvases.get_mut(root) {
        let from = canvas.pan;
        canvas.drag = Some(Drag { from, panning: false });
    }
}

pub(crate) fn pan(
    mut drag: On<PointerDrag>,
    parents: Query<&ChildOf>,
    mut canvases: Query<&mut Canvas>,
    pressed: Query<(), With<Pressed>>,
    ui_scale: Option<Res<UiScale>>,
    cursor: Option<ResMut<OverrideCursor>>,
    mut commands: Commands,
) {
    let Some(root) = canvas_of(drag.entity, &parents, &canvases) else { return };
    drag.propagate(false);
    let Ok(mut canvas) = canvases.get_mut(root) else { return };
    let Some(mut held) = canvas.drag else { return };
    let moved = drag.distance / ui_scale.map_or(1.0, |s| s.0).max(f32::EPSILON);
    if !held.panning {
        if moved.length() < DRAG_THRESHOLD {
            return;
        }
        held.panning = true;
        // No longer a press: the release that ends the pan must not activate what it began on.
        let under = std::iter::once(drag.entity).chain(parents.iter_ancestors(drag.entity));
        for entity in under.take_while(|e| *e != root).filter(|e| pressed.contains(*e)) {
            commands.entity(entity).remove::<Pressed>();
        }
        if let Some(mut cursor) = cursor {
            cursor.0 = Some(EntityCursor::System(SystemCursorIcon::Grabbing));
        }
    }
    canvas.pan = held.from + moved;
    canvas.drag = Some(held);
    canvas.fit_pending = false;
}

pub(crate) fn end_pan(
    mut end: On<PointerDragEnd>,
    parents: Query<&ChildOf>,
    mut canvases: Query<&mut Canvas>,
    cursor: Option<ResMut<OverrideCursor>>,
) {
    let Some(root) = canvas_of(end.entity, &parents, &canvases) else { return };
    end.propagate(false);
    let Ok(mut canvas) = canvases.get_mut(root) else { return };
    if canvas.drag.take().is_some_and(|d| d.panning)
        && let Some(mut cursor) = cursor
    {
        cursor.0 = None;
    }
}

/// Fits each canvas waiting to be fitted once it has a size and its content an extent, asking for a frame while it
/// waits on its size alone.
pub(crate) fn fit_canvases(
    mut canvases: Query<(&mut Canvas, &CanvasExtent, &ComputedNode)>,
    redraw: Option<ResMut<Messages<RequestRedraw>>>,
) {
    let mut waiting = false;
    for (mut canvas, extent, computed) in &mut canvases {
        if !canvas.fit_pending || extent.0.cmple(Vec2::ZERO).any() {
            continue;
        }
        let size = computed.size() * computed.inverse_scale_factor();
        if size.cmple(Vec2::ZERO).any() {
            waiting = true;
            continue;
        }
        let (pan, zoom) = fitted(size, extent.0);
        canvas.set(pan, zoom);
    }
    if waiting && let Some(mut redraw) = redraw {
        redraw.write(RequestRedraw);
    }
}

/// Moves each canvas's content to its view, writing only a change.
pub(crate) fn place_canvases(
    canvases: Query<&Canvas, Changed<Canvas>>,
    mut contents: Query<(&CanvasContent, &mut UiTransform)>,
) {
    for (content, mut transform) in &mut contents {
        let Ok(canvas) = canvases.get(content.canvas) else { continue };
        let wanted = UiTransform {
            translation: Val2::px(canvas.pan.x, canvas.pan.y),
            scale: Vec2::splat(canvas.zoom),
            ..UiTransform::IDENTITY
        };
        transform.set_if_neq(wanted);
    }
}

#[cfg(test)]
mod tests {
    use bevy::camera::NormalizedRenderTarget;
    use bevy::picking::backend::HitData;
    use bevy::picking::events::Pointer;
    use bevy::picking::pointer::{Location, PointerId};
    use bevy::ui_widgets::{Activate, Button};

    use super::*;

    #[test]
    fn zooming_keeps_what_is_under_the_pointer_where_it_was() {
        let (pan, zoom) = (Vec2::new(30.0, -20.0), 0.8);
        let pointer = Vec2::new(200.0, 150.0);
        let under = (pointer - pan) / zoom;
        let (pan2, zoom2) = zoomed(pan, zoom, pointer, 1.5);
        assert!((zoom2 - 1.2).abs() < 1e-5);
        assert!((pan2 + under * zoom2 - pointer).length() < 1e-3);
    }

    #[test]
    fn the_zoom_stays_within_its_range() {
        assert_eq!(zoomed(Vec2::ZERO, 1.0, Vec2::ZERO, 100.0).1, CANVAS_MAX_ZOOM);
        assert_eq!(zoomed(Vec2::ZERO, 1.0, Vec2::ZERO, 0.001).1, CANVAS_MIN_ZOOM);
        let mut canvas = Canvas::default();
        canvas.set(Vec2::ZERO, 9.0);
        assert_eq!(canvas.zoom(), CANVAS_MAX_ZOOM);
    }

    #[test]
    fn a_wheel_turn_up_zooms_in_and_down_zooms_out_and_across_pans() {
        assert!(wheel_factor(40.0) > 1.0 && wheel_factor(-40.0) < 1.0);
        assert!((wheel_factor(ZOOM_DOUBLING) - 2.0).abs() < 1e-5);
        let mut canvas = Canvas::default();
        wheel_canvas(&mut canvas, Vec2::new(-25.0, 0.0), Vec2::new(100.0, 100.0));
        assert_eq!((canvas.pan(), canvas.zoom()), (Vec2::new(-25.0, 0.0), 1.0));
    }

    #[test]
    fn fitting_centers_the_content_and_never_zooms_in_past_its_size() {
        let (pan, zoom) = fitted(Vec2::new(800.0, 600.0), Vec2::new(200.0, 100.0));
        assert_eq!((pan, zoom), (Vec2::new(300.0, 250.0), 1.0));
        let (pan, zoom) = fitted(Vec2::new(424.0, 1000.0), Vec2::new(800.0, 100.0));
        assert!((zoom - 0.5).abs() < 1e-5);
        assert!((pan.x - 12.0).abs() < 1e-3 && (pan.y - 475.0).abs() < 1e-3);
        // Something huge zooms out no further than the least zoom.
        assert_eq!(fitted(Vec2::new(400.0, 400.0), Vec2::new(1e6, 1e6)).1, CANVAS_MIN_ZOOM);
    }

    /// The UI's scale in these tests: a UI pixel is a pixel and a half on screen.
    const UI_SCALE: f32 = 1.5;

    #[derive(Resource, Default)]
    struct Activated(Vec<Entity>);

    struct Board {
        app: App,
        canvas: Entity,
        content: Entity,
        item: Entity,
    }

    /// A canvas 400 by 300 with a button on it, laid out at 150%.
    fn board() -> Board {
        let mut app = crate::layout_test::layout_app(1.0, UI_SCALE);
        app.add_plugins(bevy::ui_widgets::ButtonPlugin)
            .init_resource::<Activated>()
            .add_observer(start_pan)
            .add_observer(pan)
            .add_observer(end_pan)
            .add_systems(Update, (fit_canvases, place_canvases).chain())
            .add_observer(|a: On<Activate>, mut seen: ResMut<Activated>| seen.0.push(a.entity));
        let world = app.world_mut();
        let holder = world.spawn(Node { width: px(400), height: px(300), ..default() }).id();
        let mut queue = bevy::ecs::world::CommandQueue::default();
        let mut commands = Commands::new(&mut queue, world);
        let parts = spawn_canvas(&mut commands);
        queue.apply(world);
        world.entity_mut(parts.root).insert(ChildOf(holder));
        let at = Node { position_type: PositionType::Absolute, width: px(40), height: px(20), ..default() };
        let item = world.spawn((at, Button)).id();
        world.entity_mut(item).insert(ChildOf(parts.content));
        app.update();
        Board { app, canvas: parts.root, content: parts.content, item }
    }

    fn pointer(ui: Vec2) -> Pointer {
        let target = NormalizedRenderTarget::None { width: 1, height: 1 };
        Pointer::new(PointerId::Mouse, Location { target, position: ui * UI_SCALE })
    }

    fn hit() -> HitData {
        HitData::new(Entity::PLACEHOLDER, 0.0, None, None)
    }

    /// Presses `entity` at `from` (UI pixels), drags it `by`, and lets go, as picking reports it.
    fn press_drag_release(app: &mut App, entity: Entity, from: Vec2, by: Vec2) {
        let button = PointerButton::Primary;
        let world = app.world_mut();
        world.trigger(PointerPress { entity, pointer: pointer(from), button, hit: hit(), count: 1 });
        world.flush();
        world.trigger(PointerDragStart { entity, pointer: pointer(from), button, hit: hit() });
        world.flush();
        let distance = by * UI_SCALE;
        world.trigger(PointerDrag { entity, pointer: pointer(from + by), button, distance, delta: distance });
        world.flush();
        app.update();
        let world = app.world_mut();
        let (to, duration) = (from + by, std::time::Duration::from_millis(200));
        world.trigger(PointerClick { entity, pointer: pointer(to), button, hit: hit(), duration, count: 1 });
        world.trigger(PointerRelease { entity, pointer: pointer(to), button, hit: hit() });
        world.trigger(PointerDragEnd { entity, pointer: pointer(to), button, distance });
        world.flush();
        app.update();
    }

    #[test]
    fn a_drag_past_the_threshold_pans_and_does_not_press_what_it_began_on() {
        let mut board = board();
        board.app.world_mut().get_mut::<Canvas>(board.canvas).unwrap().set(Vec2::ZERO, 1.0);
        let (item, canvas) = (board.item, board.canvas);
        press_drag_release(&mut board.app, item, Vec2::new(20.0, 20.0), Vec2::new(30.0, -10.0));
        let world = board.app.world();
        assert_eq!(world.get::<Canvas>(canvas).unwrap().pan(), Vec2::new(30.0, -10.0));
        assert!(world.resource::<Activated>().0.is_empty(), "a pan is not a press");
        let moved = world.get::<UiTransform>(board.content).unwrap().translation;
        assert_eq!(moved, Val2::px(30.0, -10.0));
    }

    #[test]
    fn a_press_that_moves_less_than_the_threshold_on_screen_is_still_a_press() {
        let mut board = board();
        board.app.world_mut().get_mut::<Canvas>(board.canvas).unwrap().set(Vec2::ZERO, 1.0);
        let (item, canvas) = (board.item, board.canvas);
        // Six pixels on screen is four UI pixels at 150%: under the threshold.
        press_drag_release(&mut board.app, item, Vec2::new(20.0, 20.0), Vec2::new(0.0, 6.0 / UI_SCALE));
        let world = board.app.world();
        assert_eq!(world.get::<Canvas>(canvas).unwrap().pan(), Vec2::ZERO);
        assert_eq!(world.resource::<Activated>().0, [item]);
    }

    #[test]
    fn a_new_canvas_fits_its_content_once_it_has_some_and_keeps_its_view_after() {
        let mut board = board();
        let canvas = board.canvas;
        assert!(board.app.world().get::<Canvas>(canvas).unwrap().fitting(), "nothing to fit yet");
        board.app.world_mut().entity_mut(canvas).insert(CanvasExtent(Vec2::new(200.0, 100.0)));
        board.app.update();
        let view = board.app.world().get::<Canvas>(canvas).unwrap().clone();
        assert!(!view.fitting());
        assert_eq!((view.pan(), view.zoom()), (Vec2::new(100.0, 100.0), 1.0));
        // New content leaves the view where it was until a fit is asked for.
        board.app.world_mut().entity_mut(canvas).insert(CanvasExtent(Vec2::new(2000.0, 100.0)));
        board.app.update();
        assert_eq!(board.app.world().get::<Canvas>(canvas).unwrap().pan(), Vec2::new(100.0, 100.0));
        board.app.world_mut().get_mut::<Canvas>(canvas).unwrap().fit();
        board.app.update();
        assert!(board.app.world().get::<Canvas>(canvas).unwrap().zoom() < 1.0);
    }

    #[test]
    fn the_canvas_clips_to_its_box() {
        let board = board();
        let node = board.app.world().get::<Node>(board.canvas).unwrap();
        assert_eq!(node.overflow, Overflow::clip());
        assert_eq!((node.min_width, node.min_height), (px(0), px(0)));
    }
}
