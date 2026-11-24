//! Placement rules and crystal clusters (#204, M1b-1): the first content of the generator that
//! stands on the ground or hangs in a cave, and the rules that the content after it (vegetation,
//! the landmarks) is placed by.
//!
//! **Placement rules** are tests of a place that depend on nothing but the seed, so any brick, at
//! any level, and the server agree on them (ADR 0002, ADR 0003):
//! - [`Planet::sunlit_at`], from the world map alone: the ground's slope and the way it faces
//!   (towards the equator, the sun's side), and how much sky it has (the horizon, from the map's
//!   elevation in eight directions).
//! - [`Planet::cave_wall_at`], from the 3D density of the caves (`Caves::density`, the tubes'
//!   signed distance with the walls' roughness): a wall is where it crosses zero, and its gradient
//!   says which way it faces.
//! - [`Planet::room_at`], from the local density: how much of a ball in front of a surface is
//!   rock, which says whether there is room for something to grow out of it.
//!
//! Each rule is a decision on numbers that something else measures: [`Sunlit::is_lit`] takes the
//! slope, the aspect and the openness, [`Wall`] the density and its gradient, and [`Room`] the
//! share of rock round a point. The measuring ([`Planet::sunlit_at`] from the world map,
//! `Caves::density` from the tubes, `terrain_sized` for the rock) is apart from the deciding, so
//! another source of the same numbers (a climate or landform model) can feed the rules without
//! changing them.
//!
//! **A crystal cluster** is a few radiating hexagonal prisms with pointed tips, the principal one
//! along the surface's normal. Each is a convex polytope (a body of six sides and a six-sided
//! point), so a Voxel's distance inside it is the least of its distances to the planes that bound
//! it, and a ball of any radius that fits in the prism is a test of that number (see [`Prism`]).
//! That is the SDF of the stamp, and the rule of the coarse levels: a Voxel of mip level `L` is
//! crystal when its middle is in a prism, as the terrain is sampled at a Voxel's middle (ADR 0002's
//! majority rule would call a prism thinner than the Voxel air, but tests measure this sampling
//! as the closer, since a thin prism's Voxels are more often over half full than not).
//!
//! **Where the clusters are** is a lattice of cubes of [`SITE_CELL`] Voxels: each cube has the
//! clusters whose anchor (the point on the surface that they grow from) is in it, made from the
//! cube's index, the seed and the world map, never from the order bricks are made in, so a brick
//! asks for the cubes within [`REACH`] of it and gets the same clusters whatever it is
//! ([`Planet::clusters_near`]). A cube is worked out once per thread and kept, as the caves'
//! cubes are. Ground clusters are in sunlit places, one candidate a cube, and cave clusters are
//! along the tubes of the caves, a few candidates every [`CAVE_SPACING`] of a tube. Both are only
//! in the world map's [`Regions::CRYSTAL_CAVES`] (the crystal country, where M1b puts them), and
//! only where [`Planet::room_at`] says there is room. A cave has a dominant hue: from its system's
//! id, with an accent hue in a minority of its clusters.
//!
//! **A cluster only fills air.** The ground, the water and the caves are what the terrain says,
//! and a Voxel that is solid there is left as it is, so every shortcut that proves a brick solid
//! stays true (`solid_below`), and the ones that prove it air know the [`REACH`] (`air_below`,
//! [`Planet::max_radius`]). The clusters are in levels 0 to [`MAX_LEVEL`]: above it a Voxel is wider
//! than a cluster's thickest prism, which the pyramid leaves almost nothing of (a test counts).
//!
//! All of it uses only `+ - * /`, `sqrt`, comparisons and the integer hash, as the rest of the
//! generator does: the hexagon's sides are three fixed directions in a plane, not angles.

use std::cell::RefCell;
use std::sync::Arc;

use brick_store::Material;

use super::*;
use crate::caves::{Dice, ROUGHNESS};
use crate::world_map::Regions;

/// The size of `metres`, in Voxels, as a float.
const fn v(metres: f64) -> f64 {
    voxels(metres) as f64
}

/// The crystal Materials of the eight hues, in the order of [`HUE_NAMES`]: ids 56 to 63 of
/// `materials.toml` (a test checks them against it). Opaque and emissive, as the look-dev
/// prototype (#148) had them.
pub const HUES: [Material; 8] = [
    Material(56),
    Material(57),
    Material(58),
    Material(59),
    Material(60),
    Material(61),
    Material(62),
    Material(63),
];

/// The names of [`HUES`] in the Material table.
pub const HUE_NAMES: [&str; 8] = [
    "crystal blue",
    "crystal cyan",
    "crystal green",
    "crystal magenta",
    "crystal red",
    "crystal amber",
    "crystal white",
    "crystal violet",
];

/// The coarsest mip level that has clusters: 40 cm Voxels. A cluster's thickest prism is 3.5 Voxels
/// of level 0 across its flats at 1.6 m, and a Voxel of level 4 (80 cm) is wider than that.
pub const MAX_LEVEL: usize = 3;

/// Voxels along a cube of the lattice of sites: 6.4 m.
pub const SITE_CELL: i32 = voxels(6.4);

/// The shortest prism: 0.3 m, which is 6 Voxels of level 0, the least that reads as a crystal.
pub const MIN_PRISM_LENGTH: f64 = v(0.3);
/// The longest prism: 1.6 m.
pub const MAX_PRISM_LENGTH: f64 = v(1.6);
/// The longest prism where there is room for a small cluster only (see [`Room::Small`]): 0.7 m.
const SMALL_PRISM_LENGTH: f64 = v(0.7);
/// The least apothem (the distance from a prism's axis to the middle of a side) of a prism, in
/// Voxels: 3 Voxels across the flats of the thinnest.
const MIN_APOTHEM: f64 = 1.5;
/// A prism's apothem as a share of its length, where that is more than [`MIN_APOTHEM`].
const APOTHEM_PER_LENGTH: f64 = 0.11;
/// A prism's point is this many apothems long (at most half of the prism).
const TIP_PER_APOTHEM: f64 = 2.0;
/// How far below the surface a cluster's prisms start, in Voxels, so that they are joined to it.
const EMBED: f64 = 3.0;
/// How far from the anchor, across the surface, the base of a prism may be, in Voxels: 25 cm.
const BASE_SPREAD: f64 = v(0.25);
/// How many prisms a cluster has, and how far a prism of it tilts from the surface's normal: the
/// size of the random vector added to the normal (0.8 is 39 degrees).
const PRISMS: (u64, u64) = (3, 7);
const TILT: (f64, f64) = (0.2, 0.8);
/// A prism that is not the principal one is this share of its length.
const SIDE_LENGTH: (f64, f64) = (0.35, 0.8);

/// How far from its anchor any Voxel of a cluster is, in Voxels: a prism starts [`EMBED`] below the
/// anchor and up to 1.5 [`BASE_SPREAD`] to the side, and goes [`MAX_PRISM_LENGTH`] along its axis;
/// its widest corner is 1.2 apothems from the axis. The bricks that can hold a cluster are those
/// within this of its anchor.
pub const REACH: f64 = MAX_PRISM_LENGTH
    + EMBED
    + 1.5 * BASE_SPREAD
    + 1.2 * (MAX_PRISM_LENGTH * APOTHEM_PER_LENGTH + MIN_APOTHEM);

/// The chance, of 1, that a cube's candidate for a ground cluster is one where the ground is sunlit
/// and has room: about one in 17 of a sunlit slope's cubes, a cluster every 26 m or so.
pub const GROUND_CHANCE: f64 = 0.06;
/// Candidates for ground clusters in each cube.
const GROUND_CANDIDATES: u64 = 1;
/// How far apart candidates for cave clusters are along a tube, in Voxels: 2.4 m. Each place has
/// [`CAVE_SLOTS`] of them, round the tube.
pub const CAVE_SPACING: f64 = v(2.4);
const CAVE_SLOTS: u64 = 2;
/// The chance, of 1, that a candidate on a cave's wall is a cluster, where there is room.
pub const CAVE_CHANCE: f64 = 0.30;
/// The share of a cave's clusters that are of its accent hue and not its dominant one.
pub const ACCENT_SHARE: f64 = 0.2;
/// The least height of ground that has a ground cluster above the sea, in Voxels: 2 m.
const LAND_MIN: f64 = v(2.0);
/// Patches of ground of this many cubes along an edge (25.6 m) share a dominant hue.
const HUE_PATCH: i32 = 4;

