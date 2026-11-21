//! The shell drawn to PNGs with no window, display or GPU, behind the `shots` feature: what an app's shots test
//! (`tests/shots.rs`, `#[ignore]`d) uses to show its views, named and set up from fixtures.
//!
//! [`shot`] builds the shell as [`Shell`](crate::Shell) runs it, with Bevy's renderer but no window: the kit's
//! `ThemePlugin` worn [`fixed`](trailhead_ui::ThemePlugin::fixed) in the light or dark asked for, its `UiPlugin`, the
//! apps windowed and a [`TestConnection`]. The one camera draws into an image instead of a window. The [`ShotApp`] it
//! returns derefs to the [`App`], so [`fill`](super::fill), [`feed`](super::feed), [`show`](super::show),
//! [`set_connection`](super::set_connection), [`settle`](super::settle) and triggered events set a view up as in any
//! other test; [`ShotApp::picture`] then draws it.
//!
//! It needs a render adapter. Any GPU will do; with none, Mesa's lavapipe renders in software, picked with
//! `VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json`. A picture is the same byte for byte from run to run on the
//! same renderer: time is stepped by a fixed amount a frame, the desktop is never asked for its theme, and the size and
//! scale are fixed. Different renderers differ by a level or two a channel, so a before and an after to compare must
//! come from the same one.

use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::time::Duration;

use bevy::app::Plugins;
use bevy::asset::{LoadState, RenderAssetUsages, UntypedAssetId};
use bevy::camera::{ImageRenderTarget, RenderTarget};
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::render_resource::{PollType, TextureFormat};
use bevy::render::renderer::RenderDevice;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use bevy::text::FontSource;
use bevy::time::TimeUpdateStrategy;
use bevy::ui::IsDefaultUiCamera;
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;
use trailhead_ui::{Appearance, ThemeChoice, ThemePlugin, UiPlugin};

use super::TestConnection;
use crate::ShellPlugin;

/// What a test says when there is no adapter to draw with.
const NO_ADAPTER: &str = "no render adapter to draw the shots with: they need a GPU, or software rendering with \
                          VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json (Mesa's lavapipe)";

/// How far time moves each frame: fixed, so what animates is in the same place every run.
const FRAME: Duration = Duration::from_millis(16);
/// The frames a picture waits at least, for a view to lay out.
const MIN_FRAMES: u32 = 10;
/// The frames a picture waits at most for what it draws to load.
const MAX_FRAMES: u32 = 300;
/// The frames a capture may take to arrive.
const CAPTURE_FRAMES: u32 = 30;

/// Light or dark, as a shot wears it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShotTheme {
    Light,
    #[default]
    Dark,
}

impl ShotTheme {
    /// The theme as the last part of a shot's name: `lookout-board-dark`.
    pub fn word(self) -> &'static str {
        match self {
            ShotTheme::Light => "light",
            ShotTheme::Dark => "dark",
        }
    }

    fn choice(self) -> ThemeChoice {
        match self {
            ShotTheme::Light => ThemeChoice::Light,
            ShotTheme::Dark => ThemeChoice::Dark,
        }
    }
}

/// How a shot is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShotOptions {
    /// The window's size in logical pixels: what the views lay out in.
    pub size: (u32, u32),
    /// Physical pixels to a logical one; the picture is `size` times this.
    pub scale: f32,
    pub theme: ShotTheme,
}

impl Default for ShotOptions {
    fn default() -> Self {
        ShotOptions { size: (1280, 800), scale: 1.0, theme: ShotTheme::Dark }
    }
}

impl ShotOptions {
    /// The picture's size in pixels.
    pub fn pixels(&self) -> (u32, u32) {
        let scaled = |n: u32| (n as f32 * self.scale).round() as u32;
        (scaled(self.size.0), scaled(self.size.1))
    }
}

