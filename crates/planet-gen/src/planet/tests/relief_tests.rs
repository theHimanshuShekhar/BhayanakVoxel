//! The taller ground column of the bench (#242, `--peak-m` and `--trench-m`): the test Planet's
//! map with its heights stretched to peaks of 8 km and trenches of 4 km, four times today's
//! 2 km and 1 km. Everything derived from the range has to follow it, and the shortcuts that
//! rest on the bounds (a brick that is all sky, all sea, one layer or deep rock is not worked out
//! Voxel by Voxel) must still be right on the steeper ground.

use super::*;
use crate::world_map::{PEAK, Relief, TRENCH};

/// The test Planet's map, stretched to the 12 km column of the ticket.
fn tall() -> &'static Planet {
    static TALL: OnceLock<Planet> = OnceLock::new();
    TALL.get_or_init(|| {
        Planet::new(
            WorldMap::with_radius(SEED, SIZE, TEST_RADIUS)
                .with_relief(Relief::of_metres(8_000.0, 4_000.0)),
        )
    })
}

/// A direction of the middle of the texel of the map of `tall()` that has the most of `rank`.
fn texel_by(rank: impl Fn(&Texel) -> i32) -> [f64; 3] {
    let map = tall().map();
    let (face, i, j) = (0..6)
        .flat_map(|f| (0..map.size()).flat_map(move |j| (0..map.size()).map(move |i| (f, i, j))))
        .max_by_key(|&(f, i, j)| rank(&map.texel(f, i, j)))
        .expect("the map has texels");
    map.texel_direction(face, i, j)
}

/// Directions to try the bounds in: the highest peak, the deepest trench, the steepest ground
/// (of the stretched planet: where its elevation changes most between two texels) and the spawn.
fn places() -> Vec<[f64; 3]> {
    let map = tall().map();
    let (mut steepest, mut at) = (0.0, [0.0; 3]);
    for face in 0..6 {
        for j in 0..map.size() - 1 {
            for i in 0..map.size() - 1 {
                let (da, db) = (
                    map.texel_direction(face, i, j),
                    map.texel_direction(face, i + 1, j),
                );
                let rise = (map.elevation_at(da) - map.elevation_at(db)).abs();
                if rise > steepest {
                    (steepest, at) = (rise, [0, 1, 2].map(|k| da[k] + db[k]));
                }
            }
        }
    }
    vec![
        texel_by(|t| i32::from(t.elevation)),
        texel_by(|t| -i32::from(t.elevation)),
        at,
        tall().spawn(SPAWN_REACH),
    ]
}

#[test]
fn a_stretched_map_reads_every_height_four_times_as_high_or_deep_and_keeps_its_texels() {
    let (default, stretched) = (planet(), tall());
    assert_eq!(
        default.map().content_hash(),
        stretched.map().content_hash(),
        "only the reading of the heights is stretched, not the texels"
    );
    assert_eq!(default.map().relief(), Relief::DEFAULT);
    assert_eq!(
        stretched.map().relief(),
        Relief::of_metres(8_000.0, 4_000.0)
    );
    let mut seen = (f64::MAX, f64::MIN);
    for face in 0..6 {
        for j in (0..SIZE).step_by(7) {
            for i in (0..SIZE).step_by(7) {
                let d = stretched.map().texel_direction(face, i, j);
                // Four is a power of two, so the products are exact.
                let (a, b) = (
                    default.map().elevation_at(d),
                    stretched.map().elevation_at(d),
                );
                assert_eq!(b, a * 4.0, "{d:?}");
                seen = (seen.0.min(b), seen.1.max(b));
            }
        }
    }
    assert!(seen.0 < 0.0 && seen.1 > 0.0, "{seen:?}");
    // The highest point of the map is its peak, and the lowest its trench.
    let relief = stretched.map().relief();
    let (low, high) = stretched
        .map()
        .elevation_range(&[[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]]);
    assert_eq!(
        (low, high),
        (-f64::from(relief.trench), f64::from(relief.peak))
    );
    let top = stretched
        .map()
        .elevation_at(texel_by(|t| i32::from(t.elevation)));
    let bottom = stretched
        .map()
        .elevation_at(texel_by(|t| -i32::from(t.elevation)));
    assert!(
        top <= f64::from(relief.peak) && top > 0.9 * f64::from(relief.peak),
        "{top}"
    );
    assert!(
        bottom >= -f64::from(relief.trench) && bottom < -0.9 * f64::from(relief.trench),
        "{bottom}"
    );
}

