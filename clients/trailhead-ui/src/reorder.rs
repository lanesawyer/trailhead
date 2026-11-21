//! Reordering the items of one list by dragging them, or by Alt and an arrow key.
//!
//! Mark the container [`ReorderList`] (down or across) and each child that may move [`ReorderItem`], with a key that
//! stays with it. A child without one (a list's foot) is left where it is and nothing is dropped among it. A press that
//! moves the pointer further than [`DRAG_THRESHOLD`] lifts the item: it is drawn over its neighbours with an accent
//! border, follows the pointer along the list (and no further than the list's first and last items), and the others
//! slide aside to show where it will land. Releasing it there triggers one [`Reordered`] on the list. Escape, or a
//! release more than [`RELEASE_SLACK`] outside the list, puts everything back with no event. The press that lifted an
//! item never also activates it; one that stays under the threshold is an ordinary press.
//!
//! With an item focused, Alt and Up or Down (Left or Right across) moves it one place and triggers the same event.
//!
//! The kit only moves the items to show the drag (a [`UiTransform`], never their place among the children) and puts
//! them back on release. Applying the new order is the caller's, from its own data, in the frame it hears
//! [`Reordered`] (an observer, or a system before `PostUpdate`), so the list it keeps stays the one source of its
//! order. The list does not scroll itself while an item is dragged near its edge.

use std::time::Duration;

use bevy::ecs::system::SystemParam;
use bevy::feathers::controls::ButtonVariant;
use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::FocusedInput;
use bevy::picking::cursor::{EntityCursor, OverrideCursor};
use bevy::prelude::*;
use bevy::ui::Pressed;
use bevy::ui_widgets::ControlOrientation;
use bevy::window::{RequestRedraw, SystemCursorIcon};

use crate::hint::HintPopup;
use crate::{Appearance, BorderTone, NavItem, ease_out, patch_node, stroke, token};

/// How far the pointer moves, in UI pixels, before a press on an item becomes a drag.
pub const DRAG_THRESHOLD: f32 = 5.0;
/// How far outside its list, in UI pixels, an item can be released and still land.
pub const RELEASE_SLACK: f32 = 32.0;
/// How long the other items take to slide aside.
const SHIFT: Duration = Duration::from_millis(120);
/// A lifted item's place over its siblings.
const LIFTED_Z: i32 = 1;

/// A container whose [`ReorderItem`] children a person can put in another order.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ReorderList {
    pub orientation: ControlOrientation,
}

impl ReorderList {
    /// A list down the page: items move up and down, with Alt and Up or Down.
    pub fn vertical() -> Self {
        ReorderList { orientation: ControlOrientation::Vertical }
    }

    /// A list across: items move left and right, with Alt and Left or Right.
    pub fn horizontal() -> Self {
        ReorderList { orientation: ControlOrientation::Horizontal }
    }

    /// The keys that move a focused item, for its hint: "Alt+Up or Down".
    pub fn keys(self) -> &'static str {
        match self.orientation {
            ControlOrientation::Vertical => "Alt+Up or Down",
            ControlOrientation::Horizontal => "Alt+Left or Right",
        }
    }

    fn vertical_axis(self) -> bool {
        self.orientation == ControlOrientation::Vertical
    }
}

/// A child of a [`ReorderList`] that can be moved, and the key it is reported by.
#[derive(Component, Clone, Debug, PartialEq, Eq, Default)]
pub struct ReorderItem(pub String);

/// An item of `list` was moved: the one keyed `key`, from place `from` to place `to` among the list's items (children
/// without a [`ReorderItem`] are not counted). Apply it to your own order; the kit has put the items back.
#[derive(EntityEvent, Clone, Debug, PartialEq, Eq)]
pub struct Reordered {
    #[event_target]
    pub list: Entity,
    pub key: String,
    pub from: usize,
    pub to: usize,
}

/// On an item while it is lifted, with what it looked like before, which it gets back when it is let go.
#[derive(Component, Debug)]
pub struct Lifted {
    variant: Option<ButtonVariant>,
    border: UiRect,
    tone: Option<BorderTone>,
    z: ZIndex,
}

/// An item sliding aside (or back) to show where a dragged one will land: from `from` to `to` UI pixels along its list
/// (down it when `vertical`) since `since`, at `now`.
#[derive(Component, Clone, Copy, Debug)]
struct Shift {
    vertical: bool,
    from: f32,
    to: f32,
    since: Duration,
    now: f32,
}

