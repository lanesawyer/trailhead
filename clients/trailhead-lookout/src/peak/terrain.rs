//! The peak and its trail as plain geometry, with no window: a heightfield on a triangular lattice, its facets, and a
//! switchback trail on its face from the foot to the summit.

use bevy::math::{Vec2, Vec3};

/// The seed the one peak is grown from.
pub const SEED: u32 = 1163;
/// Lattice steps from the summit to the rim.
const RINGS: i32 = 16;
/// How far the foot is from the summit, in world units.
pub const RADIUS: f32 = 10.0;
/// How high the summit stands.
pub const HEIGHT: f32 = 7.5;
/// How fast the slope flattens toward the foot: above 1 the flanks flare out.
const FLARE: f32 = 1.6;
/// How far the noise lifts or sinks the flanks, as a share of the height; under [`FLARE`], so the summit stays highest.
const ROUGHNESS: f32 = 0.6;
/// Bumps across the peak, roughly.
const NOISE_SCALE: f32 = 0.16;

/// Where a facet sits, for its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Band {
    Ground,
    Rock,
    Snow,
}

/// A band from a facet's height as a share of the summit's, and how upright its normal is (1 is flat).
pub fn band(height: f32, upright: f32) -> Band {
    if height > 0.7 && upright > 0.45 {
        Band::Snow
    } else if height > 0.22 || upright < 0.72 {
        Band::Rock
    } else {
        Band::Ground
    }
}

/// A mesh with a normal per facet: three vertices to each triangle, none shared, so each face is shaded flat.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facets {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// One per triangle.
    pub bands: Vec<Band>,
}

impl Facets {
    fn push(&mut self, [a, b, c]: [Vec3; 3]) -> Vec3 {
        let normal = (b - a).cross(c - a).normalize_or_zero();
        let start = self.positions.len() as u32;
        for p in [a, b, c] {
            self.positions.push(p.to_array());
            self.normals.push(normal.to_array());
        }
        self.indices.extend([start, start + 1, start + 2]);
        normal
    }
}

/// The peak: heights on a triangular lattice inside a hexagon, from a fixed seed. The surface between lattice points
/// is the flat facets, so a point placed with [`Peak::height_at`] sits on what is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Peak {
    heights: Vec<f32>,
}

const SIDE: i32 = 2 * RINGS + 1;
const CELL: f32 = RADIUS / (RINGS as f32 * HALF_ROOT_3);
const HALF_ROOT_3: f32 = 0.866_025_4;

fn inside(q: i32, r: i32) -> bool {
    q.abs() <= RINGS && r.abs() <= RINGS && (q + r).abs() <= RINGS
}

fn index(q: i32, r: i32) -> usize {
    ((q + RINGS) * SIDE + (r + RINGS)) as usize
}

/// A lattice point's place on the ground.
fn plan(q: i32, r: i32) -> Vec2 {
    Vec2::new(CELL * (q as f32 + r as f32 * 0.5), CELL * r as f32 * HALF_ROOT_3)
}

impl Peak {
    pub fn new(seed: u32) -> Self {
        let mut heights = vec![0.0; (SIDE * SIDE) as usize];
        for q in -RINGS..=RINGS {
            for r in -RINGS..=RINGS {
                if inside(q, r) {
                    heights[index(q, r)] = shape(seed, plan(q, r));
                }
            }
        }
        Peak { heights }
    }

    fn at(&self, q: i32, r: i32) -> f32 {
        if inside(q, r) { self.heights[index(q, r)] } else { 0.0 }
    }

    /// The height of the facet over `p` on the ground; nothing outside the foot.
    pub fn height_at(&self, p: Vec2) -> f32 {
        let rf = p.y / (CELL * HALF_ROOT_3);
        let qf = p.x / CELL - rf * 0.5;
        let (q, r) = (qf.floor(), rf.floor());
        let (fq, fr) = (qf - q, rf - r);
        let (q, r) = (q as i32, r as i32);
        if fq + fr <= 1.0 {
            self.at(q, r) * (1.0 - fq - fr) + self.at(q + 1, r) * fq + self.at(q, r + 1) * fr
        } else {
            self.at(q + 1, r + 1) * (fq + fr - 1.0) + self.at(q, r + 1) * (1.0 - fq) + self.at(q + 1, r) * (1.0 - fr)
        }
    }

    /// The point on the surface over `p`.
    pub fn point(&self, p: Vec2) -> Vec3 {
        Vec3::new(p.x, self.height_at(p), p.y)
    }

    /// The summit, the highest point.
    pub fn summit(&self) -> Vec3 {
        Vec3::new(0.0, self.at(0, 0), 0.0)
    }