/// The sunlit rule (see [`Planet::sunlit_at`]): the slope's steepness as a rise over a run, the
/// least cosine of the angle between the way the ground faces and the equator's, and the least
/// share of the sky that is open.
pub const SUN_SLOPE: (f64, f64) = (0.07, 0.60);
pub const SUN_FACING_MIN: f64 = 0.3;
pub const SUN_OPENNESS_MIN: f64 = 0.7;
/// How far from a place the map's elevation is read for its slope: 120 m, half a texel.
const SLOPE_STEP: f64 = v(120.0);
/// How far away the horizon is looked for: 120 m, 300 m and 750 m.
const HORIZON_STEPS: [f64; 3] = [v(120.0), v(300.0), v(750.0)];
/// The rise over a run of the horizon that shuts the sky in a direction entirely: 0.1, 5.7 degrees.
/// The map's ground is gentle (its steepest is about 0.2, its median 0.07), so a hill of that is a
/// wall of the sky.
const HORIZON_FULL: f64 = 0.10;
const SQRT_HALF: f64 = std::f64::consts::FRAC_1_SQRT_2;
/// Eight directions across the ground, as (along, across) of [`tangents`].
const AZIMUTHS: [(f64, f64); 8] = [
    (1.0, 0.0),
    (SQRT_HALF, SQRT_HALF),
    (0.0, 1.0),
    (-SQRT_HALF, SQRT_HALF),
    (-1.0, 0.0),
    (-SQRT_HALF, -SQRT_HALF),
    (0.0, -1.0),
    (SQRT_HALF, -SQRT_HALF),
];

/// How far from zero the caves' density may be at a wall, in Voxels, and the least gradient
/// (1 for a tube's own wall) that it has there; the step it is differenced over.
const WALL_BAND: f64 = 3.0;
const WALL_GRADIENT_MIN: f64 = 0.4;
const WALL_STEP: f64 = 2.0;
/// How many times a ray is halved to find where a wall is: 100 Voxels to under half of one.
const WALL_BISECTIONS: u32 = 8;

/// The room rule (see [`Planet::room_at`]): the ball is [`ROOM_LIFT`] of its radius in front of
/// the surface, so that a flat one is all air; of the 14 points taken round it at that radius, no
/// more than this share may be rock. The radii: 0.6 m and 1.4 m.
const ROOM_SOLID_MAX: f64 = 0.3;
const ROOM_LIFT: f64 = 0.75;
const ROOM_SMALL: f64 = v(0.6);
const ROOM_OPEN: f64 = v(1.4);
const INV_SQRT3: f64 = 0.577_350_269_189_625_8;
const SQRT3_HALF: f64 = 0.866_025_403_784_438_6;

/// Salts that tell the clusters' streams of randomness from each other's and the caves'.
const GROUND_SALT: u64 = 0x6c07_5b3d_c8a5_9e11;
const CAVE_SALT: u64 = 0x3f1d_8a42_5b97_c6e3;
const HUE_SALT: u64 = 0x91ab_0c75_d4e2_3f68;

/// What the world map says of the sun on the ground at a place: see [`Planet::sunlit_at`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sunlit {
    /// How steep the map's ground is: a rise over a run, 0 on the flat.
    pub slope: f64,
    /// The cosine of the angle between the way the ground faces (downhill) and the way to the
    /// equator, in `[-1, 1]`; 0 where it is flat.
    pub facing: f64,
    /// How much of the sky is open, in `[0, 1]`: 1 on a plain or a summit, less where the horizon
    /// is raised by hills.
    pub openness: f64,
}

impl Sunlit {
    /// Whether the place is a sunlit slope: steep enough to be a slope and not a cliff, facing the
    /// equator, and open to the sky.
    pub fn is_lit(&self) -> bool {
        (SUN_SLOPE.0..=SUN_SLOPE.1).contains(&self.slope)
            && self.facing >= SUN_FACING_MIN
            && self.openness >= SUN_OPENNESS_MIN
    }
}

/// How much room there is in front of a surface: see [`Planet::room_at`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Room {
    /// None: a crack, a narrow passage, a hollow.
    Closed,
    /// For a cluster of short prisms.
    Small,
    /// For any cluster.
    Open,
}

/// A wall of a cave: see [`Planet::cave_wall_at`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wall {
    /// The caves' density there, in Voxels from the wall: positive in the cave.
    pub density: f64,
    /// The unit vector that the wall faces: the density's gradient, into the cave.
    pub normal: [f64; 3],
}

/// Where a cluster is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// On sunlit ground.
    Ground,
    /// On the wall of the cave that has this id (`Tube::system`).
    Cave(u64),
}

/// A cluster, as the tools and the tests see it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrystalSite {
    /// The point of the surface it grows from, in Voxels from the Planet's centre.
    pub anchor: [f64; 3],
    /// The unit vector that the surface faces there, which the principal prism is along.
    pub normal: [f64; 3],
    pub material: Material,
    pub place: Place,
    pub prisms: usize,
    /// The length of the longest prism, in Voxels.
    pub length: f64,
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn plus(a: [f64; 3], b: [f64; 3], by: f64) -> [f64; 3] {
    std::array::from_fn(|k| a[k] + b[k] * by)
}

fn minus(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|k| a[k] - b[k])
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// `a` as a unit vector, or `None` if it has no direction.
fn unit(a: [f64; 3]) -> Option<[f64; 3]> {
    let length = distance(a);
    (length > 1e-9).then(|| a.map(|c| c / length))
}

/// One hexagonal prism with a pointed tip, a convex polytope: from its base plane, up its axis
/// `axis`, six sides at an apothem from the axis (their normals are `±sides[k]`, three directions
/// 60 degrees apart across the axis), and from `length - tip` up, six sloping faces that meet at
/// the apex, `length` from the base. A Voxel is inside when it is on the inner side of all 13
/// planes, and [`Prism::depth`] is how far inside, which is the radius of the largest ball
/// centred on it that is inside too: the least of its distances to the planes (each plane's
/// normal is a unit vector, so the numbers are distances).
#[derive(Clone, Copy, Debug)]
struct Prism {
    /// The middle of the base.
    base: [f64; 3],
    axis: [f64; 3],
    sides: [[f64; 3]; 3],
    length: f64,
    apothem: f64,
    /// How long the point is, and the slant of its faces: `sqrt(tip^2 + apothem^2)`.
    tip: f64,
    slant: f64,
}

impl Prism {
    /// The prism from `base` along the unit vector `axis`, `length` long, turned about its axis
    /// by the direction `across`, which need not be across it.
    fn new(base: [f64; 3], axis: [f64; 3], length: f64, across: [f64; 3]) -> Prism {
        let side = unit(minus(across, axis.map(|c| c * dot(across, axis)))).unwrap_or_else(|| {
            let (first, _) = tangents(axis);
            first
        });
        let other = cross(axis, side);
        let apothem = MIN_APOTHEM.max(length * APOTHEM_PER_LENGTH);
        let tip = (apothem * TIP_PER_APOTHEM).min(length / 2.0);
        Prism {
            base,
            axis,
            sides: [
                side,
                plus(side.map(|c| c * 0.5), other, SQRT3_HALF),
                plus(side.map(|c| c * -0.5), other, SQRT3_HALF),
            ],
            length,
            apothem,
            tip,
            slant: (tip * tip + apothem * apothem).sqrt(),
        }
    }

    /// How far inside the prism `point` is, in Voxels: the radius of the largest ball around it
    /// that fits in the prism, negative if it is outside.
    fn depth(&self, point: [f64; 3]) -> f64 {
        let rel = minus(point, self.base);
        let along = dot(rel, self.axis);
        // The base, and the body's and the point's sides, in the least-distance form.
        let mut out = -along;
        for side in self.sides {
            let across = dot(rel, side).abs();
            out = out.max(across - self.apothem);
            out = out.max((self.tip * across + self.apothem * (along - self.length)) / self.slant);
        }
        -out
    }

    /// The farthest a Voxel of the prism is from `from`: the base's corners and the apex.
    fn farthest(&self, from: [f64; 3]) -> f64 {
        let corner = self.apothem / SQRT3_HALF;
        let apex = plus(self.base, self.axis, self.length);
        let top_ring = plus(self.base, self.axis, self.length - self.tip);
        let ring = |at: [f64; 3]| distance(minus(at, from)) + corner;
        ring(self.base)
            .max(ring(top_ring))
            .max(distance(minus(apex, from)))
    }
}

/// A cluster of prisms, with the Material of its hue.
#[derive(Clone, Debug)]
pub(crate) struct Cluster {
    anchor: [f64; 3],
    normal: [f64; 3],
    material: Material,
    place: Place,
    prisms: Vec<Prism>,
    /// How far from the anchor any Voxel of the cluster is.
    bound: f64,
}

