//! Writes `assets/icon/{32,64,128,256}.png` and the CLI's `crates/trailhead/assets/trailhead.icns` (the macOS app
//! bundle's icon) from `trailhead_shell::icon`: `cargo run -p trailhead-shell --example make_icon`. Edit `icon.rs`
//! and the SVG together.

use trailhead_shell::icon::{SIZES, icns, render_png};

fn main() {
    let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = here.join("assets/icon");
    std::fs::create_dir_all(&dir).expect("create assets/icon");
    for size in SIZES {
        std::fs::write(dir.join(format!("{size}.png")), render_png(size)).expect("write icon");
    }
    let cli = here.join("../../crates/trailhead/assets");
    std::fs::create_dir_all(&cli).expect("create the CLI's assets");
    std::fs::write(cli.join("trailhead.icns"), icns()).expect("write icns");
}