/// An item's place along its list when it was lifted, in UI pixels.
#[derive(Clone, Copy, Debug)]
struct Slot {
    item: Entity,
    start: f32,
    size: f32,
}

impl Slot {
    fn end(self) -> f32 {
        self.start + self.size
    }

    fn center(self) -> f32 {
        self.start + self.size / 2.0
    }
}

/// The press on an item, from the moment it starts to move.
#[derive(Debug)]
struct Grab {
    item: Entity,
    list: Entity,
    vertical: bool,
    /// Past the threshold: drawn lifted, following the pointer.
    lifted: bool,
    /// Escape was pressed: everything went back, and the rest of the drag is ignored.
    cancelled: bool,
    from: usize,
    to: usize,
    /// How far the item has been dragged along the list, held to it.
    offset: f32,
    /// Whether the pointer is out of the list by more than [`RELEASE_SLACK`].
    outside: bool,
    slots: Vec<Slot>,
}

/// The drag under way, if one is.
#[derive(Resource, Default, Debug)]
pub(crate) struct ReorderDrag(Option<Grab>);

impl ReorderDrag {
    /// Whether an item is lifted now, while which hints stay hidden.
    pub(crate) fn lifted(&self) -> bool {
        self.0.as_ref().is_some_and(|g| g.lifted && !g.cancelled)
    }
}

/// Dragging and the keys that reorder [`ReorderList`]s. Part of `UiPlugin`; add it on its own only in a headless test
/// without `UiPlugin`.
pub struct ReorderPlugin;

impl Plugin for ReorderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ReorderDrag>()
            .add_observer(start_drag)
            .add_observer(drag)
            .add_observer(end_drag)
            .add_observer(on_key)
            .add_systems(Update, move_items.in_set(crate::UiPlace));
    }
}

/// What a drag reads and writes.
#[derive(SystemParam)]
struct Reorder<'w, 's> {
    drag: ResMut<'w, ReorderDrag>,
    items: Query<'w, 's, (&'static ReorderItem, &'static ChildOf)>,
    lists: Query<'w, 's, (&'static ReorderList, &'static Children)>,
    placed: Query<'w, 's, (&'static ComputedNode, &'static UiGlobalTransform)>,
    shifts: Query<'w, 's, &'static mut Shift>,
    nodes: Query<'w, 's, &'static mut Node>,
    variants: Query<'w, 's, &'static mut ButtonVariant>,
    looks: Query<'w, 's, (Option<&'static BorderTone>, &'static ZIndex, Option<&'static Lifted>)>,
    nav: Query<'w, 's, &'static mut NavItem>,
    hints: Query<'w, 's, Entity, With<HintPopup>>,
    cursor: Option<ResMut<'w, OverrideCursor>>,
    ui_scale: Option<Res<'w, UiScale>>,
    appearance: Option<Res<'w, Appearance>>,
    real: Res<'w, Time<Real>>,
    commands: Commands<'w, 's>,
}

impl Reorder<'_, '_> {
    fn scale(&self) -> f32 {
        self.ui_scale.as_ref().map_or(1.0, |s| s.0).max(f32::EPSILON)
    }

    /// The list `item` is an item of.
    fn list_of(&self, item: Entity) -> Option<(Entity, ReorderList)> {
        let (_, parent) = self.items.get(item).ok()?;
        let (list, _) = self.lists.get(parent.parent()).ok()?;
        Some((parent.parent(), *list))
    }

    /// The list's items, in order.
    fn items_of(&self, list: Entity) -> Vec<Entity> {
        let Ok((_, children)) = self.lists.get(list) else { return Vec::new() };
        children.iter().filter(|c| self.items.contains(*c)).collect()
    }

    /// Where a node is on screen, in UI pixels.
    fn rect(&self, entity: Entity) -> Option<Rect> {
        let (node, transform) = self.placed.get(entity).ok()?;
        let scale = node.inverse_scale_factor();
        Some(Rect::from_center_size(transform.translation * scale, node.size() * scale))
    }

    /// Each item's place along the list as it would be with nothing dragged.
    fn slots(&self, list: Entity, vertical: bool) -> Vec<Slot> {
        let along = |v: Vec2| if vertical { v.y } else { v.x };
        self.items_of(list)
            .into_iter()
            .map(|item| {
                let shifted = self.shifts.get(item).map_or(0.0, |s| s.now);
                let rect = self.rect(item).unwrap_or_default();
                Slot { item, start: along(rect.min) - shifted, size: along(rect.size()) }
            })
            .collect()
    }

