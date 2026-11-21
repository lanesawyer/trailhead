//! The trail map as a 3D peak (`docs/factory/ui.md#views`, the spike of TRAIL-1163): one low-poly, flat-shaded peak
//! grown from a fixed seed, a switchback trail wound up its face in the accent, and a hiker on it for each live run,
//! as high as its run's phase has got, labelled with its task's key. The camera turns slowly round the peak while
//! nobody touches it; a drag orbits and the wheel zooms. Clicking a hiker opens its task.
//!
//! The geometry ([`terrain`]), where a run stands ([`place`]) and the orbit ([`orbit`]) are plain functions and
//! state, tested without a window. Drawing is here: on entering [`MapMode::Peak`] the scene is spawned (a 3D camera
//! under the shell's UI camera, a light, the peak, the ground, the trail and the hikers' root), on leaving it is
//! despawned, and in between it is updated in place.
//!
//! The 3D camera renders first (order `-1`), clearing to the sky; the shell's UI camera draws over it without
//! clearing while the peak is shown, so the window's UI stays on top and the sky shows wherever no node is drawn. The
//! map's panel lets the pointer through to the meshes under it ([`Pickable`] that doesn't block), and takes the drags
//! and wheel that land on it, not on a node above it, for the orbit.

pub mod orbit;
pub mod place;
pub mod terrain;

use bevy::asset::RenderAssetUsages;
use bevy::camera::{RenderTarget, Viewport};
use bevy::mesh::{ConeAnchor, ConeMeshBuilder, Indices, PrimitiveTopology};
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::picking::mesh_picking::{MeshPickingCamera, MeshPickingPlugin, MeshPickingSettings};
use bevy::prelude::*;
use bevy::render::view::{DebandDither, Tonemapping};
use bevy::window::RequestRedraw;
use trailhead_model::Task;
use trailhead_shell::mirror::Mirrored;
use trailhead_ui::{Appearance, DRAG_THRESHOLD, ThemeColors};

use crate::board::View;
use crate::factory::Run;
use crate::inspector::{Selection, Subject};
use crate::map::MapPanel;
use crate::widgets::{Keyed, Surface, size, space, text_toned, token};

use orbit::Orbit;
use place::Hiker;
use terrain::{Band, Facets, Peak, Trail};

/// How the trail map is drawn: the 3D peak, or the flat map of camps and waypoints it is replacing, kept until the
/// peak shows everything the flat map does (TRAIL-1164..1170).
#[derive(SubStates, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[source(View = View::Map)]
pub enum MapMode {
    #[default]
    Peak,
    Flat,
}

impl MapMode {
    pub const ALL: [MapMode; 2] = [MapMode::Peak, MapMode::Flat];

    pub fn label(self) -> &'static str {
        match self {
            MapMode::Peak => "Peak",
            MapMode::Flat => "Flat",
        }
    }
}

/// What the camera looks at: a little up the peak, so the summit sits above the middle.
const LOOK_AT: Vec3 = Vec3::new(0.0, terrain::HEIGHT * 0.32, 0.0);
/// How far above the facets the trail is drawn, so it doesn't flicker into them.
const TRAIL_LIFT: f32 = 0.05;
const TRAIL_WIDTH: f32 = 0.12;
/// A hiker: a small cone standing on the trail.
const HIKER_RADIUS: f32 = 0.24;
const HIKER_HEIGHT: f32 = 0.75;
/// The flat ground round the foot reaches this many radii out, and fades into the sky from the first distance to the
/// second, so its edge never shows.
const GROUND_REACH: f32 = 12.0;
const HAZE: (f32, f32) = (45.0, 110.0);
/// The light, by night and by day, in lux, and the sky's share of it in the shadows.
const MOONLIGHT: f32 = 2_800.0;
const SUNLIGHT: f32 = 9_000.0;
const NIGHT_AMBIENT: f32 = 380.0;
const DAY_AMBIENT: f32 = 700.0;
/// How brightly a hiker glows in its own color, as a headlamp would.
const HIKER_GLOW: f32 = 0.6;