    /// The surface as flat-shaded triangles, each wound counter-clockwise seen from above.
    pub fn facets(&self) -> Facets {
        let mut facets = Facets::default();
        let vertex = |q: i32, r: i32| {
            let p = plan(q, r);
            Vec3::new(p.x, self.at(q, r), p.y)
        };
        for q in -RINGS..RINGS {
            for r in -RINGS..RINGS {
                for tri in [[(q, r), (q, r + 1), (q + 1, r)], [(q + 1, r), (q, r + 1), (q + 1, r + 1)]] {
                    if tri.iter().all(|&(q, r)| inside(q, r)) {
                        let [a, b, c] = tri.map(|(q, r)| vertex(q, r));
                        let normal = facets.push([a, b, c]);
                        let mid = (a.y + b.y + c.y) / 3.0;
                        facets.bands.push(band(mid / HEIGHT, normal.y));
                    }
                }
            }
        }
        facets
    }

    /// Whether the surface stands between `from` and `to`: the segment dips under it somewhere.
    pub fn hides(&self, from: Vec3, to: Vec3) -> bool {
        const STEPS: usize = 64;
        (1..STEPS).any(|i| {
            let p = from.lerp(to, i as f32 / STEPS as f32);
            p.y < self.height_at(Vec2::new(p.x, p.z))
        })
    }
}

/// The smooth shape the lattice samples: a peak falling to the foot, roughened by noise that fades at the summit and
/// the foot.
fn shape(seed: u32, p: Vec2) -> f32 {
    let t = (p.length() / RADIUS).clamp(0.0, 1.0);
    let base = (1.0 - t).powf(FLARE);
    let noise = fbm(seed, p * NOISE_SCALE);
    (HEIGHT * (base + ROUGHNESS * noise * t * (1.0 - t))).max(0.0)
}

/// A number in `-1..=1` for a lattice corner of the noise.
fn hash(seed: u32, x: i32, z: i32) -> f32 {
    let mut h =
        seed.wrapping_mul(0x9E37_79B1) ^ (x as u32).wrapping_mul(0x85EB_CA77) ^ (z as u32).wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h as f32 / u32::MAX as f32 * 2.0 - 1.0
}

/// Value noise: the corners' numbers blended smoothly, in `-1..=1`.
fn noise(seed: u32, p: Vec2) -> f32 {
    let (x, z) = (p.x.floor(), p.y.floor());
    let (fx, fz) = (p.x - x, p.y - z);
    let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sz) = (smooth(fx), smooth(fz));
    let (x, z) = (x as i32, z as i32);
    let top = hash(seed, x, z) + (hash(seed, x + 1, z) - hash(seed, x, z)) * sx;
    let bottom = hash(seed, x, z + 1) + (hash(seed, x + 1, z + 1) - hash(seed, x, z + 1)) * sx;
    top + (bottom - top) * sz
}

/// Four octaves of [`noise`], in `-1..=1`.
fn fbm(seed: u32, p: Vec2) -> f32 {
    let (mut sum, mut amplitude, mut frequency, mut total) = (0.0, 1.0, 1.0, 0.0);
    for octave in 0..4 {
        sum += amplitude * noise(seed.wrapping_add(octave), p * frequency);
        total += amplitude;
        amplitude *= 0.5;
        frequency *= 2.0;
    }
    sum / total
}

/// How far up the trail starts, as a share of the radius.
const FOOT: f32 = 0.92;
/// The legs of its switchbacks.
const LEGS: f32 = 7.0;
/// How far a switchback swings round the peak at the foot, in radians; it narrows toward the summit.
const SWING: f32 = 0.55;
/// The bearing of the face the trail climbs, in radians from +x toward +z.
pub const FACE: f32 = 1.1;
/// Plan steps along the trail: fine enough that the line between them stays on the facets.
const TRAIL_STEPS: usize = 600;

/// The trail: switchbacks wound up the face from the foot to the summit, as points on the surface.
#[derive(Clone, Debug, PartialEq)]
pub struct Trail {
    points: Vec<Vec3>,
}

impl Trail {
    /// Each step climbs about as much as the last: the reach from the summit is where the peak's shape, without its
    /// noise, stands at an even share of the height, so a hiker's height says how far its run has got.
    pub fn new(peak: &Peak) -> Self {
        let foot = (1.0 - FOOT).powf(FLARE);
        let points = (0..=TRAIL_STEPS)
            .map(|i| {
                let t = i as f32 / TRAIL_STEPS as f32;
                let share = foot + (1.0 - foot) * t;
                let reach = RADIUS * (1.0 - share.powf(1.0 / FLARE));
                let bearing = FACE + SWING * (1.0 - t) * (LEGS * std::f32::consts::PI * t).sin();
                peak.point(Vec2::new(reach * bearing.cos(), reach * bearing.sin()))
            })
            .collect();
        Trail { points }
    }

    /// The point `share` of the climb up, `0` the foot and `1` the summit.
    pub fn at(&self, share: f32) -> Vec3 {
        let f = share.clamp(0.0, 1.0) * TRAIL_STEPS as f32;
        let i = (f.floor() as usize).min(TRAIL_STEPS - 1);
        self.points[i].lerp(self.points[i + 1], f - i as f32)
    }

