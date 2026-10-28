//! Deterministic Planet generation, shared by client and server.
//!
//! A brick depends only on `(seed, GENERATOR_VERSION, brick coordinate)`; never on generation
//! order, thread count or hash-map iteration order. Randomness is a stateless integer hash of
//! the lattice coordinates, and the only float operations are `+ - * /` and conversions, which
//! IEEE 754 makes exact on every platform (Rust never fuses `a * b + c`). No std transcendentals
//! or `powi`/`powf` are used; `clippy.toml` bans them. Any change to the output of this crate
//! must bump [`GENERATOR_VERSION`] and update the golden hashes in `tests/golden.rs`.

use brick_store::{BRICK_SIZE, Brick, BrickCoord, BrickStore, Material, VoxelCoord};

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

/// Smooth value noise in `[-1, 1)` at an integer point.
fn value_noise(seed: u64, point: [i32; 3], cell: i32) -> f64 {
    // `div_euclid` and `rem_euclid` floor, so negative points work. `cell` is a power of two,
    // so `frac` is exact.
    let lattice = point.map(|p| p.div_euclid(cell));
    let frac = point.map(|p| f64::from(p.rem_euclid(cell)) / f64::from(cell));
    let fade = frac.map(|t| t * t * (3.0 - 2.0 * t));
    let corner = |dx: i32, dy: i32, dz: i32| {
        let h = hash(seed, [lattice[0] + dx, lattice[1] + dy, lattice[2] + dz]);
        // The top 53 bits scaled by 2^-53 is exact: a value in [0, 1).
        (h >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    };
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x = |dy, dz| lerp(corner(0, dy, dz), corner(1, dy, dz), fade[0]);
    let y = |dz| lerp(x(0, dz), x(1, dz), fade[1]);
    lerp(y(0), y(1), fade[2]) * 2.0 - 1.0
}

/// How far the surface at `point` is from the mean radius, in Voxels.
fn relief(seed: u64, point: [i32; 3]) -> f64 {
    let mut total = 0.0;
    for (octave, (cell, amplitude)) in (0u64..).zip(OCTAVES) {
        total += amplitude
            * value_noise(
                seed ^ octave.wrapping_mul(0x9e37_79b9_7f4a_7c15),
                point,
                cell,
            );
    }
    total
}

/// Distance of `point` from the Planet's centre. `sqrt` is correctly rounded, so exact everywhere.
fn distance(point: [f64; 3]) -> f64 {
    (point[0] * point[0] + point[1] * point[1] + point[2] * point[2]).sqrt()
}

/// The Voxel at `coord`: `None` for air.
fn voxel(seed: u64, coord: VoxelCoord) -> Option<Material> {
    let dist = distance(coord.0.map(f64::from));
    // How far below the (noise-perturbed) surface this Voxel is.
    let depth = f64::from(PLANET_RADIUS) + relief(seed, coord.0) - dist;
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
    for z in 0..8 {
        for y in 0..8 {
            for x in 0..8 {
                let local = [x, y, z];
                brick.set(local, voxel(seed, coord.voxel(local)));
            }
        }
    }
    brick
}

/// Generates every non-air brick in the box from `min` to `max` inclusive, visiting bricks in
/// ascending order. The whole Planet is millions of bricks, so callers ask for a region.
pub fn generate_region(seed: u64, min: BrickCoord, max: BrickCoord) -> BrickStore {
    let mut store = BrickStore::new();
    for x in min.0[0]..=max.0[0] {
        for y in min.0[1]..=max.0[1] {
            for z in min.0[2]..=max.0[2] {
                let coord = BrickCoord([x, y, z]);
                let brick = generate_brick(seed, coord);
                if !brick.is_air() {
                    store.insert(coord, brick);
                }
            }
        }
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
                    voxel(SEED, coord.voxel(local)),
                    "{c:?} {local:?}"
                );
            }
        }
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
            let r = relief(SEED, [i * 7, i * 13 - 5, i * -11]);
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
}
