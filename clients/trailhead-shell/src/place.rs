//! The window's size and place on the screen, kept in a file beside the theme's so a restart (the launcher's, onto a
//! newer build, most of all) brings the window back as it was.
//!
//! The place is kept only where the desktop tells a window where it is. Wayland doesn't, and won't let a window put
//! itself anywhere either, so there the size alone comes back and the compositor places the window.

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowPosition, WindowResolution};
use serde::{Deserialize, Serialize};

/// How long a size or place has to hold before it is written, so a drag writes once, at its end.
const SETTLED: Duration = Duration::from_millis(500);

/// A window's size in logical pixels, and its top left corner where the desktop says.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Place {
    width: u32,
    height: u32,
    at: Option<(i32, i32)>,
}

impl Place {
    /// The window's place now, or `None` while it has no size (minimized).
    fn of(window: &Window) -> Option<Place> {
        // Logical, since a window is made before its screen's scale is known: a physical size would come back
        // doubled on a screen scaled by two.
        let (width, height) = (window.resolution.width().round() as u32, window.resolution.height().round() as u32);
        let at = match window.position {
            WindowPosition::At(at) => Some((at.x, at.y)),
            _ => None,
        };
        (width > 0 && height > 0).then_some(Place { width, height, at })
    }

    /// Makes `window` open here. A window not yet made has a scale of one, so its physical size is its logical one.
    pub fn apply(self, window: &mut Window) {
        window.resolution = WindowResolution::new(self.width, self.height);
        if let Some((x, y)) = self.at {
            window.position = WindowPosition::At(IVec2::new(x, y));
        }
    }
}

/// The file the window's place is kept in, and what it holds.
#[derive(Resource)]
pub struct PlaceFile {
    path: PathBuf,
    kept: Option<Place>,
    /// A place not yet written, and when the window came to it.
    moving: Option<(Place, Duration)>,
}

impl PlaceFile {
    /// Reads the place kept last time, if any.
    pub fn load(path: PathBuf) -> PlaceFile {
        let kept = std::fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok());
        PlaceFile { path, kept, moving: None }
    }

    pub fn kept(&self) -> Option<Place> {
        self.kept
    }

    /// Takes the window's place at `now`; true when it has held for [`SETTLED`] and is to be written.
    fn settle(&mut self, now: Duration, place: Place) -> bool {
        if self.kept == Some(place) {
            self.moving = None;
            return false;
        }
        match self.moving {
            Some((moving, since)) if moving == place => {
                let settled = now.saturating_sub(since) >= SETTLED;
                if settled {
                    self.kept = Some(place);
                    self.moving = None;
                }
                settled
            }
            _ => {
                self.moving = Some((place, now));
                false
            }
        }
    }
}

/// Writes the window's place once it has stopped changing.
pub fn keep_place(time: Res<Time<Real>>, windows: Query<&Window, With<PrimaryWindow>>, mut file: ResMut<PlaceFile>) {
    let Some(place) = windows.single().ok().and_then(Place::of) else { return };
    if file.settle(time.elapsed(), place)
        && let Err(e) =
            serde_json::to_string(&place).map_err(std::io::Error::other).and_then(|t| std::fs::write(&file.path, t))
    {
        warn!("couldn't keep the window's place in {}: {e}", file.path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(width: u32) -> Place {
        Place { width, height: 600, at: None }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_place_is_written_once_it_has_held_and_not_during_a_drag() {
        let mut file = PlaceFile { path: PathBuf::new(), kept: None, moving: None };
        assert!(!file.settle(ms(0), place(800)));
        assert!(!file.settle(ms(100), place(810)));
        assert!(!file.settle(ms(400), place(810)));
        assert!(file.settle(ms(600), place(810)));
        assert_eq!(file.kept(), Some(place(810)));
        assert!(!file.settle(ms(5000), place(810)));
    }

    #[test]
    fn a_kept_place_opens_the_window_at_its_size_and_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window");
        std::fs::write(&path, r#"{"width":1200,"height":700,"at":[40,60]}"#).unwrap();
        let mut window = Window::default();
        PlaceFile::load(path).kept().unwrap().apply(&mut window);
        assert_eq!((window.resolution.physical_width(), window.resolution.physical_height()), (1200, 700));
        assert_eq!(window.position, WindowPosition::At(IVec2::new(40, 60)));
        assert_eq!(Place::of(&window), Some(Place { width: 1200, height: 700, at: Some((40, 60)) }));
        // On a screen scaled by two the same window is twice the pixels, and still kept at its logical size.
        window.resolution.set_scale_factor(2.0);
        window.resolution.set_physical_resolution(2400, 1400);
        assert_eq!(Place::of(&window), Some(Place { width: 1200, height: 700, at: Some((40, 60)) }));
    }

    #[test]
    fn a_file_that_is_missing_or_unreadable_keeps_nothing_and_a_place_with_no_spot_leaves_it_to_the_desktop() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(PlaceFile::load(dir.path().join("none")).kept(), None);
        std::fs::write(dir.path().join("bad"), "not json").unwrap();
        assert_eq!(PlaceFile::load(dir.path().join("bad")).kept(), None);
        let mut window = Window::default();
        place(900).apply(&mut window);
        assert_eq!(window.position, WindowPosition::Automatic);
    }
}