/// The orbit the camera starts on: three quarters on to the trail's face, a little above the foot.
fn first_orbit() -> Orbit {
    Orbit::new(std::f32::consts::FRAC_PI_2 - terrain::FACE + 0.45, 0.42, 27.0)
}

impl Default for Orbit {
    fn default() -> Self {
        first_orbit()
    }
}

/// Draws the trail map as the peak while in [`MapMode::Peak`].
pub struct PeakUiPlugin;

impl Plugin for PeakUiPlugin {
    fn build(&self, app: &mut App) {
        // Mesh picking is a backend of Bevy's picking: where the app picks (the window, not a headless test), the
        // peak's meshes are picked too.
        if app.is_plugin_added::<bevy::picking::PickingPlugin>() && !app.is_plugin_added::<MeshPickingPlugin>() {
            app.add_plugins(MeshPickingPlugin);
        }
        // Only the peak's meshes, seen through its camera, are picked: never a mesh some other view draws.
        app.insert_resource(MeshPickingSettings { require_markers: true, ..default() })
            .add_sub_state::<MapMode>()
            .init_resource::<Orbit>()
            .add_systems(OnEnter(MapMode::Peak), spawn_scene)
            .add_systems(OnExit(MapMode::Peak), despawn_scene)
            .add_observer(on_press)
            .add_observer(on_release)
            .add_observer(on_drag)
            .add_observer(on_scroll)
            .add_systems(
                Update,
                (fit_camera, turn_camera, paint_scene, sync_hikers, sync_labels, place_labels)
                    .chain()
                    .run_if(in_state(MapMode::Peak)),
            );
    }
}

/// Everything the peak spawned, despawned when it goes.
#[derive(Component)]
struct PeakPart;

/// The peak's 3D camera.
#[derive(Component)]
struct PeakCamera;

#[derive(Component)]
struct PeakLight;

/// The root the hikers stand under.
#[derive(Component)]
struct HikerRoot;

/// A hiker, for its task.
#[derive(Component)]
struct HikerOf(String);

/// A hiker's label, for its task.
#[derive(Component)]
struct LabelOf(String);

/// The layer over the scene the labels are placed in.
#[derive(Component)]
struct LabelLayer;

/// What the scene keeps while it is shown: its shape, for the labels to tell what it hides, and its materials, each
/// with the token it is painted in.
#[derive(Resource)]
struct Scene3d {
    peak: Peak,
    trail: Trail,
    materials: Vec<(crate::widgets::Token, Handle<StandardMaterial>)>,
    hiker_mesh: Handle<Mesh>,
    hiker_material: Handle<StandardMaterial>,
    /// How the shell's camera cleared before the peak took the background, to put back.
    ui_clear: ClearColorConfig,
    /// Whether a press on the scene has become a drag, so its release opens no hiker.
    dragged: bool,
}

fn mesh(positions: Vec<[f32; 3]>, normals: Vec<[f32; 3]>, indices: Vec<u32>) -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_indices(Indices::U32(indices))
}