impl Cluster {
    /// The cluster that grows from `anchor` along `normal`, with prisms up to `cap` long, made
    /// from `dice`.
    fn new(
        anchor: [f64; 3],
        normal: [f64; 3],
        cap: f64,
        (material, place): (Material, Place),
        dice: &mut Dice,
    ) -> Cluster {
        let (t1, t2) = tangents(normal);
        // Most clusters are small: the principal prism's length is the square of a draw.
        let scale = dice.next();
        let principal = MIN_PRISM_LENGTH + (cap - MIN_PRISM_LENGTH) * scale * scale;
        let embedded = plus(anchor, normal, -EMBED);
        let mut prisms = Vec::new();
        for k in 0..dice.count(PRISMS) {
            let (axis, base, length) = if k == 0 {
                (normal, embedded, principal)
            } else {
                let tilt = dice.between(TILT);
                let lean = plus(
                    plus(normal, t1, tilt * dice.between((-1.0, 1.0))),
                    t2,
                    tilt * dice.between((-1.0, 1.0)),
                );
                let off = plus(
                    plus([0.0; 3], t1, BASE_SPREAD * dice.between((-1.0, 1.0))),
                    t2,
                    BASE_SPREAD * dice.between((-1.0, 1.0)),
                );
                let share = dice.between(SIDE_LENGTH);
                (
                    unit(lean).unwrap_or(normal),
                    plus(embedded, off, 1.0),
                    MIN_PRISM_LENGTH.max(principal * share),
                )
            };
            let across = [0.0; 3].map(|_| dice.between((-1.0, 1.0)));
            prisms.push(Prism::new(base, axis, length, across));
        }
        let bound = prisms
            .iter()
            .map(|p| p.farthest(anchor))
            .fold(0.0, f64::max);
        debug_assert!(bound <= REACH, "{bound} is past the reach {REACH}");
        Cluster {
            anchor,
            normal,
            material,
            place,
            prisms,
            bound,
        }
    }

    /// How far inside a prism of the cluster `point` is: the radius of the largest ball around it
    /// that fits in one, negative if it is in none.
    fn depth(&self, point: [f64; 3]) -> f64 {
        self.prisms
            .iter()
            .map(|p| p.depth(point))
            .fold(f64::NEG_INFINITY, f64::max)
    }

    /// Whether the cluster may have a Voxel in the box from `low` to `high`.
    fn reaches(&self, low: [f64; 3], high: [f64; 3]) -> bool {
        let near = distance(std::array::from_fn(|a| {
            self.anchor[a].clamp(low[a], high[a]) - self.anchor[a]
        }));
        near <= self.bound
    }

    fn site(&self) -> CrystalSite {
        CrystalSite {
            anchor: self.anchor,
            normal: self.normal,
            material: self.material,
            place: self.place,
            prisms: self.prisms.len(),
            length: self.prisms.iter().map(|p| p.length).fold(0.0, f64::max),
        }
    }
}

/// How many cubes of sites a thread keeps: a brick's neighbours are in the same few.
const KEPT_CUBES: usize = 128;

/// A thread's kept cubes of sites: where to put the next, and each as (the Planet's caves' id, the
/// cube, its clusters).
type Kept = (usize, Vec<(u64, [i32; 3], Arc<Vec<Cluster>>)>);

/// The cubes of sites of the last box a thread asked for: (the Planet's caves' id, the first and
/// the last cube, the cubes with clusters).
type Asked = Option<(u64, [i32; 3], [i32; 3], Vec<Arc<Vec<Cluster>>>)>;

thread_local! {
    static LAST: RefCell<Asked> = const { RefCell::new(None) };

    /// The cubes this thread asked for last, the oldest replaced first. It only saves work: a
    /// cube is the same however it is got.
    static KEPT: RefCell<Kept> = const { RefCell::new((0, Vec::new())) };
}

/// The index along an axis of the cube of sites that holds the coordinate `c`.
fn site_cube(c: f64) -> i32 {
    (c / f64::from(SITE_CELL)).floor() as i32
}

/// A hash of the bits of numbers, from `seed`.
fn bits_hash(seed: u64, numbers: &[f64]) -> u64 {
    numbers.iter().fold(seed, |h, n| {
        let b = n.to_bits();
        crate::hash(h, [(b >> 32) as i32, b as i32, 0])
    })
}

impl Planet {
    /// What the world map says of the sun on the ground in the direction `direction` (any length,
    /// not the centre): the slope and the way it faces from the map's elevation 120 m either side,
    /// and the sky's openness from the rise of the map's ground to the horizon in eight
    /// directions, at 120 m, 300 m and 750 m. Only the map: not the detail, which is finer than
    /// any of this, and so not the Voxels, which is what lets a place be decided before its
    /// ground is generated.
    ///
    /// The sun's path is local to the viewer (see `render::daylight`), so a slope's aspect is
    /// taken against the one direction that is the same everywhere on a Planet whose poles are on
    /// the y axis: towards the equator, where the sun is highest. A slope that faces it has the
    /// most sun, as on Earth.
    pub fn sunlit_at(&self, direction: [f64; 3]) -> Sunlit {
        let up = unit(direction).unwrap_or([1.0, 0.0, 0.0]);
        let (ahead, aside) = tangents(up);
        let sea = self.shape.sea;
        let height = |along: f64, across: f64| -> f64 {
            let at = std::array::from_fn(|a| up[a] * sea + ahead[a] * along + aside[a] * across);
            self.map.elevation_at(at)
        };
        let step = SLOPE_STEP;
        let g_along = (height(step, 0.0) - height(-step, 0.0)) / (2.0 * step);
        let g_across = (height(0.0, step) - height(0.0, -step)) / (2.0 * step);
        let slope = (g_along * g_along + g_across * g_across).sqrt();
        // The equator's way along the ground: away from the nearer pole.
        let pole = [0.0, if up[1] < 0.0 { -1.0 } else { 1.0 }, 0.0];
        let toward_pole = minus(pole, up.map(|c| c * dot(pole, up)));
        let facing = match unit(toward_pole) {
            Some(p) if slope > 1e-12 => {
                // Downhill is against the gradient; the equator is against the pole's way.
                let downhill = (-g_along / slope, -g_across / slope);
                -(downhill.0 * dot(p, ahead) + downhill.1 * dot(p, aside))
            }
            _ => 0.0,
        };
        let here = height(0.0, 0.0);
        let mut blocked = 0.0;
        for (c, s) in AZIMUTHS {
            // The steepest rise to the horizon, as a rise over a run, less the Planet's own fall.
            let mut rise = 0.0_f64;
            for d in HORIZON_STEPS {
                rise = rise.max((height(c * d, s * d) - here) / d - d / (2.0 * sea));
            }
            blocked += (rise / HORIZON_FULL).min(1.0);
        }
        Sunlit {
            slope,
            facing,
            openness: 1.0 - blocked / AZIMUTHS.len() as f64,
        }
    }

    /// Whether `point` is on a wall of a cave, and which way the wall faces: the caves' density
    /// there is within [`WALL_BAND`] of zero and its gradient (differenced over [`WALL_STEP`]) is
    /// at least [`WALL_GRADIENT_MIN`] long, pointing into the cave. A cave is a cave only under the
    /// ground, which the density does not know (see [`Planet::carved`]).
    pub fn cave_wall_at(&self, point: [f64; 3]) -> Option<Wall> {
        let margin = 2.0 * WALL_STEP + WALL_BAND;
        let tubes = self.caves.near(point, point, margin);
        self.wall_in(&tubes, point)
    }

    /// [`Planet::cave_wall_at`] given the `tubes` near `point`.
    fn wall_in(&self, tubes: &[Tube], point: [f64; 3]) -> Option<Wall> {
        let mut cache = None;
        let density = self.caves.density(tubes, point, &mut cache);
        if !density.is_finite() || density.abs() > WALL_BAND {
            return None;
        }
        let mut gradient = [0.0; 3];
        for a in 0..3 {
            let (mut before, mut after) = (point, point);
            before[a] -= WALL_STEP;
            after[a] += WALL_STEP;
            let (low, high) = (
                self.caves.density(tubes, before, &mut None),
                self.caves.density(tubes, after, &mut None),
            );
            if !low.is_finite() || !high.is_finite() {
                return None;
            }
            gradient[a] = (high - low) / (2.0 * WALL_STEP);
        }
        (distance(gradient) >= WALL_GRADIENT_MIN).then(|| Wall {
            density,
            normal: gradient.map(|c| c / distance(gradient)),
        })
    }

    /// How much room there is in front of the surface at `anchor`, which faces `normal`: the local
    /// density. A ball is put in front of the surface, so that it just clears a flat one, and the
    /// share of 14 points round it (six along the axes of the surface's frame, eight at its
    /// corners) that are solid, ground or water, says how much of it is rock: [`Room::Open`] if
    /// that is at most [`ROOM_SOLID_MAX`] for a ball 1.4 m across its radius, [`Room::Small`] if it
    /// is for one of 0.6 m, and [`Room::Closed`] if not. Counts the terrain, not other clusters.
    pub fn room_at(&self, anchor: [f64; 3], normal: [f64; 3]) -> Room {
        let (t1, t2) = tangents(normal);
        let mut directions = Vec::with_capacity(14);
        for sign in [-1.0, 1.0] {
            directions.push(normal.map(|c| c * sign));
            directions.push(t1.map(|c| c * sign));
            directions.push(t2.map(|c| c * sign));
        }
        for (a, b, c) in (0..8).map(|i| (i & 1, i >> 1 & 1, i >> 2 & 1)) {
            let sign = |bit: usize| if bit == 0 { -INV_SQRT3 } else { INV_SQRT3 };
            directions.push(plus(
                plus(normal.map(|c2| c2 * sign(a)), t1, sign(b)),
                t2,
                sign(c),
            ));
        }
        let solid_share = |radius: f64| -> f64 {
            let centre = plus(anchor, normal, radius * ROOM_LIFT);
            let solid = directions
                .iter()
                .filter(|&&d| self.terrain_sized(plus(centre, d, radius), 1.0).is_some())
                .count();
            solid as f64 / directions.len() as f64
        };
        if solid_share(ROOM_OPEN) <= ROOM_SOLID_MAX {
            Room::Open
        } else if solid_share(ROOM_SMALL) <= ROOM_SOLID_MAX {
            Room::Small
        } else {
            Room::Closed
        }
    }