    fn now(&self) -> Duration {
        self.real.elapsed()
    }

    fn still(&self) -> bool {
        self.appearance.as_ref().is_some_and(|a| a.reduce_motion())
    }

    /// Slides `item` to `to` along its list from wherever it is now.
    fn slide(&mut self, item: Entity, to: f32, vertical: bool) {
        let now = self.now();
        let still = self.still();
        match self.shifts.get_mut(item) {
            Ok(mut shift) if shift.to != to => {
                let from = if still { to } else { shift.now };
                *shift = Shift { vertical, from, to, since: now, now: from };
            }
            Ok(_) => {}
            Err(_) if to != 0.0 => {
                let from = if still { to } else { 0.0 };
                self.commands.entity(item).insert(Shift { vertical, from, to, since: now, now: from });
            }
            Err(_) => {}
        }
    }

    /// Lifts the grabbed item: no longer a press, drawn over the others, its place among them measured.
    fn lift(&mut self) {
        let Some(grab) = self.drag.0.as_ref() else { return };
        let (item, list, vertical) = (grab.item, grab.list, grab.vertical);
        let slots = self.slots(list, vertical);
        let from = slots.iter().position(|s| s.item == item).unwrap_or_default();
        if let Some(grab) = self.drag.0.as_mut() {
            *grab = Grab { lifted: true, from, to: from, slots, ..*grab };
        }
        let Ok((tone, z, None)) = self.looks.get(item) else { return };
        let (tone, z) = (tone.cloned(), *z);
        let variant = self.variants.get_mut(item).ok().map(|mut v| std::mem::replace(&mut *v, ButtonVariant::Normal));
        let mut border = UiRect::default();
        if let Ok(node) = self.nodes.get_mut(item) {
            border = node.border;
            patch_node(node, |n| n.border = UiRect::all(px(stroke::GRIP)));
        }
        self.commands.entity(item).remove::<Pressed>().insert((
            Lifted { variant, border, tone, z },
            BorderTone(token::FILL_ACCENT),
            ZIndex(LIFTED_Z),
        ));
        let shown: Vec<Entity> = self.hints.iter().collect();
        for hint in shown {
            self.commands.entity(hint).despawn();
        }
        if let Some(cursor) = self.cursor.as_mut() {
            cursor.0 = Some(EntityCursor::System(SystemCursorIcon::Grabbing));
        }
    }

    /// Gives `item` back the look it had before it was lifted.
    fn set_down(&mut self, item: Entity) {
        if let Some(cursor) = self.cursor.as_mut() {
            cursor.0 = None;
        }
        let Ok((_, _, Some(lifted))) = self.looks.get(item) else { return };
        let (variant, border, tone, z) = (lifted.variant.clone(), lifted.border, lifted.tone.clone(), lifted.z);
        if let (Some(old), Ok(mut variant)) = (variant, self.variants.get_mut(item)) {
            *variant = old;
        }
        if let Ok(node) = self.nodes.get_mut(item) {
            patch_node(node, |n| n.border = border);
        }
        // A nav item's mark may have moved while it was lifted, and its paint waited.
        if let Ok(mut nav) = self.nav.get_mut(item) {
            nav.set_changed();
        }
        let mut entity = self.commands.entity(item);
        entity.remove::<Lifted>().insert(z);
        match tone {
            Some(tone) => entity.insert(tone),
            None => entity.remove::<BorderTone>(),
        };
    }

    /// Follows the pointer `distance` (logical pixels) from where the drag began, at `pointer` on screen.
    fn follow(&mut self, distance: Vec2, pointer: Vec2) {
        let scale = self.scale();
        let outside = self.outside(pointer);
        let Some(grab) = self.drag.0.as_mut() else { return };
        let Some(dragged) = grab.slots.get(grab.from).copied() else { return };
        let (first, last) = (grab.slots[0], grab.slots[grab.slots.len() - 1]);
        let along = if grab.vertical { distance.y } else { distance.x } / scale;
        grab.offset = along.clamp(first.start - dragged.start, last.end() - dragged.end());
        grab.outside = outside;
        // An item is passed once the dragged one's leading edge crosses its middle.
        let (start, end) = (dragged.start + grab.offset, dragged.end() + grab.offset);
        let from = grab.from;
        let before =
            |(at, slot): &(usize, &Slot)| if *at < from { slot.center() < start } else { slot.center() <= end };
        grab.to = if outside {
            from
        } else {
            grab.slots.iter().enumerate().filter(|(at, _)| *at != from).filter(before).count()
        };
        let (targets, vertical) = (landing(&grab.slots, grab.from, grab.to), grab.vertical);
        for (item, to) in targets.into_iter().filter(|(item, _)| *item != dragged.item) {
            self.slide(item, to, vertical);
        }
    }

