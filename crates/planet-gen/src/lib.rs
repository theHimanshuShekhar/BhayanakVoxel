//! Deterministic Planet generation, shared by client and server.
//!
//! A brick depends only on `(seed, GENERATOR_VERSION, brick coordinate)`; never on generation
//! order, thread count or hash-map iteration order. Randomness is a stateless integer hash of
//! the lattice coordinates, and the only float operations are `+ - * /` and conversions, which
//! IEEE 754 makes exact on every platform (Rust never fuses `a * b + c`). No std transcendentals
//! or `powi`/`powf` are used; `clippy.toml` bans them. Any change to the output of this crate
//! must bump [`GENERATOR_VERSION`] and update the golden hashes in `tests/golden.rs`.

use std::sync::atomic::{AtomicI32, Ordering};

use brick_store::{BRICK_SIZE, Brick, BrickCoord, BrickStore, Interior, Material, VoxelCoord};

pub mod world_map;

/// Identifies the generation algorithm. Bump it whenever generated bricks change.
pub const GENERATOR_VERSION: u32 = 1;

/// Mean radius of the test Planet in Voxels: 150 m at 10 cm Voxels, so 300 m across.
pub const PLANET_RADIUS: i32 = 1500;

pub const ROCK: Material = Material(1);
pub const SOIL: Material = Material(2);

/// Thickness of the soil layer on top of the rock, in Voxels.
const SOIL_DEPTH: f64 = 4.0;

/// Value-noise octaves as (lattice cell size in Voxels, amplitude in Voxels). Cell sizes are
/// powers of two so that the in-cell fraction is exact.
const OCTAVES: [(i32, f64); 3] = [(256, 24.0), (64, 8.0), (16, 2.0)];

/// Upper bound on the surface's distance from [`PLANET_RADIUS`]: the amplitudes' sum.
const MAX_RELIEF: f64 = 34.0;

/// Every Voxel within this many Voxels of the centre is rock: the surface is never more than
/// [`MAX_RELIEF`] below [`PLANET_RADIUS`], and the soil is [`SOIL_DEPTH`] thick. A brick wholly
/// inside is therefore uniform rock, which [`generate_planet`] leaves out.
pub const CORE_RADIUS: i32 = PLANET_RADIUS - MAX_RELIEF as i32 - SOIL_DEPTH as i32;

/// The core as the brick store reads it: where [`generate_planet`] holds no brick, a brick wholly
/// inside [`CORE_RADIUS`] is [`ROCK`]. `sim::apply` digs it, and the store's coarse levels are
/// made from it.
pub const CORE: Interior = Interior {
    radius: CORE_RADIUS,
    material: ROCK,
};