    /// Whether any of `points` is in a texel that the world map marks as crystal country, which
    /// is where clusters are. Texels are 245 m, so a cube of sites, or a brick and the reach of a
    /// cluster, is in one or a few: its corners find them all.
    fn crystal_country(&self, points: &[[f64; 3]]) -> bool {
        points
            .iter()
            .any(|&p| self.map.sample(p).regions.contains(Regions::CRYSTAL_CAVES))
    }

    /// The box from `low` to `high` and its eight corners, for [`Planet::crystal_country`].
    fn box_points(low: [f64; 3], high: [f64; 3]) -> [[f64; 3]; 9] {
        let mut points = [[0.0; 3]; 9];
        for (i, point) in points.iter_mut().take(8).enumerate() {
            *point = std::array::from_fn(|a| if i >> a & 1 == 0 { low[a] } else { high[a] });
        }
        points[8] = std::array::from_fn(|a| (low[a] + high[a]) / 2.0);
        points
    }

    /// How far above the ground a Voxel of the brick or box from `low` to `high` of mip `level`
    /// may be and still be a cluster's: [`REACH`] if clusters can be there, which is where the box
    /// is within the reach of crystal country and a level that has them, and 0 if not.
    pub(super) fn crystal_reach(&self, level: usize, low: [f64; 3], high: [f64; 3]) -> f64 {
        if level > MAX_LEVEL || !self.crystal_country(&Self::box_points(low, high)) {
            return 0.0;
        }
        REACH
    }

    /// The material of the ground or the caves at `point`, a Voxel `size` Voxels wide, without a
    /// cluster: what the terrain says. `None` for air.
    pub(super) fn terrain_sized(&self, point: [f64; 3], size: f64) -> Option<Material> {
        let ground = self.ground_at(point, size, &mut detail::Cache::default());
        if ground.is_none() {
            return ground;
        }
        let half = (size - 1.0) / 2.0;
        let tubes = self.caves.near(point, point, 1.0);
        if !tubes.is_empty() && self.carved(point, half, &tubes, &mut None) {
            return None;
        }
        ground
    }