/// The facets of one band, as a mesh of their own, so each band wears its token's material.
fn band_mesh(facets: &Facets, band: Band) -> Mesh {
    let (mut positions, mut normals, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    for (i, b) in facets.bands.iter().enumerate() {
        if *b == band {
            for k in 3 * i..3 * i + 3 {
                indices.push(positions.len() as u32);
                positions.push(facets.positions[k]);
                normals.push(facets.normals[k]);
            }
        }
    }
    mesh(positions, normals, indices)
}

/// The trail as a thin ribbon lying on the facets.
fn trail_mesh(peak: &Peak, trail: &Trail) -> Mesh {
    let points = trail.points();
    let (mut positions, mut indices) = (Vec::new(), Vec::new());
    for (i, p) in points.iter().enumerate() {
        let ahead = points[(i + 1).min(points.len() - 1)] - points[i.saturating_sub(1)];
        let side = Vec3::new(-ahead.z, 0.0, ahead.x).normalize_or_zero() * TRAIL_WIDTH / 2.0;
        for edge in [*p - side, *p + side] {
            let ground = peak.height_at(Vec2::new(edge.x, edge.z)).max(p.y);
            positions.push([edge.x, ground + TRAIL_LIFT, edge.z]);
        }
        if i > 0 {
            let at = 2 * i as u32;
            indices.extend([at - 2, at - 1, at, at - 1, at + 1, at]);
        }
    }
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    mesh(positions, normals, indices)
}

fn spawn_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut ui_camera: Query<&mut Camera, With<Camera2d>>,
) {
    let peak = Peak::new(terrain::SEED);
    let trail = Trail::new(&peak);
    let facets = peak.facets();
    let mut lit = |unlit: bool| {
        materials.add(StandardMaterial { perceptual_roughness: 1.0, reflectance: 0.1, unlit, ..default() })
    };
    let bands = [(Band::Ground, token::PEAK_GROUND), (Band::Rock, token::PEAK_ROCK), (Band::Snow, token::PEAK_SNOW)];
    let mut painted = Vec::new();
    for (band, tone) in bands {
        let material = lit(false);
        let facets = Mesh3d(meshes.add(band_mesh(&facets, band)));
        commands.spawn((facets, MeshMaterial3d(material.clone()), Pickable::default(), PeakPart));
        painted.push((tone, material));
    }
    // The ground under the foot is the foothills' own color, so the two meet without a seam.
    let ground = Circle::new(terrain::RADIUS * GROUND_REACH);
    commands.spawn((
        Mesh3d(meshes.add(ground)),
        MeshMaterial3d(painted[0].1.clone()),
        Pickable::default(),
        Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
            .with_translation(Vec3::NEG_Y * 0.01),
        PeakPart,
    ));
    let trail_material = lit(true);
    commands.spawn((Mesh3d(meshes.add(trail_mesh(&peak, &trail))), MeshMaterial3d(trail_material.clone()), PeakPart));
    painted.push((token::FILL_ACCENT, trail_material));
    let hiker_material = lit(false);
    painted.push((token::AGENT, hiker_material.clone()));

    commands.spawn((
        Camera3d::default(),
        Camera { order: -1, ..default() },
        Tonemapping::None,
        DebandDither::Disabled,
        AmbientLight::default(),
        DistanceFog { falloff: FogFalloff::Linear { start: HAZE.0, end: HAZE.1 }, ..default() },
        MeshPickingCamera,
        PeakCamera,
        PeakPart,
    ));
    commands.spawn((
        DirectionalLight::default(),
        Transform::from_xyz(-1.0, 1.1, 0.7).looking_at(Vec3::ZERO, Vec3::Y),
        PeakLight,
        PeakPart,
    ));
    commands.spawn((Transform::default(), Visibility::default(), HikerRoot, PeakPart));

    let mut ui_clear = ClearColorConfig::Default;
    for mut camera in &mut ui_camera {
        ui_clear = std::mem::replace(&mut camera.clear_color, ClearColorConfig::None);
    }
    let hiker = ConeMeshBuilder::new(HIKER_RADIUS, HIKER_HEIGHT, 6).anchor(ConeAnchor::Base);
    commands.insert_resource(Scene3d {
        peak,
        trail,
        materials: painted,
        hiker_mesh: meshes.add(hiker),
        hiker_material,
        ui_clear,
        dragged: false,
    });
}

fn despawn_scene(
    mut commands: Commands,
    parts: Query<Entity, With<PeakPart>>,
    scene: Option<Res<Scene3d>>,
    mut ui_camera: Query<&mut Camera, With<Camera2d>>,
) {
    for part in &parts {
        commands.entity(part).despawn();
    }
    if let Some(scene) = scene {
        for mut camera in &mut ui_camera {
            camera.clear_color = scene.ui_clear;
        }
        commands.remove_resource::<Scene3d>();
    }
}