    /// Whether `pointer` (logical pixels) is out of the grabbed item's list by more than [`RELEASE_SLACK`].
    fn outside(&self, pointer: Vec2) -> bool {
        let Some(grab) = self.drag.0.as_ref() else { return false };
        let Some(rect) = self.rect(grab.list) else { return false };
        !rect.inflate(RELEASE_SLACK).contains(pointer / self.scale())
    }

    /// Puts every item of the drag back where it belongs: at once when the caller is about to move them, else sliding.
    fn put_back(&mut self, grab: &Grab, at_once: bool) {
        for slot in &grab.slots {
            if at_once {
                self.commands.entity(slot.item).remove::<Shift>().insert(UiTransform::IDENTITY);
            } else if slot.item != grab.item {
                self.slide(slot.item, 0.0, grab.vertical);
            }
        }
        if grab.lifted && !at_once {
            let now = self.now();
            let from = if self.still() { 0.0 } else { grab.offset };
            let back = Shift { vertical: grab.vertical, from, to: 0.0, since: now, now: from };
            self.commands.entity(grab.item).insert(back);
        }
    }

    /// Escape: everything back, no event, and the rest of this drag ignored.
    fn cancel(&mut self) {
        let Some(grab) = self.drag.0.as_mut().filter(|g| g.lifted && !g.cancelled) else { return };
        let done = Grab { slots: std::mem::take(&mut grab.slots), ..*grab };
        grab.cancelled = true;
        self.put_back(&done, false);
        self.set_down(done.item);
    }
}

/// Where each item sits, relative to where it was, with the one at `from` moved to `to`: each in the new order starts
/// where the last ended and a gap after, the gap the list's own.
fn landing(slots: &[Slot], from: usize, to: usize) -> Vec<(Entity, f32)> {
    let (Some(first), Some(last)) = (slots.first(), slots.last()) else { return Vec::new() };
    let gaps = slots.len().saturating_sub(1).max(1) as f32;
    let gap = (last.end() - first.start - slots.iter().map(|s| s.size).sum::<f32>()) / gaps;
    let mut order: Vec<Slot> = slots.to_vec();
    let moved = order.remove(from);
    order.insert(to.min(order.len()), moved);
    let mut at = first.start;
    order
        .into_iter()
        .map(|slot| {
            let shift = at - slot.start;
            at += slot.size + gap;
            (slot.item, shift)
        })
        .collect()
}

fn start_drag(mut start: On<PointerDragStart>, mut reorder: Reorder) {
    if start.button != PointerButton::Primary {
        return;
    }
    let item = start.entity;
    let Some((list, kind)) = reorder.list_of(item) else { return };
    start.propagate(false);
    if let Some(old) = reorder.drag.0.take() {
        reorder.put_back(&old, true);
        reorder.set_down(old.item);
    }
    reorder.drag.0 = Some(Grab {
        item,
        list,
        vertical: kind.vertical_axis(),
        lifted: false,
        cancelled: false,
        from: 0,
        to: 0,
        offset: 0.0,
        outside: false,
        slots: Vec::new(),
    });
}

fn drag(mut drag: On<PointerDrag>, mut reorder: Reorder) {
    let Some(grab) = reorder.drag.0.as_ref().filter(|g| g.item == drag.entity) else { return };
    drag.propagate(false);
    if grab.cancelled {
        return;
    }
    if !grab.lifted {
        if drag.distance.length() / reorder.scale() < DRAG_THRESHOLD {
            return;
        }
        reorder.lift();
    }
    reorder.follow(drag.distance, drag.pointer.position);
}

fn end_drag(mut end: On<PointerDragEnd>, mut reorder: Reorder) {
    if reorder.drag.0.as_ref().is_none_or(|g| g.item != end.entity) {
        return;
    }
    end.propagate(false);
    let outside = reorder.outside(end.pointer.position);
    let Some(grab) = reorder.drag.0.take() else { return };
    if !grab.lifted || grab.cancelled {
        return;
    }
    let lands = !outside && grab.to != grab.from;
    reorder.put_back(&grab, lands);
    reorder.set_down(grab.item);
    if lands && let Ok((key, _)) = reorder.items.get(grab.item) {
        let key = key.0.clone();
        reorder.commands.trigger(Reordered { list: grab.list, key, from: grab.from, to: grab.to });
    }
}

/// Escape lets go of a lifted item; Alt and an arrow along the list moves a focused item one place.
fn on_key(mut key: On<FocusedInput<KeyboardInput>>, held: Option<Res<ButtonInput<KeyCode>>>, mut reorder: Reorder) {
    let start = key.original_event_target();
    let pressed = &key.input;
    if key.event_target() != start || !pressed.state.is_pressed() {
        return;
    }
    if pressed.key_code == KeyCode::Escape {
        if reorder.drag.lifted() {
            key.propagate(false);
            reorder.cancel();
        }
        return;
    }
    let alt = held.is_some_and(|h| h.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]));
    if !alt || reorder.drag.0.is_some() {
        return;
    }
    let Some((list, kind)) = reorder.list_of(start) else { return };
    let step = match (kind.vertical_axis(), pressed.key_code) {
        (true, KeyCode::ArrowUp) | (false, KeyCode::ArrowLeft) => -1,
        (true, KeyCode::ArrowDown) | (false, KeyCode::ArrowRight) => 1,
        _ => return,
    };
    key.propagate(false);
    let items = reorder.items_of(list);
    let Some(from) = items.iter().position(|e| *e == start) else { return };
    let Some(to) = from.checked_add_signed(step).filter(|to| *to < items.len()) else { return };
    let Ok((item, _)) = reorder.items.get(start) else { return };
    let key = item.0.clone();
    reorder.commands.trigger(Reordered { list, key, from, to });
}

