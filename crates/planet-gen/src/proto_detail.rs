//! PROTOTYPE (#267, throwaway, never merged): per-brick ridge, gully and talus detail below the
//! world-map texel, so that the ranges of the landform model read crisp from 5 km and from the
//! ground. Three layers, each behind a flag ([`Set`]), added to the Relief by the generator the way
//! `detail` is, and bounded the way ADR 0012 asks: a proven bound per brick with a declared reach.
//!
//! **Where the aligned layers come from.** A layer is a sum of octaves of *oriented stripes* on the
//! same 3D lattice that `detail`'s value noise lives on (10 cm points; an octave's cell is a power
//! of two of them). Each lattice corner of an octave's cell has a stripe pattern of its own: a
//! direction on the ground, a phase and a strength, all drawn from the map at the corner (see
//! [`corner_of`]) and the seed, and a point takes the trilinear (smoothstep) blend of its cell's
//! eight patterns, as value noise blends its corners' numbers. So the stripes are continuous, are
//! a function of the position and the seed alone, and follow the ground the map has:
//!
//! - **`ridged`** (4 octaves, cells of 410 m, 102 m, 26 m and 6.4 m, a crest 25 m high at the widest): crests along the range, that is
//!   along the contours, turned up to 48 degrees either way per corner so that they do not read as
//!   terraces; sharp crests and round valleys, scaled by the slope and the height of the map.
//! - **`gullies`** (3 octaves, 51 m, 13 m, 3.2 m): V-shaped channels that run along the map's
//!   *stored flow receiver* of the texel (D8, [`crate::world_map`]'s `receivers`), carved deeper
//!   where the slope and the drainage are greater.
//! - **`talus`** (4 octaves of value noise, 6.4 m to 0.8 m, creased): a rougher, broken surface, and
//!   a loose-rock Material (gravel) in the top 30 cm, where the slope is about the angle of repose
//!   and the ground is concave (the foot of a slope). Its mask is the blend of the corners'.
//!
//! **Bounds** ([`Proto::bounds`]). Each stripe octave is bounded over a brick's box by its value at
//! the box's centre plus or minus its Lipschitz constant times the box's half-diagonal, clipped to
//! the octave's own range: a convex blend of the eight patterns, each of at most `m` times a
//! profile that has slope at most 6 per unit of phase, whose phase has the gradient `1 / wavelength`,
//! under weights whose gradients sum to at most `5.2 / cell` in each pattern's difference of at
//! most `1.5 m`: so `L = amp * m * (7.8 / cell + 6 / (0.85 * wavelength))`, `m` the greatest of
//! the strengths of the corners of the cells the box is over (and none at all, an exact 0, where
//! they are all 0). The box is in lattice points, the box of the points that `detail` rounds the
//! Voxels of the brick to (`Detail::lattice_box`), so the rounding is in it. Talus's noise has
//! `detail`'s exact corner bounds, times the range of its blended mask. **All of this is proven
//! (and tested against sampling) for the corners' patterns as they are; what is approximate is
//! only how loose it is**, which costs bricks near the ground a full evaluation.
//!
//! **Reach.** [`Set::reach`]: the most that the layers move the ground up and down, which the sky
//! and the interior shortcuts of `planet` are widened by.
//!
//! Not for the game: it costs per brick (see the PR), and the map's stored receivers only exist in
//! the landform prototype's map.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use brick_store::VOXELS_PER_METRE;

use crate::detail::{
    Detail, Octave, VOXELS_PER_POINT, has, noise_range, octave, salt, shape, shape_bounds,
};
use crate::world_map::{Lie, WorldMap};
use crate::{Corners, corner_value, distance, value_noise};

/// Which layers are on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Set(u8);

impl Set {
    pub const NONE: Set = Set(0);
    pub const RIDGED: Set = Set(1);
    pub const GULLIES: Set = Set(2);
    pub const TALUS: Set = Set(4);
    pub const ALL: Set = Set(7);

