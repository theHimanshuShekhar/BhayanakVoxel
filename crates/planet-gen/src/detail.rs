//! Detail at generation (ADR 0008): the ground's shape between the world map's texels (about
//! 240 m) down to a few metres, made per brick from the map's height and a stateless noise of
//! the seed, so that there is nothing to precompute below the texel, no seams, and the same
//! ground on every machine at every level of detail.
//!
//! The ground's height above sea level at a point is the map's elevation there (bilinear between
//! texel centres, see `WorldMap::elevation_at`) plus the *detail*: a sum of octaves of value noise
//! on the sphere of sea level, in the 10 cm lattice of the map, with cells from 205 m down to
//! 3.2 m. Each octave's amplitude is [`AMPLITUDE`] of its cell, so the ground gets rougher at
//! every scale alike (hills of 10 m on 205 m, rises of 0.2 m on 3 m). The three widest are
//! *ridged* (`2|n| - 1`), which cuts a valley along each line where the noise is zero: the small
//! valleys of the issue. The whole is scaled by how rough the map says the ground is there, which
//! is [`factor`] of the map's height (the sea floor and the lowlands are gentle, the mountains
//! rough) times `0.35 + 0.65 * rough`, where the map's `rough` field is made from the slope
//! (steeper is rougher), the biome (volcanic is jagged, desert smooth) and the drainage (calmer
//! in a valley). And where much water drains (the map's `drainage` field) the ground is cut into
//! a valley, up to [`VALLEY`] deep, which follows the drainage the way the map has it. All the
//! fields are interpolated between texels like the elevation, so the ground is continuous
//! everywhere, over the edges of the cube's faces too.
//!
//! A brick's Voxels of mip `level` are samples `size = 2^level` Voxels apart. An octave whose
//! cell is under [`MIN_CELL`] samples is left out of them: it is under a Voxel's own amplitude,
//! and a coarse Voxel is a sample, not an average. [`Detail::bounds`] gives bounds on the detail
//! over a box that the shortcuts of `planet` rely on (a brick that is all above or all below the
//! ground is not worked out Voxel by Voxel): tight over a few lattice cells, from their corners,
//! and the whole amplitude where the box is wider.

use brick_store::VOXELS_PER_METRE;

use crate::world_map::{ELEVATION_UNITS_PER_METRE, FieldRange, Fields};
use crate::{Corners, value_noise};

/// An octave: its cell in lattice points (10 cm, a power of two) and whether it is ridged.
#[derive(Clone, Copy)]
struct Octave {
    cell: i32,
    ridged: bool,
}

const fn octave(cell: i32, ridged: bool) -> Octave {
    Octave { cell, ridged }
}

const OCTAVES: [Octave; 7] = [
    octave(2048, true),
    octave(1024, true),
    octave(512, true),
    octave(256, false),
    octave(128, false),
    octave(64, false),
    octave(32, false),
];

/// An octave's amplitude as a share of its cell.
const AMPLITUDE: f64 = 0.05;

/// Voxels in one lattice point: 2 for 5 cm Voxels.
const VOXELS_PER_POINT: f64 = VOXELS_PER_METRE / ELEVATION_UNITS_PER_METRE as f64;

/// An octave's amplitude in Voxels.
const fn amplitude(o: Octave) -> f64 {
    o.cell as f64 * VOXELS_PER_POINT * AMPLITUDE
}

/// How deep the ground is cut where the most water drains, in Voxels: 3 m.
const VALLEY: f64 = 3.0 * VOXELS_PER_METRE;
/// The drainage (of 1) from which the ground is cut, and where it is cut the whole `VALLEY`.
const VALLEY_FROM: f64 = 0.25;
const VALLEY_FULL: f64 = 0.75;
/// The height above sea level, in Voxels, from which a valley is cut the whole way: 30 m. On the
/// coast and under the sea there are none.
const VALLEY_ABOVE: f64 = 30.0 * VOXELS_PER_METRE;