#[test]
fn what_is_derived_from_the_range_follows_the_relief() {
    let relief = tall().map().relief();
    let (default, stretched) = (planet(), tall());
    assert_eq!(
        stretched.max_radius(),
        max_radius_for(TEST_RADIUS, relief),
        "the highest radius"
    );
    assert_eq!(
        stretched.interior_radius(),
        interior_radius_for(TEST_RADIUS, relief),
        "the interior's radius"
    );
    // 6 km higher and 3 km lower than the default Planet, to the Voxel.
    assert_eq!(
        stretched.max_radius() - default.max_radius(),
        relief.peak - PEAK
    );
    assert_eq!(
        default.interior_radius() - stretched.interior_radius(),
        relief.trench - TRENCH
    );
    assert_eq!(stretched.molten_radius(), default.molten_radius());
    // The default Planet's are unchanged: the constants' own.
    assert_eq!(default.max_radius(), max_radius(TEST_RADIUS));
    assert_eq!(default.interior_radius(), interior_radius(TEST_RADIUS));
    // No ground is above the highest radius, or below the interior's, at the extremes of the map.
    let sea = stretched.sea_radius();
    for d in places() {
        let ground = sea + stretched.ground_height(d);
        assert!(ground < f64::from(stretched.max_radius()), "{d:?} {ground}");
        assert!(
            ground > f64::from(stretched.interior_radius()),
            "{d:?} {ground}"
        );
    }
    // A brick above the old highest radius has ground in it on the stretched Planet, and is
    // not claimed to be sky.
    let peak = texel_by(|t| i32::from(t.elevation));
    let height = stretched.ground_height(peak);
    assert!(
        height > f64::from(default.max_radius() - default.radius()),
        "{height}"
    );
    let coord = along(peak, sea + height - 4.0).brick();
    assert!(!stretched.brick(coord).is_air());
    assert!(!stretched.air_below(0, coord));
}

#[test]
fn the_default_relief_is_what_the_planet_always_had() {
    // The constants, as a value, and a map that was never stretched reads as it did.
    assert_eq!(
        Relief::DEFAULT,
        Relief {
            peak: PEAK,
            trench: TRENCH
        }
    );
    assert_eq!(Relief::default(), Relief::DEFAULT);
    assert_eq!(
        Relief::of_metres(2_000.0, 1_000.0),
        Relief::DEFAULT,
        "2 km and 1 km are the constants"
    );
    let unstretched = Planet::new(WorldMap::with_radius(SEED, SIZE, TEST_RADIUS));
    let same =
        Planet::new(WorldMap::with_radius(SEED, SIZE, TEST_RADIUS).with_relief(Relief::DEFAULT));
    for d in places() {
        assert_eq!(
            unstretched.ground_height(d).to_bits(),
            same.ground_height(d).to_bits()
        );
    }
}

#[test]
#[should_panic(expected = "does not fit a relief")]
fn a_relief_taller_than_the_planet_is_refused() {
    let _ = WorldMap::with_radius(SEED, SIZE, TEST_RADIUS)
        .with_relief(Relief::of_metres(30_000.0, 20_000.0));
}

/// The bounds on the ground over a brick, on the stretched Planet, around the highest peak, the
/// deepest trench, the steepest slope and the spawn: every one of the 512 Voxels' heights is
/// within them. They are the bounds that the shortcuts of `terrain` rest on.
#[test]
fn the_bounds_on_the_ground_over_a_brick_hold_on_the_stretched_planet() {
    let mut bricks = 0;
    for place in places() {
        let ground = tall().ground_height(place);
        let centre = along(place, tall().sea_radius() + ground).brick().0;
        let up = place.map(|c| c / distance(place));
        let axis = (0..3)
            .max_by(|&a, &b| up[a].abs().total_cmp(&up[b].abs()))
            .unwrap();
        let sign = if up[axis] < 0.0 { -1 } else { 1 };
        for dx in -6..6 {
            for dy in -6..6 {
                for dz in -40..40 {
                    let mut c = centre;
                    c[axis] += dz * sign;
                    c[(axis + 1) % 3] += dx;
                    c[(axis + 2) % 3] += dy;
                    let samples = Samples::new(0, BrickCoord(c));
                    let corners: [[f64; 3]; 8] =
                        std::array::from_fn(|i| samples.corner([i & 1, i >> 1 & 1, i >> 2]));
                    let (low, high) = tall().ground_range(&corners, 1.0);
                    for k in 0..512 {
                        let local = [k & 7, k >> 3 & 7, k >> 6 & 7].map(|v| v as u8);
                        let h = tall().ground_height(samples.at(local));
                        assert!(low <= h && h <= high, "{c:?}: {h} not in {low}..{high}");
                    }
                    bricks += 1;
                }
            }
        }
    }
    assert!(bricks > 1000, "{bricks}");
}

