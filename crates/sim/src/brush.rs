//! What an Edit changes: the Voxels of its Brush, worked out from the op and the Planet's bricks
//! by integer maths in a fixed order, so that the client, the server and the ghost preview all
//! get the same list (ADR 0003, and `clippy.toml` bans what is not exact).
//!
//! # Orientation
//!
//! A Brush's axes are three mutually perpendicular integer vectors of whatever lengths come
//! out, so that no angle is ever turned into a number. The up axis is the exact radial direction
//! at the centre: the centre's own vector, as the Planet's centre is the origin (or the grid's +y
//! in [`Alignment::World`]). One horizontal axis is the cross product of up with the grid axis it
//! is least along, which cannot be parallel to it, and the other is the cross product of up with
//! that. The Brush's `rotation` turns it about up in steps of 45 degrees.
//!
//! A Voxel is in a cube or a cylinder when its offset `d` from the centre, dotted with each axis
//! `e`, is within the radius times that axis's length: `(d . e)^2 <= r^2 |e|^2`, in `i128`, so
//! that no square root is taken. A cube turned by an odd number of 45 degree steps is a diamond
//! in the horizontal plane: `|a| + |b| <= r sqrt 2` for the distances `a` and `b` along the two
//! horizontal axes. Written with the axes' own lengths that is `A >= 2 |p0 p1| sqrt U` for
//! integers `A`, `p0`, `p1` and `U` (the up axis's length squared), which is decided by squaring
//! both sides in 256 bits. `|u|`, a square root, never appears.
//!
//! The integers stay in range because the centre is at most [`MAX_COORD`] from the origin and the
//! radius at most [`PLAN_RADIUS`].

use std::collections::{BTreeMap, VecDeque};
use std::ops::Deref;
use std::sync::Arc;

use brick_store::{BRICK_SIZE, Brick, BrickCoord, BrickStore, Material, VoxelCoord};
use net::{Alignment, EditMode, EditOp, Shape};

use crate::Ground;

/// Bricks that a [`BrickCache`] holds at the most.
pub const CACHE_BRICKS: usize = 4096;

/// A bounded cache of the bricks that the store reads as where it holds none (the generator's, or
/// air), so that an Edit on ground the store does not hold does not generate the same bricks again
/// each time. The first brick in is the first out. It is a cache of what the store would say: a
/// brick that the store holds is read from the store and never from here, so it can hold nothing
/// stale, and it is no part of the Planet's state, which is hashed and compared without it.
///
/// It is for one store (the one that is asked for the bricks).
///
/// At most [`CACHE_BRICKS`] bricks: a brick that is uniform is a few bytes, and one that is mixed
/// at most about 1.2 KiB (512 indices of 16 bits, a palette, a header), so under 5 MiB, and a
/// tilted Brush of 4 m reads some 700 of them.
#[derive(Debug, Default)]
pub struct BrickCache {
    bricks: BTreeMap<BrickCoord, Arc<Brick>>,
    order: VecDeque<BrickCoord>,
}

impl BrickCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bricks held.
    pub fn len(&self) -> usize {
        self.bricks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bricks.is_empty()
    }

    /// The brick at `coord` as `store` reads it where it holds none, from the cache if it is there,
    /// and else made, and kept.
    fn get(&mut self, store: &BrickStore, coord: BrickCoord) -> Arc<Brick> {
        if let Some(brick) = self.bricks.get(&coord) {
            return Arc::clone(brick);
        }
        let brick = Arc::new(store.brick_at(0, coord).into_owned());
        if self.order.len() >= CACHE_BRICKS
            && let Some(oldest) = self.order.pop_front()
        {
            self.bricks.remove(&oldest);
        }
        self.bricks.insert(coord, Arc::clone(&brick));
        self.order.push_back(coord);
        brick
    }
}

/// A brick as [`Reads`] finds it.
enum BrickRef<'a> {
    Held(&'a Brick),
    Shared(Arc<Brick>),
    Owned(Brick),
}

impl Deref for BrickRef<'_> {
    type Target = Brick;

    fn deref(&self) -> &Brick {
        match self {
            Self::Held(brick) => brick,
            Self::Shared(brick) => brick,
            Self::Owned(brick) => brick,
        }
    }
}

/// Where an Edit reads the Planet from, in this order: the bricks the store holds (the Edits, and
/// what is loaded), then the character's [`Ground`], which holds what the generator makes around it
/// (as [`crate::Footing`] reads), and then what the store says where it holds nothing, through a
/// [`BrickCache`] if there is one. The store comes first, so that an Edit is never read past; the
/// others are what the store would say, so what is read is the same from any of them.
pub struct Reads<'a> {
    store: &'a BrickStore,
    ground: Option<&'a Ground>,
    cache: Option<&'a mut BrickCache>,
}

impl<'a> Reads<'a> {
    pub fn new(store: &'a BrickStore) -> Self {
        Self {
            store,
            ground: None,
            cache: None,
        }
    }

    pub fn with_ground(mut self, ground: &'a Ground) -> Self {
        self.ground = Some(ground);
        self
    }

    pub fn with_cache(mut self, cache: &'a mut BrickCache) -> Self {
        self.cache = Some(cache);
        self
    }

    fn brick(&mut self, coord: BrickCoord) -> BrickRef<'a> {
        if let Some(brick) = self.store.brick(coord) {
            return BrickRef::Held(brick);
        }
        if let Some(brick) = self.ground.and_then(|g| g.get(coord)) {
            return BrickRef::Held(brick);
        }
        match &mut self.cache {
            Some(cache) => BrickRef::Shared(cache.get(self.store, coord)),
            None => BrickRef::Owned(self.store.brick_at(0, coord).into_owned()),
        }
    }
}

/// What the store would hold at `coord` if an Edit were to make it: the same reads as [`Reads`].
pub(crate) fn implied_brick(
    store: &BrickStore,
    ground: Option<&Ground>,
    cache: Option<&mut BrickCache>,
    coord: BrickCoord,
) -> Brick {
    let mut reads = Reads::new(store);
    reads.ground = ground;
    reads.cache = cache;
    (*reads.brick(coord)).clone()
}

/// The farthest a centre may be from the origin along an axis, in Voxels: 1 677 km, a hundred
/// times the Planet's radius. It keeps the products of the maths below inside 128 bits.
pub const MAX_COORD: i32 = 1 << 24;

/// The biggest radius [`plan`] and [`examined`] are defined for, which is well past what the
/// server allows (`MAX_BRUSH_RADIUS`): a client that predicts an Edit that will be turned down
/// still gets a result, and the integers and the buffer stay a sensible size.
const PLAN_RADIUS: u16 = 100;