    /// Whether every layer of `other` is on (and `other` is not empty).
    pub fn contains(self, other: Set) -> bool {
        other.0 != 0 && self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `none`, `all`, or a comma list of `ridged`, `gullies` and `talus`.
    pub fn parse(text: &str) -> Result<Set, String> {
        let mut set = 0;
        for word in text.split(',').map(str::trim).filter(|w| !w.is_empty()) {
            set |= match word {
                "none" => 0,
                "all" => Set::ALL.0,
                "ridged" => Set::RIDGED.0,
                "gullies" => Set::GULLIES.0,
                "talus" => Set::TALUS.0,
                other => {
                    return Err(format!(
                        "`{other}` is not a detail layer: none, all, or a list of ridged, gullies, talus"
                    ));
                }
            };
        }
        Ok(Set(set))
    }

    /// The set as `--proto-detail` takes it.
    pub fn name(self) -> String {
        if self == Set::ALL {
            return "all".into();
        }
        let words: Vec<&str> = [
            (Set::RIDGED, "ridged"),
            (Set::GULLIES, "gullies"),
            (Set::TALUS, "talus"),
        ]
        .into_iter()
        .filter(|&(s, _)| self.contains(s))
        .map(|(_, w)| w)
        .collect();
        if words.is_empty() {
            "none".into()
        } else {
            words.join(",")
        }
    }

    /// The most that the layers of the set raise the ground and lower it, in Voxels (both
    /// positive): the sums of their octaves' amplitudes where the strength is 1.
    pub fn reach(self) -> (f64, f64) {
        let (mut up, mut down) = (0.0, 0.0);
        if self.contains(Set::RIDGED) {
            let sum: f64 = RIDGED.iter().map(|o| o.amp).sum();
            up += sum;
            down += 0.5 * sum;
        }
        if self.contains(Set::GULLIES) {
            down += GULLIES.iter().map(|o| o.amp).sum::<f64>();
        }
        if self.contains(Set::TALUS) {
            let sum: f64 = TALUS.iter().map(|&o| talus_amp(o)).sum();
            up += sum;
            down += sum;
        }
        (up, down)
    }
}

static SET: AtomicU8 = AtomicU8::new(0);

/// Sets the layers that every [`crate::planet::Planet`] made from now on has (`client lookdev
/// --landform --proto-detail`, `client gen-cost`). None unless asked for, so the goldens are main's.
pub fn use_proto_detail(set: Set) {
    SET.store(set.0, Ordering::Relaxed);
}

/// The layers [`use_proto_detail`] set.
pub fn proto_detail() -> Set {
    Set(SET.load(Ordering::Relaxed))
}

/// An octave of oriented stripes: its cell in lattice points (a power of two), its amplitude in
/// Voxels where the strength is 1, and its wavelength in lattice points.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stripe {
    cell: i32,
    amp: f64,
    wavelength: f64,
}

const fn stripe(cell: i32, share: f64, waves: f64) -> Stripe {
    Stripe {
        cell,
        amp: share * cell as f64 * VOXELS_PER_POINT,
        wavelength: cell as f64 / waves,
    }
}

/// 6% of the cell, three waves across it: 25 m of crest at the widest, a few cm at the finest.
const RIDGED: [Stripe; 4] = [
    stripe(4096, 0.06, 3.0),
    stripe(1024, 0.06, 3.0),
    stripe(256, 0.06, 3.0),
    stripe(64, 0.06, 3.0),
];

/// 6% of the cell, two and a half waves across it: channels 3 m deep at the widest.
const GULLIES: [Stripe; 3] = [
    stripe(512, 0.06, 2.5),
    stripe(128, 0.06, 2.5),
    stripe(32, 0.06, 2.5),
];

/// Talus's noise: creased octaves, cells of 6.4 m down to 0.8 m.
const TALUS: [Octave; 4] = [
    octave(64, true),
    octave(32, true),
    octave(16, true),
    octave(8, true),
];

/// An octave of talus's amplitude as a share of its cell.
const TALUS_AMPLITUDE: f64 = 0.16;

fn talus_amp(o: Octave) -> f64 {
    TALUS_AMPLITUDE * f64::from(o.cell) * VOXELS_PER_POINT
}

/// The cell of the lattice that talus's mask is blended from (205 m).
const MASK_CELL: i32 = 2048;

/// The first slot of [`detail`](crate::detail)'s per-thread corners that talus's octaves use
/// (the base octaves have the first seven).
const NOISE_SLOT: usize = 7;

/// How deep the loose rock goes, in Voxels: 30 cm.
pub(crate) const SCREE_DEPTH: f64 = 0.3 * VOXELS_PER_METRE;

/// The fewest samples an octave's cell must be for a brick sampled `size` Voxels apart to have it
/// (the same rule as `detail`'s).
const MIN_CELL: f64 = 4.0;