/// A drawn frame, as RGBA rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Picture {
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// How many different colors it holds: a handful means nothing was drawn.
    pub fn colors(&self) -> usize {
        self.rgba.as_chunks::<4>().0.iter().collect::<HashSet<_>>().len()
    }

    /// The picture as a PNG.
    pub fn png(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("a PNG header");
        writer.write_image_data(&self.rgba).expect("the PNG's pixels");
        writer.finish().expect("the PNG's end");
        bytes
    }

    /// Writes the picture to `<dir>/<name>.png`, where `dir` is `SHOTS_DIR` or else `shots` in the target directory,
    /// and returns the path.
    pub fn save(&self, name: &str) -> PathBuf {
        let dir = shots_dir();
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("couldn't make {}: {e}", dir.display()));
        let path = dir.join(format!("{name}.png"));
        std::fs::write(&path, self.png()).unwrap_or_else(|e| panic!("couldn't write {}: {e}", path.display()));
        path
    }
}

/// `SHOTS_DIR`, or `shots` beside the build's profile directories in the target directory, which a test binary is
/// three levels under (`target/debug/deps/<test>`).
pub fn shots_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SHOTS_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let exe = std::env::current_exe().expect("the test binary's path");
    exe.ancestors().nth(3).expect("a test binary under target/<profile>/deps").join("shots")
}

/// The shell set up to be drawn: deref to its [`App`] to set a view up, then take a [`ShotApp::picture`].
pub struct ShotApp {
    app: App,
    target: Handle<Image>,
    options: ShotOptions,
}

impl Deref for ShotApp {
    type Target = App;

    fn deref(&self) -> &App {
        &self.app
    }
}

impl DerefMut for ShotApp {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.app
    }
}

/// The captured frame, until a picture takes it.
#[derive(Resource, Default)]
struct Captured(Option<Image>);

/// `shell` windowed and drawn into an image as `options` say, settled. Panics, saying how to render in software, when
/// there is no render adapter.
pub fn shot(shell: ShellPlugin, options: ShotOptions) -> ShotApp {
    shot_with(shell, options, ())
}

/// [`shot`] with `plugins` of the test's own, added before the app starts: a piece of the shell that one not told to
/// connect leaves out, such as [`ConnectUiPlugin`](crate::connect::ConnectUiPlugin).
pub fn shot_with<M>(shell: ShellPlugin, options: ShotOptions, plugins: impl Plugins<M>) -> ShotApp {
    let mut app = App::new();
    let renderer = catch_unwind(AssertUnwindSafe(|| {
        app.add_plugins(
            DefaultPlugins
                .set(WindowPlugin { primary_window: None, exit_condition: ExitCondition::DontExit, ..default() })
                .set(RenderPlugin { synchronous_pipeline_compilation: true, ..default() })
                .disable::<WinitPlugin>(),
        );
    }));
    if let Err(cause) = renderer {
        let cause =
            cause.downcast_ref::<String>().cloned().or_else(|| cause.downcast_ref::<&str>().map(|s| s.to_string()));
        panic!("{NO_ADAPTER} ({})", cause.unwrap_or_default());
    }
    app.insert_resource(TimeUpdateStrategy::ManualDuration(FRAME))
        .add_plugins(TestConnection)
        .add_plugins(ThemePlugin::fixed(shell.theme(), options.theme.choice()))
        .add_plugins(UiPlugin)
        .add_plugins(shell.windowed())
        .add_plugins(plugins)
        .init_resource::<Captured>();
    app.finish();
    app.cleanup();
    assert!(app.world().get_resource::<RenderDevice>().is_some(), "{NO_ADAPTER}");
    // The shell spawns its camera at startup, aimed at the window there isn't.
    step(&mut app);
    let target = target_image(&mut app, &options);
    let mut shot = ShotApp { app, target, options };
    shot.aim();
    super::settle(&mut shot.app);
    shot
}

/// A frame, and the GPU's work for it finished.
fn step(app: &mut App) {
    app.update();
    let device = app.world().resource::<RenderDevice>();
    device.wgpu_device().poll(PollType::Wait { submission_index: None, timeout: None }).expect("the GPU's work");
}

fn target_image(app: &mut App, options: &ShotOptions) -> Handle<Image> {
    let (width, height) = options.pixels();
    let mut image = Image::new_target_texture(width, height, TextureFormat::Rgba8UnormSrgb, None);
    image.asset_usage = RenderAssetUsages::RENDER_WORLD;
    app.world_mut().resource_mut::<Assets<Image>>().add(image)
}

impl ShotApp {
    /// What it is drawn as.
    pub fn options(&self) -> ShotOptions {
        self.options
    }