type Vec3 = [i64; 3];

/// Up for [`Alignment::World`], and for a centre that has no direction (the Planet's own).
const GRID_UP: Vec3 = [0, 1, 0];

fn dot(a: Vec3, b: Vec3) -> i64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

/// [`dot`] of vectors that are long, in 128 bits.
fn dot_wide(a: Vec3, b: Vec3) -> i128 {
    (0..3).map(|i| i128::from(a[i]) * i128::from(b[i])).sum()
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// The square of `x`, in 128 bits.
fn sq(x: i64) -> i128 {
    i128::from(x) * i128::from(x)
}

/// One Voxel that an Edit changes: what it becomes (`None` for air), which is never what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub voxel: VoxelCoord,
    pub to: Option<Material>,
}

fn up_vector(centre: VoxelCoord, alignment: Alignment) -> Vec3 {
    let radial = centre.0.map(i64::from);
    match alignment {
        Alignment::Local if radial != [0; 3] => radial,
        _ => GRID_UP,
    }
}

/// The two horizontal axes and then up, as perpendicular integer vectors: `[h0, h1, up]`.
fn frame(up: Vec3) -> [Vec3; 3] {
    // The grid axis that up is least along (the first of a tie), which cannot be parallel to it,
    // so the cross product is not zero.
    let least = (0..3).min_by_key(|&a| up[a].abs()).expect("three axes");
    let mut grid = [0; 3];
    grid[least] = 1;
    let h0 = cross(up, grid);
    [h0, cross(up, h0), up]
}

/// The axes of a Brush centred on `centre`, as integer vectors that are perpendicular to each
/// other, in the grid's frame: the two horizontal ones, which a rotation of 0 puts the Brush's
/// sides along, and then up, which is `centre` itself for [`Alignment::Local`].
pub fn axes(centre: VoxelCoord, alignment: Alignment) -> [[i64; 3]; 3] {
    frame(up_vector(centre, alignment))
}

/// The Voxels a Brush covers, as a test of an offset from its centre.
struct Footprint {
    centre: Vec3,
    shape: Shape,
    radius: i64,
    /// Turned by an odd number of 45 degree steps.
    turned: bool,
    /// The two horizontal axes, then up.
    axes: [Vec3; 3],
    /// What each axis dotted with itself is.
    norms: [i128; 3],
}

impl Footprint {
    /// The footprint of `op`, if it is one that the maths below holds for: a centre within
    /// [`MAX_COORD`] and a radius within [`PLAN_RADIUS`].
    fn of(op: &EditOp) -> Option<Self> {
        let centre = op.centre.0.map(i64::from);
        if centre.iter().any(|c| c.abs() > i64::from(MAX_COORD)) || op.brush.radius > PLAN_RADIUS {
            return None;
        }
        let axes = frame(up_vector(op.centre, op.alignment));
        Some(Self {
            centre,
            shape: op.brush.shape,
            radius: i64::from(op.brush.radius),
            turned: op.rotation & 1 == 1,
            axes,
            norms: axes.map(|a| dot_wide(a, a)),
        })
    }

    /// Whether the Voxel `d` from the centre is covered.
    fn contains(&self, d: Vec3) -> bool {
        let r2 = i128::from(self.radius * self.radius);
        let [n0, n1, up] = self.norms;
        let along_up = || sq(dot(d, self.axes[2]));
        match self.shape {
            Shape::Sphere => i128::from(dot(d, d)) <= r2,
            Shape::Cube => {
                if along_up() > r2 * up {
                    return false;
                }
                let (p0, p1) = (dot(d, self.axes[0]), dot(d, self.axes[1]));
                if self.turned {
                    self.in_diamond(p0, p1)
                } else {
                    sq(p0) <= r2 * n0 && sq(p1) <= r2 * n1
                }
            }
            Shape::Cylinder => {
                // The distance from the axis squared, times the axis's length squared.
                let along = along_up();
                let across = i128::from(dot(d, d)) * up - along;
                along <= r2 * up && across <= r2 * up
            }
        }
    }

    /// Whether a Voxel that is `p0` and `p1` along the two horizontal axes (dotted with the
    /// axes themselves, whose lengths are `sqrt n0` and `sqrt n1`, with `n1 = n0 * up`) is within
    /// the square that is turned an eighth of a turn: `|a| + |b| <= r sqrt 2`, for the distances
    /// `a = p0 / sqrt n0` and `b = p1 / sqrt n1`. Squared and multiplied by `up * n0` that is
    /// `p0^2 up + p1^2 + 2 |p0 p1| sqrt up <= 2 r^2 up n0`.
    fn in_diamond(&self, p0: i64, p1: i64) -> bool {
        let r2 = i128::from(self.radius * self.radius);
        let [n0, _, up] = self.norms;
        let room = 2 * r2 * up * n0 - sq(p0) * up - sq(p1);
        if room < 0 {
            return false;
        }
        let twice = 2 * i128::from(p0).unsigned_abs() * i128::from(p1).unsigned_abs();
        // twice * sqrt(up) <= room, with both sides not negative.
        !square_times_exceeds(twice, up.unsigned_abs(), room.unsigned_abs())
    }

    /// The most whole Voxels that a Voxel of the Brush can be from its centre along grid axis `a`,
    /// or a little more where it is not known exactly.
    fn half_extent(&self, a: usize) -> i64 {
        let r = self.radius;
        let [h0, h1, up] = self.axes;
        let [n0, n1, nu] = self.norms;
        // `16 r |c| / sqrt n`, rounded up, for the part of an axis along the grid axis.
        let along = |c: i64, n: i128| sixteenths(256 * i128::from(r * r) * sq(c), n);
        // The same for the distance from the axis, `r sqrt (1 - up_a^2 / |up|^2)`, times `k`.
        let around = |k: i128| sixteenths(k * 256 * i128::from(r * r) * (nu - sq(up[a])), nu);
        let total = match self.shape {
            Shape::Sphere => return r,
            Shape::Cube if !self.turned => along(h0[a], n0) + along(h1[a], n1) + along(up[a], nu),
            // Turned, the square's reach in any direction is at most `sqrt 2` times r.
            Shape::Cube => along(up[a], nu) + around(2),
            Shape::Cylinder => along(up[a], nu) + around(1),
        };
        // A Voxel is a whole number of Voxels from the centre, so the whole number below.
        (total / 16) as i64
    }

