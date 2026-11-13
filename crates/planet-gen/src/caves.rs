//! Caves: tunnels and chambers in the crust under land, which break the surface in places.
//!
//! A cave is a *tube*: a straight piece of tunnel from one point to another, whose radius runs
//! from one end's to the other's, so a chain of them makes a winding passage with chambers where
//! the radius swells. A *system* is a chain of tubes grown by a random walk from an anchor under
//! the ground, with a few side branches. Everything is made from the seed and the world map
//! alone, a cube of the world at a time and as it is asked for (ADR 0008: at 320 km the Planet has
//! millions of cubes of ground, which are too many to make or hold, and a cube is a pure function
//! of the seed and the cube, so it costs a few microseconds whenever it is made again): [`Caves`]
//! hands out the tubes of a cube, and keeps the last few that a thread asked for. A Voxel is then
//! a cave if it is inside a tube, which a brick finds out by asking for the few tubes near it:
//! most bricks have none near, and cost a lookup.
//!
//! The limits of a cave hold by construction, not by luck:
//! - **Depth.** Only a Voxel less than [`MAX_DEPTH`] (300 m) below the ground is carved, and only
//!   where the ground is land. Above the ground a tube is only air.
//! - **Width.** A tube's radius is at most [`MAX_TUBE_RADIUS`] (3.6 m), so a tunnel is at most
//!   7.2 m across, and a chamber is no wider. Where tubes meet, the space between them can be a
//!   little wider than one tube, but not by much: three tubes at right angles through one
//!   point leave room for a ball 1.22 times a tube's radius.
//! - **Reach.** The world is cut into cubes of [`CELL`] Voxels (1.02 km). A system belongs to
//!   one cube, and every point of every tube of it is within [`SPAN`] (490 m) of the cube's
//!   middle, which is wholly inside the cube. So systems of different cubes never touch (two of
//!   one cube may meet), and no two points of a cave, which is connected, are farther apart than
//!   980 m. That is the horizon cap's bound (#81): a cave's interior cannot be seen from beyond
//!   a kilometre of it.
//!
//! The walk and the table use only `+ - * /`, `sqrt`, comparisons and integer hashing, as the
//! rest of the generator does (ADR 0003): no `sin` or `cos`, which differ between platforms.
//! Directions come from random vectors that are normalised, not from random angles.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::planet::tangents;
use crate::world_map::{PEAK, SEA_LEVEL, WorldMap, voxels};
use crate::{Corners, distance, hash, value_noise};

/// The size of `metres`, in Voxels, as a float.
const fn v(metres: f64) -> f64 {
    voxels(metres) as f64
}

/// Voxels along the edge of the cubes that the world is cut into: 1.024 km.
pub const CELL: i32 = voxels(1_024.0);

/// How many cubes a thread keeps: a walk and its bricks stay within a few.
const KEPT: usize = 48;

/// How far below the ground caves go, in Voxels: 300 m.
pub const MAX_DEPTH: f64 = v(300.0);
/// How far, from the tube's own radius, the wall's roughness moves the cave's surface: 40 cm.
pub const ROUGHNESS: f64 = v(0.4);
/// The widest a tube gets, roughness and all: its radius, in Voxels, 3.6 m.
pub const MAX_TUBE_RADIUS: f64 = CHAMBER.1 + ROUGHNESS;
/// Radii of a passage at its ends, and of a chamber, in Voxels: 0.8 to 2 m, and 2.4 to 3.2 m.
const PASSAGE: (f64, f64) = (v(0.8), v(2.0));
const CHAMBER: (f64, f64) = (v(2.4), v(3.2));
/// How many of the ends of tubes are chambers.
const CHAMBER_SHARE: f64 = 0.25;
/// How far from the middle of its cube a system gets, tubes' radii included, in Voxels: 490 m,
/// 30 m short of the cube's side, and so the most that a cave reaches is twice this, 980 m.
pub const SPAN: f64 = v(490.0);
/// How far from the middle of its cube the ends of tubes stay.
const HEART: f64 = SPAN - MAX_TUBE_RADIUS;

