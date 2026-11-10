//! Lookout's app icon: "TL" in light letters on the kit's accent, on a rounded square.
//!
//! [`render`] draws it at any size, `examples/make_icon.rs` writes the PNGs under `assets/icon/` with it, and a test
//! fails if the committed ones drift from it. [`IconUiPlugin`] sets the 256 px one as the window's icon, and the
//! window's [`APP_ID`] is what the desktop matches to `lookout.desktop` for the dock's icon.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy::winit::WINIT_WINDOWS;
use winit::window::Icon;

use crate::widgets::palette;

/// The window's `Window.name`: the Wayland app id and X11 `WM_CLASS`, and `StartupWMClass` in `lookout.desktop`.
pub const APP_ID: &str = "lookout";

/// The primary window: titled, and named [`APP_ID`] so the dock finds its icon.
pub fn primary_window() -> Window {
    Window { title: "Lookout".into(), name: Some(APP_ID.into()), ..default() }
}

/// The sizes the PNGs are committed in.
pub const SIZES: [u32; 4] = [32, 64, 128, 256];

/// The icon the window wears.
const WINDOW_ICON: &[u8] = include_bytes!("../assets/icon/256.png");

const SUPERSAMPLE: u32 = 4;
/// The corner radius, as a fraction of the side.
const RADIUS: f32 = 0.22;
/// The letters as boxes `[x0, y0, x1, y1]` on a unit square: T's bar and stem, then L's stem and foot.
const LETTERS: [[f32; 4]; 4] =
    [[0.17, 0.25, 0.47, 0.35], [0.27, 0.25, 0.37, 0.75], [0.53, 0.25, 0.63, 0.75], [0.53, 0.65, 0.83, 0.75]];

fn srgb(color: Color) -> [f32; 4] {
    let c = color.to_srgba();
    [c.red, c.green, c.blue, 1.0]
}

fn in_rounded_square(x: f32, y: f32) -> bool {
    let (dx, dy) = ((x - 0.5).abs() - (0.5 - RADIUS), (y - 0.5).abs() - (0.5 - RADIUS));
    dx.max(0.0).hypot(dy.max(0.0)) <= RADIUS
}

/// The icon as `size` x `size` RGBA bytes (straight alpha, antialiased by supersampling).
pub fn render(size: u32) -> Vec<u8> {
    let (background, letters) = (srgb(palette::ACCENT), srgb(palette::WHITE));
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let n = SUPERSAMPLE * SUPERSAMPLE;
    for py in 0..size {
        for px in 0..size {
            let (mut rgb, mut coverage) = ([0.0f32; 3], 0.0f32);
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let x = (px * SUPERSAMPLE + sx) as f32 / (size * SUPERSAMPLE) as f32
                        + 0.5 / (size * SUPERSAMPLE) as f32;
                    let y = (py * SUPERSAMPLE + sy) as f32 / (size * SUPERSAMPLE) as f32
                        + 0.5 / (size * SUPERSAMPLE) as f32;
                    if !in_rounded_square(x, y) {
                        continue;
                    }
                    let lit = LETTERS.iter().any(|b| x >= b[0] && x < b[2] && y >= b[1] && y < b[3]);
                    let c = if lit { letters } else { background };
                    (0..3).for_each(|i| rgb[i] += c[i]);
                    coverage += 1.0;
                }
            }
            for c in rgb {
                let c = if coverage > 0.0 { c / coverage } else { 0.0 };
                out.push((c.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            out.push((coverage / n as f32 * 255.0).round() as u8);
        }
    }
    out
}

/// Encode [`render`] as a PNG.
pub fn render_png(size: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().and_then(|mut w| w.write_image_data(&render(size))).expect("encoding into memory");
    bytes
}

/// Decode an RGBA PNG into its width, height and pixels.
pub fn decode(png_bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), png::DecodingError> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes)).read_info()?;
    let mut pixels = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut pixels)?;
    pixels.truncate(info.buffer_size());
    Ok((info.width, info.height, pixels))
}

/// Sets the window icon. Drawing, so only `main.rs` adds it.
pub struct IconUiPlugin;

impl Plugin for IconUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, set_window_icon);
    }
}

/// Winit creates the window after startup, so try each frame until it exists, then once.
fn set_window_icon(window: Query<Entity, With<PrimaryWindow>>, mut done: Local<bool>) {
    if *done {
        return;
    }
    let Ok(entity) = window.single() else { return };
    let icon = match decode(WINDOW_ICON).map(|(w, h, rgba)| Icon::from_rgba(rgba, w, h)) {
        Ok(Ok(icon)) => icon,
        Ok(Err(e)) => return warn_once(&mut done, e),
        Err(e) => return warn_once(&mut done, e),
    };
    WINIT_WINDOWS.with_borrow(|windows| {
        if let Some(window) = windows.get_window(entity) {
            window.set_window_icon(Some(icon));
            *done = true;
        }
    });
}

fn warn_once(done: &mut bool, error: impl std::fmt::Display) {
    warn!("the window icon didn't load: {error}");
    *done = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Largest per-channel difference allowed between a committed PNG and a fresh render.
    const TOLERANCE: u8 = 2;

    #[test]
    fn committed_pngs_decode_and_match_the_drawing() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon");
        for size in SIZES {
            let bytes = std::fs::read(dir.join(format!("{size}.png"))).expect("icon png");
            let (w, h, pixels) = decode(&bytes).expect("decodes");
            assert_eq!((w, h), (size, size));
            // The renderer uses f32 maths, so another platform's libm can nudge an
            // antialiased edge pixel by a level or two; a stale PNG differs far more.
            let drawn = render(size);
            assert_eq!(pixels.len(), drawn.len(), "{size}.png has the wrong size");
            let worst = pixels.iter().zip(&drawn).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            assert!(
                worst <= TOLERANCE,
                "{size}.png is stale (off by {worst}): run `cargo run -p trailhead-lookout --example make_icon`"
            );
        }
    }

    #[test]
    fn the_icon_has_transparent_corners_and_light_letters() {
        let pixels = render(64);
        assert_eq!(pixels[3], 0, "corner is transparent");
        let at = |x: usize, y: usize| &pixels[(y * 64 + x) * 4..][..4];
        assert_eq!(at(2, 2)[3], 0);
        assert_eq!(at(32, 32)[3], 255);
        assert!(at(20, 20)[0] > 240, "T's bar is light");
        assert!(at(8, 32)[0] < 200, "the margin is the accent");
    }

    #[test]
    fn the_primary_window_has_a_stable_app_id() {
        assert_eq!(primary_window().name.as_deref(), Some("lookout"));
    }

    #[test]
    fn the_window_icon_is_a_valid_winit_icon() {
        let (w, h, rgba) = decode(WINDOW_ICON).unwrap();
        assert!(Icon::from_rgba(rgba, w, h).is_ok());
    }
}
