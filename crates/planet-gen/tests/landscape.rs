//! The Planet's landscape does not depend on the size of a Voxel: the same seed has the same
//! ground at the same places, in metres, whether Voxels are 10 cm or 5 cm. The heights below
//! were recorded from the generator at 10 cm Voxels (`GENERATOR_VERSION` 3), before the switch
//! to 5 cm (#147).

use brick_store::{VOXELS_PER_METRE, VoxelCoord};
use planet_gen::planet::{Planet, WATER};
use planet_gen::world_map::{SEA_LEVEL, WORLD_RADIUS, WorldMap};

const SEED: u64 = 0x5EED_B41A_2024;
const MAP_SIZE: u32 = 256;

/// The surface height of each of [`PLACES`] in metres above sea level, as the generator made it
/// with 10 cm Voxels (`GENERATOR_VERSION` 3), to 10 cm. Recorded from `main` before the switch.
const BEFORE: [f64; 12] = [
    214.8, 18.4, -320.8, 478.6, 73.6, 323.5, 394.7, 0.0, 609.8, 62.7, -517.4, 672.4,
];

/// How far the height may move: one Voxel of the old size.
const TOLERANCE: f64 = 0.1;

/// Fixed places on the Planet as (latitude, longitude) in degrees, spread over the biomes, the
/// sea and the poles.
const PLACES: [(f64, f64); 12] = [
    (0.0, 0.0),
    (12.0, 40.0),
    (-25.0, 110.0),
    (33.0, -75.0),
    (48.0, 10.0),
    (-50.0, -120.0),
    (70.0, 170.0),
    (-72.0, 15.0),
    (5.0, -160.0),
    (-10.0, 75.0),
    (22.0, -30.0),
    (60.0, -100.0),
];

/// The height, in metres above sea level, of the highest solid Voxel that is not water along the
/// ray from the centre through (`lat`, `lon`).
#[allow(clippy::disallowed_methods)]
fn surface_height(planet: &Planet, (lat, lon): (f64, f64)) -> f64 {
    let (lat, lon) = (lat.to_radians(), lon.to_radians());
    let direction = [lat.cos() * lon.cos(), lat.sin(), lat.cos() * lon.sin()];
    let sea = f64::from(WORLD_RADIUS + i32::from(SEA_LEVEL));
    let top = sea + 3000.0 * VOXELS_PER_METRE;
    let mut radius = top.floor();
    loop {
        let at = VoxelCoord(direction.map(|d| (d * radius).round() as i32));
        if planet.voxel(at).is_some_and(|m| m != WATER) {
            return (radius - sea) / VOXELS_PER_METRE;
        }
        radius -= 1.0;
    }
}

#[test]
fn the_ground_is_at_the_same_height_at_the_same_places_as_with_10_cm_voxels() {
    let planet = Planet::new(WorldMap::generate(SEED, MAP_SIZE));
    for (place, before) in PLACES.into_iter().zip(BEFORE) {
        let now = surface_height(&planet, place);
        assert!(
            (now - before).abs() <= TOLERANCE + 1e-9,
            "{place:?}: {before} m before, {now} m now"
        );
    }
}