/// How many systems each cube tries to have. A try is a system if its anchor is under land at a
/// depth that [`ANCHOR_DEPTH`] allows, which is about a third of the tries of a cube that the
/// ground goes through.
const ATTEMPTS: u64 = 8;
/// How far under the ground a system's anchor is, in Voxels: 15 to 240 m.
const ANCHOR_DEPTH: std::ops::RangeInclusive<f64> = v(15.0)..=v(240.0);
/// Segments of the main walk of a system, and of each of its branches (up to two).
const WALK: (u64, u64) = (8, 14);
const BRANCH: (u64, u64) = (3, 6);
/// How long a segment is, in Voxels: 50 to 150 m.
const LENGTH: (f64, f64) = (v(50.0), v(150.0));
/// How much a segment turns from the one before: the size of the random vector added to the
/// heading, along the ground, and (half of it) up and down.
const TURN: f64 = 0.8;
/// The most a tunnel climbs or sinks, as the sine of its angle to the ground: 20 degrees.
const PITCH: f64 = 0.35;
/// How much of half a coarse Voxel's width must be inside a tube for the Voxel to be carved: see
/// [`Caves::carves`]. At least 0.57, so that no tube carves a Voxel of level 8 (12.8 m) or up.
const BALL: f64 = 0.6;
/// Whether a tube can carve a coarse Voxel whose `half` is as [`Caves::carves`] says: not one of
/// level 8 or up, which is 12.8 m across, wider than any tube.
pub(crate) const fn carves_voxels_of(half: f64) -> bool {
    half * BALL < MAX_TUBE_RADIUS
}

// No tube carves a Voxel of level 8 (12.8 m: `half` 127.5) or coarser: the Guard, and the levels
// the horizon cap applies to, have no caves in them.
const _: () = assert!(!carves_voxels_of(127.5));
// ...and one of level 7 can be carved, which the agreement tests at levels 6 and 7 rely on.
const _: () = assert!(carves_voxels_of(63.5));

/// Distance, in Voxels, of the cells of the noise that makes the walls rough: 1.6 m, a power of
/// two so that the in-cell fraction is exact.
const ROUGH_CELL: i32 = voxels(1.6);
const _: () = assert!(ROUGH_CELL.count_ones() == 1);

/// Salts that tell the cave streams of randomness from the world map's and each other's.
const SALT: u64 = 0xca5e_5bad_d00d_1e55;
const ROUGH_SALT: u64 = 0x2545_f491_4f6c_dd1d;

/// A piece of tunnel: a segment from `a` to `b`, in Voxels from the Planet's centre, with the
/// radius `ra` at one end and `rb` at the other, linearly between.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tube {
    pub a: [f64; 3],
    pub b: [f64; 3],
    pub ra: f64,
    pub rb: f64,
}

impl Tube {
    /// Whether the tube may carve a Voxel of the box from `low` to `high`, or one within `margin`
    /// of it: whether its segment passes through the box grown by the tube's widest radius,
    /// roughness and `margin`. It is a bound, not an exact test: a tube it says yes to need
    /// not touch the box, and one that says no never does.
    fn reaches(&self, low: [f64; 3], high: [f64; 3], margin: f64) -> bool {
        let grow = self.ra.max(self.rb) + ROUGHNESS + margin;
        // The part of the segment, as fractions of its length, that is within the box so far.
        let (mut from, mut to) = (0.0_f64, 1.0_f64);
        for k in 0..3 {
            let (lo, hi) = (low[k] - grow, high[k] + grow);
            let (start, along) = (self.a[k], self.b[k] - self.a[k]);
            if along == 0.0 {
                if start < lo || start > hi {
                    return false;
                }
            } else {
                let (u, v) = ((lo - start) / along, (hi - start) / along);
                from = from.max(u.min(v));
                to = to.min(u.max(v));
                if from > to {
                    return false;
                }
            }
        }
        true
    }