/// The most that the detail moves the ground, in Voxels: the amplitudes' sum (20 m) and a valley.
pub const REACH: f64 = {
    let (mut sum, mut k) = (0.0, 0);
    while k < OCTAVES.len() {
        sum += amplitude(OCTAVES[k]);
        k += 1;
    }
    sum + VALLEY
};

/// The fewest samples an octave's cell must be for a brick sampled `size` Voxels apart to have it.
const MIN_CELL: f64 = 4.0;

/// How rough the ground is at the sea floor and in the lowlands, against the mountains' 1.
const CALM: f64 = 0.35;
/// The height above sea level, in Voxels, where the ground is as rough as it gets: 600 m.
const ROUGH_FROM: f64 = 600.0 * VOXELS_PER_METRE;

/// How rough the ground is where the map's elevation is `elevation` Voxels above sea level:
/// `CALM` at and below sea level, rising to 1 at 600 m, never more.
fn factor(elevation: f64) -> f64 {
    CALM + (1.0 - CALM) * (elevation / ROUGH_FROM).clamp(0.0, 1.0)
}

/// How much of the octaves' sum is kept where the map's fields are `elevation` Voxels and `rough`
/// (of 1): `factor` of the height, and from 0.35 of that where the map says the ground is smooth
/// to all of it where it is rough. Rises with both.
fn scale(elevation: f64, rough: f64) -> f64 {
    factor(elevation) * (0.35 + 0.65 * rough)
}

/// How far the ground is cut into a valley where the map's fields are `elevation` Voxels and
/// `drainage` (of 1), in Voxels, never negative: none where little drains or the ground is near
/// the sea, the whole `VALLEY` where much does and it is high. Rises with both.
fn valley(elevation: f64, drainage: f64) -> f64 {
    let wet = ((drainage - VALLEY_FROM) / (VALLEY_FULL - VALLEY_FROM)).clamp(0.0, 1.0);
    let high = (elevation / VALLEY_ABOVE).clamp(0.0, 1.0);
    VALLEY * wet * wet * (3.0 - 2.0 * wet) * high
}

/// Whether a brick sampled `size` Voxels apart has the octave.
fn has(o: Octave, size: f64) -> bool {
    f64::from(o.cell) * VOXELS_PER_POINT >= MIN_CELL * size
}

/// The last lattice cell of each octave, so that the Voxels of a brick, which nearly always share
/// one, hash its corners once. It only saves work.
///
/// And, for one brick ([`Cache::for_brick`]), the sum of the octaves at the lattice points it was
/// asked for: a Voxel of level 0 is half a lattice point, so the 512 Voxels of a brick are on 125
/// of them, and so are their neighbours', and the Voxel's top is on the lattice point of its middle.
/// The sum is the same with or without it.
#[derive(Default)]
pub(crate) struct Cache {
    corners: [Option<Corners>; OCTAVES.len()],
    sums: Vec<Option<([i32; 3], f64)>>,
}

/// Lattice points whose sums a [`Cache`] keeps.
const SUMS: usize = 256;

impl Cache {
    /// A cache for the Voxels of one brick sampled `size` Voxels apart: only those of level 0
    /// (1 apart) share lattice points.
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

/// The detail of one Planet.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Detail {
    seed: u64,
    /// Radius of sea level in lattice points.
    sea: f64,
}