    /// The box, from its first Voxel to its last, that holds the Brush.
    fn bounds(&self) -> (Vec3, Vec3) {
        let reach: Vec3 = std::array::from_fn(|a| self.half_extent(a));
        let limit = |c: i64| c.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        (
            std::array::from_fn(|a| limit(self.centre[a] - reach[a])),
            std::array::from_fn(|a| limit(self.centre[a] + reach[a])),
        )
    }
}

/// `ceil(sqrt(num / n))`, for the sixteenths of Voxels that [`Footprint::half_extent`] adds up:
/// `num` is 256 times a square, so this is 16 times a length. Exact when the length is.
fn sixteenths(num: i128, n: i128) -> i128 {
    let root = (num / n).isqrt();
    if root * root * n == num {
        root
    } else {
        root + 1
    }
}

/// The product of `a` and `b`, in 256 bits, as the high and low halves.
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    const LOW: u128 = u64::MAX as u128;
    let (a0, a1, b0, b1) = (a & LOW, a >> 64, b & LOW, b >> 64);
    let (ll, lh, hl, hh) = (a0 * b0, a0 * b1, a1 * b0, a1 * b1);
    let mid = (ll >> 64) + (lh & LOW) + (hl & LOW);
    (
        hh + (lh >> 64) + (hl >> 64) + (mid >> 64),
        (ll & LOW) | (mid << 64),
    )
}

/// Whether `b^2 * u > a^2`, exactly, for a `u` of at most 64 bits.
fn square_times_exceeds(b: u128, u: u128, a: u128) -> bool {
    assert!(u <= u128::from(u64::MAX));
    let (b2_high, b2_low) = mul_wide(b, b);
    let (carry, low) = mul_wide(b2_low, u);
    let Some(high) = b2_high.checked_mul(u).and_then(|h| h.checked_add(carry)) else {
        return true;
    };
    (high, low) > mul_wide(a, a)
}

/// How many Voxels `op`'s Brush looks at: those of the box that holds it, the same Voxels that
/// [`plan`] goes through, so it is the cost of the Edit whatever it ends up changing. It is
/// worked out from the op alone, before anything is read. A centre or a radius that the maths
/// does not hold for is more than anyone can have.
pub fn examined(op: &EditOp) -> u64 {
    let Some(footprint) = Footprint::of(op) else {
        return u64::MAX;
    };
    let (low, high) = footprint.bounds();
    (0..3).map(|a| (high[a] - low[a] + 1) as u64).product()
}

/// The first and last Voxel of the box that [`examined`] counts, if the op has one.
pub fn examined_box(op: &EditOp) -> Option<([i64; 3], [i64; 3])> {
    Footprint::of(op).map(|footprint| footprint.bounds())
}

/// The Voxels of a box, as they were before the Edit, read once, for the Edit that needs to
/// see what is beside a Voxel.
struct Snapshot {
    first: Vec3,
    size: [usize; 3],
    cells: Vec<Option<Material>>,
}

impl Snapshot {
    /// The Voxels of the box from `first` to `last`.
    fn new(reads: &mut Reads, first: Vec3, last: Vec3) -> Self {
        let size: [usize; 3] = std::array::from_fn(|a| (last[a] - first[a] + 1) as usize);
        let mut snapshot = Self {
            first,
            size,
            cells: vec![None; size[0] * size[1] * size[2]],
        };
        let (low, high) = bricks_of(first, last);
        for x in low[0]..=high[0] {
            for y in low[1]..=high[1] {
                for z in low[2]..=high[2] {
                    let coord = BrickCoord([x, y, z]);
                    let brick = reads.brick(coord);
                    if brick.is_air() {
                        continue;
                    }
                    let origin = coord.origin().0.map(i64::from);
                    let locals = locals_within(origin, first, last);
                    for lz in locals[2].clone() {
                        for ly in locals[1].clone() {
                            for lx in locals[0].clone() {
                                let voxel = [
                                    origin[0] + i64::from(lx),
                                    origin[1] + i64::from(ly),
                                    origin[2] + i64::from(lz),
                                ];
                                if let (Some(material), Some(at)) =
                                    (brick.get([lx, ly, lz]), snapshot.index(voxel))
                                {
                                    snapshot.cells[at] = Some(material);
                                }
                            }
                        }
                    }
                }
            }
        }
        snapshot
    }

    fn index(&self, voxel: Vec3) -> Option<usize> {
        let at: [i64; 3] = std::array::from_fn(|a| voxel[a] - self.first[a]);
        let inside = (0..3).all(|a| (0..self.size[a] as i64).contains(&at[a]));
        inside.then(|| {
            at[0] as usize + self.size[0] * (at[1] as usize + self.size[1] * at[2] as usize)
        })
    }

    fn get(&self, voxel: Vec3) -> Option<Material> {
        self.index(voxel).and_then(|at| self.cells[at])
    }
}

/// The first and last brick, clamped to the Voxel range, that the Voxels from `first` to `last`
/// are in.
fn bricks_of(first: Vec3, last: Vec3) -> ([i32; 3], [i32; 3]) {
    let brick = |voxel: Vec3| -> [i32; 3] {
        std::array::from_fn(|a| {
            let c = voxel[a].clamp(i64::from(i32::MIN), i64::from(i32::MAX));
            (c as i32).div_euclid(BRICK_SIZE)
        })
    };
    (brick(first), brick(last))
}

/// The local coordinates, on each axis, of the Voxels of the brick at `origin` that are in the
/// box from `first` to `last`.
fn locals_within(origin: Vec3, first: Vec3, last: Vec3) -> [std::ops::RangeInclusive<u8>; 3] {
    std::array::from_fn(|a| {
        let low = (first[a] - origin[a]).clamp(0, i64::from(BRICK_SIZE));
        let high = (last[a] - origin[a]).clamp(-1, i64::from(BRICK_SIZE) - 1);
        // An empty range, if the box misses the brick on this axis.
        if low > high {
            std::ops::RangeInclusive::new(1, 0)
        } else {
            low as u8..=high as u8
        }
    })
}

/// A Voxel becomes solid when at least this many of the 27 around it (itself included) are.
const SMOOTH_SOLID: u32 = 14;