    /// The cubes of sites from the one with `low` to the one with `high`, each as (its clusters).
    /// The last asked is kept too: the bricks of a row or a column ask the same few cubes.
    pub(crate) fn clusters_near(&self, low: [f64; 3], high: [f64; 3]) -> Vec<Arc<Vec<Cluster>>> {
        let (first, last) = (low.map(site_cube), high.map(site_cube));
        let id = self.caves.id();
        let asked = LAST.with(|last_asked| {
            last_asked
                .borrow()
                .as_ref()
                .filter(|(planet, a, b, _)| *planet == id && *a == first && *b == last)
                .map(|(.., cubes)| cubes.clone())
        });
        if let Some(cubes) = asked {
            return cubes;
        }
        let mut cubes = Vec::new();
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    let clusters = self.sites_in([x, y, z]);
                    if !clusters.is_empty() {
                        cubes.push(clusters);
                    }
                }
            }
        }
        LAST.with(|last_asked| *last_asked.borrow_mut() = Some((id, first, last, cubes.clone())));
        cubes
    }

    /// The clusters whose anchor is in the cube `cube` of sites, from this thread's kept cubes or
    /// worked out.
    fn sites_in(&self, cube: [i32; 3]) -> Arc<Vec<Cluster>> {
        let id = self.caves.id();
        let kept = KEPT.with(|kept| {
            kept.borrow()
                .1
                .iter()
                .find(|(planet, at, _)| *planet == id && *at == cube)
                .map(|(.., made)| made.clone())
        });
        if let Some(kept) = kept {
            return kept;
        }
        let made = Arc::new(self.make_sites(cube));
        KEPT.with(|kept| {
            let mut kept = kept.borrow_mut();
            let (next, entries) = &mut *kept;
            if entries.len() < KEPT_CUBES {
                entries.push((id, cube, made.clone()));
            } else {
                entries[*next] = (id, cube, made.clone());
                *next = (*next + 1) % KEPT_CUBES;
            }
        });
        made
    }

    /// Works out the clusters in the cube `cube` of sites. Nothing in it depends on the order
    /// it is asked in, or on the mip level of the brick that asks.
    fn make_sites(&self, cube: [i32; 3]) -> Vec<Cluster> {
        let low = cube.map(|c| f64::from(c) * f64::from(SITE_CELL));
        let high = low.map(|l| l + f64::from(SITE_CELL));
        // Texels are far bigger than a cube: its corners are in every texel it is in.
        if !self.crystal_country(&Self::box_points(low, high)) {
            return Vec::new();
        }
        // The origin has no direction, and nothing grows there.
        if distance(low) < f64::from(SITE_CELL) * 2.0 {
            return Vec::new();
        }
        let mut found = Vec::new();
        for k in 0..GROUND_CANDIDATES {
            found.extend(self.ground_cluster(cube, low, k));
        }
        self.cave_clusters((low, high), &mut found);
        found
    }

    /// The dominant and the accent hue of the group of sites `key`, as indexes of [`HUES`], and
    /// which of the two the next cluster of it is made of (`dice`).
    fn hue_of(&self, key: u64, dice: &mut Dice) -> Material {
        let seed = self.map.seed() ^ HUE_SALT;
        let dominant = (crate::hash(seed, [key as i32, (key >> 32) as i32, 0]) % 8) as usize;
        let shift = 1 + crate::hash(seed, [key as i32, (key >> 32) as i32, 1]) % 7;
        let accent = (dominant + shift as usize) % 8;
        HUES[if dice.next() < ACCENT_SHARE {
            accent
        } else {
            dominant
        }]
    }

    /// The ground cluster of candidate `k` of the cube `cube` of sites (`low` its corner), if the
    /// candidate is one: it is in crystal country, on sunlit ground (the rule of
    /// [`Planet::sunlit_at`]) at least [`LAND_MIN`] above the sea, with room in front of it.
    fn ground_cluster(&self, cube: [i32; 3], low: [f64; 3], k: u64) -> Option<Cluster> {
        let mut dice = Dice {
            seed: self.map.seed() ^ GROUND_SALT ^ k.wrapping_mul(0x9e37_79b9_7f4a_7c15),
            cube,
            drawn: 0,
        };
        if dice.next() >= GROUND_CHANCE {
            return None;
        }
        let cell = f64::from(SITE_CELL);
        let point: [f64; 3] = std::array::from_fn(|a| low[a] + dice.next() * cell);
        let radius = distance(point);
        let up = point.map(|c| c / radius);
        if !self.map.sample(up).regions.contains(Regions::CRYSTAL_CAVES) {
            return None;
        }
        // The ground's anchor is where the line from the centre through the candidate meets it:
        // the one cube of its column that holds it has it.
        let sea = self.shape.sea;
        if (radius - (sea + self.map.elevation_at(up))).abs() > f64::from(DETAIL_REACH) + cell * 2.0
        {
            return None;
        }
        if !self.sunlit_at(up).is_lit() {
            return None;
        }
        let height = self.ground_height(up);
        if height < LAND_MIN {
            return None;
        }
        let anchor = up.map(|u| u * (sea + height));
        if (0..3).any(|a| anchor[a] < low[a] || anchor[a] >= low[a] + cell) {
            return None;
        }
        let normal = self.ground_normal(up, height);
        let room = self.room_at(anchor, normal);
        if room == Room::Closed {
            return None;
        }
        // Patches of ground share a dominant hue.
        let patch = cube.map(|c| c.div_euclid(HUE_PATCH));
        let key = crate::hash(self.map.seed() ^ HUE_SALT, patch);
        let material = self.hue_of(key, &mut dice);
        let cap = if room == Room::Open {
            MAX_PRISM_LENGTH
        } else {
            SMALL_PRISM_LENGTH
        };
        Some(Cluster::new(
            anchor,
            normal,
            cap,
            (material, Place::Ground),
            &mut dice,
        ))
    }

    /// The unit vector that the ground faces at the point of it in the direction `up`, `height`
    /// above sea level: the gradient of the ground's height over 6 Voxels either way, across the
    /// ground, which is the same as the density's (ground is where it is above zero).
    fn ground_normal(&self, up: [f64; 3], height: f64) -> [f64; 3] {
        const STEP: f64 = 6.0;
        let (ahead, aside) = tangents(up);
        let radius = self.shape.sea + height;
        let at = |across: [f64; 3], d: f64| {
            self.ground_height(std::array::from_fn(|a| up[a] * radius + across[a] * d))
        };
        let g_along = (at(ahead, STEP) - at(ahead, -STEP)) / (2.0 * STEP);
        let g_across = (at(aside, STEP) - at(aside, -STEP)) / (2.0 * STEP);
        unit(plus(plus(up, ahead, -g_along), aside, -g_across)).unwrap_or(up)
    }

    /// The cave clusters whose anchor is in the cube from `low` to `high`, added
    /// to `found`: for each tube near the cube, candidates every [`CAVE_SPACING`] along it and
    /// [`CAVE_SLOTS`] to a place, each a cluster with the chance [`CAVE_CHANCE`] if its anchor, the
    /// wall at the end of a ray from the tube's axis, is a wall (see [`Planet::cave_wall_at`]) in
    /// crystal country with room in front of it.
    fn cave_clusters(&self, (low, high): ([f64; 3], [f64; 3]), found: &mut Vec<Cluster>) {
        let margin = 2.0 * WALL_STEP + WALL_BAND + 1.0;
        let tubes = self.caves.near(low, high, margin);
        if tubes.is_empty() {
            return;
        }
        let seed = self.map.seed() ^ CAVE_SALT;
        let inside = |p: [f64; 3]| (0..3).all(|a| p[a] >= low[a] && p[a] < high[a]);
        for tube in &tubes {
            let Some((from, to)) = tube.span_in(low, high, margin) else {
                continue;
            };
            let length = distance(minus(tube.b, tube.a));
            let places = (length / CAVE_SPACING).ceil().max(1.0);
            let id = bits_hash(
                seed,
                &[
                    tube.a[0], tube.a[1], tube.a[2], tube.b[0], tube.b[1], tube.b[2], tube.ra,
                    tube.rb,
                ],
            );
            let (first, last) = ((from * places).floor() as i64, (to * places).ceil() as i64);
            for place in first.max(0)..=last.min(places as i64 - 1) {
                for slot in 0..CAVE_SLOTS {
                    let mut dice = Dice {
                        seed: id ^ slot.wrapping_mul(0x9e37_79b9_7f4a_7c15),
                        cube: [place as i32, slot as i32, 0],
                        drawn: 0,
                    };
                    if dice.next() >= CAVE_CHANCE {
                        continue;
                    }
                    let along = (place as f64 + dice.next()) / places;
                    let (on_axis, radius) = tube.at(along);
                    let about = [0.0; 3].map(|_| dice.between((-1.0, 1.0)));
                    let axis = unit(minus(tube.b, tube.a));
                    let Some(away) = axis
                        .and_then(|axis| unit(minus(about, axis.map(|c| c * dot(about, axis)))))
                    else {
                        continue;
                    };
                    let reach = radius + ROUGHNESS + 1.0;
                    let Some(anchor) = self.wall_along(on_axis, away, reach) else {
                        continue;
                    };
                    if !inside(anchor) {
                        continue;
                    }
                    if !self.crystal_country(&[anchor]) {
                        continue;
                    }
                    let near = self.caves.near(anchor, anchor, margin);
                    let Some(wall) = self.wall_in(&near, anchor) else {
                        continue;
                    };
                    // Into the cave, with the rock behind it: the wall is what it says.
                    if self
                        .terrain_sized(plus(anchor, wall.normal, 1.5), 1.0)
                        .is_some()
                        || self
                            .terrain_sized(plus(anchor, wall.normal, -1.5), 1.0)
                            .is_none()
                    {
                        continue;
                    }
                    let room = self.room_at(anchor, wall.normal);
                    if room == Room::Closed {
                        continue;
                    }
                    let material = self.hue_of(tube.system, &mut dice);
                    let cap = if room == Room::Open {
                        MAX_PRISM_LENGTH
                    } else {
                        SMALL_PRISM_LENGTH
                    };
                    found.push(Cluster::new(
                        anchor,
                        wall.normal,
                        cap,
                        (material, Place::Cave(tube.system)),
                        &mut dice,
                    ));
                }
            }
        }
    }

    /// The wall where the ray from `start`, a point of the air of a cave, along the unit vector
    /// `away`, goes from air to solid within `reach` Voxels, found by halving: `None` if it does
    /// not (the start is in rock, or the far end is not).
    fn wall_along(&self, start: [f64; 3], away: [f64; 3], reach: f64) -> Option<[f64; 3]> {
        if self.terrain_sized(start, 1.0).is_some()
            || self.terrain_sized(plus(start, away, reach), 1.0).is_none()
        {
            return None;
        }
        let (mut near, mut far) = (0.0, reach);
        for _ in 0..WALL_BISECTIONS {
            let middle = (near + far) / 2.0;
            if self.terrain_sized(plus(start, away, middle), 1.0).is_none() {
                near = middle;
            } else {
                far = middle;
            }
        }
        Some(plus(start, away, (near + far) / 2.0))
    }

    /// Makes crystal of the air Voxels of `brick`, the brick at `coord` of mip `level`, that a
    /// cluster has a prism round: the middle of a Voxel of the brick is inside one. Does nothing
    /// above [`MAX_LEVEL`], or where no cluster is within [`REACH`] of the brick (the crystal
    /// country, the ground and the caves are the cubes of sites'): only air Voxels change.
    pub(super) fn plant_crystals(&self, level: usize, coord: BrickCoord, brick: &mut Brick) {
        if level > MAX_LEVEL {
            return;
        }
        let samples = Samples::new(level, coord);
        let (low, high) = (samples.corner([0; 3]), samples.corner([1; 3]));
        let (low, high) = (low.map(|c| c - REACH), high.map(|c| c + REACH));
        let may = self.may_hold_crystals(&samples);
        if !may {
            return;
        }
        let cubes = self.clusters_near(low, high);
        let (inner_low, inner_high) = (samples.corner([0; 3]), samples.corner([1; 3]));
        let near: Vec<&Cluster> = cubes
            .iter()
            .flat_map(|cube| cube.iter())
            .filter(|c| {
                c.reaches(
                    inner_low.map(|c| c - samples.size),
                    inner_high.map(|c| c + samples.size),
                )
            })
            .collect();
        if near.is_empty() {
            return;
        }
        // Clusters are tried in the order of the cubes and of the cube's list, which is fixed.
        for z in 0..8 {
            for y in 0..8 {
                for x in 0..8 {
                    let local = [x, y, z];
                    if brick.get(local).is_some() {
                        continue;
                    }
                    let point = samples.at(local);
                    if let Some(cluster) = near.iter().find(|c| c.depth(point) >= 0.0) {
                        brick.set(local, Some(cluster.material));
                    }
                }
            }
        }
    }

    /// Whether the brick sampled at `samples` may have a cluster: not in the sky above every
    /// cluster there is, and not in the interior, below every cave. The rest is for the cubes of
    /// sites to say (the crystal country, the ground and the caves are theirs), and the last
    /// box a thread asked about is kept, so that most bricks of a row ask nothing new.
    fn may_hold_crystals(&self, samples: &Samples) -> bool {
        let (near, far) = samples.radii();
        near < f64::from(self.shape.max) && far >= f64::from(self.shape.interior)
    }

    /// The material of the cluster that `point` is in, a Voxel `size` Voxels wide (see
    /// [`Planet::terrain_sized`] for the terrain): `None` if it is in none. Level 0 is `size`
    /// 1; a coarse Voxel is as [`Planet::plant_crystals`] has it.
    pub fn crystal_at(&self, point: [f64; 3], size: f64) -> Option<Material> {
        if !(1.0..=f64::from(1u32 << MAX_LEVEL)).contains(&size) {
            return None;
        }
        let (low, high) = (point.map(|c| c - REACH), point.map(|c| c + REACH));
        if !self.crystal_country(&Self::box_points(low, high)) {
            return None;
        }
        self.clusters_near(low, high)
            .iter()
            .flat_map(|cube| cube.iter())
            .find(|c| c.depth(point) >= 0.0)
            .map(|c| c.material)
    }

    /// The clusters whose anchor is in the box from `low` to `high`, in the order of the cubes of
    /// sites (x fastest, then y, then z) and of the clusters of a cube: for the tools that look
    /// for one and the tests.
    pub fn crystal_sites(&self, low: [f64; 3], high: [f64; 3]) -> Vec<CrystalSite> {
        self.clusters_near(low, high)
            .iter()
            .flat_map(|cube| cube.iter())
            .filter(|c| (0..3).all(|a| c.anchor[a] >= low[a] && c.anchor[a] <= high[a]))
            .map(Cluster::site)
            .collect()
    }

    /// FNV-1a hash of the clusters whose anchor is in the box from `low` to `high`, bit for bit
    /// (their anchors, normals, Materials and every prism), in the order of
    /// [`Planet::crystal_sites`]: what the golden test holds the placement to, on every platform.
    pub fn crystals_hash(&self, low: [f64; 3], high: [f64; 3]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed =
            |byte: u8| hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        for cluster in self
            .clusters_near(low, high)
            .iter()
            .flat_map(|cube| cube.iter())
            .filter(|c| (0..3).all(|a| c.anchor[a] >= low[a] && c.anchor[a] <= high[a]))
        {
            let mut numbers: Vec<f64> = cluster.anchor.into_iter().chain(cluster.normal).collect();
            for p in &cluster.prisms {
                numbers.extend(p.base.into_iter().chain(p.axis));
                numbers.extend(p.sides.iter().flatten());
                numbers.extend([p.length, p.apothem, p.tip, p.slant]);
            }
            for value in numbers {
                value.to_le_bytes().into_iter().for_each(&mut feed);
            }
            cluster
                .material
                .0
                .to_le_bytes()
                .into_iter()
                .for_each(&mut feed);
        }
        hash
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::planet::tests::planet;
    use brick_store::{BrickCoord, VoxelCoord};
    use std::sync::OnceLock;

    /// Where the test Planet has crystal clusters: found once, from boxes round lit texels of the
    /// crystal country.
    pub(in crate::planet) struct Scan {
        /// Directions of lit texels of the country (see [`Sunlit::is_lit`]).
        pub lit: Vec<[f64; 3]>,
        pub ground: Vec<CrystalSite>,
        pub cave: Vec<CrystalSite>,
        /// Boxes (low, high) that were searched.
        pub boxes: Vec<([f64; 3], [f64; 3])>,
    }

    pub(in crate::planet) fn scan() -> &'static Scan {
        static SCAN: OnceLock<Scan> = OnceLock::new();
        SCAN.get_or_init(|| {
            let planet = planet();
            let map = planet.map();
            let mut lit = Vec::new();
            for face in 0..6 {
                for j in 0..map.size() {
                    for i in 0..map.size() {
                        let t = map.texel(face, i, j);
                        if t.elevation < 20 || !t.regions.contains(Regions::CRYSTAL_CAVES) {
                            continue;
                        }
                        let d = map.texel_direction(face, i, j);
                        if planet.sunlit_at(d).is_lit() {
                            lit.push(d);
                        }
                    }
                }
            }
            assert!(lit.len() > 200, "{} lit texels", lit.len());
            let (mut ground, mut cave, mut boxes) = (Vec::new(), Vec::new(), Vec::new());
            for d in lit.iter().step_by(lit.len() / 40) {
                let up = unit(*d).unwrap();
                let at = up.map(|u| u * (planet.sea_radius() + planet.ground_height(up)));
                let (low, high) = (at.map(|c| c - 1280.0), at.map(|c| c + 1280.0));
                boxes.push((low, high));
                for site in planet.crystal_sites(low, high) {
                    match site.place {
                        Place::Ground => ground.push(site),
                        Place::Cave(_) => cave.push(site),
                    }
                }
            }
            Scan {
                lit,
                ground,
                cave,
                boxes,
            }
        })
    }

    impl Scan {
        pub fn all(&self) -> impl Iterator<Item = &CrystalSite> {
            self.ground.iter().chain(&self.cave)
        }
    }

    /// A point on the principal prism of a site: half way up it.
    pub(in crate::planet) fn along_prism(site: &CrystalSite) -> [f64; 3] {
        plus(site.anchor, site.normal, (site.length - EMBED) / 2.0)
    }

    /// The brick of mip `level` that holds `point`.
    pub(in crate::planet) fn brick_of(point: [f64; 3], level: usize) -> BrickCoord {
        let mut coord = VoxelCoord(point.map(|c| c.round() as i32)).brick();
        for _ in 0..level {
            coord = coord.parent();
        }
        coord
    }

    fn is_crystal(material: Option<Material>) -> bool {
        material.is_some_and(|m| HUES.contains(&m))
    }

    /// A deterministic spread of numbers in `[-1, 1)`, for the tests of geometry.
    fn spread(n: usize) -> Vec<f64> {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f64 / (1u64 << 52) as f64 - 1.0
            })
            .collect()
    }

    fn upright(length: f64) -> Prism {
        Prism::new([0.0; 3], [0.0, 0.0, 1.0], length, [1.0, 0.0, 0.0])
    }

    #[test]
    fn a_prism_is_a_hexagon_with_a_pointed_tip() {
        let p = upright(40.0);
        let r = p.apothem;
        let at = |angle_deg_index: usize, out: f64, up: f64| -> f64 {
            // The six side directions (±sides) and the six corners between them.
            let dirs = [
                p.sides[0].map(|c| -c),
                p.sides[1].map(|c| -c),
                p.sides[2],
                p.sides[0],
                p.sides[1],
                p.sides[2].map(|c| -c),
            ];
            let d = dirs[angle_deg_index];
            p.depth([d[0] * out, d[1] * out, up])
        };
        // Every one of the six sides is the same distance from the axis, a little less than the
        // apothem is inside and a little more is outside.
        for k in 0..6 {
            assert!(at(k, r * 0.95, 10.0) > 0.0, "side {k} inside");
            assert!(at(k, r * 1.05, 10.0) < 0.0, "side {k} outside");
        }
        // The corners are at the apothem over cos 30 degrees (1.155), between the sides: a point
        // at 1.1 apothems towards each of the six is inside and at 1.25 is outside, which a round
        // section (all alike) or a square one (corners at 1.41, inside at 1.25) does not do.
        let [s0, s1, s2] = p.sides;
        let neg = |a: [f64; 3]| a.map(|c| -c);
        for sum in [plus(s0, s1, 1.0), plus(s1, s2, 1.0), plus(s2, neg(s0), 1.0)] {
            let d = unit(sum).unwrap();
            for sign in [1.0, -1.0] {
                let (near, far) = (r * 1.1 * sign, r * 1.25 * sign);
                assert!(
                    p.depth([d[0] * near, d[1] * near, 10.0]) > 0.0,
                    "corner inside"
                );
                assert!(
                    p.depth([d[0] * far, d[1] * far, 10.0]) < 0.0,
                    "corner outside"
                );
            }
        }
        // The tip: on the axis the prism reaches its length and not past it, and off the axis
        // near the top it has narrowed to nothing, where lower down it is full width.
        assert!(p.depth([0.0, 0.0, 39.9]) > 0.0);
        assert!(p.depth([0.0, 0.0, 40.1]) < 0.0);
        let off = [p.sides[0][0] * r * 0.8, p.sides[0][1] * r * 0.8, 0.0];
        assert!(p.depth([off[0], off[1], 10.0]) > 0.0);
        assert!(p.depth([off[0], off[1], 39.0]) < 0.0, "pointed, not flat");
        // And its base is flat: nothing below it.
        assert!(p.depth([0.0, 0.0, -0.1]) < 0.0);
    }

    #[test]
    fn a_prism_depth_is_the_radius_of_a_ball_that_fits_in_it() {
        // The least of the distances to the 13 planes: a ball of the depth, around any point of
        // the prism, has every point of its surface inside it (to rounding), and one of a little
        // more has some point outside along the nearest plane's normal.
        let numbers = spread(3 * 600);
        for length in [6.0, 14.0, 32.0] {
            let axis = unit([0.3, -0.5, 0.8]).unwrap();
            let base = [1.0, 2.0, 3.0];
            let p = Prism::new(base, axis, length, [0.2, 0.9, -0.1]);
            let other = cross(axis, p.sides[0]);
            let mut inside = 0;
            for chunk in numbers.chunks(3) {
                // Points along the prism and across it, out to a little past its flats.
                let up = (chunk[0] * 0.5 + 0.5) * length;
                let (a, b) = (chunk[1] * p.apothem * 1.2, chunk[2] * p.apothem * 1.2);
                let point = plus(plus(plus(base, axis, up), p.sides[0], a), other, b);
                let depth = p.depth(point);
                if depth <= 0.05 {
                    continue;
                }
                inside += 1;
                for dir in spread(3 * 40).chunks(3) {
                    let Some(d) = unit([dir[0], dir[1], dir[2]]) else {
                        continue;
                    };
                    let probe = plus(point, d, depth * 0.999);
                    assert!(
                        p.depth(probe) >= -1e-9,
                        "the ball of {depth} leaves the prism"
                    );
                }
            }
            assert!(inside > 20, "length {length}: {inside} points inside");
        }
    }

    #[test]
    fn the_smallest_prism_is_six_voxels_long_and_three_across() {
        // 0.3 m is 6 Voxels at 5 cm, and the hexagon is at least 3 Voxels across its flats.
        const { assert!(MIN_PRISM_LENGTH >= 6.0) };
        let p = upright(MIN_PRISM_LENGTH);
        assert!(2.0 * p.apothem >= 3.0, "{}", p.apothem);
        assert!(p.depth([0.0, 0.0, MIN_PRISM_LENGTH * 0.4]) >= p.apothem * 0.9);
        // None of the clusters that are made has a prism shorter than that.
        let scan = scan();
        assert!(scan.all().count() > 50);
        for site in scan.all() {
            assert!(site.length >= MIN_PRISM_LENGTH, "{site:?}");
            assert!(site.length <= MAX_PRISM_LENGTH, "{site:?}");
            assert!((3..=7).contains(&site.prisms), "{site:?}");
        }
    }

    #[test]
    fn the_cluster_reach_holds_every_cluster() {
        // `REACH` is what the shortcuts add to the ground: no Voxel of a cluster is further from
        // its anchor, whatever the dice made.
        let planet = planet();
        let scan = scan();
        let mut checked = 0;
        for (low, high) in &scan.boxes {
            for cube in planet.clusters_near(*low, *high) {
                for cluster in cube.iter() {
                    assert!(cluster.bound <= REACH, "{} > {REACH}", cluster.bound);
                    checked += 1;
                }
            }
        }
        assert!(checked > 50);
    }

    #[test]
    fn sunlit_ground_clusters_are_on_lit_slopes_and_grow_along_the_surface_normal() {
        let planet = planet();
        let scan = scan();
        assert!(scan.ground.len() > 30, "{}", scan.ground.len());
        for site in &scan.ground {
            let up = unit(site.anchor).unwrap();
            assert!(planet.sunlit_at(up).is_lit(), "{site:?}");
            // The ground is at most a slope of 0.6 (31 degrees) off level: the normal is near
            // the vertical, and not the vertical itself on the steeper ground.
            let cos = dot(up, site.normal);
            assert!(cos >= 0.85, "{cos}");
            // The principal prism is along it: a point up the normal is in the cluster, and one
            // as far to the side is not (the cluster is thin, a prism across is 3 to 6 Voxels).
            assert!(
                planet.crystal_at(along_prism(site), 1.0) == Some(site.material),
                "{site:?}"
            );
        }
        // A good share of them lean: the ground has slopes, and so do their normals.
        let leaning = scan
            .ground
            .iter()
            .filter(|s| dot(unit(s.anchor).unwrap(), s.normal) < 0.995)
            .count();
        assert!(leaning * 10 > scan.ground.len(), "{leaning} lean");
    }

    #[test]
    fn a_cave_cluster_grows_into_the_cave_from_a_wall_along_its_normal() {
        let planet = planet();
        let scan = scan();
        assert!(scan.cave.len() > 30, "{}", scan.cave.len());
        for site in &scan.cave {
            let wall = planet.cave_wall_at(site.anchor).expect("a wall");
            assert!(dot(wall.normal, site.normal) > 0.999999);
            // Air in front of it, rock behind.
            assert!(
                planet
                    .terrain_sized(plus(site.anchor, site.normal, 1.5), 1.0)
                    .is_none(),
                "{site:?}"
            );
            assert!(
                planet
                    .terrain_sized(plus(site.anchor, site.normal, -1.5), 1.0)
                    .is_some(),
                "{site:?}"
            );
            assert!(
                planet.crystal_at(along_prism(site), 1.0).is_some(),
                "{site:?}"
            );
        }
    }

    #[test]
    fn the_sunlit_rule_wants_a_slope_that_faces_the_equator_under_open_sky() {
        let good = Sunlit {
            slope: 0.3,
            facing: 0.9,
            openness: 0.95,
        };
        assert!(good.is_lit());
        for bad in [
            Sunlit {
                slope: 0.02,
                ..good
            },
            Sunlit { slope: 0.9, ..good },
            Sunlit {
                facing: 0.0,
                ..good
            },
            Sunlit {
                facing: -0.9,
                ..good
            },
            Sunlit {
                openness: 0.3,
                ..good
            },
        ] {
            assert!(!bad.is_lit(), "{bad:?}");
        }
    }

    #[test]
    fn lit_ground_faces_the_equator_and_has_sun() {
        // Heights taken on their own, from the world map: a point of lit ground is lower towards
        // the equator than towards the pole (it faces the equator) in nearly every case, and the
        // opposite on ground that faces the pole is true.
        let planet = planet();
        let scan = scan();
        let map = planet.map();
        let sea = planet.sea_radius();
        let (mut towards_equator, mut counted) = (0, 0);
        let mut towards_pole_lit = 0;
        for d in &scan.lit {
            let up = unit(*d).unwrap();
            let pole = [0.0, up[1].signum(), 0.0];
            let along_ground = unit(minus(pole, up.map(|c| c * dot(pole, up)))).unwrap();
            let height = |by: f64| {
                map.elevation_at(std::array::from_fn(|a| up[a] * sea + along_ground[a] * by))
            };
            let (pole_side, equator_side) = (height(240.0), height(-240.0));
            counted += 1;
            if equator_side < pole_side {
                towards_equator += 1;
            }
            // Facing the pole: higher towards the equator. A lit place is not one.
            if planet.sunlit_at(up).facing < 0.0 {
                towards_pole_lit += 1;
            }
        }
        assert_eq!(towards_pole_lit, 0);
        assert!(
            towards_equator * 10 >= counted * 9,
            "{towards_equator} of {counted}"
        );
        // And the rule is not everything: the country has ground that is not lit.
        let mut unlit = 0;
        for face in 0..6 {
            for i in (0..map.size()).step_by(3) {
                let t = map.texel(face, i, i);
                if t.elevation >= 20 && t.regions.contains(Regions::CRYSTAL_CAVES) {
                    unlit +=
                        usize::from(!planet.sunlit_at(map.texel_direction(face, i, i)).is_lit());
                }
            }
        }
        assert!(unlit > 5, "{unlit}");
    }

    #[test]
    fn a_room_in_front_of_rock_is_closed_and_in_front_of_open_air_is_open() {
        let planet = planet();
        let scan = scan();
        let site = scan.ground[0];
        let buried = plus(site.anchor, site.normal, -25.0);
        assert_eq!(planet.room_at(buried, site.normal), Room::Closed);
        let sky = plus(site.anchor, site.normal, 120.0);
        assert_eq!(planet.room_at(sky, site.normal), Room::Open);
        // Every cluster that was made has a room (not closed), and some have only a small one.
        for site in scan.all() {
            assert!(planet.room_at(site.anchor, site.normal) != Room::Closed);
        }
        let (mut small, mut open) = (0, 0);
        for site in scan.all() {
            match planet.room_at(site.anchor, site.normal) {
                Room::Small => small += 1,
                Room::Open => open += 1,
                Room::Closed => unreachable!(),
            }
        }
        assert!(open > 0 && small > 0, "{open} open, {small} small");
        // A small room has only short prisms.
        for site in scan.all() {
            if planet.room_at(site.anchor, site.normal) == Room::Small {
                assert!(site.length <= SMALL_PRISM_LENGTH, "{site:?}");
            }
        }
    }

    #[test]
    fn a_passage_is_not_open_and_a_chamber_is() {
        // A tube is the narrowest thing a cave has: in the middle of a stretch under 1.6 m in
        // radius, the room round a point is not open to a ball of 1.4 m; in the middle of a
        // chamber of 2.8 m or more it is, facing across the tube.
        let planet = planet();
        let scan = scan();
        let (mut narrow, mut wide) = (0, 0);
        for site in scan.cave.iter() {
            for tube in planet.caves.near(site.anchor, site.anchor, 100.0) {
                let (on_axis, radius) = tube.at(0.5);
                if planet.terrain_sized(on_axis, 1.0).is_some() {
                    continue;
                }
                let axis = unit(minus(tube.b, tube.a)).unwrap();
                let across = unit(cross(axis, [0.0, 1.0, 0.0]))
                    .or_else(|| unit(cross(axis, [1.0, 0.0, 0.0])))
                    .unwrap();
                if radius < v(1.6) {
                    narrow += 1;
                    for normal in [across, cross(axis, across)] {
                        assert!(planet.room_at(on_axis, normal) != Room::Open, "{radius}");
                    }
                } else if radius >= v(2.8) {
                    wide += 1;
                    assert_eq!(planet.room_at(on_axis, across), Room::Open, "{radius}");
                }
            }
        }
        assert!(narrow > 0 && wide > 0, "{narrow} narrow, {wide} wide tubes");
    }

    #[test]
    fn a_cluster_only_fills_air() {
        // Every Voxel that the terrain makes solid is as the terrain has it, in bricks with
        // clusters in them, and some Voxels of air have become crystal.
        let planet = planet();
        let scan = scan();
        let (mut crystal, mut bricks) = (0, 0);
        for site in scan.all().step_by(3) {
            let coord = brick_of(along_prism(site), 0);
            let made = planet.brick(coord);
            let terrain = planet.terrain(0, coord);
            bricks += 1;
            for z in 0..8u8 {
                for y in 0..8u8 {
                    for x in 0..8u8 {
                        let local = [x, y, z];
                        match terrain.get(local) {
                            Some(m) => assert_eq!(made.get(local), Some(m)),
                            None => crystal += usize::from(is_crystal(made.get(local))),
                        }
                    }
                }
            }
        }
        assert!(
            bricks > 20 && crystal > 100,
            "{bricks} bricks, {crystal} Voxels"
        );
    }

    #[test]
    fn the_hues_are_the_eight_crystal_materials_and_a_cave_has_a_dominant_one() {
        let planet = planet();
        let scan = scan();
        for site in scan.all() {
            assert!(HUES.contains(&site.material), "{site:?}");
        }
        // The caves by system: most of a cave's clusters are of its dominant hue, and caves differ.
        let mut caves: Vec<(u64, Vec<Material>)> = Vec::new();
        for site in &scan.cave {
            let Place::Cave(id) = site.place else {
                unreachable!()
            };
            match caves.iter_mut().find(|(c, _)| *c == id) {
                Some((_, hues)) => hues.push(site.material),
                None => caves.push((id, vec![site.material])),
            }
        }
        let mut dominants = Vec::new();
        let mut judged = 0;
        for (_, hues) in &caves {
            let (commonest, count) = HUES
                .iter()
                .map(|h| (*h, hues.iter().filter(|m| *m == h).count()))
                .max_by_key(|&(_, n)| n)
                .unwrap();
            if hues.len() >= 6 {
                judged += 1;
                assert!(
                    count * 10 >= hues.len() * 6,
                    "{count} of {} of the commonest hue",
                    hues.len()
                );
            }
            dominants.push(commonest);
        }
        assert!(
            judged >= 3,
            "{judged} caves with enough clusters ({} caves)",
            caves.len()
        );
        dominants.sort_by_key(|m| m.0);
        dominants.dedup();
        assert!(dominants.len() >= 3, "{dominants:?}");
        // The accent is there: not every cluster of a big cave is the one hue.
        let accents: usize = caves
            .iter()
            .filter(|(_, h)| h.len() >= 10)
            .map(|(_, h)| h.iter().filter(|m| **m != h[0]).count().min(1))
            .sum();
        assert!(accents > 0 || caves.iter().all(|(_, h)| h.len() < 10));
        // The hue of a cave is what its system says, not where the cluster is: asking again
        // gives the same.
        let again = planet.crystal_sites(scan.boxes[0].0, scan.boxes[0].1);
        let first: Vec<_> = scan
            .all()
            .filter(|s| {
                (0..3)
                    .all(|a| s.anchor[a] >= scan.boxes[0].0[a] && s.anchor[a] <= scan.boxes[0].1[a])
            })
            .collect();
        assert_eq!(again.len(), first.len());
    }

    #[test]
    fn there_are_no_clusters_outside_the_crystal_caves_region() {
        let planet = planet();
        let scan = scan();
        let map = planet.map();
        for site in scan.all() {
            assert!(
                map.sample(site.anchor)
                    .regions
                    .contains(Regions::CRYSTAL_CAVES),
                "{site:?}"
            );
        }
        // Ground that is lit but not crystal country has none, nor do its caves.
        let mut searched = 0;
        'faces: for face in 0..6 {
            for j in (0..map.size()).step_by(4) {
                for i in (0..map.size()).step_by(4) {
                    let t = map.texel(face, i, j);
                    if t.elevation < 20 || t.regions.contains(Regions::CRYSTAL_CAVES) {
                        continue;
                    }
                    let up = unit(map.texel_direction(face, i, j)).unwrap();
                    if !planet.sunlit_at(up).is_lit() {
                        continue;
                    }
                    let at = up.map(|u| u * (planet.sea_radius() + planet.ground_height(up)));
                    // The box has to be wholly outside the region too: the texels are 245 m.
                    let half = 200.0;
                    let (low, high) = (at.map(|c| c - half), at.map(|c| c + half));
                    if Planet::box_points(low.map(|c| c - REACH), high.map(|c| c + REACH))
                        .iter()
                        .any(|&p| map.sample(p).regions.contains(Regions::CRYSTAL_CAVES))
                    {
                        continue;
                    }
                    assert!(planet.crystal_sites(low, high).is_empty());
                    assert_eq!(planet.crystal_at(at, 1.0), None);
                    searched += 1;
                    if searched >= 30 {
                        break 'faces;
                    }
                }
            }
        }
        assert!(searched >= 10, "{searched} lit places outside the region");
    }

    #[test]
    fn clusters_are_the_same_from_a_fresh_thread_and_a_warm_cache() {
        let planet = planet();
        let scan = scan();
        let (low, high) = scan.boxes[3];
        let first = planet.crystals_hash(low, high);
        assert_ne!(
            first,
            planet.crystals_hash(low.map(|c| c + 5000.0), high.map(|c| c + 5000.0))
        );
        // Warm: the cubes are kept now; fresh: another thread has none, and asks in the other
        // order.
        let warm = planet.crystals_hash(low, high);
        let fresh = std::thread::scope(|s| {
            s.spawn(|| {
                let coord = brick_of(along_prism(&scan.all().next().unwrap().clone()), 0);
                planet.brick(coord);
                planet.crystals_hash(low, high)
            })
            .join()
            .unwrap()
        });
        assert_eq!(first, warm);
        assert_eq!(first, fresh);
        // A brick is the same whether the cubes round it were made for it or for a neighbour.
        let site = scan.all().nth(5).unwrap();
        let coord = brick_of(along_prism(site), 0);
        let alone = std::thread::scope(|s| {
            s.spawn(|| planet.brick(coord).content_hash())
                .join()
                .unwrap()
        });
        let after_neighbours = std::thread::scope(|s| {
            s.spawn(|| {
                for dz in -1..=1 {
                    for dy in -1..=1 {
                        planet.brick(BrickCoord([
                            coord.0[0] + 1,
                            coord.0[1] + dy,
                            coord.0[2] + dz,
                        ]));
                    }
                }
                planet.brick(coord).content_hash()
            })
            .join()
            .unwrap()
        });
        assert_eq!(alone, after_neighbours);
    }

    #[test]
    fn a_voxel_is_what_its_brick_says_in_the_bricks_with_clusters() {
        let planet = planet();
        let scan = scan();
        let mut crystal = 0;
        for site in scan.all().step_by(4) {
            let coord = brick_of(along_prism(site), 0);
            let brick = planet.brick(coord);
            for z in 0..8u8 {
                for y in 0..8u8 {
                    for x in 0..8u8 {
                        let local = [x, y, z];
                        let voxel = coord.voxel(local);
                        assert_eq!(brick.get(local), planet.voxel(voxel), "{coord:?} {local:?}");
                        crystal += usize::from(is_crystal(brick.get(local)));
                    }
                }
            }
        }
        assert!(crystal > 100, "{crystal}");
    }

    #[test]
    fn air_below_is_never_claimed_for_a_brick_with_a_cluster_in_it() {
        let planet = planet();
        let scan = scan();
        let mut checked = 0;
        for site in scan.all() {
            for level in 0..=MAX_LEVEL {
                let coord = brick_of(along_prism(site), level);
                let brick = if level == 0 {
                    planet.brick(coord)
                } else {
                    planet.coarse_brick(level, coord)
                };
                if (0..512usize).any(|i| {
                    is_crystal(brick.get([(i % 8) as u8, (i / 8 % 8) as u8, (i / 64) as u8]))
                }) {
                    checked += 1;
                    assert!(!planet.air_below(level, coord), "{level} {coord:?}");
                }
            }
        }
        assert!(checked > 50, "{checked}");
        // The tallest cluster and the highest brick it can be in: the shortcut for the sky does
        // not clip it.
        let top = scan
            .ground
            .iter()
            .map(|s| distance(plus(s.anchor, s.normal, s.length)))
            .fold(0.0, f64::max);
        assert!(f64::from(planet.max_radius()) >= top);
    }

    #[test]
    fn there_are_no_clusters_above_level_3() {
        // Level 4 has 80 cm Voxels, wider than a cluster's thickest prism (the pyramid leaves
        // almost nothing of one at level 3, which the test of the pyramid counts): not here, and
        // not in `crystal_at`, whatever `MAX_LEVEL` says.
        let planet = planet();
        let scan = scan();
        let mut with_crystal = [0usize; 5];
        for site in scan.all() {
            for (level, count) in with_crystal.iter_mut().enumerate().skip(1) {
                let coord = brick_of(along_prism(site), level);
                let brick = planet.coarse_brick(level, coord);
                if (0..512usize).any(|i| {
                    is_crystal(brick.get([(i % 8) as u8, (i / 8 % 8) as u8, (i / 64) as u8]))
                }) {
                    *count += 1;
                }
            }
            assert_eq!(planet.crystal_at(along_prism(site), 16.0), None);
        }
        assert_eq!(with_crystal[4], 0, "{with_crystal:?}");
        assert!(with_crystal[1] > 10, "{with_crystal:?}");
    }
}