/// Moves the lifted item to the pointer and the others along their slides, asking for a redraw while any slides.
fn move_items(
    drag: Res<ReorderDrag>,
    real: Res<Time<Real>>,
    mut shifts: Query<(Entity, &mut Shift)>,
    mut transforms: Query<&mut UiTransform>,
    redraw: Option<ResMut<Messages<RequestRedraw>>>,
    mut commands: Commands,
) {
    let place = |vertical: bool, at: f32| {
        let translation = if vertical { Val2::px(0.0, at) } else { Val2::px(at, 0.0) };
        UiTransform::from_translation(translation)
    };
    let mut moving = false;
    for (item, mut shift) in &mut shifts {
        let t = real.elapsed().saturating_sub(shift.since).as_secs_f32() / SHIFT.as_secs_f32();
        shift.now = shift.from + (shift.to - shift.from) * ease_out(t);
        if let Ok(mut transform) = transforms.get_mut(item) {
            transform.set_if_neq(place(shift.vertical, shift.now));
        }
        if t < 1.0 {
            moving = true;
        } else if shift.to == 0.0 {
            commands.entity(item).remove::<Shift>();
        }
    }
    if let Some(grab) = drag.0.as_ref().filter(|g| g.lifted && !g.cancelled)
        && let Ok(mut transform) = transforms.get_mut(grab.item)
    {
        transform.set_if_neq(place(grab.vertical, grab.offset));
    }
    if moving && let Some(mut redraw) = redraw {
        redraw.write(RequestRedraw);
    }
}

#[cfg(test)]
mod tests {
    use bevy::camera::NormalizedRenderTarget;
    use bevy::input::keyboard::Key;
    use bevy::picking::backend::HitData;
    use bevy::picking::events::Pointer;
    use bevy::picking::pointer::{Location, PointerId};
    use bevy::ui_widgets::{Activate, Button};

    use super::*;
    use crate::hint::HintPopup;
    use crate::test_support::{press_escape, press_key};

    /// The UI's scale in these tests: a UI pixel is a pixel and a half on screen.
    const UI_SCALE: f32 = 1.5;
    /// An item's height, and the gap between items, in UI pixels.
    const ITEM: f32 = 24.0;
    const GAP: f32 = 8.0;

    #[derive(Resource, Default)]
    struct Heard {
        reordered: Vec<(String, usize, usize)>,
        activated: Vec<Entity>,
    }