/// What a Voxel becomes under [`EditMode::Smooth`]: `None` for air, and if it is solid now and
/// stays so, its own Material, which smoothing never changes. A Voxel that becomes solid takes the
/// Material most of its solid neighbours have, the lowest id of a tie.
fn smoothed(snapshot: &Snapshot, voxel: Vec3, now: Option<Material>) -> Option<Material> {
    let mut tally: Vec<(Material, u32)> = Vec::new();
    let mut solid = 0;
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let near = [voxel[0] + dx, voxel[1] + dy, voxel[2] + dz];
                if let Some(material) = snapshot.get(near) {
                    solid += 1;
                    match tally.iter_mut().find(|(m, _)| *m == material) {
                        Some((_, count)) => *count += 1,
                        None => tally.push((material, 1)),
                    }
                }
            }
        }
    }
    if solid < SMOOTH_SOLID {
        return None;
    }
    now.or_else(|| {
        tally
            .into_iter()
            .max_by_key(|&(material, count)| (count, std::cmp::Reverse(material.0)))
            .map(|(material, _)| material)
    })
}

/// The Voxels that `op` changes in `store`, in ascending brick order and within a brick by index,
/// with what each becomes. Every one of them differs from what it is now: a dig lists only the
/// solid Voxels it covers, a place only the air ones. This is the one function that says what an
/// Edit does: [`crate::apply`] writes exactly this list, and the ghost preview shows it.
///
/// It goes through the [`examined`] Voxels, and nothing else. An op that has none (a centre or a
/// radius out of range) changes nothing. The op should have passed [`crate::validate`].
pub fn plan(store: &BrickStore, op: &EditOp) -> Vec<Change> {
    plan_from(&mut Reads::new(store), op)
}