/// Keeps the 3D camera drawing where the UI camera does (the window, or a shot's image) and only in the map's panel,
/// so the peak stands in the middle of what is left beside the sidebar.
fn fit_camera(
    ui_camera: Query<Ref<RenderTarget>, (With<Camera2d>, Without<PeakCamera>)>,
    mut camera: Query<(Entity, &mut Camera, Ref<PeakCamera>), Without<Camera2d>>,
    panel: Query<(&ComputedNode, &UiGlobalTransform), With<MapPanel>>,
    mut commands: Commands,
) {
    let Ok((entity, mut camera, marker)) = camera.single_mut() else { return };
    if let Ok(target) = ui_camera.single()
        && (target.is_changed() || marker.is_added())
    {
        commands.entity(entity).insert(RenderTarget::clone(&target));
    }
    let Ok((node, at)) = panel.single() else { return };
    let Some(room) = camera.physical_target_size() else { return };
    let corner = (at.translation - node.size / 2.0).max(Vec2::ZERO).as_uvec2().min(room);
    let size = node.size.as_uvec2().min(room - corner);
    if size.x == 0 || size.y == 0 {
        return;
    }
    let viewport = Viewport { physical_position: corner, physical_size: size, ..default() };
    if camera.viewport.as_ref().is_none_or(|v| v.physical_position != corner || v.physical_size != size) {
        camera.viewport = Some(viewport);
    }
}

/// Turns the camera slowly while untouched, unless the desktop asks for less motion, and puts it where the orbit
/// says. While it may turn it asks for every frame, since the window otherwise draws only on input.
fn turn_camera(
    time: Res<Time>,
    appearance: Res<Appearance>,
    mut orbit: ResMut<Orbit>,
    mut camera: Query<&mut Transform, With<PeakCamera>>,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    let now = time.elapsed_secs();
    if !appearance.reduce_motion() {
        orbit.turn(time.delta_secs(), now);
        redraw.write(RequestRedraw);
    }
    let wanted = orbit.transform(LOOK_AT);
    for mut transform in &mut camera {
        transform.set_if_neq(wanted);
    }
}

/// Paints the materials, the light and the sky from their tokens when the scene appears and when the theme changes:
/// night under the moon in the dark theme, day under the sun in the light.
fn paint_scene(
    colors: ThemeColors,
    appearance: Res<Appearance>,
    scene: Res<Scene3d>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut light: Query<(&mut DirectionalLight, Ref<PeakLight>)>,
    mut camera: Query<(&mut Camera, &mut AmbientLight, &mut DistanceFog), With<PeakCamera>>,
) {
    let fresh = light.iter().any(|(_, marker)| marker.is_added());
    if !(fresh || colors.is_changed() || scene.is_added()) {
        return;
    }
    for (tone, handle) in &scene.materials {
        if let Some(mut material) = materials.get_mut(handle) {
            material.base_color = colors.color(tone);
            if *handle == scene.hiker_material {
                material.emissive = colors.color(tone).to_linear() * HIKER_GLOW;
            }
        }
    }
    let (sun, ambient) = if appearance.is_dark() { (MOONLIGHT, NIGHT_AMBIENT) } else { (SUNLIGHT, DAY_AMBIENT) };
    for (mut light, _) in &mut light {
        light.color = colors.color(&token::PEAK_LIGHT);
        light.illuminance = sun;
    }
    for (mut camera, mut sky, mut haze) in &mut camera {
        camera.clear_color = ClearColorConfig::Custom(colors.color(&token::PEAK_SKY));
        haze.color = colors.color(&token::PEAK_SKY);
        sky.color = colors.color(&token::PEAK_SKY);
        sky.brightness = ambient;
    }
}

/// A run's hiker as drawn: where it stands.
#[derive(Clone, PartialEq)]
struct HikerModel {
    at: Vec3,
}