    /// The square of the distance from `point` to the tube's axis, and the radius there.
    fn nearest(&self, point: [f64; 3]) -> (f64, f64) {
        let along: [f64; 3] = std::array::from_fn(|k| self.b[k] - self.a[k]);
        let from: [f64; 3] = std::array::from_fn(|k| point[k] - self.a[k]);
        let length2 = dot(along, along);
        let t = if length2 > 0.0 {
            (dot(from, along) / length2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let off: [f64; 3] = std::array::from_fn(|k| from[k] - along[k] * t);
        (dot(off, off), self.ra + (self.rb - self.ra) * t)
    }
}

/// One cube's tubes, in a box that holds all of them, grown by their radii.
struct Cube {
    tubes: Vec<Tube>,
    low: [f64; 3],
    high: [f64; 3],
}

/// The caves of one Planet. See the module documentation.
pub struct Caves {
    /// Tells this Planet's cubes from another's in a thread's cache.
    id: u64,
    map: Arc<WorldMap>,
}

/// A thread's kept cubes: where to put the next, and each as (the Planet's id, the cube, its tubes).
type Kept = (usize, Vec<(u64, [i32; 3], Arc<Cube>)>);

thread_local! {
    /// The cubes this thread asked for last, as (the Planet's id, the cube, its tubes), the oldest
    /// replaced first. It only saves work: a cube is the same however it is got.
    static KEPT_CUBES: RefCell<Kept> =
        const { RefCell::new((0, Vec::new())) };
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn plus(a: [f64; 3], b: [f64; 3], by: f64) -> [f64; 3] {
    std::array::from_fn(|k| a[k] + b[k] * by)
}

/// `v` made a unit vector, or `None` if it is too short to have a direction.
fn unit(v: [f64; 3]) -> Option<[f64; 3]> {
    let length = distance(v);
    (length > 1e-9).then(|| v.map(|c| c / length))
}

/// A stream of numbers in `[0, 1)`: each a hash of the seed, the cube and how many came before.
struct Dice {
    seed: u64,
    cube: [i32; 3],
    drawn: u64,
}

impl Dice {
    fn next(&mut self) -> f64 {
        let salt = self.drawn.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.drawn += 1;
        // The top 53 bits scaled by 2^-53 is exact.
        (hash(self.seed ^ salt, self.cube) >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }

    fn between(&mut self, (lo, hi): (f64, f64)) -> f64 {
        lo + (hi - lo) * self.next()
    }

    /// A whole number from `lo` to `hi` inclusive.
    fn count(&mut self, (lo, hi): (u64, u64)) -> u64 {
        (lo + (self.next() * (hi - lo + 1) as f64) as u64).min(hi)
    }
}

/// The ground at a system: which way is up, and the directions along it.
struct Frame {
    centre: [f64; 3],
    up: [f64; 3],
    ahead: [f64; 3],
    aside: [f64; 3],
}

impl Frame {
    /// A random direction along the ground.
    fn heading(&self, dice: &mut Dice) -> [f64; 3] {
        let (x, y) = (dice.between((-1.0, 1.0)), dice.between((-1.0, 1.0)));
        unit(plus(plus([0.0; 3], self.ahead, x), self.aside, y)).unwrap_or(self.ahead)
    }

    /// `heading` turned a little, at random, and kept within [`PITCH`] of the ground.
    fn turn(&self, heading: [f64; 3], dice: &mut Dice) -> [f64; 3] {
        let mut next = plus(heading, self.ahead, TURN * dice.between((-1.0, 1.0)));
        next = plus(next, self.aside, TURN * dice.between((-1.0, 1.0)));
        next = plus(next, self.up, TURN / 2.0 * dice.between((-1.0, 1.0)));
        let mut next = unit(next).unwrap_or(heading);
        let climb = dot(next, self.up);
        if climb.abs() > PITCH {
            // Flatten it to the limit, keeping the way it points along the ground.
            next = plus(next, self.up, PITCH.copysign(climb) - climb);
            next = unit(next).unwrap_or(heading);
        }
        next
    }

    /// `point` brought back into [`HEART`] of the cube's middle by bouncing it off the edge of
    /// that ball (so that the tube to it, which is a straight line between two points of a ball,
    /// stays in it too).
    fn confine(&self, point: [f64; 3]) -> [f64; 3] {
        let off: [f64; 3] = std::array::from_fn(|k| point[k] - self.centre[k]);
        let length = distance(off);
        if length <= HEART {
            return point;
        }
        plus(self.centre, off, (2.0 * HEART - length).max(0.0) / length)
    }
}

/// The radius of a tube's end: a chamber's or a passage's.
fn end_radius(dice: &mut Dice) -> f64 {
    if dice.next() < CHAMBER_SHARE {
        dice.between(CHAMBER)
    } else {
        dice.between(PASSAGE)
    }
}

/// A walk of `segments` tubes from `start` with radius `radius`, heading `heading`, added to
/// `tubes`; its ends are added to `ends`, the first being the start.
fn walk(
    dice: &mut Dice,
    frame: &Frame,
    (start, radius, heading): ([f64; 3], f64, [f64; 3]),
    segments: u64,
    ends: &mut Vec<([f64; 3], f64)>,
    tubes: &mut Vec<Tube>,
) {
    let (mut at, mut radius, mut heading) = (start, radius, heading);
    ends.push((at, radius));
    for _ in 0..segments {
        heading = frame.turn(heading, dice);
        let to = frame.confine(plus(at, heading, dice.between(LENGTH)));
        let to_radius = end_radius(dice);
        tubes.push(Tube {
            a: at,
            b: to,
            ra: radius,
            rb: to_radius,
        });
        (at, radius) = (to, to_radius);
        ends.push((at, radius));
    }
}

/// Tries to grow a system in the cube `cube` (its index and its middle), `attempt`, and adds its
/// tubes to `tubes`.
fn grow(
    seed: u64,
    map: &WorldMap,
    (cube, centre): ([i32; 3], [f64; 3]),
    attempt: u64,
    sea: f64,
    tubes: &mut Vec<Tube>,
) {
    let mut dice = Dice {
        seed: seed ^ SALT ^ attempt.wrapping_mul(0xd6e8_feb8_6659_fd93),
        cube,
        drawn: 0,
    };
    // An anchor in the ball that the system stays in: the first of a few tries that is in it.
    let anchor = (0..4).find_map(|_| {
        let off = [0; 3].map(|_| dice.between((-HEART, HEART)));
        (dot(off, off) <= HEART * HEART).then(|| plus(centre, off, 1.0))
    });
    let Some(anchor) = anchor else {
        return;
    };
    let radius = distance(anchor);
    let up = anchor.map(|c| c / radius);
    let elevation = map.elevation_at(up);
    let depth = sea + elevation - radius;
    if elevation < f64::from(SEA_LEVEL) || !ANCHOR_DEPTH.contains(&depth) {
        return;
    }
    let (ahead, aside) = tangents(up);
    let frame = Frame {
        centre,
        up,
        ahead,
        aside,
    };
    let mut ends = Vec::new();
    let start = (anchor, end_radius(&mut dice), frame.heading(&mut dice));
    let segments = dice.count(WALK);
    walk(&mut dice, &frame, start, segments, &mut ends, tubes);
    // Side branches, from the ends of the main walk.
    let main = ends.clone();
    for _ in 0..dice.count((0, 2)) {
        let from = main[(dice.next() * main.len() as f64) as usize % main.len()];
        let heading = frame.heading(&mut dice);
        let segments = dice.count(BRANCH);
        walk(
            &mut dice,
            &frame,
            (from.0, from.1, heading),
            segments,
            &mut ends,
            tubes,
        );
    }
}

impl Caves {
    /// The caves of the Planet of `map`, from its seed and radius: nothing is made until it is
    /// asked for.
    pub fn new(map: Arc<WorldMap>) -> Caves {
        Caves {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            map,
        }
    }

    fn seed(&self) -> u64 {
        self.map.seed()
    }

    /// The cube `index` (in cubes of [`CELL`] Voxels): every tube of the systems grown in it, or
    /// none if it is too far from the ground for any anchor to be under land in it.
    fn cube(&self, index: [i32; 3]) -> Arc<Cube> {
        let kept = KEPT_CUBES.with(|kept| {
            let kept = kept.borrow();
            kept.1
                .iter()
                .find(|(id, cube, _)| *id == self.id && *cube == index)
                .map(|(.., made)| made.clone())
        });
        if let Some(cube) = kept {
            return cube;
        }
        let made = Arc::new(self.make_cube(index));
        KEPT_CUBES.with(|kept| {
            let mut kept = kept.borrow_mut();
            let (next, entries) = &mut *kept;
            if entries.len() < KEPT {
                entries.push((self.id, index, made.clone()));
            } else {
                entries[*next] = (self.id, index, made.clone());
                *next = (*next + 1) % KEPT;
            }
        });
        made
    }

    fn make_cube(&self, cube: [i32; 3]) -> Cube {
        let map = &*self.map;
        let sea = f64::from(map.radius()) + f64::from(SEA_LEVEL);
        let lowest = sea - ANCHOR_DEPTH.end();
        let highest = sea + f64::from(PEAK) - ANCHOR_DEPTH.start();
        let centre = cube.map(|c| f64::from(c) * f64::from(CELL) + f64::from(CELL / 2));
        let none = Cube {
            tubes: Vec::new(),
            low: [0.0; 3],
            high: [0.0; 3],
        };
        // A cube too far from the ground for any anchor to be under land in it has none.
        let middle = distance(centre);
        if middle + HEART < lowest || middle - HEART > highest {
            return none;
        }
        let mut tubes = Vec::new();
        for attempt in 0..ATTEMPTS {
            grow(self.seed(), map, (cube, centre), attempt, sea, &mut tubes);
        }
        if tubes.is_empty() {
            return none;
        }
        let (mut low, mut high) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for tube in &tubes {
            let grow = tube.ra.max(tube.rb) + ROUGHNESS;
            for k in 0..3 {
                low[k] = low[k].min(tube.a[k].min(tube.b[k]) - grow);
                high[k] = high[k].max(tube.a[k].max(tube.b[k]) + grow);
            }
        }
        Cube { tubes, low, high }
    }

    /// The tubes of the cubes that the box from `low` to `high` touches, in the order of the cubes
    /// (x fastest, then y, then z): see [`crate::planet::Planet::cave_mouths`].
    pub fn tubes_in(&self, low: [f64; 3], high: [f64; 3]) -> Vec<Tube> {
        let mut tubes = Vec::new();
        self.each_cube(low, high, 0.0, |cube| {
            tubes.extend_from_slice(&cube.tubes);
            false
        });
        tubes
    }

    /// Calls `visit` for each cube from the one with `low` to the one with `high`, until it says
    /// `true`; whether it did.
    fn each_cube(
        &self,
        low: [f64; 3],
        high: [f64; 3],
        margin: f64,
        mut visit: impl FnMut(&Cube) -> bool,
    ) -> bool {
        let first = low.map(|c| cube_index(c - margin));
        let last = high.map(|c| cube_index(c + margin));
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    if visit(&self.cube([x, y, z])) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Calls `visit` for every tube that may carve a Voxel in the box from `low` to `high`, or
    /// one within `margin` of it (see [`Tube::reaches`]), until it says `true`; whether it did.
    fn any_near(
        &self,
        low: [f64; 3],
        high: [f64; 3],
        margin: f64,
        mut visit: impl FnMut(&Tube) -> bool,
    ) -> bool {
        // A tube is inside its cube, and so is the Voxels it carves.
        self.each_cube(low, high, margin, |cube| {
            if cube.tubes.is_empty()
                || (0..3).any(|k| low[k] - margin > cube.high[k] || high[k] + margin < cube.low[k])
            {
                return false;
            }
            cube.tubes
                .iter()
                .any(|t| t.reaches(low, high, margin) && visit(t))
        })
    }

    /// The tubes that may carve a Voxel in the box from `low` to `high`, a Voxel of which is
    /// within `margin` of those corners at the most.
    pub fn near(&self, low: [f64; 3], high: [f64; 3], margin: f64) -> Vec<Tube> {
        let mut found = Vec::new();
        self.any_near(low, high, margin, |tube| {
            found.push(*tube);
            false
        });
        found
    }

    /// Whether a tube may carve a Voxel in the box from `low` to `high`: false means none does.
    pub fn touches(&self, low: [f64; 3], high: [f64; 3], margin: f64) -> bool {
        self.any_near(low, high, margin, |_| true)
    }

    /// Whether no tube comes within `clear` of the axis from `bottom` to `top`: so no Voxel
    /// of a cave is that near it, whatever its depth.
    pub fn clear_of(&self, bottom: [f64; 3], top: [f64; 3], clear: f64) -> bool {
        let low = std::array::from_fn(|k| bottom[k].min(top[k]));
        let high = std::array::from_fn(|k| bottom[k].max(top[k]));
        !self.any_near(low, high, clear, |tube| {
            let reach = clear + tube.ra.max(tube.rb) + ROUGHNESS;
            segments_distance2((tube.a, tube.b), (bottom, top)) <= reach * reach
        })
    }

    /// Whether the Voxel sampled at `point` is carved by one of `tubes` (from [`Caves::near`]):
    /// whether it is inside a tube, with a ball around it inside too. The walls are roughened.
    ///
    /// A Voxel of level 0 is a point, `half` 0. A coarse Voxel is sampled at its middle, and
    /// `half` is half its width less half a Voxel of level 0. The pyramid makes it air if half of
    /// it is. Against a flat wall that is so if the middle is in the cave, and for a tube
    /// through the middle it is so when the tube is 0.8 of the Voxel across, that is the middle
    /// 0.8 of `half` inside. The Voxel is carved when a ball of [`BALL`] of `half` around the
    /// middle is inside the tube, between the two: measured against the pyramid, that agrees
    /// better than either (see the tests).
    pub(crate) fn carves(
        &self,
        tubes: &[Tube],
        point: [f64; 3],
        half: f64,
        cache: &mut Option<Corners>,
    ) -> bool {
        let mut rough = None;
        tubes.iter().any(|tube| {
            let (off2, radius) = tube.nearest(point);
            // Even the roughest wall does not reach so far.
            let farthest = radius + ROUGHNESS - half * BALL;
            if farthest <= 0.0 || off2 >= farthest * farthest {
                return false;
            }
            let rough = *rough.get_or_insert_with(|| {
                value_noise(
                    self.seed() ^ ROUGH_SALT,
                    point.map(|c| c.floor() as i32),
                    ROUGH_CELL,
                    cache,
                )
            });
            let reach = radius + ROUGHNESS * rough - half * BALL;
            reach > 0.0 && off2 < reach * reach
        })
    }

    /// FNV-1a hash of the tubes of the cubes that the box from `low` to `high` touches: the
    /// tubes' ends and radii, bit for bit, cube by cube. It is what the golden test
    /// (`tests/golden.rs`) holds the generator's walk to.
    pub fn content_hash(&self, low: [f64; 3], high: [f64; 3]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed =
            |byte: u8| hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        for tube in self.tubes_in(low, high) {
            for value in tube.a.into_iter().chain(tube.b).chain([tube.ra, tube.rb]) {
                value.to_le_bytes().into_iter().for_each(&mut feed);
            }
        }
        hash
    }
}

/// The index along an axis of the cube that holds the coordinate `c`.
fn cube_index(c: f64) -> i32 {
    (c / f64::from(CELL)).floor() as i32
}

/// The square of the least distance between the segments `(p1, q1)` and `(p2, q2)`.
fn segments_distance2((p1, q1): ([f64; 3], [f64; 3]), (p2, q2): ([f64; 3], [f64; 3])) -> f64 {
    let sub = |a: [f64; 3], b: [f64; 3]| -> [f64; 3] { std::array::from_fn(|k| a[k] - b[k]) };
    let (d1, d2, r) = (sub(q1, p1), sub(q2, p2), sub(p1, p2));
    let (a, e, f) = (dot(d1, d1), dot(d2, d2), dot(d2, r));
    const TINY: f64 = 1e-12;
    let (s, t) = if a <= TINY && e <= TINY {
        (0.0, 0.0)
    } else if a <= TINY {
        (0.0, (f / e).clamp(0.0, 1.0))
    } else {
        let c = dot(d1, r);
        if e <= TINY {
            ((-c / a).clamp(0.0, 1.0), 0.0)
        } else {
            let b = dot(d1, d2);
            let denominator = a * e - b * b;
            let s = if denominator > TINY {
                ((b * f - c * e) / denominator).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let t = (b * s + f) / e;
            if t < 0.0 {
                ((-c / a).clamp(0.0, 1.0), 0.0)
            } else if t > 1.0 {
                (((b - c) / a).clamp(0.0, 1.0), 1.0)
            } else {
                (s, t)
            }
        }
    };
    let (c1, c2) = (plus(p1, d1, s), plus(p2, d2, t));
    let off = sub(c1, c2);
    dot(off, off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_map::TEST_RADIUS;

    fn dice(seed: u64) -> Dice {
        Dice {
            seed,
            cube: [0; 3],
            drawn: 0,
        }
    }

    fn random_point(dice: &mut Dice, size: f64) -> [f64; 3] {
        [0; 3].map(|_| dice.between((-size, size)))
    }

    fn random_tube(dice: &mut Dice) -> Tube {
        Tube {
            a: random_point(dice, 300.0),
            b: random_point(dice, 300.0),
            ra: dice.between(PASSAGE),
            rb: dice.between(CHAMBER),
        }
    }

    #[test]
    fn a_box_is_reached_by_every_tube_that_carves_a_voxel_in_it() {
        // The bound that bricks and `solid_below` rely on, over tubes and boxes at random.
        let mut dice = dice(7);
        let mut carving = 0;
        for _ in 0..20_000 {
            let tube = random_tube(&mut dice);
            // A point near the tube, so that many are in it.
            let along = dice.next();
            let near: [f64; 3] =
                std::array::from_fn(|k| tube.a[k] + (tube.b[k] - tube.a[k]) * along);
            let point = near.map(|c| c + dice.between((-40.0, 40.0)));
            let (off2, radius) = tube.nearest(point);
            if off2 < (radius + ROUGHNESS) * (radius + ROUGHNESS) {
                carving += 1;
                // Boxes that hold the point: a Voxel, a brick, and one with it in a corner.
                for (before, after) in [(0.0, 0.0), (3.0, 4.0), (7.0, 0.0), (0.0, 7.0)] {
                    let low = point.map(|c| c - before);
                    let high = point.map(|c| c + after);
                    assert!(tube.reaches(low, high, 0.0), "{tube:?} {point:?}");
                }
            }
        }
        assert!(carving > 500, "{carving}");
    }

    #[test]
    fn a_tube_is_not_reached_by_a_box_that_is_far_from_it() {
        let tube = Tube {
            a: [0.0; 3],
            b: [100.0, 0.0, 0.0],
            ra: 10.0,
            rb: 10.0,
        };
        let far = |at: [f64; 3]| tube.reaches(at, at.map(|c| c + 7.0), 1.0);
        assert!(far([50.0, 10.0, 0.0]));
        assert!(!far([50.0, 30.0, 0.0]));
        assert!(!far([150.0, 0.0, 0.0]));
        assert!(!far([50.0, 0.0, -30.0]));
    }

    #[test]
    fn the_distance_between_segments_is_the_least_over_the_points_of_both() {
        let mut dice = dice(11);
        for _ in 0..200 {
            let (a, b) = (random_tube(&mut dice), random_tube(&mut dice));
            let exact = segments_distance2((a.a, a.b), (b.a, b.b)).sqrt();
            // Sampled, it can only be more, and not by more than the samples are apart.
            let at = |t: &Tube, k: u32| {
                plus(
                    t.a,
                    std::array::from_fn(|i| t.b[i] - t.a[i]),
                    f64::from(k) / 100.0,
                )
            };
            let sampled = (0..=100)
                .flat_map(|i| (0..=100).map(move |j| (i, j)))
                .map(|(i, j)| {
                    let (p, q) = (at(&a, i), at(&b, j));
                    distance(std::array::from_fn(|k| p[k] - q[k]))
                })
                .fold(f64::INFINITY, f64::min);
            assert!(exact <= sampled + 1e-9, "{exact} {sampled}");
            assert!(sampled - exact < 12.0, "{exact} {sampled}");
        }
    }

    #[test]
    fn the_walls_are_rough_but_never_further_than_the_roughness_from_the_radius() {
        let planet_seed = 5;
        let caves = Caves::new(Arc::new(WorldMap::with_radius(planet_seed, 4, TEST_RADIUS)));
        let tube = Tube {
            a: [0.0; 3],
            b: [400.0, 0.0, 0.0],
            ra: 40.0,
            rb: 40.0,
        };
        let (mut inside, mut outside) = (0, 0);
        for x in 0..200 {
            for dy in 0..80 {
                let point = [f64::from(x) * 2.0, f64::from(dy), 3.0];
                let carved = caves.carves(&[tube], point, 0.0, &mut None);
                // Within 40 - 8 of the axis it is air, and beyond 40 + 8 it is not.
                let off = (f64::from(dy) * f64::from(dy) + 9.0).sqrt();
                assert!(off >= 32.0 || carved, "{point:?}");
                assert!(off < 48.0 || !carved, "{point:?}");
                inside += usize::from(off >= 32.0 && carved);
                outside += usize::from(off < 48.0 && !carved);
            }
        }
        // Within the roughness the wall is neither always there nor never.
        assert!(inside > 100 && outside > 100, "{inside} {outside}");
        // A coarse Voxel is carved only if a ball around its middle fits in the tube: one that
        // is more than the tube's radius across is not, whatever the roughness.
        assert!(!caves.carves(&[tube], [100.0, 0.0, 0.0], 80.0, &mut None));
        assert!(caves.carves(&[tube], [100.0, 0.0, 0.0], 20.0, &mut None));
    }
}