/// No brick is declared empty, solid or one layer wrongly: a brick over the steepest stretched
/// ground, and the bricks about the peak, the trench and the spawn, are their Voxels, whichever
/// shortcut decided them.
#[test]
fn bricks_over_the_stretched_ground_are_what_their_voxels_are() {
    let mut mixed = 0;
    for place in places() {
        let ground = tall().ground_height(place);
        let middle = along(place, tall().sea_radius() + ground).brick();
        for dx in -2..=2 {
            for dy in -2..=2 {
                for dz in -14..=14 {
                    let coord = BrickCoord([middle.0[0] + dx, middle.0[1] + dy, middle.0[2] + dz]);
                    assert_brick_is_its_voxels(tall(), coord);
                    mixed += usize::from(!tall().brick(coord).is_uniform());
                }
            }
        }
    }
    assert!(mixed > 100, "{mixed}");
}

/// What `solid_below` and `air_below` claim is true on the stretched Planet: a brick is claimed
/// air all the way down, or solid all the way down, only if every Voxel of level 0 under it
/// (a lattice of them for the big ones) is, over the peak, the trench, the steepest ground and the
/// spawn, from deep under the ground to over it.
#[test]
fn solid_below_and_air_below_are_never_wrongly_claimed_on_the_stretched_planet() {
    let (mut solid, mut air) = (0, 0);
    for place in places() {
        let surface = tall().ground_height(place);
        for level in 0..=3usize {
            let size = f64::from(BRICK_SIZE << level);
            let stride = 1usize << level.saturating_sub(2);
            let mut height = surface - 6.0 * size;
            while height < surface + 8.0 * size {
                let coord = ancestor(along(place, tall().sea_radius() + height).brick(), level);
                let first = coord.0.map(|c| c * (BRICK_SIZE << level));
                let all = |holds: &dyn Fn(Option<Material>) -> bool| {
                    (0..BRICK_SIZE << level).step_by(stride).all(|z| {
                        (0..BRICK_SIZE << level).step_by(stride).all(|y| {
                            (0..BRICK_SIZE << level).step_by(stride).all(|x| {
                                let at = VoxelCoord([first[0] + x, first[1] + y, first[2] + z]);
                                holds(tall().voxel(at))
                            })
                        })
                    })
                };
                if tall().solid_below(level, coord) {
                    solid += 1;
                    assert!(
                        all(&|v| v.is_some()),
                        "level {level} {coord:?} is solid below, but has air"
                    );
                }
                if tall().air_below(level, coord) {
                    air += 1;
                    assert!(
                        all(&|v| v.is_none()),
                        "level {level} {coord:?} is air below, but has ground or sea"
                    );
                }
                height += size;
            }
        }
    }
    assert!(solid > 20 && air > 20, "{solid} solid, {air} air");
}

/// The Guard of the stretched Planet is every brick of its levels that is neither air nor
/// interior, as it is on the default one: its box is the stretched Planet's highest radius.
#[test]
fn the_guard_of_the_stretched_planet_covers_its_taller_column() {
    // A level where the test Planet's Guard is a few hundred cells, as the client's is at its top.
    let top = 14;
    let cells_default = planet().guard_cells(top).len();
    let cells_tall = tall().guard_cells(top).len();
    assert!(
        cells_tall > cells_default,
        "{cells_tall} cells, {cells_default}"
    );
    // The cell a little under the peak is one of the Guard, and holds ground.
    let peak = texel_by(|t| i32::from(t.elevation));
    let size = f64::from(BRICK_SIZE << top);
    let height = tall().sea_radius() + tall().ground_height(peak) - size;
    let cell = ancestor(along(peak, height).brick(), top);
    assert!(tall().guard_cells(top).contains(&cell));
    assert!(
        !tall().guard_cell(top..=top, top, cell).is_empty(),
        "the peak's cell holds ground at level {top} (a brick is {size} Voxels)"
    );
}