/// Keeps a hiker on the trail for each live run, moved in place when its run changes phase.
fn sync_hikers(
    runs: Query<&Run>,
    changed: Query<(), Changed<Run>>,
    mut removed: RemovedComponents<Run>,
    scene: Res<Scene3d>,
    root: Query<Entity, With<HikerRoot>>,
    mut commands: Commands,
    mut drawn: Local<(Option<Entity>, Keyed<HikerModel>)>,
) {
    let Ok(root) = root.single() else { return };
    let fresh = drawn.0 != Some(root);
    if !(fresh || !changed.is_empty() || removed.read().count() > 0) {
        return;
    }
    if fresh {
        drawn.1.forget();
        drawn.0 = Some(root);
    }
    let hikers = place::hikers(runs.iter().map(|r| (r.status.task.0.as_str(), r.status.phase)));
    let models: Vec<(String, HikerModel)> =
        hikers.into_iter().map(|Hiker { key, distance }| (key, HikerModel { at: scene.trail.at(distance) })).collect();
    let (mesh, material) = (scene.hiker_mesh.clone(), scene.hiker_material.clone());
    drawn.1.sync(
        &mut commands,
        &[(root, models)],
        |c, key, h| {
            let hiker = (Mesh3d(mesh.clone()), MeshMaterial3d(material.clone()), Pickable::default());
            c.spawn((hiker, Transform::from_translation(h.at), HikerOf(key.to_string()))).observe(open_hiker).id()
        },
        |c, e, h| {
            c.entity(e).insert(Transform::from_translation(h.at));
        },
    );
}

/// A click on a hiker opens its task, unless the press became a drag that orbited the camera.
fn open_hiker(
    click: On<PointerClick>,
    hikers: Query<&HikerOf>,
    tasks: Query<&Mirrored<Task>>,
    scene: Option<Res<Scene3d>>,
    mut selection: ResMut<Selection>,
) {
    if click.button != PointerButton::Primary || scene.is_none_or(|s| s.dragged) {
        return;
    }
    let Ok(HikerOf(key)) = hikers.get(click.entity) else { return };
    if let Some(task) = tasks.iter().find(|t| t.0.key.as_str() == key) {
        selection.0 = Some(Subject::Task(task.0.id.to_string()));
    }
}

/// Whether a pointer event is the map panel's own, not one bubbling up from a node over it.
fn on_panel(target: Entity, original: Entity, panels: &Query<(), With<MapPanel>>) -> bool {
    target == original && panels.contains(target)
}

fn on_press(
    press: On<PointerPress>,
    panels: Query<(), With<MapPanel>>,
    time: Res<Time>,
    mut orbit: ResMut<Orbit>,
    scene: Option<ResMut<Scene3d>>,
) {
    if let Some(mut scene) = scene
        && on_panel(press.event_target(), press.entity, &panels)
    {
        scene.dragged = false;
        orbit.hold(true, time.elapsed_secs());
    }
}

fn on_release(
    release: On<PointerRelease>,
    panels: Query<(), With<MapPanel>>,
    time: Res<Time>,
    mut orbit: ResMut<Orbit>,
) {
    if on_panel(release.event_target(), release.entity, &panels) {
        orbit.hold(false, time.elapsed_secs());
    }
}

/// A drag on the scene orbits the camera; once it has gone [`DRAG_THRESHOLD`] it is no longer a click.
fn on_drag(
    drag: On<PointerDrag>,
    panels: Query<(), With<MapPanel>>,
    time: Res<Time>,
    ui_scale: Res<UiScale>,
    mut orbit: ResMut<Orbit>,
    scene: Option<ResMut<Scene3d>>,
) {
    let Some(mut scene) = scene else { return };
    if drag.button != PointerButton::Primary || !on_panel(drag.event_target(), drag.entity, &panels) {
        return;
    }
    let scale = ui_scale.0.max(f32::EPSILON);
    if drag.distance.length() / scale >= DRAG_THRESHOLD {
        scene.dragged = true;
    }
    orbit.drag(drag.delta / scale, time.elapsed_secs());
}