    /// The points it passes through, foot first.
    pub fn points(&self) -> &[Vec3] {
        &self.points
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_grows_the_same_peak_and_another_seed_another() {
        assert_eq!(Peak::new(SEED).facets(), Peak::new(SEED).facets());
        assert_ne!(Peak::new(SEED), Peak::new(SEED + 1));
    }

    #[test]
    fn every_normal_is_unit_length_and_faces_up() {
        let facets = Peak::new(SEED).facets();
        assert_eq!(facets.positions.len(), facets.normals.len());
        assert_eq!(facets.indices.len(), facets.positions.len());
        assert_eq!(facets.bands.len() * 3, facets.positions.len());
        for n in &facets.normals {
            let n = Vec3::from_array(*n);
            assert!((n.length() - 1.0).abs() < 1e-4, "{n} isn't unit length");
            assert!(n.y > 0.1, "{n} faces down or sideways: the face would be culled from above");
        }
    }

    #[test]
    fn the_heights_stay_between_the_ground_and_the_summit_which_is_highest() {
        let peak = Peak::new(SEED);
        let facets = peak.facets();
        let summit = peak.summit();
        assert!((summit.y - HEIGHT).abs() < 1e-4);
        for p in &facets.positions {
            assert!((0.0..=HEIGHT).contains(&p[1]), "{p:?} is out of bounds");
            if Vec2::new(p[0], p[2]).length() > 0.1 {
                assert!(p[1] < summit.y, "{p:?} is as high as the summit");
            }
        }
        // The foot meets the ground all round.
        for i in 0..36 {
            let angle = i as f32 * std::f32::consts::TAU / 36.0;
            assert_eq!(peak.height_at(Vec2::from_angle(angle) * RADIUS * 1.16), 0.0);
        }
    }

    #[test]
    fn it_has_all_three_bands_and_more_rock_than_snow() {
        let bands = Peak::new(SEED).facets().bands;
        let count = |b: Band| bands.iter().filter(|x| **x == b).count();
        assert!(count(Band::Snow) > 0 && count(Band::Ground) > 0);
        assert!(count(Band::Rock) > count(Band::Snow));
    }

    #[test]
    fn a_point_on_the_surface_is_on_the_facet_under_it() {
        let peak = Peak::new(SEED);
        // At a lattice point the facets meet at its height; between them, the height is between theirs.
        assert_eq!(peak.height_at(Vec2::ZERO), peak.summit().y);
        let p = plan(3, -2);
        assert!((peak.height_at(p) - peak.at(3, -2)).abs() < 1e-4);
        let mid = (plan(3, -2) + plan(4, -2)) / 2.0;
        assert!((peak.height_at(mid) - (peak.at(3, -2) + peak.at(4, -2)) / 2.0).abs() < 1e-4);
    }

    #[test]
    fn the_trail_climbs_from_the_foot_to_the_summit() {
        let peak = Peak::new(SEED);
        let trail = Trail::new(&peak);
        assert!(trail.at(0.0).y < HEIGHT * 0.1, "it starts near the foot: {}", trail.at(0.0));
        assert!(trail.at(1.0).distance(peak.summit()) < 1e-3, "it ends at the summit");
        let tenths: Vec<f32> = (0..=10).map(|i| trail.at(i as f32 / 10.0).y).collect();
        assert!(tenths.windows(2).all(|w| w[1] > w[0]), "each tenth is higher than the last: {tenths:?}");
        for p in trail.points() {
            assert!((p.y - peak.height_at(Vec2::new(p.x, p.z))).abs() < 1e-4, "{p} is on the surface");
        }
    }

    #[test]
    fn the_trail_switches_back_rather_than_going_straight_up() {
        let trail = Trail::new(&Peak::new(SEED));
        let bearing = |p: Vec3| p.z.atan2(p.x) - FACE;
        let turns = trail.points().windows(3).filter(|w| {
            let (a, b, c) = (bearing(w[0]), bearing(w[1]), bearing(w[2]));
            (b - a) * (c - b) < 0.0
        });
        assert!(turns.count() >= 5);
    }

    #[test]
    fn the_peak_hides_what_is_behind_it() {
        let peak = Peak::new(SEED);
        let front = Vec3::new(RADIUS * 2.0, 2.0, 0.0);
        let behind = Vec3::new(-RADIUS * 0.6, peak.height_at(Vec2::new(-RADIUS * 0.6, 0.0)) + 0.1, 0.0);
        assert!(peak.hides(front, behind));
        let near_side = Vec3::new(RADIUS * 0.6, peak.height_at(Vec2::new(RADIUS * 0.6, 0.0)) + 0.1, 0.0);
        assert!(!peak.hides(front, near_side));
    }
}
