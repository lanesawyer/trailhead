//! Writes `assets/icon/{32,64,128,256}.png` from `trailhead_lookout::icon`:
//! `cargo run -p trailhead-lookout --example make_icon`. Edit `icon.rs` and the SVG together.

use trailhead_lookout::icon::{SIZES, render_png};

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon");
    std::fs::create_dir_all(&dir).expect("create assets/icon");
    for size in SIZES {
        std::fs::write(dir.join(format!("{size}.png")), render_png(size)).expect("write icon");
    }
}