/// The salt of octave `k`'s noise.
fn salt(seed: u64, k: usize) -> u64 {
    seed ^ (k as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// An octave's value from the noise `n` in `[-1, 1)`: ridged, or as it is.
fn shape(o: Octave, n: f64) -> f64 {
    if o.ridged { 2.0 * n.abs() - 1.0 } else { n }
}

/// The bounds on [`shape`] over noise within `[lo, hi]`.
fn shape_bounds(o: Octave, lo: f64, hi: f64) -> (f64, f64) {
    if !o.ridged {
        return (lo, hi);
    }
    let top = 2.0 * lo.abs().max(hi.abs()) - 1.0;
    let bottom = if lo <= 0.0 && hi >= 0.0 {
        -1.0
    } else {
        2.0 * lo.abs().min(hi.abs()) - 1.0
    };
    (bottom, top)
}

impl Detail {
    /// The detail of the Planet of `seed` whose sea level is `radius` Voxels from the centre.
    pub fn new(seed: u64, radius: i32) -> Detail {
        Detail {
            seed: seed ^ 0x6a09_e667_f3bc_c908,
            sea: f64::from(radius) / VOXELS_PER_POINT,
        }
    }

    /// The lattice point of the sphere of sea level under `point`, which is `radius` from the
    /// centre.
    fn lattice(&self, point: [f64; 3], radius: f64) -> [i32; 3] {
        let scale = self.sea / radius;
        point.map(|c| (c * scale).round() as i32)
    }

    /// The detail, in Voxels, at `point`, `radius` from the centre (not zero), where the map's
    /// fields are `fields`, in a brick sampled `size` Voxels apart.
    pub fn height(
        &self,
        point: [f64; 3],
        radius: f64,
        fields: Fields,
        size: f64,
        cache: &mut Cache,
    ) -> f64 {
        let Fields {
            elevation,
            rough,
            drainage,
        } = fields;
        let at = self.lattice(point, radius);
        let slot = (at[0].wrapping_mul(73_856_093)
            ^ at[1].wrapping_mul(19_349_663)
            ^ at[2].wrapping_mul(83_492_791)) as u32 as usize
            % SUMS;
        if let Some(Some((kept, total))) = cache.sums.get(slot)
            && *kept == at
        {
            return total * scale(elevation, rough) - valley(elevation, drainage);
        }
        let mut total = 0.0;
        for (k, (o, cached)) in OCTAVES.into_iter().zip(&mut cache.corners).enumerate() {
            if has(o, size) {
                let n = value_noise(salt(self.seed, k), at, o.cell, cached);
                total += amplitude(o) * shape(o, n);
            }
        }
        if let Some(kept) = cache.sums.get_mut(slot) {
            *kept = Some((at, total));
        }
        total * scale(elevation, rough) - valley(elevation, drainage)
    }

    /// Bounds `(lowest, highest)` on [`Detail::height`] at every point in the box that has
    /// `corners` among its corners (the hull of them), for a brick sampled `size` Voxels apart,
    /// where the map's fields are within `fields`.
    pub fn bounds(&self, corners: &[[f64; 3]], size: f64, fields: FieldRange) -> (f64, f64) {
        // The scale and the valley both rise with each field, so their extremes are at the ends.
        let calm = scale(fields.elevation.0, fields.rough.0);
        let rough = scale(fields.elevation.1, fields.rough.1);
        let (lo, hi) = self.sum_bounds(corners, size);
        let extremes = [lo * calm, lo * rough, hi * calm, hi * rough];
        let cut = (
            valley(fields.elevation.0, fields.drainage.0),
            valley(fields.elevation.1, fields.drainage.1),
        );
        (
            extremes.into_iter().fold(f64::INFINITY, f64::min) - cut.1 - SLACK,
            extremes.into_iter().fold(f64::NEG_INFINITY, f64::max) - cut.0 + SLACK,
        )
    }

    /// Bounds on the sum of the octaves, before it is scaled.
    fn sum_bounds(&self, corners: &[[f64; 3]], size: f64) -> (f64, f64) {
        // Where the box's points are on the sphere of sea level, in lattice points: the box that
        // holds the corners', and room for the sphere's curve between them and the rounding.
        let mut low = [i32::MAX; 3];
        let mut high = [i32::MIN; 3];
        let mut near = f64::INFINITY;
        for &corner in corners {
            let radius = crate::distance(corner);
            near = near.min(radius);
            // Near the centre the directions of the box's points are not those of its corners.
            if radius < self.sea * VOXELS_PER_POINT / 2.0 {
                return self.whole(size);
            }
            let at = self.lattice(corner, radius);
            for a in 0..3 {
                low[a] = low[a].min(at[a]);
                high[a] = high[a].max(at[a]);
            }
        }
        let span = (0..3)
            .map(|a| f64::from(high[a] - low[a]))
            .fold(0.0, f64::max);
        // The rounding of a point to its lattice point is monotone, so a point of the box is
        // within the corners' lattice points but for the sphere's curve between them.
        let margin = 1 + (span * span / (8.0 * self.sea)).ceil() as i32;
        let low = low.map(|l| l - margin);
        let high = high.map(|h| h + margin);
        let (mut lo, mut hi) = (0.0, 0.0);
        for (k, o) in OCTAVES.into_iter().enumerate() {
            if !has(o, size) {
                continue;
            }
            let (n_lo, n_hi) = noise_range(salt(self.seed, k), k, low, high, o.cell);
            let (s_lo, s_hi) = shape_bounds(o, n_lo, n_hi);
            lo += amplitude(o) * s_lo;
            hi += amplitude(o) * s_hi;
        }
        (lo, hi)
    }

    /// The bounds over the whole Planet.
    fn whole(&self, size: f64) -> (f64, f64) {
        let sum: f64 = OCTAVES
            .into_iter()
            .filter(|&o| has(o, size))
            .map(amplitude)
            .sum();
        (-sum, sum)
    }
}

/// The least and greatest of [`value_noise`] of octave `k` over the box of lattice points from
/// `low` to `high`, exactly. In one lattice cell the noise is multilinear in the three faded
/// fractions, which rise with the point, so over a box in the cell it is at its extremes at the
/// box's corners; across the face between two cells it is so on each side, and the face is where
/// the box is cut. So the extremes are at the corners of the box and where it is cut. A box
/// across more than two cells along an axis is bounded by the whole range.
fn noise_range(seed: u64, k: usize, low: [i32; 3], high: [i32; 3], cell: i32) -> (f64, f64) {
    let shift = cell.trailing_zeros();
    // The places along each axis: the ends, and the face between two cells if it is between them.
    let mut places = [[0; 3]; 3];
    let mut counts = [0; 3];
    for a in 0..3 {
        let (first, last) = (low[a] >> shift, high[a] >> shift);
        if last - first > 1 {
            return (-1.0, 1.0);
        }
        places[a][0] = low[a];
        counts[a] = 1;
        if last > first {
            places[a][1] = last << shift;
            counts[a] = 2;
        }
        places[a][counts[a]] = high[a];
        counts[a] += 1;
    }
    let one_cell = (0..3).all(|a| low[a] >> shift == high[a] >> shift);
    let mut local = [None; 8];
    let (mut n_lo, mut n_hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for iz in 0..counts[2] {
        for iy in 0..counts[1] {
            for ix in 0..counts[0] {
                let point = [places[0][ix], places[1][iy], places[2][iz]];
                let n = if one_cell {
                    middle_noise(seed, k, point, cell)
                } else {
                    // Two cells along an axis are told apart by the odd one.
                    let slot =
                        (0..3).fold(0, |slot, a| slot | ((point[a] >> shift) as usize & 1) << a);
                    value_noise(seed, point, cell, &mut local[slot])
                };
                n_lo = n_lo.min(n);
                n_hi = n_hi.max(n);
            }
        }
    }
    (n_lo, n_hi)
}

/// [`value_noise`] of octave `k` at `point`, for the bounds: the corners of the last cell are kept
/// by this thread, with the seed they are of, since the bricks that are bounded one after another
/// are mostly in one cell of the wider octaves, and hashing its corners is what costs.
fn middle_noise(seed: u64, k: usize, point: [i32; 3], cell: i32) -> f64 {
    thread_local! {
        static KEPT: std::cell::RefCell<[(u64, Option<Corners>); OCTAVES.len()]> =
            const { std::cell::RefCell::new([(0, None); OCTAVES.len()]) };
    }
    KEPT.with(|kept| {
        let slot = &mut kept.borrow_mut()[k];
        if slot.0 != seed {
            *slot = (seed, None);
        }
        value_noise(seed, point, cell, &mut slot.1)
    })
}

/// Room for the rounding of the sums in the bounds.
const SLACK: f64 = 1e-6;

#[cfg(test)]
mod tests {
    use super::*;

    const SEA: i32 = 800_000;

    #[test]
    fn the_noise_range_over_a_box_holds_at_every_point_and_is_reached_at_its_corners() {
        // Boxes in one cell and across the face between two, of cells of 32 lattice points.
        for (low, high) in [
            ([3, 5, 7], [9, 8, 11]),
            ([28, 5, 7], [37, 8, 11]),
            ([28, 29, 30], [35, 34, 33]),
            ([-5, -3, -40], [4, 2, -30]),
        ] {
            let (lo, hi) = noise_range(9, 6, low, high, 32);
            let (mut least, mut greatest) = (f64::INFINITY, f64::NEG_INFINITY);
            for x in low[0]..=high[0] {
                for y in low[1]..=high[1] {
                    for z in low[2]..=high[2] {
                        let n = value_noise(9, [x, y, z], 32, &mut None);
                        least = least.min(n);
                        greatest = greatest.max(n);
                    }
                }
            }
            assert!(lo <= least && greatest <= hi, "{low:?} {high:?}");
            assert!(
                least - lo < 1e-9 && hi - greatest < 1e-9,
                "{low:?} {high:?}"
            );
        }
        // A box across more cells than two is the whole range.
        assert_eq!(noise_range(9, 6, [0, 0, 0], [100, 0, 0], 32), (-1.0, 1.0));
    }

    #[test]
    fn the_amplitudes_sum_to_twenty_metres_and_the_valley_is_three_more() {
        assert!((REACH / VOXELS_PER_METRE - 23.32).abs() < 0.01, "{REACH}");
    }

    #[test]
    fn each_field_shapes_the_detail_the_way_it_is_said_to() {
        let detail = Detail::new(5, SEA);
        let at = [f64::from(SEA) + 1.0, 4_000.0, 9_000.0];
        let radius = crate::distance(at);
        let fields = |elevation, rough, drainage| Fields {
            elevation,
            rough,
            drainage,
        };
        let h = |f: Fields| detail.height(at, radius, f, 1.0, &mut Cache::default());
        // Rougher ground (from slope or biome) is moved more, either way; valleys do not move the
        // noise but lower it.
        let (smooth, rough) = (h(fields(30_000.0, 0.0, 0.0)), h(fields(30_000.0, 1.0, 0.0)));
        assert!(
            smooth.abs() <= rough.abs() + 1e-9 && smooth != rough,
            "{smooth} {rough}"
        );
        // More drainage cuts deeper, to the whole valley and no more, and not at the sea.
        let dry = h(fields(30_000.0, 0.5, 0.0));
        let wet = h(fields(30_000.0, 0.5, 1.0));
        assert!((dry - wet - VALLEY).abs() < 1e-9, "{dry} {wet}");
        let little = h(fields(30_000.0, 0.5, 0.5));
        assert!(wet < little && little < dry);
        assert_eq!(h(fields(0.0, 0.5, 1.0)), h(fields(0.0, 0.5, 0.0)));
        // Higher ground is rougher: the same noise, scaled up.
        let (low, high) = (h(fields(0.0, 0.5, 0.0)), h(fields(30_000.0, 0.5, 0.0)));
        assert!(low.abs() < high.abs(), "{low} {high}");
    }

    /// Points on the ground's sphere, and boxes of Voxels at them.
    fn points() -> impl Iterator<Item = [f64; 3]> {
        (0..300).map(|k: i32| {
            let d = [k * 37 % 101 - 50, k * 53 % 103 - 51, k * 71 % 107 - 53].map(f64::from);
            let len = crate::distance(d);
            d.map(|c| c / len * f64::from(SEA))
        })
    }

    #[test]
    fn the_height_is_within_its_bounds_everywhere_in_a_box() {
        let detail = Detail::new(9, SEA);
        for (n, centre) in points().enumerate() {
            for (width, size) in [(7.0, 1.0), (63.0, 8.0), (1000.0, 1.0), (60_000.0, 1.0)] {
                let corners: [[f64; 3]; 8] = std::array::from_fn(|i| {
                    std::array::from_fn(|a| centre[a] + if i >> a & 1 == 0 { 0.0 } else { width })
                });
                for (elevation, rough, drainage) in [
                    ((-300.0, 100.0), (0.0, 1.0), (0.0, 1.0)),
                    ((0.0, 5_000.0), (0.2, 0.6), (0.3, 0.9)),
                    ((-200.0, -100.0), (0.5, 0.5), (0.0, 0.0)),
                    ((1_000.0, 3_000.0), (0.0, 1.0), (0.5, 0.5)),
                ] {
                    let range = FieldRange {
                        elevation,
                        rough,
                        drainage,
                    };
                    let (low, high) = detail.bounds(&corners, size, range);
                    assert!(low <= high);
                    for k in 0..27 {
                        let at: [f64; 3] = std::array::from_fn(|a| {
                            centre[a] + width * f64::from(k / 3i32.pow(a as u32) % 3) / 2.0
                        });
                        let radius = crate::distance(at);
                        for t in [0.0, 0.5, 1.0] {
                            let mix = |(a, b): (f64, f64)| a + (b - a) * t;
                            let fields = Fields {
                                elevation: mix(elevation),
                                rough: mix(rough),
                                drainage: mix(drainage),
                            };
                            let h = detail.height(at, radius, fields, size, &mut Cache::default());
                            assert!(
                                (low..=high).contains(&h),
                                "{n} {width} {range:?}: {low} {h} {high}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_small_box_has_tight_bounds_and_the_whole_planets_are_the_amplitudes() {
        let detail = Detail::new(3, SEA);
        let centre = [f64::from(SEA), 100.0, 100.0];
        let corners: [[f64; 3]; 8] = std::array::from_fn(|i| {
            std::array::from_fn(|a| centre[a] + if i >> a & 1 == 0 { 0.0 } else { 7.0 })
        });
        let still = |elevation: f64, rough: f64, drainage: f64| FieldRange {
            elevation: (elevation, elevation),
            rough: (rough, rough),
            drainage: (drainage, drainage),
        };
        let (low, high) = detail.bounds(&corners, 1.0, still(10_000.0, 1.0, 0.0));
        assert!(high - low < 30.0 * VOXELS_PER_METRE, "{low} {high}");
        let wide = [[0.0, 0.0, 1.0], [f64::from(SEA) * 2.0, 0.0, 1.0]];
        let (low, high) = detail.bounds(&wide, 1.0, still(50_000.0, 1.0, 1.0));
        // The octaves' sum up, less the valley that is there, and down with the valley too.
        assert!(
            (high - (REACH - 2.0 * VALLEY)).abs() < 1e-3 && (low + REACH).abs() < 1e-3,
            "{low} {high} {REACH}"
        );
    }

    #[test]
    fn the_calm_ground_is_calmer_than_the_mountains_and_the_factor_is_continuous() {
        assert_eq!(factor(-5_000.0), CALM);
        assert_eq!(factor(0.0), CALM);
        assert_eq!(factor(ROUGH_FROM * 3.0), 1.0);
        assert!((factor(1e-9) - factor(0.0)).abs() < 1e-6);
    }
}