/// The wheel on the scene zooms.
fn on_scroll(
    scroll: On<PointerScroll>,
    panels: Query<(), With<MapPanel>>,
    per_line: Res<bevy::input::mouse::MouseScrollPixelsPerLine>,
    time: Res<Time>,
    mut orbit: ResMut<Orbit>,
    scene: Option<Res<Scene3d>>,
) {
    if scene.is_none() || !on_panel(scroll.event_target(), scroll.entity, &panels) {
        return;
    }
    let lines = scroll.to_lines(&per_line).y;
    orbit.zoom(lines, time.elapsed_secs());
}

/// Keeps a label for each hiker in a layer over the map's panel; [`place_labels`] puts them by their hikers.
fn sync_labels(
    mut commands: Commands,
    panel: Query<Entity, With<MapPanel>>,
    hikers: Query<&HikerOf>,
    layer: Query<Entity, With<LabelLayer>>,
    mut drawn: Local<(Option<Entity>, Keyed<String>)>,
) {
    let Ok(panel) = panel.single() else { return };
    let Ok(layer) = layer.single() else {
        let fill =
            Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() };
        commands.spawn((LabelLayer, PeakPart, fill, Pickable::IGNORE, ChildOf(panel)));
        return;
    };
    if drawn.0 != Some(layer) {
        drawn.1.forget();
        drawn.0 = Some(layer);
    }
    let mut keys: Vec<(String, String)> = hikers.iter().map(|h| (h.0.clone(), h.0.clone())).collect();
    keys.sort();
    drawn.1.sync(
        &mut commands,
        &[(layer, keys)],
        |c, _, key| {
            let label = c.spawn_scene(text_toned(key.clone(), size::SMALL, token::TEXT)).insert(Pickable::IGNORE).id();
            let node = Node {
                position_type: PositionType::Absolute,
                padding: UiRect::all(px(space::LABEL_INSET)),
                ..default()
            };
            let chip = c.spawn((node, Surface::Floating, Pickable::IGNORE, Visibility::Hidden, LabelOf(key.clone())));
            let chip = chip.id();
            c.entity(label).insert(ChildOf(chip));
            chip
        },
        |_, _, _| {},
    );
}

