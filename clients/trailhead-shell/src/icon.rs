//! Trailhead's window icon: "TL" in light letters on the kit's accent, on a rounded square.
//!
//! [`render`] draws it at any size, `examples/make_icon.rs` writes the PNGs under `assets/icon/` with it, and a test
//! fails if the committed ones drift from it. [`IconUiPlugin`] sets the 256 px one as the window's icon, and the
//! window's [`APP_ID`] is what the desktop matches to `trailhead.desktop` for the dock's icon.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy::winit::WINIT_WINDOWS;
use winit::window::Icon;

use trailhead_ui::palette;

/// The window's `Window.name`: the Wayland app id and X11 `WM_CLASS`, and `StartupWMClass` in `trailhead.desktop`.
pub const APP_ID: &str = "trailhead";

/// The primary window: titled, and named [`APP_ID`] so the dock finds its icon.
pub fn primary_window() -> Window {
    Window { title: "Trailhead".into(), name: Some(APP_ID.into()), ..default() }
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
    // Squared distance, not `hypot`, so the corner needs no libm call.
    let (dx, dy) = (dx.max(0.0), dy.max(0.0));
    dx * dx + dy * dy <= RADIUS * RADIUS
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
    encode(size, &render(size))
}

fn encode(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().and_then(|mut w| w.write_image_data(rgba)).expect("encoding into memory");
    bytes
}

/// How much of a macOS icon's side the drawing takes: the system's own icons leave a margin around the shape, and one
/// drawn to the edge looks oversized beside them in the Dock.
const MAC_FILL: f32 = 0.8;

/// The `.icns` entry types that hold a PNG, with the side of each in pixels.
pub const ICNS: [(&[u8; 4], u32); 8] = [
    (b"ic11", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// [`render`] on a transparent `size` x `size` canvas with macOS's margin around it.
pub fn render_mac(size: u32) -> Vec<u8> {
    let inner = (size as f32 * MAC_FILL).round() as u32;
    let (offset, drawing) = ((size - inner) / 2, render(inner));
    let mut out = vec![0; (size * size * 4) as usize];
    for row in 0..inner {
        let (from, to) = ((row * inner * 4) as usize, (((row + offset) * size + offset) * 4) as usize);
        out[to..to + (inner * 4) as usize].copy_from_slice(&drawing[from..from + (inner * 4) as usize]);
    }
    out
}

/// The icon as a macOS `.icns`, for the app bundle `trailhead app install` writes: a header, then each of [`ICNS`] as
/// its type, its length and a PNG.
pub fn icns() -> Vec<u8> {
    let mut entries = Vec::new();
    for (kind, size) in ICNS {
        let png = encode(size, &render_mac(size));
        entries.extend_from_slice(kind);
        entries.extend_from_slice(&(png.len() as u32 + 8).to_be_bytes());
        entries.extend_from_slice(&png);
    }
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&(entries.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(&entries);
    out
}

/// The PNGs in an `.icns`, by entry type.
pub fn icns_entries(icns: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut entries = Vec::new();
    let mut rest = icns.get(8..).unwrap_or_default();
    while rest.len() >= 8 {
        let kind = [rest[0], rest[1], rest[2], rest[3]];
        let len = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        if len < 8 || len > rest.len() {
            break;
        }
        entries.push((kind, &rest[8..len]));
        rest = &rest[len..];
    }
    entries
}

/// Decode an RGBA PNG into its width, height and pixels.
pub fn decode(png_bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), png::DecodingError> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes)).read_info()?;
    let mut pixels = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut pixels)?;
    pixels.truncate(info.buffer_size());
    Ok((info.width, info.height, pixels))
}

/// Sets the window icon. Drawing, so only [`Shell`](crate::Shell) adds it.
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
            // The colours come from float conversions (`powf`, `sin`/`cos`) and the edges
            // are blended in f32, so another platform's libm can nudge a pixel by a level
            // or two; a stale PNG differs far more.
            let drawn = render(size);
            assert_eq!(pixels.len(), drawn.len(), "{size}.png has the wrong size");
            let worst = pixels.iter().zip(&drawn).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            assert!(
                worst <= TOLERANCE,
                "{size}.png is stale (off by {worst}): run `cargo run -p trailhead-shell --example make_icon`"
            );
        }
    }

    #[test]
    fn the_committed_icns_holds_every_size_and_matches_the_drawing() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/trailhead/assets/trailhead.icns");
        let bytes = std::fs::read(path).expect("trailhead.icns");
        assert_eq!(&bytes[..4], b"icns");
        assert_eq!(u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize, bytes.len());
        let entries = icns_entries(&bytes);
        assert_eq!(entries.len(), ICNS.len());
        for ((kind, png), (expected, size)) in entries.iter().zip(ICNS) {
            assert_eq!(kind, expected);
            let (w, h, pixels) = decode(png).expect("decodes");
            assert_eq!((w, h), (size, size));
            let worst = pixels.iter().zip(&render_mac(size)).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            assert!(
                worst <= TOLERANCE,
                "trailhead.icns is stale at {size} px (off by {worst}): run `cargo run -p trailhead-shell --example make_icon`"
            );
        }
    }

    #[test]
    fn the_mac_icon_leaves_a_margin_around_the_drawing() {
        let pixels = render_mac(100);
        let alpha = |x: usize, y: usize| pixels[(y * 100 + x) * 4 + 3];
        assert_eq!(alpha(5, 50), 0, "the margin is transparent");
        assert_eq!(alpha(50, 5), 0);
        assert_eq!(alpha(12, 50), 255, "the drawing starts a tenth in");
        assert_eq!(alpha(50, 50), 255);
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
        assert_eq!(primary_window().name.as_deref(), Some("trailhead"));
        assert_eq!(primary_window().title, "Trailhead");
    }

    #[test]
    fn the_window_icon_is_a_valid_winit_icon() {
        let (w, h, rgba) = decode(WINDOW_ICON).unwrap();
        assert!(Icon::from_rgba(rgba, w, h).is_ok());
    }
}