fn has_stripe(o: &Stripe, size: f64) -> bool {
    f64::from(o.cell) * VOXELS_PER_POINT >= MIN_CELL * size
}

/// What a corner of the lattice says of the ground there, for one stripe layer.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Wave {
    /// The strength, in `[0, 1]`: 0 where the layer is not.
    pub m: f64,
    /// The unit vector the phase runs along, along the ground and across the stripes.
    pub w: [f64; 3],
    /// Where in a wave the corner is, in `[0, 1)`.
    pub phase: f64,
    /// How much longer than the octave's wavelength this corner's waves are, in `[0.85, 1.15]`.
    pub length: f64,
}

/// What the layers read of the lattice corner: its two stripe patterns and talus's mask.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Corner {
    pub ridge: Wave,
    pub gully: Wave,
    pub talus: f64,
}

#[derive(Clone, Copy)]
enum Kind {
    Ridged,
    Gullies,
}

fn smooth(from: f64, to: f64, x: f64) -> f64 {
    let t = ((x - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// `v` as a unit vector, or zero.
fn unit(v: [f64; 3]) -> [f64; 3] {
    let n = distance(v);
    if n > 1e-12 {
        v.map(|c| c / n)
    } else {
        [0.0; 3]
    }
}

const SALT: u64 = 0x7e57_ab1e_5ca1_ab1e;

/// The patterns of the corner `c` of the lattice, from the map's `lie` there and the seed: pure
/// arithmetic of `+ - * /` and `sqrt`, so the same on every machine.
pub(crate) fn corner_of(seed: u64, c: [i32; 3], lie: &Lie) -> Corner {
    let h = |k: u64| corner_value(seed ^ SALT ^ k.wrapping_mul(0x9e37_79b9_7f4a_7c15), c);
    let slope = distance(lie.grad);
    let elevation = lie.elevation / VOXELS_PER_METRE;
    let uphill = unit(lie.grad);
    let up = lie.up;
    // The crests run along the contours: the phase runs up the slope, turned by up to 40 degrees.
    let ridge_w = unit({
        let k = (2.0 * h(2) - 1.0) * 0.84;
        let side = cross(up, uphill);
        std::array::from_fn(|a| uphill[a] + k * side[a])
    });
    let ridge = Wave {
        m: if slope > 1e-9 {
            smooth(0.12, 0.5, slope) * smooth(150.0, 900.0, elevation) * (0.6 + 0.4 * h(1))
        } else {
            0.0
        },
        w: ridge_w,
        phase: h(3),
        length: 0.85 + 0.3 * h(4),
    };
    // The channels run down the stored receiver (down the slope where the texel has none), so the
    // phase runs across it.
    let flow = if distance(lie.down) > 0.0 {
        lie.down
    } else {
        uphill.map(|c| -c)
    };
    let gully_w = unit({
        let k = (2.0 * h(6) - 1.0) * 0.36;
        let side = cross(up, flow);
        std::array::from_fn(|a| side[a] + k * flow[a])
    });
    let gully = Wave {
        m: if distance(flow) > 0.0 && slope > 1e-9 {
            smooth(0.2, 0.7, slope)
                * (0.35 + 0.65 * lie.drainage)
                * smooth(30.0, 150.0, elevation)
                * (0.5 + 0.5 * h(5))
        } else {
            0.0
        },
        w: gully_w,
        phase: h(7),
        length: 0.85 + 0.3 * h(8),
    };
    // Loose rock lies at about the angle of repose (slopes of 0.45 to 0.7 and a little over) where
    // the ground is concave, and not on the cliffs above it.
    let band = smooth(0.45, 0.7, slope) * (1.0 - smooth(1.0, 1.4, slope));
    let foot = smooth(-0.02, 0.04, lie.curvature);
    Corner {
        ridge,
        gully,
        talus: band * (0.4 + 0.6 * foot) * smooth(100.0, 500.0, elevation),
    }
}

/// The octave `o` of stripes at the point `x` of the lattice, in the cell `idx` of the octave,
/// whose eight corners are `corners`: x fastest. `x` may be any point (the bounds take the box's
/// centre); `idx` is the cell it is in.
fn stripes(o: &Stripe, kind: Kind, x: [f64; 3], idx: [i32; 3], corners: &[Corner; 8]) -> f64 {
    let cell = f64::from(o.cell);
    let frac: [f64; 3] = std::array::from_fn(|a| x[a] / cell - f64::from(idx[a]));
    let fade = frac.map(|t| t * t * (3.0 - 2.0 * t));
    let mut sum = 0.0;
    for (k, corner) in corners.iter().enumerate() {
        let d = [k & 1, k >> 1 & 1, k >> 2];
        let weight: f64 = (0..3)
            .map(|a| if d[a] == 1 { fade[a] } else { 1.0 - fade[a] })
            .product();
        let wave = match kind {
            Kind::Ridged => &corner.ridge,
            Kind::Gullies => &corner.gully,
        };
        if wave.m == 0.0 || weight == 0.0 {
            continue;
        }
        let from: [f64; 3] =
            std::array::from_fn(|a| x[a] - f64::from((idx[a] + d[a] as i32).wrapping_mul(o.cell)));
        let f = dot(from, wave.w) / (o.wavelength * wave.length) + wave.phase;
        sum += weight * wave.m * profile(kind, (2.0 * (f - f.floor()) - 1.0).abs());
    }
    o.amp * sum
}

/// The stripe profile at `u`, 0 in the middle of a wave and 1 at its edge: sharp crests and round
/// valleys, zero on average, for ridges; a V of a channel for gullies.
fn profile(kind: Kind, u: f64) -> f64 {
    match kind {
        Kind::Ridged => 1.5 * ((1.0 - u) * (1.0 - u) - 1.0 / 3.0),
        Kind::Gullies => -(1.0 - u) * (1.0 - u) * (1.0 - u),
    }
}

/// The least and greatest [`profile`] of a phase between `from` and `to`. The profile is
/// monotone in `u`, and `u` is 0 at a half-wave and 1 at a whole one.
fn profile_range(kind: Kind, from: f64, to: f64) -> (f64, f64) {
    let u_of = |f: f64| (2.0 * (f - f.floor()) - 1.0).abs();
    let (mut u_lo, mut u_hi) = (u_of(from).min(u_of(to)), u_of(from).max(u_of(to)));
    if to - from >= 1.0 || (from - 0.5).ceil() <= (to - 0.5).floor() {
        u_lo = 0.0;
    }
    if to - from >= 1.0 || from.ceil() <= to.floor() {
        u_hi = 1.0;
    }
    let (a, b) = (profile(kind, u_lo), profile(kind, u_hi));
    (a.min(b), a.max(b))
}

/// The range of an octave's [`stripes`] where the strengths are at most `m`.
fn stripe_range(o: &Stripe, kind: Kind, m: f64) -> (f64, f64) {
    match kind {
        Kind::Ridged => (-0.5 * o.amp * m, o.amp * m),
        Kind::Gullies => (-o.amp * m, 0.0),
    }
}

/// Per brick: the last cell of each octave with its corners, and the sum at the lattice points
/// asked for (a Voxel of level 0 is half a lattice point). Only saves work.
#[derive(Default)]
pub(crate) struct Cache {
    sums: Vec<Option<([i32; 3], f64)>>,
    cells: [Option<([i32; 3], [Corner; 8])>; 8],
    noise: [Option<Corners>; TALUS.len()],
}

const SUMS: usize = 256;

impl Cache {
    pub fn for_brick(size: f64) -> Cache {
        Cache {
            sums: if size < VOXELS_PER_POINT {
                vec![None; SUMS]
            } else {
                Vec::new()
            },
            ..Cache::default()
        }
    }
}

/// The layers of one Planet, apart from the map they read.
#[derive(Clone, Copy)]
pub(crate) struct Layers {
    seed: u64,
    set: Set,
    detail: Detail,
}

/// What corner `c` says: the map's, or a synthetic world in a test.
pub(crate) type Source<'a> = &'a dyn Fn([i32; 3]) -> Corner;

/// How many cells of an octave a box may be over for its corners to be read one by one; a wider
/// box takes the octave's whole range.
const MOST_CELLS: i32 = 3;

impl Layers {
    pub fn new(seed: u64, radius: i32, set: Set) -> Layers {
        Layers {
            seed: seed ^ SALT,
            set,
            detail: Detail::new(seed, radius),
        }
    }

    fn corners(idx: [i32; 3], cell: i32, source: Source<'_>) -> [Corner; 8] {
        std::array::from_fn(|k| {
            let d = [k & 1, k >> 1 & 1, k >> 2];
            source(std::array::from_fn(|a| {
                (idx[a] + d[a] as i32).wrapping_mul(cell)
            }))
        })
    }

    /// The cell `idx` of slot `slot`, from the cache or made.
    fn cell<'c>(
        cache: &'c mut Cache,
        slot: usize,
        idx: [i32; 3],
        cell: i32,
        source: Source<'_>,
    ) -> &'c [Corner; 8] {
        let stale = !matches!(&cache.cells[slot], Some((kept, _)) if *kept == idx);
        if stale {
            cache.cells[slot] = Some((idx, Layers::corners(idx, cell, source)));
        }
        &cache.cells[slot].as_ref().unwrap().1
    }

    /// The talus mask at the lattice point `at`: the blend of its cell's corners' masks.
    fn mask_at(cache: &mut Cache, at: [i32; 3], source: Source<'_>) -> f64 {
        let shift = MASK_CELL.trailing_zeros();
        let idx = at.map(|v| v >> shift);
        let x = at.map(f64::from);
        let corners = Layers::cell(cache, 7, idx, MASK_CELL, source);
        let cell = f64::from(MASK_CELL);
        let fade: [f64; 3] = std::array::from_fn(|a| {
            let t = x[a] / cell - f64::from(idx[a]);
            t * t * (3.0 - 2.0 * t)
        });
        let mut sum = 0.0;
        for (k, corner) in corners.iter().enumerate() {
            let d = [k & 1, k >> 1 & 1, k >> 2];
            let weight: f64 = (0..3)
                .map(|a| if d[a] == 1 { fade[a] } else { 1.0 - fade[a] })
                .product();
            sum += weight * corner.talus;
        }
        sum
    }

    /// The layers' sum at the lattice point `at`, in Voxels, for a brick sampled `size` apart.
    pub fn height(&self, at: [i32; 3], size: f64, cache: &mut Cache, source: Source<'_>) -> f64 {
        let slot = (at[0].wrapping_mul(73_856_093)
            ^ at[1].wrapping_mul(19_349_663)
            ^ at[2].wrapping_mul(83_492_791)) as u32 as usize
            % SUMS;
        if let Some(Some((kept, total))) = cache.sums.get(slot)
            && *kept == at
        {
            return *total;
        }
        let x = at.map(f64::from);
        let mut total = 0.0;
        for (base, kind, octaves) in [
            (0, Kind::Ridged, &RIDGED[..]),
            (4, Kind::Gullies, &GULLIES[..]),
        ] {
            let on = match kind {
                Kind::Ridged => self.set.contains(Set::RIDGED),
                Kind::Gullies => self.set.contains(Set::GULLIES),
            };
            if !on {
                continue;
            }
            for (k, o) in octaves.iter().enumerate() {
                if !has_stripe(o, size) {
                    continue;
                }
                let idx = at.map(|v| v >> o.cell.trailing_zeros());
                let corners = *Layers::cell(cache, base + k, idx, o.cell, source);
                total += stripes(o, kind, x, idx, &corners);
            }
        }
        if self.set.contains(Set::TALUS) {
            let mask = Layers::mask_at(cache, at, source);
            if mask > 0.0 {
                let mut noise = 0.0;
                for (k, (o, kept)) in TALUS.into_iter().zip(&mut cache.noise).enumerate() {
                    if has(o, size) {
                        let n = value_noise(salt(self.seed, NOISE_SLOT + k), at, o.cell, kept);
                        noise += talus_amp(o) * shape(o, n);
                    }
                }
                total += mask * noise;
            }
        }
        if let Some(kept) = cache.sums.get_mut(slot) {
            *kept = Some((at, total));
        }
        total
    }

    /// Whether the Voxel at the lattice point `at` is loose rock if it is under the top of the
    /// ground by less than [`SCREE_DEPTH`].
    pub fn scree(&self, at: [i32; 3], cache: &mut Cache, source: Source<'_>) -> bool {
        self.set.contains(Set::TALUS) && Layers::mask_at(cache, at, source) > 0.5
    }

    /// The corners of the cells of a cell size `cell` that the lattice box `low..=high` is over,
    /// as their lattice indices from and to, if there are at most [`MOST_CELLS`] along each axis.
    fn block(cell: i32, low: [i32; 3], high: [i32; 3]) -> Option<([i32; 3], [i32; 3])> {
        let shift = cell.trailing_zeros();
        let first = low.map(|v| v >> shift);
        let last = high.map(|v| (v >> shift) + 1);
        (0..3)
            .all(|a| last[a] - first[a] <= MOST_CELLS)
            .then_some((first, last))
    }

    /// Bounds `(lowest, highest)` on [`Layers::height`] at every lattice point in the box
    /// `low..=high`.
    pub fn bounds(
        &self,
        low: [i32; 3],
        high: [i32; 3],
        size: f64,
        source: Source<'_>,
    ) -> (f64, f64) {
        let centre: [f64; 3] =
            std::array::from_fn(|a| (f64::from(low[a]) + f64::from(high[a])) / 2.0);
        let half = 0.5
            * distance(std::array::from_fn(|a| {
                f64::from(high[a]) - f64::from(low[a])
            }));
        let (mut lo, mut hi) = (0.0, 0.0);
        for (kind, octaves, on) in [
            (Kind::Ridged, &RIDGED[..], self.set.contains(Set::RIDGED)),
            (Kind::Gullies, &GULLIES[..], self.set.contains(Set::GULLIES)),
        ] {
            if !on {
                continue;
            }
            for o in octaves.iter().filter(|o| has_stripe(o, size)) {
                let (a, b) = Layers::stripe_bounds(o, kind, low, high, centre, half, source);
                lo += a;
                hi += b;
            }
        }
        if self.set.contains(Set::TALUS) {
            let (mask_lo, mask_hi) = Layers::mask_bounds(low, high, source);
            let (mut n_lo, mut n_hi) = (0.0, 0.0);
            for (k, o) in TALUS.into_iter().enumerate() {
                if !has(o, size) {
                    continue;
                }
                let (a, b) = noise_range(
                    salt(self.seed, NOISE_SLOT + k),
                    NOISE_SLOT + k,
                    low,
                    high,
                    o.cell,
                );
                let (a, b) = shape_bounds(o, a, b);
                n_lo += talus_amp(o) * a;
                n_hi += talus_amp(o) * b;
            }
            let products = [
                mask_lo * n_lo,
                mask_lo * n_hi,
                mask_hi * n_lo,
                mask_hi * n_hi,
            ];
            lo += products.into_iter().fold(f64::INFINITY, f64::min);
            hi += products.into_iter().fold(f64::NEG_INFINITY, f64::max);
        }
        (lo - SLACK, hi + SLACK)
    }

    fn stripe_bounds(
        o: &Stripe,
        kind: Kind,
        low: [i32; 3],
        high: [i32; 3],
        centre: [f64; 3],
        half: f64,
        source: Source<'_>,
    ) -> (f64, f64) {
        let shift = o.cell.trailing_zeros();
        // A box in one cell of the octave is bounded by interval arithmetic on its corners'
        // patterns, which is tight; a box across cells by a Lipschitz bound, which is not.
        if (0..3).all(|a| low[a] >> shift == high[a] >> shift) {
            return Layers::interval_bounds(o, kind, low, high, source);
        }
        let Some((first, last)) = Layers::block(o.cell, low, high) else {
            return stripe_range(o, kind, 1.0);
        };
        let mut strongest: f64 = 0.0;
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    let c = [x, y, z].map(|v| v.wrapping_mul(o.cell));
                    let corner = source(c);
                    strongest = strongest.max(match kind {
                        Kind::Ridged => corner.ridge.m,
                        Kind::Gullies => corner.gully.m,
                    });
                }
            }
        }
        if strongest == 0.0 {
            return (0.0, 0.0);
        }
        let idx = centre.map(|v| (v / f64::from(o.cell)).floor() as i32);
        let corners = Layers::corners(idx, o.cell, source);
        let at_centre = stripes(o, kind, centre, idx, &corners);
        let lipschitz = o.amp * strongest * (7.8 / f64::from(o.cell) + 6.0 / (0.85 * o.wavelength));
        let (range_lo, range_hi) = stripe_range(o, kind, strongest);
        (
            (at_centre - lipschitz * half).max(range_lo),
            (at_centre + lipschitz * half).min(range_hi),
        )
    }

    /// The bounds of an octave over a box in one cell of it: each of the cell's eight corners'
    /// pattern is `weight * m * profile(phase)`, the weight a product of faded fractions and the
    /// phase linear in the point, so each has an interval over the box, and the sum of the
    /// products of the intervals holds the sum.
    fn interval_bounds(
        o: &Stripe,
        kind: Kind,
        low: [i32; 3],
        high: [i32; 3],
        source: Source<'_>,
    ) -> (f64, f64) {
        let cell = f64::from(o.cell);
        let shift = o.cell.trailing_zeros();
        let idx = low.map(|v| v >> shift);
        let corners = Layers::corners(idx, o.cell, source);
        // The faded fraction's interval along each axis.
        let fade: [(f64, f64); 3] = std::array::from_fn(|a| {
            let f = |v: i32| {
                let t = f64::from(v) / cell - f64::from(idx[a]);
                t * t * (3.0 - 2.0 * t)
            };
            (f(low[a]), f(high[a]))
        });
        let mid: [f64; 3] = std::array::from_fn(|a| (f64::from(low[a]) + f64::from(high[a])) / 2.0);
        let reach: [f64; 3] =
            std::array::from_fn(|a| (f64::from(high[a]) - f64::from(low[a])) / 2.0);
        let (mut lo, mut hi) = (0.0, 0.0);
        for (k, corner) in corners.iter().enumerate() {
            let wave = match kind {
                Kind::Ridged => &corner.ridge,
                Kind::Gullies => &corner.gully,
            };
            if wave.m == 0.0 {
                continue;
            }
            let d = [k & 1, k >> 1 & 1, k >> 2];
            let (mut w_lo, mut w_hi) = (1.0, 1.0);
            for a in 0..3 {
                let (a_lo, a_hi) = if d[a] == 1 {
                    fade[a]
                } else {
                    (1.0 - fade[a].1, 1.0 - fade[a].0)
                };
                w_lo *= a_lo;
                w_hi *= a_hi;
            }
            // The phase over the box.
            let length = o.wavelength * wave.length;
            let at: f64 = (0..3)
                .map(|a| {
                    (mid[a] - f64::from((idx[a] + d[a] as i32).wrapping_mul(o.cell))) * wave.w[a]
                })
                .sum::<f64>()
                / length
                + wave.phase;
            let spread: f64 = (0..3).map(|a| reach[a] * wave.w[a].abs()).sum::<f64>() / length;
            let (p_lo, p_hi) = profile_range(kind, at - spread - 1e-9, at + spread + 1e-9);
            let products = [w_lo * p_lo, w_lo * p_hi, w_hi * p_lo, w_hi * p_hi];
            lo += wave.m * products.into_iter().fold(f64::INFINITY, f64::min);
            hi += wave.m * products.into_iter().fold(f64::NEG_INFINITY, f64::max);
        }
        let (range_lo, range_hi) = stripe_range(o, kind, 1.0);
        ((o.amp * lo).max(range_lo), (o.amp * hi).min(range_hi))
    }

    /// Bounds on the talus mask over the box: in one cell, the corners' masks under the intervals
    /// of their weights, else their least and greatest.
    fn mask_bounds(low: [i32; 3], high: [i32; 3], source: Source<'_>) -> (f64, f64) {
        let shift = MASK_CELL.trailing_zeros();
        if (0..3).all(|a| low[a] >> shift == high[a] >> shift) {
            let idx = low.map(|v| v >> shift);
            let corners = Layers::corners(idx, MASK_CELL, source);
            let cell = f64::from(MASK_CELL);
            let fade: [(f64, f64); 3] = std::array::from_fn(|a| {
                let f = |v: i32| {
                    let t = f64::from(v) / cell - f64::from(idx[a]);
                    t * t * (3.0 - 2.0 * t)
                };
                (f(low[a]), f(high[a]))
            });
            let (mut lo, mut hi) = (0.0, 0.0);
            let (mut least, mut greatest) = (f64::INFINITY, f64::NEG_INFINITY);
            for (k, corner) in corners.iter().enumerate() {
                let d = [k & 1, k >> 1 & 1, k >> 2];
                let (mut w_lo, mut w_hi) = (1.0, 1.0);
                for a in 0..3 {
                    let (a_lo, a_hi) = if d[a] == 1 {
                        fade[a]
                    } else {
                        (1.0 - fade[a].1, 1.0 - fade[a].0)
                    };
                    w_lo *= a_lo;
                    w_hi *= a_hi;
                }
                lo += w_lo * corner.talus;
                hi += w_hi * corner.talus;
                least = least.min(corner.talus);
                greatest = greatest.max(corner.talus);
            }
            return (lo.max(least), hi.min(greatest));
        }
        let Some((first, last)) = Layers::block(MASK_CELL, low, high) else {
            return (0.0, 1.0);
        };
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    let m = source([x, y, z].map(|v| v.wrapping_mul(MASK_CELL))).talus;
                    lo = lo.min(m);
                    hi = hi.max(m);
                }
            }
        }
        (lo, hi)
    }

    /// Whether the mask can be over a half anywhere in the box, which is when its Voxels may be
    /// loose rock.
    pub fn scree_possible(&self, low: [i32; 3], high: [i32; 3], source: Source<'_>) -> bool {
        self.set.contains(Set::TALUS) && Layers::mask_bounds(low, high, source).1 > 0.5
    }
}

