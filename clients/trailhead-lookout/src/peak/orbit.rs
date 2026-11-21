//! The camera's orbit round the peak: where it is, how a drag and the wheel move it, and the slow turn while nobody
//! touches it. Plain state, moved by the scene's observers and tested without a window.

use std::f32::consts::{PI, TAU};

use bevy::prelude::*;

/// Radians of turn per UI pixel dragged.
const DRAG_TURN: f32 = 0.006;
/// How much nearer one line of the wheel brings the camera, as a share of its distance.
const ZOOM_STEP: f32 = 0.1;
/// One turn round the peak while untouched takes this many seconds.
const TURN_SECONDS: f32 = 150.0;
/// After a drag or a zoom, the turn waits this long before it starts again.
const RESUME_AFTER: f32 = 4.0;

/// The camera's place on its orbit, looking at the peak.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct Orbit {
    /// Round the peak, in radians: `0` looks from +z.
    pub yaw: f32,
    /// Above the ground, in radians.
    pub pitch: f32,
    /// From what it looks at, in world units.
    pub distance: f32,
    /// When a person last moved it, in seconds of the app's clock; it stays still until [`RESUME_AFTER`] has passed.
    touched: Option<f32>,
    /// Whether a button is held down on the scene, which holds the turn.
    held: bool,
}

impl Orbit {
    pub const MIN_PITCH: f32 = 0.08;
    pub const MAX_PITCH: f32 = 1.25;
    pub const MIN_DISTANCE: f32 = 9.0;
    pub const MAX_DISTANCE: f32 = 48.0;

    /// Where it starts: three quarters on to the trail's face, a little above it.
    pub fn new(yaw: f32, pitch: f32, distance: f32) -> Self {
        Orbit {
            yaw: yaw.rem_euclid(TAU),
            pitch: pitch.clamp(Self::MIN_PITCH, Self::MAX_PITCH),
            distance: distance.clamp(Self::MIN_DISTANCE, Self::MAX_DISTANCE),
            touched: None,
            held: false,
        }
    }

    /// A button pressed (or let go) on the scene: the turn holds while it is down.
    pub fn hold(&mut self, held: bool, now: f32) {
        self.held = held;
        self.touched = Some(now);
    }

    /// A drag of `delta` UI pixels: across turns round the peak, down tips the camera up over it.
    pub fn drag(&mut self, delta: Vec2, now: f32) {
        self.yaw = (self.yaw - delta.x * DRAG_TURN).rem_euclid(TAU);
        self.pitch = (self.pitch + delta.y * DRAG_TURN).clamp(Self::MIN_PITCH, Self::MAX_PITCH);
        self.touched = Some(now);
    }

    /// `lines` of the wheel: forward comes nearer.
    pub fn zoom(&mut self, lines: f32, now: f32) {
        let factor = (1.0 - ZOOM_STEP).powf(lines);
        self.distance = (self.distance * factor).clamp(Self::MIN_DISTANCE, Self::MAX_DISTANCE);
        self.touched = Some(now);
    }

    /// Whether it turns by itself at `now`: not while held, nor soon after a person moved it.
    pub fn turning(&self, now: f32) -> bool {
        !self.held && self.touched.is_none_or(|t| now - t >= RESUME_AFTER)
    }

    /// Turns it on by `seconds` if it is [`turning`](Self::turning); says whether it did.
    pub fn turn(&mut self, seconds: f32, now: f32) -> bool {
        if !self.turning(now) {
            return false;
        }
        self.yaw = (self.yaw + seconds * TAU / TURN_SECONDS).rem_euclid(TAU);
        true
    }

    /// The camera's transform, looking at `target` from its orbit.
    pub fn transform(&self, target: Vec3) -> Transform {
        let (yaw, pitch) = (self.yaw, self.pitch.min(PI / 2.0));
        let eye = target + self.distance * Vec3::new(pitch.cos() * yaw.sin(), pitch.sin(), pitch.cos() * yaw.cos());
        Transform::from_translation(eye).looking_at(target, Vec3::Y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orbit() -> Orbit {
        Orbit::new(0.5, 0.4, 24.0)
    }

    #[test]
    fn a_drag_turns_it_and_tips_it_but_never_under_the_ground_or_over_the_top() {
        let mut o = orbit();
        o.drag(Vec2::new(100.0, 0.0), 0.0);
        let off = (o.yaw - (0.5 - 0.6)).rem_euclid(TAU);
        assert!(off.min(TAU - off) < 1e-4, "turned by 0.6 radians: {}", o.yaw);
        o.drag(Vec2::new(0.0, 10_000.0), 0.0);
        assert_eq!(o.pitch, Orbit::MAX_PITCH);
        o.drag(Vec2::new(0.0, -10_000.0), 0.0);
        assert_eq!(o.pitch, Orbit::MIN_PITCH);
        assert!((0.0..TAU).contains(&o.yaw));
    }

    #[test]
    fn the_wheel_zooms_within_bounds() {
        let mut o = orbit();
        o.zoom(1.0, 0.0);
        assert!(o.distance < 24.0);
        o.zoom(100.0, 0.0);
        assert_eq!(o.distance, Orbit::MIN_DISTANCE);
        o.zoom(-100.0, 0.0);
        assert_eq!(o.distance, Orbit::MAX_DISTANCE);
        assert_eq!(Orbit::new(0.0, 9.0, 1.0), Orbit::new(0.0, Orbit::MAX_PITCH, Orbit::MIN_DISTANCE));
    }

    #[test]
    fn it_turns_slowly_until_touched_and_again_after_a_pause() {
        let mut o = orbit();
        assert!(o.turn(1.0, 0.0));
        let turned = o.yaw - 0.5;
        assert!(turned > 0.0 && turned < 0.1, "a slow turn: {turned} radians a second");
        o.drag(Vec2::new(5.0, 0.0), 10.0);
        let yaw = o.yaw;
        assert!(!o.turn(1.0, 11.0));
        assert_eq!(o.yaw, yaw);
        assert!(o.turn(1.0, 10.0 + RESUME_AFTER));
    }

    #[test]
    fn a_held_button_holds_the_turn_however_long() {
        let mut o = orbit();
        o.hold(true, 0.0);
        assert!(!o.turning(1000.0));
        o.hold(false, 1000.0);
        assert!(!o.turning(1001.0));
        assert!(o.turning(1000.0 + RESUME_AFTER));
    }

    #[test]
    fn the_camera_looks_at_the_target_from_its_distance() {
        let o = orbit();
        let target = Vec3::new(0.0, 2.0, 0.0);
        let t = o.transform(target);
        assert!((t.translation.distance(target) - 24.0).abs() < 1e-3);
        assert!(t.translation.y > target.y, "above the target");
        assert!((t.forward().dot((target - t.translation).normalize()) - 1.0).abs() < 1e-4);
    }
}