    /// Draws as `options` say from the next picture on: another theme, size or scale.
    pub fn set_options(&mut self, options: ShotOptions) {
        if options.theme != self.options.theme {
            self.app.world_mut().resource_mut::<Appearance>().choice = options.theme.choice();
        }
        if (options.size, options.scale) != (self.options.size, self.options.scale) {
            let old = std::mem::replace(&mut self.target, target_image(&mut self.app, &options));
            self.app.world_mut().resource_mut::<Assets<Image>>().remove(&old);
        }
        self.options = options;
        self.aim();
        super::settle(&mut self.app);
    }

    /// Wears `theme` from the next picture on.
    pub fn set_theme(&mut self, theme: ShotTheme) {
        self.set_options(ShotOptions { theme, ..self.options });
    }

    /// Lays out in `size` from the next picture on.
    pub fn set_size(&mut self, size: (u32, u32)) {
        self.set_options(ShotOptions { size, ..self.options });
    }

    fn render_target(&self) -> RenderTarget {
        // A screenshot of the target must name the same scale as the camera's, or it captures nothing.
        RenderTarget::Image(ImageRenderTarget { handle: self.target.clone(), scale_factor: self.options.scale })
    }

    /// Points the shell's camera at the image; without `IsDefaultUiCamera` no UI is drawn through it.
    fn aim(&mut self) {
        let target = self.render_target();
        let world = self.app.world_mut();
        let cameras: Vec<Entity> = world.query_filtered::<Entity, With<Camera2d>>().iter(world).collect();
        assert_eq!(cameras.len(), 1, "the shell draws through one camera");
        world.entity_mut(cameras[0]).insert((target, IsDefaultUiCamera));
    }

    /// Runs frames until what is drawn has loaded and laid out, then captures one. Panics when no capture arrives.
    pub fn picture(&mut self) -> Picture {
        self.wait_for_assets();
        let target = self.render_target();
        self.app.world_mut().spawn(Screenshot(target)).observe(
            |shot: On<ScreenshotCaptured>, mut captured: ResMut<Captured>| captured.0 = Some(shot.image.clone()),
        );
        for _ in 0..CAPTURE_FRAMES {
            step(&mut self.app);
            if let Some(image) = self.app.world_mut().resource_mut::<Captured>().0.take() {
                return picture_of(image);
            }
        }
        panic!("no capture after {CAPTURE_FRAMES} frames; is there a render adapter? {NO_ADAPTER}");
    }

    /// [`ShotApp::picture`] as a PNG.
    pub fn png(&mut self) -> Vec<u8> {
        self.picture().png()
    }

    /// Takes a [`ShotApp::picture`] and writes it to `<SHOTS_DIR or target/shots>/<name>.png`, returning the path.
    pub fn save(&mut self, name: &str) -> PathBuf {
        self.picture().save(name)
    }

    /// Runs frames until every font and image a node uses has loaded, and a few more for the text to lay out.
    fn wait_for_assets(&mut self) {
        let mut ready = 0;
        for frame in 0..MAX_FRAMES {
            step(&mut self.app);
            ready = if self.loading() { 0 } else { ready + 1 };
            if frame >= MIN_FRAMES && ready >= 3 {
                return;
            }
        }
        panic!("what the view draws was still loading after {MAX_FRAMES} frames");
    }

    fn loading(&mut self) -> bool {
        let world = self.app.world_mut();
        let mut ids: Vec<UntypedAssetId> = world
            .query::<&TextFont>()
            .iter(world)
            .filter_map(|f| match &f.font {
                FontSource::Handle(handle) => Some(handle.id().untyped()),
                _ => None,
            })
            .collect();
        ids.extend(world.query::<&ImageNode>().iter(world).map(|i| i.image.id().untyped()));
        let server = world.resource::<AssetServer>();
        ids.into_iter().any(|id| matches!(server.get_load_state(id), Some(LoadState::Loading)))
    }
}

/// A captured image as RGBA, whichever order its format keeps the channels in.
fn picture_of(image: Image) -> Picture {
    let format = image.texture_descriptor.format;
    let size = image.size();
    let mut rgba = image.data.expect("a captured image has its pixels");
    if matches!(format, TextureFormat::Bgra8UnormSrgb | TextureFormat::Bgra8Unorm) {
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
    }
    Picture { width: size.x, height: size.y, rgba }
}