/// [`plan`], reading the Planet as `reads` says (see [`Reads`]): the same list, whichever way the
/// bricks are found.
pub fn plan_from(reads: &mut Reads, op: &EditOp) -> Vec<Change> {
    let Some(footprint) = Footprint::of(op) else {
        return Vec::new();
    };
    let (first, last) = footprint.bounds();
    // Flatten's plane is the tangent one: square to the exact radial direction.
    let normal = footprint.axes[2];
    let snapshot = (op.mode == EditMode::Smooth).then(|| {
        let beside = |box_: Vec3, step: i64| box_.map(|c| c + step);
        Snapshot::new(reads, beside(first, -1), beside(last, 1))
    });

    let mut changes = Vec::new();
    let (low, high) = bricks_of(first, last);
    for x in low[0]..=high[0] {
        for y in low[1]..=high[1] {
            for z in low[2]..=high[2] {
                let coord = BrickCoord([x, y, z]);
                let brick = reads.brick(coord);
                // Nothing to dig in air, and nowhere to place in a brick that is full.
                match op.mode {
                    EditMode::Dig if brick.is_air() => continue,
                    EditMode::Place if brick.is_full() => continue,
                    _ => {}
                }
                let origin = coord.origin().0.map(i64::from);
                let locals = locals_within(origin, first, last);
                for lz in locals[2].clone() {
                    for ly in locals[1].clone() {
                        for lx in locals[0].clone() {
                            let local = [lx, ly, lz];
                            let at: Vec3 = [
                                origin[0] + i64::from(lx),
                                origin[1] + i64::from(ly),
                                origin[2] + i64::from(lz),
                            ];
                            let d: Vec3 = std::array::from_fn(|a| at[a] - footprint.centre[a]);
                            if !footprint.contains(d) {
                                continue;
                            }
                            let now = match &snapshot {
                                Some(snapshot) => snapshot.get(at),
                                None => brick.get(local),
                            };
                            let to = match op.mode {
                                EditMode::Dig => now.is_some().then_some(None),
                                EditMode::Place => now.is_none().then_some(Some(op.material)),
                                EditMode::Flatten if dot(normal, d) > 0 => {
                                    now.is_some().then_some(None)
                                }
                                EditMode::Flatten => now.is_none().then_some(Some(op.material)),
                                EditMode::Smooth => {
                                    let snapshot = snapshot.as_ref().expect("made for Smooth");
                                    let to = smoothed(snapshot, at, now);
                                    (to.is_some() != now.is_some()).then_some(to)
                                }
                            };
                            if let Some(to) = to {
                                changes.push(Change {
                                    voxel: coord.voxel(local),
                                    to,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::{Brush, Shape};

    const STONE: Material = Material(7);
    const SAND: Material = Material(6);

    fn op(mode: EditMode, shape: Shape, radius: u16, centre: [i32; 3]) -> EditOp {
        EditOp {
            mode,
            brush: Brush { shape, radius },
            centre: VoxelCoord(centre),
            material: STONE,
            alignment: Alignment::Local,
            rotation: 0,
        }
    }

    fn placed(
        shape: Shape,
        radius: u16,
        centre: [i32; 3],
        alignment: Alignment,
        rotation: u8,
    ) -> Vec<Change> {
        let op = EditOp {
            alignment,
            rotation,
            ..op(EditMode::Place, shape, radius, centre)
        };
        plan(&BrickStore::new(), &op)
    }

    #[test]
    fn a_world_aligned_cube_is_the_box_that_reaches_radius_along_each_axis() {
        for radius in [0u16, 1, 3, 10] {
            let r = u64::from(radius);
            let cells = placed(Shape::Cube, radius, [5, -9, 3], Alignment::World, 0);
            assert_eq!(cells.len() as u64, (2 * r + 1).pow(3), "radius {radius}");
        }
        // And it is where it should be: the corner is in, the Voxel past it is not.
        let cells = placed(Shape::Cube, 2, [0, 0, 0], Alignment::World, 0);
        let has = |v: [i32; 3]| cells.iter().any(|c| c.voxel == VoxelCoord(v));
        assert!(has([2, -2, 2]) && has([-2, 2, -2]));
        assert!(!has([3, 0, 0]) && !has([0, -3, 0]) && !has([0, 0, 3]));
    }

    #[test]
    fn a_world_aligned_cylinder_is_a_disc_in_x_and_z_as_tall_as_it_is_wide() {
        // The integer points of a disc: 5, 13, 81 and 317 for radius 1, 2, 5 and 10.
        for (radius, disc) in [(1u16, 5usize), (2, 13), (5, 81), (10, 317)] {
            let cells = placed(Shape::Cylinder, radius, [0, 0, 0], Alignment::World, 0);
            assert_eq!(
                cells.len(),
                disc * (2 * usize::from(radius) + 1),
                "{radius}"
            );
            assert!(
                cells
                    .iter()
                    .all(|c| c.voxel.0[1].abs() <= i32::from(radius))
            );
        }
    }

    #[test]
    fn up_is_exactly_the_centre_and_the_axes_are_perpendicular_integer_vectors() {
        let centres = [
            [1500, 0, 0],
            [1000, 400, -3],
            [-870, 870, 870],
            [3, 4, 12],
            [160_000, -90_000, 100_000],
            [MAX_COORD, MAX_COORD, -MAX_COORD],
            [0, -7, 0],
        ];
        for centre in centres {
            let [h0, h1, up] = axes(VoxelCoord(centre), Alignment::Local);
            assert_eq!(up, centre.map(i64::from), "up is the centre itself");
            let dots = [dot_wide(h0, h1), dot_wide(h0, up), dot_wide(h1, up)];
            assert_eq!(dots, [0; 3], "{centre:?}: {h0:?} {h1:?} {up:?}");
            assert!(h0 != [0; 3] && h1 != [0; 3], "{centre:?}");
        }
        // On the grid, and at the Planet's own centre, up is +y and the axes are the grid's.
        for (centre, alignment) in [
            ([1500, 0, 0], Alignment::World),
            ([0, 0, 0], Alignment::Local),
        ] {
            let [h0, h1, up] = axes(VoxelCoord(centre), alignment);
            assert_eq!(up, [0, 1, 0]);
            assert!(h0.iter().chain(&h1).all(|c| c.abs() <= 1));
            assert_eq!([h0, h1].map(|h| dot_wide(h, h)), [1, 1]);
        }
    }

    #[test]
    fn a_cube_standing_on_an_axis_of_the_grid_is_the_box_and_turned_an_eighth_it_is_a_diamond() {
        // At (1000, 0, 0) up is +x and the Brush's other two axes are the grid's z and y. Not
        // turned, a cube is the box. Turned, the Voxels are those with |y| + |z| <= r sqrt 2 and
        // |x| <= r: a diamond of L1 radius floor(r sqrt 2) = 2, 4 and 14 for r = 2, 3 and 10, which
        // has 2 n^2 + 2 n + 1 points (13, 41, 421), each for 2 r + 1 values of x.
        for (radius, diamond) in [(2u16, 13usize), (3, 41), (10, 421)] {
            let r = usize::from(radius);
            let box_ = placed(Shape::Cube, radius, [1000, 0, 0], Alignment::Local, 0);
            assert_eq!(box_.len(), (2 * r + 1).pow(3), "{radius}");
            let turned = placed(Shape::Cube, radius, [1000, 0, 0], Alignment::Local, 1);
            assert_eq!(turned.len(), diamond * (2 * r + 1), "{radius}");
        }
        // A quarter turn is the same cube; an eighth more is the diamond again.
        let set = |rotation| {
            let mut v: Vec<_> = placed(Shape::Cube, 6, [1000, 0, 0], Alignment::Local, rotation)
                .into_iter()
                .map(|c| c.voxel.0)
                .collect();
            v.sort();
            v
        };
        assert_eq!(set(0), set(2));
        assert_eq!(set(1), set(3));
        assert_eq!(set(1), set(7));
        assert_ne!(set(0), set(1));
    }

    #[test]
    fn a_cube_leaning_to_a_face_diagonal_is_counted_exactly() {
        // The centre (100, 100, 0) has up along (1, 1, 0). With radius 2 and no turn the Voxels
        // are those with |x + y| <= 2 (2 sqrt 2 is 2.83), |x - y| <= 2 and |z| <= 2. Of the
        // (x + y, x - y) pairs 9 of the 25 are both even and 4 both odd: 13, for each of 5 z.
        assert_eq!(
            placed(Shape::Cube, 2, [100, 100, 0], Alignment::Local, 0).len(),
            13 * 5
        );
        // Turned an eighth, the horizontal square is |x - y| / sqrt 2 + |z| <= 2 sqrt 2, that is
        // |x - y| + sqrt 2 |z| <= 4: |x - y| <= 4 for z = 0, <= 2 for z = +-1 (4 - 1.41) and
        // <= 1 for z = +-2 (4 - 2.83), with |x + y| <= 2. Counting the parity matched pairs (3
        // even and 2 odd values of x + y): z = 0 has 5 even and 4 odd values of x - y, 23 pairs;
        // z = +-1 has 3 and 2, 13 pairs each; z = +-2 has 1 and 2, 7 pairs each: 63.
        assert_eq!(
            placed(Shape::Cube, 2, [100, 100, 0], Alignment::Local, 1).len(),
            23 + 2 * 13 + 2 * 7
        );
    }

    #[test]
    fn a_cylinder_leaning_to_a_face_diagonal_is_counted_exactly_and_does_not_turn() {
        // The axis is (1, 1, 0): |x + y| <= 2 along it, and across it (x - y)^2 + 2 z^2 <= 8.
        // Of the (x + y, x - y) pairs, 13 have |x - y| <= 2 for z = 0 and for each of z = +-1,
        // and 3 have x - y = 0 for each of z = +-2: 13 + 26 + 6.
        for rotation in 0..8 {
            let cells = placed(
                Shape::Cylinder,
                2,
                [100, 100, 0],
                Alignment::Local,
                rotation,
            );
            assert_eq!(cells.len(), 45, "{rotation}");
        }
    }

    /// Whether the true shape, in floating point, has the Voxel `d` from its centre `c`, if it is
    /// not near an edge (then `None`): a test's own arithmetic, with orthonormal axes made with
    /// square roots, which the sim's maths avoids.
    fn really_covers(op: &EditOp, d: [f64; 3]) -> Option<bool> {
        let [h0, h1, up] = axes(op.centre, op.alignment);
        let unit = |v: [i64; 3]| {
            let length = v
                .iter()
                .map(|&c| (c as f64) * (c as f64))
                .sum::<f64>()
                .sqrt();
            v.map(|c| c as f64 / length)
        };
        let along = |axis: [f64; 3]| (0..3).map(|i| d[i] * axis[i]).sum::<f64>();
        let (a, b, h) = (along(unit(h0)), along(unit(h1)), along(unit(up)));
        let r = f64::from(op.brush.radius);
        // Distances to the edge, negative inside: none may be near zero.
        let edges: Vec<f64> = match op.brush.shape {
            Shape::Sphere => vec![(a * a + b * b + h * h).sqrt() - r],
            Shape::Cube if op.rotation & 1 == 1 => {
                let s = std::f64::consts::FRAC_1_SQRT_2;
                vec![
                    ((a + b) * s).abs() - r,
                    ((a - b) * s).abs() - r,
                    h.abs() - r,
                ]
            }
            Shape::Cube => vec![a.abs() - r, b.abs() - r, h.abs() - r],
            Shape::Cylinder => vec![(a * a + b * b).sqrt() - r, h.abs() - r],
        };
        if edges.iter().any(|e| e.abs() < 1e-6) {
            return None;
        }
        Some(edges.iter().all(|&e| e < 0.0))
    }

    #[test]
    fn every_voxel_is_in_the_shape_exactly_when_the_real_shape_has_it() {
        // Centres all round, including ones with a length that is a whole number (3, 4, 12) and
        // far ones, and every turn: the integer test agrees with the real geometry wherever the
        // Voxel is not on an edge.
        let centres = [
            [3, 4, 12],
            [1000, 400, -3],
            [-870, 870, 870],
            [100, 100, 0],
            [-5, 7, -2],
            [160_000, -90_000, 100_000],
            [0, 0, -1500],
        ];
        for centre in centres {
            for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
                for rotation in [0, 1] {
                    let op = EditOp {
                        rotation,
                        ..op(EditMode::Place, shape, 9, centre)
                    };
                    let listed: std::collections::BTreeSet<_> = plan(&BrickStore::new(), &op)
                        .into_iter()
                        .map(|c| c.voxel.0)
                        .collect();
                    let mut compared = 0;
                    for x in -17..=17 {
                        for y in -17..=17 {
                            for z in -17..=17 {
                                let voxel = [centre[0] + x, centre[1] + y, centre[2] + z];
                                let d = [f64::from(x), f64::from(y), f64::from(z)];
                                if let Some(inside) = really_covers(&op, d) {
                                    compared += 1;
                                    assert_eq!(
                                        listed.contains(&voxel),
                                        inside,
                                        "{shape:?} {rotation} at {centre:?} + {d:?}"
                                    );
                                }
                            }
                        }
                    }
                    assert!(compared > 30_000, "{compared}");
                }
            }
        }
    }

    #[test]
    fn the_square_test_in_256_bits_is_exact() {
        // 10^20 squared is 10^40, past 128 bits, times 3 is past it by more.
        let big = 10u128.pow(20);
        assert!(square_times_exceeds(big, 4, 2 * big - 1));
        assert!(!square_times_exceeds(big, 4, 2 * big));
        assert!(!square_times_exceeds(big, 4, 2 * big + 1));
        assert!(square_times_exceeds(3, 5, 6));
        assert!(!square_times_exceeds(3, 5, 7));
        assert!(!square_times_exceeds(0, 5, 0));
        let (high, low) = mul_wide(u128::MAX, u128::MAX);
        // (2^128 - 1)^2 = 2^256 - 2^129 + 1.
        assert_eq!((high, low), (u128::MAX - 1, 1));
    }

    /// The angle, as the square of its cosine signed by the cosine, between two vectors (a test's
    /// own arithmetic; it avoids the inverse cosine, which the sim bans).
    fn cos2(a: [f64; 3], b: [f64; 3]) -> f64 {
        let dot: f64 = (0..3).map(|i| a[i] * b[i]).sum();
        let norm2 = |v: [f64; 3]| v.iter().map(|c| c * c).sum::<f64>();
        dot * dot.abs() / (norm2(a) * norm2(b))
    }

    /// cos^2 of 2 degrees.
    const WITHIN_2_DEGREES: f64 = 0.998_782;

    #[test]
    fn a_cylinder_placed_on_the_side_of_the_planet_stands_upright_at_points_far_from_every_axis() {
        // Points on a Planet of radius about 1500 Voxels: on the axes (where a snapped up would
        // have been right), and between them, where it leaned by up to 27 degrees. Negative
        // coordinates too.
        let points = [
            [1500, 0, 0],
            [1100, 700, 700],
            [-870, 870, 870],
            [1000, -700, 1000],
            [-1300, 400, -600],
            [300, -1400, 400],
            [900, 1100, 0],
            [450, 620, 1280],
        ];
        for centre in points {
            let cells = placed(Shape::Cylinder, 20, centre, Alignment::Local, 0);
            // The axis of the Voxels, as the line between the middles of the two halves on either
            // side of the plane through the centre that is square to the radial direction.
            let radial = centre.map(f64::from);
            let (mut top, mut bottom) = (([0.0; 3], 0.0), ([0.0; 3], 0.0));
            for cell in &cells {
                let d: [f64; 3] = std::array::from_fn(|a| f64::from(cell.voxel.0[a] - centre[a]));
                let side = if (0..3).map(|a| d[a] * radial[a]).sum::<f64>() > 0.0 {
                    &mut top
                } else {
                    &mut bottom
                };
                (0..3).for_each(|a| side.0[a] += d[a]);
                side.1 += 1.0;
            }
            let axis: [f64; 3] = std::array::from_fn(|a| top.0[a] / top.1 - bottom.0[a] / bottom.1);
            let upright = cos2(axis, radial);
            assert!(
                upright >= WITHIN_2_DEGREES,
                "{centre:?}: the cylinder leans, cos^2 {upright}"
            );
            // And a Voxel on the axis, far up it, is in the Brush where the radial says.
            let [_, _, up] = axes(VoxelCoord(centre), Alignment::Local);
            assert_eq!(up, centre.map(i64::from));
        }
    }

    #[test]
    fn the_box_a_brush_is_in_is_tight_on_the_grid_and_always_holds_it() {
        // A sphere and a cube on a grid axis are cubes of 2 r + 1 Voxels, a cylinder is too.
        for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
            let at = |centre| EditOp {
                brush: Brush { shape, radius: 20 },
                ..op(EditMode::Dig, shape, 20, centre)
            };
            assert_eq!(examined(&at([1500, 0, 0])), 41 * 41 * 41, "{shape:?}");
            assert_eq!(examined(&at([0, 0, -1500])), 41 * 41 * 41, "{shape:?}");
        }
        // Tilted, it grows, but holds every Voxel and not by much more than they need.
        for centre in [
            [1100, 700, 700],
            [3, 4, 12],
            [100, 100, 0],
            [-870, 870, 870],
        ] {
            for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
                for rotation in [0, 1] {
                    let op = EditOp {
                        rotation,
                        ..op(EditMode::Place, shape, 12, centre)
                    };
                    let (first, last) = examined_box(&op).unwrap();
                    let cells = plan(&BrickStore::new(), &op);
                    for cell in &cells {
                        for a in 0..3 {
                            let c = i64::from(cell.voxel.0[a]);
                            assert!((first[a]..=last[a]).contains(&c), "{op:?}");
                        }
                    }
                    let box_ = examined(&op);
                    assert!(box_ >= cells.len() as u64, "{op:?}");
                    assert!(box_ < 6 * cells.len() as u64, "{op:?}: {box_}");
                }
            }
        }
    }

    #[test]
    fn a_centre_or_radius_the_maths_does_not_hold_for_changes_nothing_and_costs_everything() {
        let far = op(EditMode::Place, Shape::Cube, 3, [MAX_COORD + 1, 0, 0]);
        assert!(plan(&BrickStore::new(), &far).is_empty());
        assert_eq!(examined(&far), u64::MAX);
        let wide = op(EditMode::Place, Shape::Cube, PLAN_RADIUS + 1, [5, 5, 5]);
        assert!(plan(&BrickStore::new(), &wide).is_empty());
        assert_eq!(examined(&wide), u64::MAX);
        // At the edge of the range the Brush is whole.
        let edge = EditOp {
            alignment: Alignment::World,
            ..op(EditMode::Place, Shape::Cube, 3, [MAX_COORD; 3])
        };
        assert_eq!(plan(&BrickStore::new(), &edge).len(), 7 * 7 * 7);
    }

    /// A world aligned Brush is the same wherever it is.
    #[test]
    fn a_world_aligned_brush_does_not_depend_on_where_it_is() {
        let at = |c: [i32; 3]| -> Vec<[i32; 3]> {
            let mut v: Vec<_> = placed(Shape::Cylinder, 6, c, Alignment::World, 0)
                .iter()
                .map(|cell| std::array::from_fn(|a| cell.voxel.0[a] - c[a]))
                .collect();
            // Listed by brick, which depends on where the centre is, so compare as sets.
            v.sort();
            v
        };
        assert_eq!(at([0, 0, 0]), at([-777, 1500, 2_000_000]));
    }

    /// Stone below y = 0 with a rough top: the bricks of y = 0 have columns of it, and a mixed
    /// layer under them, so that what is read from a brick depends on which brick.
    #[derive(Debug)]
    struct Rough;

    impl brick_store::BrickSource for Rough {
        fn brick(&self, _level: usize, coord: BrickCoord) -> Brick {
            match coord.0[1] {
                ..0 => Brick::solid(STONE),
                0 => {
                    let mut brick = Brick::AIR;
                    for x in 0..8u8 {
                        for z in 0..8u8 {
                            let height = 1
                                + (i32::from(x) * 3 + i32::from(z) * 5 + coord.0[0] * 7
                                    - coord.0[2] * 2)
                                    .rem_euclid(5);
                            for y in 0..height as u8 {
                                brick.set([x, y, z], Some(SAND));
                            }
                        }
                    }
                    brick
                }
                _ => Brick::AIR,
            }
        }
    }

    fn rough_store() -> BrickStore {
        let mut store = BrickStore::new();
        store.set_source(Rough);
        store
    }

    /// Edits of every kind round the character, some of them past the ground it holds.
    fn edits_round(centre: [i32; 3]) -> Vec<EditOp> {
        let mut ops = Vec::new();
        for (i, mode) in [
            EditMode::Dig,
            EditMode::Place,
            EditMode::Smooth,
            EditMode::Flatten,
        ]
        .into_iter()
        .enumerate()
        {
            for (j, shape) in [Shape::Sphere, Shape::Cube, Shape::Cylinder]
                .into_iter()
                .enumerate()
            {
                let step = (i * 3 + j) as i32;
                ops.push(EditOp {
                    mode,
                    brush: Brush { shape, radius: 9 },
                    centre: VoxelCoord([
                        centre[0] + step * 9 - 50,
                        centre[1] + (step % 3) * 2 - 3,
                        centre[2] - step * 7 + 40,
                    ]),
                    material: STONE,
                    alignment: if step % 2 == 0 {
                        Alignment::World
                    } else {
                        Alignment::Local
                    },
                    rotation: (step % 3) as u8,
                });
            }
        }
        ops
    }

    #[test]
    fn reading_through_the_ground_and_the_cache_gives_the_same_edits_and_the_same_bricks() {
        use crate::{Ground, apply_changes_from};
        let centre = [2000, 0, -300];
        let mut plain = rough_store();
        let mut through = rough_store();
        let mut ground = Ground::new(through.source());
        ground.reset(centre.map(f64::from));
        while !ground.is_complete() {
            ground.fill();
        }
        let mut cache = BrickCache::new();
        // Twice over, so that the second time the cache is warm, and with each Edit applied in
        // turn, so that what the store holds (an Edit) is read before the ground and the cache.
        for round in 0..2 {
            for op in edits_round(centre) {
                let want = plan(&plain, &op);
                let mut reads = Reads::new(&through)
                    .with_ground(&ground)
                    .with_cache(&mut cache);
                let got = plan_from(&mut reads, &op);
                assert_eq!(got, want, "round {round}: {op:?}");
                // And with the cache alone, and with neither.
                let mut cached = Reads::new(&through).with_cache(&mut cache);
                assert_eq!(plan_from(&mut cached, &op), want);
                assert_eq!(plan(&through, &op), want);
                crate::apply_changes(&mut plain, &want);
                apply_changes_from(&mut through, &got, Some(&ground), Some(&mut cache));
                assert_eq!(through.content_hash(), plain.content_hash(), "{op:?}");
            }
        }
        // Some of it was made and read from the cache, and no more than it holds.
        assert!(!cache.is_empty() && cache.len() <= CACHE_BRICKS);
        // The Edits changed things, or this proves nothing.
        assert_ne!(plain.content_hash(), rough_store().content_hash());
    }

    #[test]
    fn the_cache_holds_no_more_than_its_bound_and_forgets_the_oldest_first() {
        let store = rough_store();
        let mut cache = BrickCache::new();
        let at = |i: usize| BrickCoord([(i % 64) as i32, -1 - (i / 64) as i32, 0]);
        for i in 0..CACHE_BRICKS + 10 {
            cache.get(&store, at(i));
        }
        assert_eq!(cache.len(), CACHE_BRICKS);
        // The first ten are gone, the next is there: asking again makes it once and holds it.
        assert!(!cache.bricks.contains_key(&at(0)) && !cache.bricks.contains_key(&at(9)));
        assert!(cache.bricks.contains_key(&at(10)));
        let again = cache.get(&store, at(CACHE_BRICKS + 9));
        assert_eq!(cache.len(), CACHE_BRICKS);
        assert!(Arc::ptr_eq(
            &again,
            &cache.get(&store, at(CACHE_BRICKS + 9))
        ));
    }

    #[test]
    fn an_edit_is_read_before_the_ground_and_the_cache_whatever_they_hold() {
        use crate::Ground;
        let store_and_ground = |dig: bool| {
            let mut store = rough_store();
            let mut ground = Ground::new(store.source());
            ground.reset([0.0; 3]);
            while !ground.is_complete() {
                ground.fill();
            }
            let mut cache = BrickCache::new();
            // The cache and the ground hold what the generator made of the stone below.
            let below = VoxelCoord([3, -3, 3]);
            assert_eq!(
                cache.get(&store, below.brick()).get(below.local()),
                Some(STONE)
            );
            if dig {
                crate::apply(&mut store, &op(EditMode::Dig, Shape::Sphere, 2, [3, -3, 3]));
            }
            (store, ground, cache)
        };
        let (store, ground, mut cache) = store_and_ground(true);
        assert_eq!(store.get(VoxelCoord([3, -3, 3])), None, "dug");
        // A dig of the same Voxel has nothing left to dig, though the ground and the cache still
        // say it is stone.
        let again = op(EditMode::Dig, Shape::Sphere, 0, [3, -3, 3]);
        let mut reads = Reads::new(&store)
            .with_ground(&ground)
            .with_cache(&mut cache);
        assert!(plan_from(&mut reads, &again).is_empty());
        let (store, ground, mut cache) = store_and_ground(false);
        let mut reads = Reads::new(&store)
            .with_ground(&ground)
            .with_cache(&mut cache);
        assert_eq!(plan_from(&mut reads, &again).len(), 1);
    }

    /// A store with a solid slab of `STONE` below y = 0, from -6 to 5 in x and z.
    fn slab() -> BrickStore {
        let mut store = BrickStore::new();
        for x in -6..6 {
            for z in -6..6 {
                for y in -6..0 {
                    store.set(VoxelCoord([x, y, z]), Some(STONE));
                }
            }
        }
        store
    }

    #[test]
    fn a_dig_lists_only_the_solid_voxels_and_a_place_only_the_air_ones() {
        let store = slab();
        // Centred on the surface layer: half of a radius 2 sphere is in the slab. At (0, -1, 0)
        // up is -y, but the sphere has no top.
        let dig = plan(&store, &op(EditMode::Dig, Shape::Sphere, 2, [0, -1, 0]));
        assert!(dig.iter().all(|c| c.to.is_none() && c.voxel.0[1] < 0));
        let place = plan(&store, &op(EditMode::Place, Shape::Sphere, 2, [0, -1, 0]));
        assert!(
            place
                .iter()
                .all(|c| c.to == Some(STONE) && c.voxel.0[1] >= 0)
        );
        // Together they are the sphere's 33 Voxels, none twice.
        assert_eq!(dig.len() + place.len(), 33);
    }

    #[test]
    fn smoothing_leaves_a_flat_surface_alone_and_shaves_a_spike_and_fills_a_pit() {
        let mut store = slab();
        let flat = plan(&store, &op(EditMode::Smooth, Shape::Sphere, 4, [0, -1, 0]));
        assert!(flat.is_empty(), "{flat:?}");

        // One voxel standing on the surface, and one missing from it.
        store.set(VoxelCoord([0, 0, 0]), Some(SAND));
        store.set(VoxelCoord([2, -1, 2]), None);
        let changes = plan(&store, &op(EditMode::Smooth, Shape::Sphere, 4, [0, -1, 0]));
        assert!(changes.contains(&Change {
            voxel: VoxelCoord([0, 0, 0]),
            to: None
        }));
        // The pit is filled with what is round it, not with the spike's sand.
        assert!(changes.contains(&Change {
            voxel: VoxelCoord([2, -1, 2]),
            to: Some(STONE)
        }));
        assert_eq!(changes.len(), 2, "{changes:?}");
    }

    #[test]
    fn a_smoothed_voxel_takes_the_commonest_material_round_it_and_the_lowest_id_of_a_tie() {
        // A pit at the origin with the 26 Voxels round it: the layer below is all STONE and the
        // one above all SAND, and the 8 beside it are split. 13 of each is a tie.
        let mut store = BrickStore::new();
        fn around(store: &mut BrickStore, stone_beside: usize) {
            let mut beside = 0;
            for x in -1..=1 {
                for y in -1..=1 {
                    for z in -1..=1 {
                        let material = match (y, (x, z) == (0, 0)) {
                            (_, true) if y == 0 => continue,
                            (-1, _) => STONE,
                            (1, _) => SAND,
                            _ => {
                                beside += 1;
                                if beside <= stone_beside { STONE } else { SAND }
                            }
                        };
                        store.set(VoxelCoord([x, y, z]), Some(material));
                    }
                }
            }
        }
        let pit = |store: &BrickStore| {
            plan(store, &op(EditMode::Smooth, Shape::Sphere, 0, [0, 0, 0]))
                .first()
                .map(|c| c.to)
        };
        around(&mut store, 4);
        assert_eq!(pit(&store), Some(Some(SAND)), "tied: the lower id");
        around(&mut store, 5);
        assert_eq!(
            pit(&store),
            Some(Some(STONE)),
            "one more STONE: the commoner"
        );
    }

    #[test]
    fn flatten_cuts_what_is_above_the_tangent_plane_and_fills_what_is_below() {
        let mut store = slab();
        // A bump above y = 0, and a pit in the slab. Flatten at the centre of the surface, in
        // world alignment, so the plane is y = -1: the bump and the top layer go, and the pit,
        // which is under the plane, is filled.
        store.set(VoxelCoord([1, 0, 1]), Some(SAND));
        store.set(VoxelCoord([-2, -3, 2]), None);
        let flatten = EditOp {
            alignment: Alignment::World,
            ..op(EditMode::Flatten, Shape::Cube, 4, [0, -1, 0])
        };
        let changes = plan(&store, &flatten);
        let cut = |v: [i32; 3]| Change {
            voxel: VoxelCoord(v),
            to: None,
        };
        assert!(changes.contains(&cut([1, 0, 1])));
        assert!(changes.contains(&Change {
            voxel: VoxelCoord([-2, -3, 2]),
            to: Some(STONE)
        }));
        // Nothing at or below the plane is cut, and everything above it that is solid is.
        assert!(changes.iter().all(|c| match c.to {
            None => c.voxel.0[1] > -1,
            Some(_) => c.voxel.0[1] <= -1,
        }));
        assert_eq!(changes.len(), 2);

        // Tangent to the Planet at (1500, 0, 0): the plane is x = 1500, and what is past it goes.
        let mut wall = BrickStore::new();
        for x in 1496..=1504 {
            wall.set(VoxelCoord([x, 0, 0]), Some(STONE));
        }
        let changes = plan(
            &wall,
            &op(EditMode::Flatten, Shape::Sphere, 6, [1500, 0, 0]),
        );
        let cut: Vec<_> = changes.iter().filter(|c| c.to.is_none()).collect();
        assert_eq!(cut.len(), 4, "x = 1501..=1504");
        assert!(cut.iter().all(|c| c.voxel.0[0] > 1500));
    }
}