    struct List {
        app: App,
        list: Entity,
        items: Vec<Entity>,
    }

    /// A list down the page of four buttons keyed `a` to `d`, then a foot that is not an item, laid out at 150%.
    fn list() -> List {
        let mut app = crate::layout_test::layout_app(1.0, UI_SCALE);
        app.add_plugins((ReorderPlugin, bevy::ui_widgets::ButtonPlugin))
            .init_resource::<Heard>()
            .init_resource::<ButtonInput<KeyCode>>()
            .add_observer(|r: On<Reordered>, mut heard: ResMut<Heard>| {
                heard.reordered.push((r.key.clone(), r.from, r.to));
            })
            .add_observer(|a: On<Activate>, mut heard: ResMut<Heard>| heard.activated.push(a.entity));
        let world = app.world_mut();
        let column = Node {
            flex_direction: FlexDirection::Column,
            width: px(48),
            height: px(300),
            row_gap: px(GAP),
            ..default()
        };
        let list = world.spawn((column, ReorderList::vertical())).id();
        let items = ["a", "b", "c", "d"]
            .map(|key| {
                let node = Node { width: px(ITEM), height: px(ITEM), flex_shrink: 0.0, ..default() };
                let item = (node, Button, ButtonVariant::Plain, NavItem::default(), ReorderItem(key.into()));
                world.spawn((item, ChildOf(list))).id()
            })
            .to_vec();
        let foot = Node { margin: UiRect::top(Val::Auto), height: px(ITEM), ..default() };
        world.spawn((foot, ChildOf(list)));
        app.update();
        List { app, list, items }
    }

    fn pointer(ui: Vec2) -> Pointer {
        let target = NormalizedRenderTarget::None { width: 1, height: 1 };
        Pointer::new(PointerId::Mouse, Location { target, position: ui * UI_SCALE })
    }

    fn hit() -> HitData {
        HitData::new(Entity::PLACEHOLDER, 0.0, None, None)
    }

    /// The middle of item `at`, in UI pixels.
    fn middle(at: usize) -> Vec2 {
        Vec2::new(ITEM / 2.0, ITEM / 2.0 + at as f32 * (ITEM + GAP))
    }

    impl List {
        fn world(&mut self) -> &mut World {
            self.app.world_mut()
        }

        /// Presses item `at` and drags it by `by` UI pixels, as picking reports it, then runs a frame.
        fn press_and_drag(&mut self, at: usize, by: Vec2) {
            let (item, from) = (self.items[at], middle(at));
            let button = PointerButton::Primary;
            let world = self.world();
            world.trigger(PointerPress { entity: item, pointer: pointer(from), button, hit: hit(), count: 1 });
            world.flush();
            world.trigger(PointerDragStart { entity: item, pointer: pointer(from), button, hit: hit() });
            world.flush();
            let distance = by * UI_SCALE;
            world.trigger(PointerDrag { entity: item, pointer: pointer(from + by), button, distance, delta: distance });
            world.flush();
            self.app.update();
        }

        /// Lets item `at` go `by` UI pixels from where it was pressed, the pointer still over it, as picking reports a
        /// release: a click, then the release, then the drag's end.
        fn release(&mut self, at: usize, by: Vec2) {
            let (item, to) = (self.items[at], middle(at) + by);
            let button = PointerButton::Primary;
            let world = self.world();
            let duration = Duration::from_millis(300);
            world.trigger(PointerClick { entity: item, pointer: pointer(to), button, hit: hit(), duration, count: 1 });
            world.trigger(PointerRelease { entity: item, pointer: pointer(to), button, hit: hit() });
            world.trigger(PointerDragEnd { entity: item, pointer: pointer(to), button, distance: by * UI_SCALE });
            world.flush();
            self.app.update();
        }

        fn heard(&self) -> &Heard {
            self.app.world().resource::<Heard>()
        }

        fn offset(&self, at: usize) -> Val2 {
            self.app.world().get::<UiTransform>(self.items[at]).unwrap().translation
        }

        fn shift_to(&self, at: usize) -> Option<f32> {
            self.app.world().get::<Shift>(self.items[at]).map(|s| s.to)
        }
    }

