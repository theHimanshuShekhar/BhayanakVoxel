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
//! valleys of the issue. The whole is scaled by [`factor`] of the map's height, so that the
//! sea floor and the lowlands are gentle and the mountains rough, and is continuous everywhere.
//!
//! A brick's Voxels of mip `level` are samples `size = 2^level` Voxels apart. An octave whose
//! cell is under [`MIN_CELL`] samples is left out of them: it is under a Voxel's own amplitude,
//! and a coarse Voxel is a sample, not an average. [`Detail::bounds`] gives bounds on the detail
//! over a box that the shortcuts of `planet` rely on (a brick that is all above or all below the
//! ground is not worked out Voxel by Voxel): tight over a few lattice cells, from their corners,
//! and the whole amplitude where the box is wider.

use brick_store::VOXELS_PER_METRE;

use crate::world_map::ELEVATION_UNITS_PER_METRE;
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

/// The most that the detail moves the ground, in Voxels: the amplitudes' sum, which is 20 m.
pub const REACH: f64 = {
    let (mut sum, mut k) = (0.0, 0);
    while k < OCTAVES.len() {
        sum += amplitude(OCTAVES[k]);
        k += 1;
    }
    sum
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
    /// elevation is `elevation` Voxels, in a brick sampled `size` Voxels apart.
    pub fn height(
        &self,
        point: [f64; 3],
        radius: f64,
        elevation: f64,
        size: f64,
        cache: &mut Cache,
    ) -> f64 {
        let at = self.lattice(point, radius);
        let slot = (at[0].wrapping_mul(73_856_093)
            ^ at[1].wrapping_mul(19_349_663)
            ^ at[2].wrapping_mul(83_492_791)) as u32 as usize
            % SUMS;
        if let Some(Some((kept, total))) = cache.sums.get(slot)
            && *kept == at
        {
            return total * factor(elevation);
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
        total * factor(elevation)
    }

    /// Bounds `(lowest, highest)` on [`Detail::height`] at every point in the box that has
    /// `corners` among its corners (the hull of them), for a brick sampled `size` Voxels apart,
    /// where the map's elevation is within `elevation`.
    pub fn bounds(&self, corners: &[[f64; 3]], size: f64, elevation: (f64, f64)) -> (f64, f64) {
        let (calm, rough) = (factor(elevation.0), factor(elevation.1));
        let (lo, hi) = self.sum_bounds(corners, size);
        let extremes = [lo * calm, lo * rough, hi * calm, hi * rough];
        (
            extremes.into_iter().fold(f64::INFINITY, f64::min) - SLACK,
            extremes.into_iter().fold(f64::NEG_INFINITY, f64::max) + SLACK,
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
        let margin = 2 + (span * span / (8.0 * self.sea)).ceil() as i32;
        // The box in lattice points, with room for the sphere's curve and the rounding: its middle
        // and how far a point of it can be from the middle along each axis.
        let middle = std::array::from_fn::<i32, 3, _>(|a| {
            ((i64::from(low[a]) + i64::from(high[a])) / 2) as i32
        });
        let reach = std::array::from_fn::<i32, 3, _>(|a| {
            (high[a] - middle[a]).max(middle[a] - low[a]) + margin
        });
        let (mut lo, mut hi) = (0.0, 0.0);
        for (k, o) in OCTAVES.into_iter().enumerate() {
            if !has(o, size) {
                continue;
            }
            // The noise goes at most 3 / cell along an axis: the fade `3t² - 2t³` goes at most
            // 1.5 as fast as `t`, the corners are within `[0, 1)` of each other, and the noise
            // is twice that. So within `reach` of the middle on each axis it is within the sum of
            // those of its value at the middle: tight for a box of a few lattice points in a cell
            // of hundreds, and no tighter than the whole range for a big one.
            let n = middle_noise(salt(self.seed, k), k, middle, o.cell);
            let slope = reach.iter().map(|&r| f64::from(r)).sum::<f64>() * 3.0 / f64::from(o.cell)
                * (1.0 + 1e-12)
                + 1e-12;
            let (n_lo, n_hi) = ((n - slope).max(-1.0), (n + slope).min(1.0));
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
    fn the_amplitudes_sum_to_twenty_metres() {
        assert!((REACH / VOXELS_PER_METRE - 20.32).abs() < 0.01, "{REACH}");
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
                for elevation in [(-300.0, 100.0), (0.0, 5_000.0), (-200.0, -100.0)] {
                    let (low, high) = detail.bounds(&corners, size, elevation);
                    assert!(low <= high);
                    for k in 0..27 {
                        let at: [f64; 3] = std::array::from_fn(|a| {
                            centre[a] + width * f64::from(k / 3i32.pow(a as u32) % 3) / 2.0
                        });
                        let radius = crate::distance(at);
                        for e in [elevation.0, elevation.1, (elevation.0 + elevation.1) / 2.0] {
                            let h = detail.height(at, radius, e, size, &mut Cache::default());
                            assert!(
                                (low..=high).contains(&h),
                                "{n} {width} {elevation:?}: {low} {h} {high}"
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
        let (low, high) = detail.bounds(&corners, 1.0, (10_000.0, 10_000.0));
        assert!(high - low < 30.0 * VOXELS_PER_METRE, "{low} {high}");
        let wide = [[0.0, 0.0, 1.0], [f64::from(SEA) * 2.0, 0.0, 1.0]];
        let (low, high) = detail.bounds(&wide, 1.0, (50_000.0, 50_000.0));
        assert!(
            (high - REACH).abs() < 1e-3 && (low + REACH).abs() < 1e-3,
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