/// Hashes a seed and lattice point to 64 well-mixed bits (splitmix64 finalizer per coordinate).
fn hash(seed: u64, point: [i32; 3]) -> u64 {
    fn mix(mut z: u64) -> u64 {
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    point
        .into_iter()
        .fold(mix(seed), |h, c| mix(h ^ u64::from(c as u32)))
}

/// The eight lattice-corner values around one lattice cell, x fastest.
#[derive(Clone, Copy)]
struct Corners {
    lattice: [i32; 3],
    values: [f64; 8],
}

/// The last lattice cell of each octave, so that neighbouring Voxels, which nearly always share
/// a cell, hash its corners once instead of once each. It only saves work: results are identical
/// with or without it.
#[derive(Default)]
struct ReliefCache([Option<Corners>; OCTAVES.len()]);

/// Smooth value noise in `[-1, 1)` at an integer point.
fn value_noise(seed: u64, point: [i32; 3], cell: i32, cache: &mut Option<Corners>) -> f64 {
    // `div_euclid` and `rem_euclid` floor, so negative points work. `cell` is a power of two,
    // so `frac` is exact.
    let lattice = point.map(|p| p.div_euclid(cell));
    let frac = point.map(|p| f64::from(p.rem_euclid(cell)) / f64::from(cell));
    let fade = frac.map(|t| t * t * (3.0 - 2.0 * t));
    let corners = match cache {
        Some(corners) if corners.lattice == lattice => *corners,
        _ => {
            let values = std::array::from_fn(|i| {
                let [dx, dy, dz] = [i & 1, i >> 1 & 1, i >> 2].map(|d| d as i32);
                let h = hash(seed, [lattice[0] + dx, lattice[1] + dy, lattice[2] + dz]);
                // The top 53 bits scaled by 2^-53 is exact: a value in [0, 1).
                (h >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
            });
            *cache.insert(Corners { lattice, values })
        }
    };
    let corner = |dx: usize, dy: usize, dz: usize| corners.values[dx | dy << 1 | dz << 2];
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x = |dy, dz| lerp(corner(0, dy, dz), corner(1, dy, dz), fade[0]);
    let y = |dz| lerp(x(0, dz), x(1, dz), fade[1]);
    lerp(y(0), y(1), fade[2]) * 2.0 - 1.0
}

/// How far the surface at `point` is from the mean radius, in Voxels.
fn relief(seed: u64, point: [i32; 3], cache: &mut ReliefCache) -> f64 {
    let mut total = 0.0;
    for (octave, ((cell, amplitude), cached)) in (0u64..).zip(OCTAVES.into_iter().zip(&mut cache.0))
    {
        total += amplitude
            * value_noise(
                seed ^ octave.wrapping_mul(0x9e37_79b9_7f4a_7c15),
                point,
                cell,
                cached,
            );
    }
    total
}

/// Distance of `point` from the Planet's centre. `sqrt` is correctly rounded, so exact everywhere.
fn distance(point: [f64; 3]) -> f64 {
    (point[0] * point[0] + point[1] * point[1] + point[2] * point[2]).sqrt()
}

/// The Voxel at `coord`: `None` for air.
fn voxel(seed: u64, coord: VoxelCoord, cache: &mut ReliefCache) -> Option<Material> {
    let dist = distance(coord.0.map(f64::from));
    // How far below the (noise-perturbed) surface this Voxel is.
    let depth = f64::from(PLANET_RADIUS) + relief(seed, coord.0, cache) - dist;
    if depth <= 0.0 {
        None
    } else if depth < SOIL_DEPTH {
        Some(SOIL)
    } else {
        Some(ROCK)
    }
}

/// Generates the brick at `coord`.
pub fn generate_brick(seed: u64, coord: BrickCoord) -> Brick {
    // Bricks far from the surface are uniform, which spares evaluating 512 noise samples. A
    // Voxel sits at origin + 0..=7 and `centre` is at origin + 4, so a Voxel is at most
    // 4 * sqrt(3) = 6.93 from `centre`. REACH = 7.0 leaves a margin of only 0.07: do not tighten
    // it. The surface lies within MAX_RELIEF of PLANET_RADIUS, so these bounds agree with
    // `voxel` (a test checks this).
    const REACH: f64 = 7.0;
    let origin = coord.origin().0.map(f64::from);
    let centre = distance(origin.map(|o| o + f64::from(BRICK_SIZE) / 2.0));
    let radius = f64::from(PLANET_RADIUS);
    if centre - REACH > radius + MAX_RELIEF {
        return Brick::AIR;
    }
    if centre + REACH < radius - MAX_RELIEF - SOIL_DEPTH {
        return Brick::solid(ROCK);
    }
    let mut brick = Brick::AIR;
    let mut cache = ReliefCache::default();
    for z in 0..8 {
        for y in 0..8 {
            for x in 0..8 {
                let local = [x, y, z];
                brick.set(local, voxel(seed, coord.voxel(local), &mut cache));
            }
        }
    }
    brick
}

/// Generates every non-air brick in the box from `min` to `max` inclusive.
pub fn generate_region(seed: u64, min: BrickCoord, max: BrickCoord) -> BrickStore {
    generate_box(seed, min, max, false)
}

/// Generates the whole Planet except its core: every non-air brick that is not wholly inside
/// [`CORE_RADIUS`]. The core is a ball of uniform [`ROCK`] about 24 million bricks big, which
/// the store represents as its source [`CORE`], without a brick per cell. About 2.6 million
/// bricks remain; generating them takes about 20 seconds on eight cores in a release build.
///
/// Every brick that is not wholly inside the core and not air is held, which is what makes the
/// coarse levels that [`CORE`] helps to make true to the Planet (see [`brick_store::BrickSource`]).
pub fn generate_planet(seed: u64) -> BrickStore {
    // The surface reaches PLANET_RADIUS + MAX_RELIEF, which is inside this many bricks.
    let extent = (PLANET_RADIUS + MAX_RELIEF as i32) / BRICK_SIZE + 1;
    generate_box(
        seed,
        BrickCoord([-extent; 3]),
        BrickCoord([extent; 3]),
        true,
    )
}

/// Slabs of constant x are generated on all available cores. A brick depends only on its
/// coordinate, so the result does not depend on the thread count or on scheduling.
fn generate_box(seed: u64, min: BrickCoord, max: BrickCoord, skip_core: bool) -> BrickStore {
    let next_x = AtomicI32::new(min.0[0]);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let slabs: Vec<Vec<(BrickCoord, Brick)>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut found = Vec::new();
                    loop {
                        let x = next_x.fetch_add(1, Ordering::Relaxed);
                        if x > max.0[0] {
                            return found;
                        }
                        for y in min.0[1]..=max.0[1] {
                            for z in min.0[2]..=max.0[2] {
                                let coord = BrickCoord([x, y, z]);
                                if skip_core && coord.within_ball(CORE_RADIUS) {
                                    continue;
                                }
                                let brick = generate_brick(seed, coord);
                                if !brick.is_air() {
                                    found.push((coord, brick));
                                }
                            }
                        }
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("generator thread panicked"))
            .collect()
    });
    let mut store = BrickStore::new();
    for (coord, brick) in slabs.into_iter().flatten() {
        store.insert(coord, brick);
    }
    if skip_core {
        store.set_source(CORE);
    }
    store
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: u64 = 0xB4A7_A9AC;
    /// Brick coordinate range along +x that crosses the surface (voxel x = 1440 to 1559).
    const SURFACE_SPAN: std::ops::RangeInclusive<i32> = 180..=194;

    fn along_x(seed: u64) -> Vec<Brick> {
        SURFACE_SPAN
            .map(|x| generate_brick(seed, BrickCoord([x, 0, 0])))
            .collect()
    }

    #[test]
    fn the_distance_shortcuts_agree_with_evaluating_every_voxel() {
        let near_surface = [[186, 0, 0], [-188, 2, 1], [0, 0, -189], [110, 110, 110]];
        let others = [
            [0, 0, 0],
            [-1, -1, -1],
            [150, 0, 0],
            [195, 0, 0],
            [300, 300, 300],
        ];
        for c in near_surface.into_iter().chain(others) {
            let coord = BrickCoord(c);
            let brick = generate_brick(SEED, coord);
            for i in 0..512usize {
                let local = [(i % 8) as u8, (i / 8 % 8) as u8, (i / 64) as u8];
                assert_eq!(
                    brick.get(local),
                    voxel(SEED, coord.voxel(local), &mut ReliefCache::default()),
                    "{c:?} {local:?}"
                );
            }
        }
    }

    /// Worlds already generated hold these ids; changing what they mean needs a
    /// `GENERATOR_VERSION` bump and new golden hashes.
    #[test]
    fn rock_and_soil_are_materials_of_the_shared_table() {
        let table = materials::MaterialTable::built_in();
        assert_eq!(table.get(ROCK).map(|d| d.name.as_str()), Some("rock"));
        assert_eq!(table.get(SOIL).map(|d| d.name.as_str()), Some("soil"));
    }

    #[test]
    fn the_centre_is_uniform_rock_and_far_space_is_air() {
        let core = generate_brick(SEED, BrickCoord([0, 0, 0]));
        assert!(core.is_uniform() && core.get([0, 0, 0]) == Some(ROCK));
        assert!(generate_brick(SEED, BrickCoord([-1, -1, -1])).is_uniform());
        assert!(generate_brick(SEED, BrickCoord([400, 0, 0])).is_air());
        assert!(generate_brick(SEED, BrickCoord([i32::MIN / 8, i32::MAX / 8, 5])).is_air());
    }

    #[test]
    fn crossing_the_surface_goes_rock_then_mixed_then_air() {
        let bricks = along_x(SEED);
        assert!(bricks.first().unwrap().get([0, 0, 0]) == Some(ROCK));
        assert!(bricks.first().unwrap().is_uniform());
        assert!(bricks.last().unwrap().is_air());
        assert!(bricks.iter().any(|b| !b.is_uniform()));
    }

    #[test]
    fn a_planet_without_its_core_reads_the_core_as_rock() {
        let store = generate_box(SEED, BrickCoord([-3; 3]), BrickCoord([3; 3]), true);
        assert!(store.is_empty());
        assert_eq!(
            *store.brick_at(0, BrickCoord([0, 0, 0])),
            Brick::solid(ROCK)
        );
        // Well outside the core, nothing is implied.
        assert!(store.brick_at(0, BrickCoord([300, 0, 0])).is_air());
        // A region holds the bricks it is asked for, core or not, and implies nothing.
        let region = generate_region(SEED, BrickCoord([0; 3]), BrickCoord([1; 3]));
        assert!(region.brick_at(0, BrickCoord([3, 3, 3])).is_air());
    }

    #[test]
    fn soil_covers_rock_along_a_surface_column() {
        let column: Vec<_> = (1440..1560)
            .map(|x| brick_store_get(SEED, VoxelCoord([x, 3, 3])))
            .collect();
        let first_soil = column.iter().position(|v| *v == Some(SOIL)).unwrap();
        let first_air = column.iter().position(|v| v.is_none()).unwrap();
        assert!(column[..first_soil].iter().all(|v| *v == Some(ROCK)));
        assert!(
            column[first_soil..first_air]
                .iter()
                .all(|v| *v == Some(SOIL))
        );
        assert!(column[first_air..].iter().all(|v| v.is_none()));
    }

    fn brick_store_get(seed: u64, v: VoxelCoord) -> Option<Material> {
        generate_brick(seed, v.brick()).get(v.local())
    }

    #[test]
    fn the_same_seed_gives_identical_bricks_and_another_seed_does_not() {
        let hashes = |seed| {
            along_x(seed)
                .iter()
                .map(Brick::content_hash)
                .collect::<Vec<_>>()
        };
        assert_eq!(hashes(SEED), hashes(SEED));
        assert_ne!(hashes(SEED), hashes(SEED + 1));
    }

    #[test]
    fn relief_stays_within_the_bound_the_classifier_relies_on() {
        for i in -300..300 {
            let r = relief(
                SEED,
                [i * 7, i * 13 - 5, i * -11],
                &mut ReliefCache::default(),
            );
            assert!(r.abs() <= MAX_RELIEF, "{r}");
        }
    }

    #[test]
    fn a_region_is_the_same_every_run_and_matches_per_brick_generation() {
        let (min, max) = (BrickCoord([-2, 183, -2]), BrickCoord([2, 190, 2]));
        let a = generate_region(SEED, min, max);
        let b = generate_region(SEED, min, max);
        assert_eq!(a.sorted_coords(), b.sorted_coords());
        assert!(!a.is_empty());
        for coord in a.sorted_coords() {
            let expected = generate_brick(SEED, coord).content_hash();
            assert_eq!(a.brick(coord).unwrap().content_hash(), expected);
            assert_eq!(b.brick(coord).unwrap().content_hash(), expected);
        }
        // Air is left out of the store, solid and mixed bricks are kept.
        assert!(
            a.sorted_coords()
                .iter()
                .all(|&c| !a.brick(c).unwrap().is_air())
        );
        assert!(a.brick(BrickCoord([0, 183, 0])).is_some());
        assert!(a.brick(BrickCoord([0, 190, 0])).is_none());
    }

    #[test]
    fn everything_inside_the_core_radius_is_rock() {
        // Bricks straddling the core boundary, on the axes and on diagonals.
        for c in [
            [181, 0, 0],
            [-182, 1, 0],
            [0, 0, 182],
            [105, 105, 105],
            [-104, 104, 106],
        ] {
            let coord = BrickCoord(c);
            let brick = generate_brick(SEED, coord);
            for i in 0..512usize {
                let local = [(i % 8) as u8, (i / 8 % 8) as u8, (i / 64) as u8];
                let v = coord.voxel(local).0;
                let d2: i64 = v.iter().map(|&c| i64::from(c).pow(2)).sum();
                if d2 <= i64::from(CORE_RADIUS).pow(2) {
                    assert_eq!(brick.get(local), Some(ROCK), "{c:?} {local:?}");
                }
            }
        }
    }

    #[test]
    fn skipping_the_core_leaves_out_only_core_bricks() {
        // A box straddling the core boundary along +x.
        let (min, max) = (BrickCoord([178, -3, -3]), BrickCoord([186, 3, 3]));
        let full = generate_box(SEED, min, max, false);
        let skipped = generate_box(SEED, min, max, true);
        assert!(skipped.len() < full.len());
        for coord in full.sorted_coords() {
            assert_eq!(
                skipped.brick(coord).is_none(),
                coord.within_ball(CORE_RADIUS),
                "{coord:?}"
            );
        }
    }
}