    #[test]
    fn dragging_an_item_past_two_others_moves_them_aside_and_reports_it_once_without_a_press() {
        let mut list = list();
        let step = ITEM + GAP;
        list.press_and_drag(0, Vec2::new(0.0, step * 2.0 + 2.0));
        let a = list.items[0];
        let world = list.world();
        assert!(world.get::<Lifted>(a).is_some(), "lifted");
        assert!(world.get::<Pressed>(a).is_none(), "no longer a press");
        assert_eq!(world.get::<ZIndex>(a), Some(&ZIndex(LIFTED_Z)), "over the others");
        assert_eq!(world.get::<BorderTone>(a), Some(&BorderTone(token::FILL_ACCENT)));
        assert_eq!(world.get::<ButtonVariant>(a), Some(&ButtonVariant::Normal), "on a surface of its own");
        assert_eq!(list.offset(0), Val2::px(0.0, step * 2.0 + 2.0), "following the pointer");
        assert_eq!((list.shift_to(1), list.shift_to(2), list.shift_to(3)), (Some(-step), Some(-step), None));

        list.release(0, Vec2::new(0.0, step * 2.0 + 2.0));
        assert_eq!(list.heard().reordered, [("a".to_string(), 0, 2)]);
        assert!(list.heard().activated.is_empty(), "the press that lifted it doesn't also activate it");
        let (items, root) = (list.items.clone(), list.list);
        let world = list.world();
        assert!(world.get::<Lifted>(a).is_none());
        assert_eq!(world.get::<ButtonVariant>(a), Some(&ButtonVariant::Plain), "its own look back");
        assert_eq!(world.get::<ZIndex>(a), Some(&ZIndex(0)));
        assert!(world.get::<BorderTone>(a).is_none());
        for at in 0..4 {
            assert_eq!(list.offset(at), Val2::ZERO, "everything put back for the caller to move");
        }
        let children: Vec<Entity> = list.world().get::<Children>(root).unwrap().iter().take(4).collect();
        assert_eq!(children, items, "the kit leaves the order to the caller");
    }

    #[test]
    fn a_press_that_moves_less_than_the_threshold_on_screen_is_still_a_press() {
        let mut list = list();
        // Six pixels on screen is four UI pixels at 150%: under the threshold.
        let by = Vec2::new(0.0, 6.0 / UI_SCALE);
        list.press_and_drag(1, by);
        let b = list.items[1];
        assert!(list.world().get::<Lifted>(b).is_none());
        list.release(1, by);
        assert_eq!(list.heard().activated, [b]);
        assert!(list.heard().reordered.is_empty());
    }

    #[test]
    fn the_dragged_item_stays_between_the_first_and_last_items_and_never_over_the_foot() {
        let mut list = list();
        // Down past the foot, but still over the list.
        list.press_and_drag(2, Vec2::new(0.0, 200.0));
        assert_eq!(list.offset(2), Val2::px(0.0, ITEM + GAP), "no further than the last item's place");
        list.release(2, Vec2::new(0.0, 200.0));
        assert_eq!(list.heard().reordered, [("c".to_string(), 2, 3)]);
        let mut list = self::list();
        list.press_and_drag(2, Vec2::new(0.0, -400.0));
        assert_eq!(list.offset(2), Val2::px(0.0, -(ITEM + GAP) * 2.0));
    }

    #[test]
    fn escape_puts_everything_back_and_nothing_is_reported_or_pressed() {
        let mut list = list();
        list.press_and_drag(0, Vec2::new(0.0, 40.0));
        assert_eq!(list.shift_to(1), Some(-(ITEM + GAP)));
        let a = list.items[0];
        press_escape(list.world(), a);
        list.world().flush();
        assert!(list.world().get::<Lifted>(a).is_none());
        assert_eq!((list.shift_to(0), list.shift_to(1)), (Some(0.0), Some(0.0)), "sliding back");
        // The rest of the drag is ignored.
        let (by, button) = (Vec2::new(0.0, 80.0), PointerButton::Primary);
        let (pointer, distance) = (pointer(middle(0) + by), by * UI_SCALE);
        list.world().trigger(PointerDrag { entity: a, pointer, button, distance, delta: distance });
        list.app.update();
        assert!(list.world().get::<Lifted>(a).is_none(), "not lifted again");
        assert!(list.shift_to(1).is_none_or(|to| to == 0.0), "and nothing makes room for it");
        list.release(0, by);
        assert!(list.heard().reordered.is_empty());
    }