/// Puts each label by its hiker, in UI pixels over the map's panel, hidden while its hiker is off screen or behind
/// the peak.
fn place_labels(
    scene: Res<Scene3d>,
    orbit: Res<Orbit>,
    panel: Query<(&ComputedNode, &UiGlobalTransform), With<MapPanel>>,
    camera: Query<&Camera, With<PeakCamera>>,
    hikers: Query<(&HikerOf, &Transform)>,
    mut labels: Query<(&LabelOf, &mut Node, &mut Visibility)>,
) {
    let (Ok((node, at)), Ok(camera)) = (panel.single(), camera.single()) else { return };
    let eye = GlobalTransform::from(orbit.transform(LOOK_AT));
    let to_ui = node.inverse_scale_factor();
    let scale = camera.target_scaling_factor().unwrap_or(1.0);
    let corner = at.translation - node.size / 2.0;
    for (LabelOf(key), mut label, mut visibility) in &mut labels {
        let Some((_, hiker)) = hikers.iter().find(|(h, _)| h.0 == *key) else { continue };
        let top = hiker.translation + Vec3::Y * HIKER_HEIGHT;
        let shown = camera.world_to_viewport(&eye, top).ok().map(|p| (p * scale - corner) * to_ui).filter(|p| {
            let room = node.size * to_ui;
            p.x >= 0.0 && p.y >= 0.0 && p.x < room.x && p.y < room.y && !scene.peak.hides(eye.translation(), top)
        });
        let Some(p) = shown else {
            visibility.set_if_neq(Visibility::Hidden);
            continue;
        };
        visibility.set_if_neq(Visibility::Inherited);
        let (left, top) = (px(p.x + space::ROWS), px(p.y - space::GROUPS));
        if label.left != left || label.top != top {
            label.left = left;
            label.top = top;
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::camera::NormalizedRenderTarget;
    use bevy::picking::backend::HitData;
    use bevy::picking::pointer::{Location, PointerId};
    use serde_json::json;
    use trailhead_shell::fixture::{self, sid};

    use super::*;

    fn pointer() -> bevy::picking::events::Pointer {
        let location = Location { target: NormalizedRenderTarget::None { width: 1, height: 1 }, position: Vec2::ZERO };
        bevy::picking::events::Pointer::new(PointerId::Mouse, location)
    }

    fn hit() -> HitData {
        HitData::new(Entity::PLACEHOLDER, 0.0, None, None)
    }

    struct Scene {
        app: App,
        panel: Entity,
        toggle: Entity,
        hiker: Entity,
    }

    fn scene() -> Scene {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<Selection>()
            .init_resource::<Orbit>()
            .init_resource::<UiScale>()
            .add_observer(on_press)
            .add_observer(on_release)
            .add_observer(on_drag);
        let peak = Peak::new(terrain::SEED);
        let trail = Trail::new(&peak);
        app.insert_resource(Scene3d {
            peak,
            trail,
            materials: Vec::new(),
            hiker_mesh: Handle::default(),
            hiker_material: Handle::default(),
            ui_clear: ClearColorConfig::Default,
            dragged: false,
        });
        let task = fixture::task(json!({"id": "t3", "key": "TRAIL-3", "title": "climb", "status": "in_progress"}));
        app.world_mut().spawn(Mirrored(task));
        let panel = app.world_mut().spawn(MapPanel).id();
        let toggle = app.world_mut().spawn(ChildOf(panel)).id();
        let hiker = app.world_mut().spawn(HikerOf("TRAIL-3".into())).observe(open_hiker).id();
        Scene { app, panel, toggle, hiker }
    }

    impl Scene {
        fn click(&mut self) -> Option<Subject> {
            self.app.world_mut().resource_mut::<Selection>().0 = None;
            let click = PointerClick {
                entity: self.hiker,
                pointer: pointer(),
                button: PointerButton::Primary,
                hit: hit(),
                duration: std::time::Duration::ZERO,
                count: 1,
            };
            self.app.world_mut().trigger(click);
            self.app.world_mut().flush();
            self.app.world().resource::<Selection>().0.clone()
        }

        fn press(&mut self) {
            let press = PointerPress {
                entity: self.panel,
                pointer: pointer(),
                button: PointerButton::Primary,
                hit: hit(),
                count: 1,
            };
            self.app.world_mut().trigger(press);
        }

        fn drag(&mut self, on: Entity, distance: Vec2) {
            let drag = PointerDrag {
                entity: on,
                pointer: pointer(),
                button: PointerButton::Primary,
                distance,
                delta: distance,
            };
            self.app.world_mut().trigger(drag);
        }
    }

    #[test]
    fn a_click_on_a_hiker_opens_its_task() {
        let mut scene = scene();
        assert_eq!(scene.click(), Some(Subject::Task(sid("t3"))));
    }

    #[test]
    fn a_press_that_became_a_drag_orbits_and_opens_nothing_but_a_shaking_hand_still_clicks() {
        let mut scene = scene();
        let before = *scene.app.world().resource::<Orbit>();
        scene.press();
        scene.drag(scene.panel, Vec2::new(DRAG_THRESHOLD / 2.0, 0.0));
        assert!(scene.click().is_some(), "under the threshold it is still a click");
        scene.press();
        scene.drag(scene.panel, Vec2::new(DRAG_THRESHOLD * 8.0, 0.0));
        assert_ne!(scene.app.world().resource::<Orbit>().yaw, before.yaw, "the drag turned the camera");
        assert_eq!(scene.click(), None, "the release that ends a drag opens no task");
        scene.press();
        assert!(scene.click().is_some(), "the next press is a click again");
    }

    #[test]
    fn a_drag_on_a_control_over_the_scene_leaves_the_camera_alone() {
        let mut scene = scene();
        let before = *scene.app.world().resource::<Orbit>();
        scene.drag(scene.toggle, Vec2::new(200.0, 50.0));
        assert_eq!(*scene.app.world().resource::<Orbit>(), before);
        scene.drag(scene.panel, Vec2::new(200.0, 50.0));
        assert_ne!(*scene.app.world().resource::<Orbit>(), before);
    }
}