/// Room for the rounding of the sums in the bounds.
const SLACK: f64 = 1e-6;

thread_local! {
    /// The corners this thread has read from a map: a direct-mapped table keyed by the map and the
    /// lattice point. It only saves work: a corner is a function of the map and the point.
    static CORNERS: RefCell<Vec<(usize, [i32; 3], Corner)>> = const { RefCell::new(Vec::new()) };
}

const CORNER_SLOTS: usize = 4096;

/// The layers of a Planet, with the map they read their corners from.
#[derive(Clone)]
pub(crate) struct Proto {
    layers: Layers,
    map: Arc<WorldMap>,
    reach: (f64, f64),
}

impl Proto {
    /// The layers of `set` over `map`, `None` for none.
    pub fn new(set: Set, map: Arc<WorldMap>) -> Option<Proto> {
        (!set.is_empty()).then(|| Proto {
            layers: Layers::new(map.seed(), map.radius(), set),
            reach: set.reach(),
            map,
        })
    }

    /// The most the layers move the ground up and down, in Voxels.
    pub fn reach(&self) -> (f64, f64) {
        self.reach
    }

    fn corner(&self, c: [i32; 3]) -> Corner {
        let id = self.map.lie_id() | 1;
        let slot = (c[0].wrapping_mul(73_856_093)
            ^ c[1].wrapping_mul(19_349_663)
            ^ c[2].wrapping_mul(83_492_791)) as u32 as usize
            % CORNER_SLOTS;
        CORNERS.with(|table| {
            let mut table = table.borrow_mut();
            if table.is_empty() {
                table.resize(CORNER_SLOTS, (0, [0; 3], Corner::default()));
            }
            let entry = &mut table[slot];
            if entry.0 == id && entry.1 == c {
                return entry.2;
            }
            let corner = self.compute(c);
            *entry = (id, c, corner);
            corner
        })
    }