    #[test]
    fn what_slides_back_takes_a_moment_asking_for_redraws_and_then_rests() {
        use bevy::time::TimeUpdateStrategy;
        let mut list = list();
        list.app.add_message::<RequestRedraw>();
        list.app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(16)));
        list.press_and_drag(0, Vec2::new(0.0, 40.0));
        let a = list.items[0];
        press_escape(list.world(), a);
        list.release(0, Vec2::new(0.0, 40.0));
        let mut redraws = 0;
        for _ in 0..12 {
            list.app.update();
            redraws += list.world().resource_mut::<Messages<RequestRedraw>>().drain().count();
        }
        assert!(redraws > 0);
        for at in 0..4 {
            assert_eq!((list.offset(at), list.shift_to(at)), (Val2::ZERO, None), "item {at} at rest");
        }
        list.app.update();
        assert_eq!(list.world().resource_mut::<Messages<RequestRedraw>>().drain().count(), 0, "and quiet");
    }

    #[test]
    fn letting_go_well_outside_the_list_puts_it_back() {
        let mut list = list();
        let by = Vec2::new(48.0 + RELEASE_SLACK + 10.0, 40.0);
        list.press_and_drag(0, by);
        assert_eq!(list.shift_to(1), None, "out of the list, the others show it going back");
        list.release(0, by);
        assert!(list.heard().reordered.is_empty() && list.heard().activated.is_empty());
        // A little outside still lands.
        let mut list = self::list();
        let by = Vec2::new(48.0, 40.0);
        list.press_and_drag(0, by);
        list.release(0, by);
        assert_eq!(list.heard().reordered, [("a".to_string(), 0, 1)]);
    }

    #[test]
    fn alt_and_an_arrow_along_the_list_moves_the_focused_item_one_place() {
        let mut list = list();
        let (a, b) = (list.items[0], list.items[1]);
        let world = list.world();
        press_key(world, b, KeyCode::ArrowUp, Key::ArrowUp);
        world.flush();
        assert!(world.resource::<Heard>().reordered.is_empty(), "not without Alt");
        world.resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::AltLeft);
        press_key(world, b, KeyCode::ArrowUp, Key::ArrowUp);
        press_key(world, a, KeyCode::ArrowUp, Key::ArrowUp);
        press_key(world, a, KeyCode::ArrowRight, Key::ArrowRight);
        press_key(world, a, KeyCode::ArrowDown, Key::ArrowDown);
        world.flush();
        let heard = &world.resource::<Heard>().reordered;
        assert_eq!(*heard, [("b".to_string(), 1, 0), ("a".to_string(), 0, 1)], "the first goes no higher");
    }

    #[test]
    fn a_list_not_marked_is_left_alone() {
        let mut list = list();
        let (root, a) = (list.list, list.items[0]);
        list.world().entity_mut(root).remove::<ReorderList>();
        list.press_and_drag(0, Vec2::new(0.0, 80.0));
        assert!(list.world().get::<Lifted>(a).is_none());
        list.release(0, Vec2::new(0.0, 80.0));
        assert!(list.heard().reordered.is_empty());
        assert_eq!(list.heard().activated, [a], "an ordinary button, dragged off and back");
    }

    #[test]
    fn a_hint_shown_goes_when_an_item_is_lifted() {
        let mut list = list();
        let a = list.items[0];
        let hint = list.world().spawn((HintPopup, ChildOf(a))).id();
        list.press_and_drag(0, Vec2::new(0.0, 40.0));
        assert!(list.world().get_entity(hint).is_err());
        assert!(list.world().resource::<ReorderDrag>().lifted());
    }

    #[test]
    fn the_others_land_one_after_another_a_gap_apart() {
        let e = |i| Entity::from_raw_u32(i).unwrap();
        let slots = [(0.0, 10.0), (14.0, 20.0), (38.0, 10.0)].map(|(start, size)| Slot { item: e(0), start, size });
        let slots: Vec<Slot> = slots.iter().enumerate().map(|(i, s)| Slot { item: e(i as u32 + 1), ..*s }).collect();
        let shifts: Vec<f32> = landing(&slots, 0, 2).into_iter().map(|(_, s)| s).collect();
        // b and c move up by a's size and a gap; a lands after c.
        assert_eq!(shifts, [-14.0, -14.0, 38.0]);
        assert_eq!(landing(&slots, 1, 1).into_iter().map(|(_, s)| s).collect::<Vec<_>>(), [0.0, 0.0, 0.0]);
    }
}