    fn compute(&self, c: [i32; 3]) -> Corner {
        let p = c.map(f64::from);
        let length = distance(p);
        // Near the centre there is no ground.
        if length < f64::from(self.map.radius()) / VOXELS_PER_POINT / 2.0 {
            return Corner::default();
        }
        let (face, i, j) = self.map.lie_texel(p.map(|v| v / length));
        let lie = self.map.lie(face, i as i32, j as i32);
        corner_of(self.layers.seed, c, &lie)
    }

    /// The layers' sum at `point`, `radius` from the centre, in Voxels.
    pub fn height(
        &self,
        point: [f64; 3],
        radius: f64,
        size: f64,
        cache: &mut crate::detail::Cache,
    ) -> f64 {
        let at = self.layers.detail.lattice(point, radius);
        self.layers
            .height(at, size, &mut cache.proto, &|c| self.corner(c))
    }

    /// Whether the Voxel at `point` is loose rock, if it is within [`SCREE_DEPTH`] of the top.
    pub fn scree(&self, point: [f64; 3], radius: f64, cache: &mut crate::detail::Cache) -> bool {
        let at = self.layers.detail.lattice(point, radius);
        self.layers.scree(at, &mut cache.proto, &|c| self.corner(c))
    }

    /// Bounds on [`Proto::height`] over every point in the hull of `corners`.
    pub fn bounds(&self, corners: &[[f64; 3]], size: f64) -> (f64, f64) {
        match self.layers.detail.lattice_box(corners) {
            Some((low, high)) => self.layers.bounds(low, high, size, &|c| self.corner(c)),
            None => (-self.reach.1 - SLACK, self.reach.0 + SLACK),
        }
    }

    /// Whether a Voxel in the hull of `corners` may be loose rock.
    pub fn scree_possible(&self, corners: &[[f64; 3]]) -> bool {
        match self.layers.detail.lattice_box(corners) {
            Some((low, high)) => self.layers.scree_possible(low, high, &|c| self.corner(c)),
            None => self.layers.set.contains(Set::TALUS),
        }
    }
}

#[cfg(test)]
mod tests;
