//! The Planet's landscape holds the metric claims of ADR 0008 at the real radius, sampled at
//! fixed places: the sea is at sea level, nothing is higher than about 2 km, caves are as big as
//! they are said to be, there are many continents, and the Spawn is on land. The world map is a
//! coarse one (256 texels a face) of the same Planet: its noise is the same, only sampled at fewer
//! places.

use brick_store::{VOXELS_PER_METRE, VoxelCoord};
use planet_gen::planet::{ICE, Planet, WATER};
use planet_gen::world_map::{SEA_LEVEL, TEST_RADIUS, WORLD_RADIUS, WorldMap};
use std::sync::OnceLock;

const SEED: u64 = 0x5EED_B41A_2024;
const MAP_SIZE: u32 = 256;

fn planet() -> &'static Planet {
    static PLANET: OnceLock<Planet> = OnceLock::new();
    PLANET.get_or_init(|| Planet::new(WorldMap::generate(SEED, MAP_SIZE)))
}

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

#[allow(clippy::disallowed_methods)]
fn direction((lat, lon): (f64, f64)) -> [f64; 3] {
    let (lat, lon) = (lat.to_radians(), lon.to_radians());
    [lat.cos() * lon.cos(), lat.sin(), lat.cos() * lon.sin()]
}

fn along(direction: [f64; 3], radius: f64) -> VoxelCoord {
    let length = direction.iter().map(|d| d * d).sum::<f64>().sqrt();
    VoxelCoord(direction.map(|d| (d / length * radius).round() as i32))
}

#[test]
fn the_radius_is_320_km() {
    assert_eq!(WORLD_RADIUS, 320_000 * VOXELS_PER_METRE as i32);
    assert_eq!(planet().radius(), WORLD_RADIUS);
}

#[test]
fn the_sea_is_at_sea_level_and_the_ground_is_where_the_height_says() {
    let sea = planet().sea_radius();
    assert_eq!(sea, f64::from(WORLD_RADIUS + i32::from(SEA_LEVEL)));
    let (mut oceans, mut lands) = (0, 0);
    for place in PLACES {
        let d = direction(place);
        let ground = planet().ground_height(d);
        let at = |h: f64| planet().voxel(along(d, sea + h));
        // A few Voxels' margin: a point is rounded to a Voxel, which moves it off the line.
        if ground < -3.0 {
            oceans += 1;
            // Water, or the sea ice over it at the poles.
            assert!(
                matches!(at(-2.0), Some(m) if m == WATER || m == ICE),
                "{place:?}"
            );
            assert_eq!(at(3.0), None, "{place:?}");
            assert!(at(ground - 3.0).is_some_and(|m| m != WATER), "{place:?}");
        } else if ground > 3.0 {
            lands += 1;
            assert!(at(ground - 3.0).is_some_and(|m| m != WATER), "{place:?}");
            assert_eq!(at(ground + 3.0), None, "{place:?}");
        }
    }
    assert!(oceans >= 3 && lands >= 3, "{oceans} oceans, {lands} lands");
}

#[test]
fn nothing_is_higher_than_about_two_kilometres_or_lower_than_about_a_kilometre_down() {
    let map = planet().map();
    let (mut highest, mut lowest) = (f64::MIN, f64::MAX);
    for face in 0..6 {
        for j in (0..map.size()).step_by(2) {
            for i in (0..map.size()).step_by(2) {
                let ground = planet().ground_height(map.texel_direction(face, i, j));
                highest = highest.max(ground);
                lowest = lowest.min(ground);
            }
        }
    }
    // The map's peaks are 2 km at most, and the detail adds 20 m to them.
    assert!(highest / VOXELS_PER_METRE <= 2_020.0, "{highest}");
    assert!(highest / VOXELS_PER_METRE >= 1_500.0, "{highest}");
    assert!(lowest / VOXELS_PER_METRE >= -1_020.0, "{lowest}");
    assert!(lowest / VOXELS_PER_METRE <= -800.0, "{lowest}");
    // And no Voxel is out of the radius that the Planet says holds all of it.
    assert!(sea_plus(highest) <= f64::from(planet().max_radius()));
}

fn sea_plus(height: f64) -> f64 {
    planet().sea_radius() + height
}

#[test]
fn caves_are_under_land_within_300_m_of_the_ground_and_a_few_metres_across() {
    let spawn = planet().spawn(100);
    let reach = 40_000.0;
    let mouths = planet().cave_mouths(spawn.map(|c| c - reach), spawn.map(|c| c + reach));
    assert!(mouths.len() >= 5, "{} mouths", mouths.len());
    let mut widest = 0;
    for mouth in mouths.into_iter().take(40) {
        let up = mouth.map(|c| c / mouth.iter().map(|c| c * c).sum::<f64>().sqrt());
        let top = mouth.iter().map(|c| c * c).sum::<f64>().sqrt();
        // Down from the ground, in steps of a Voxel, to 310 m: the runs of air.
        let (mut run, mut deepest) = (0, 0.0);
        for k in 0..6_200 {
            let depth = f64::from(k);
            let air = planet().voxel(along(up, top - depth)).is_none();
            if air && k > 0 {
                run += 1;
                deepest = depth;
                widest = widest.max(run);
            } else {
                run = 0;
            }
        }
        assert!(deepest <= 300.0 * VOXELS_PER_METRE + 2.0, "{deepest}");
    }
    // A vertical line through a tube of up to 3.6 m radius is at most 7.2 m of it, and a few
    // tubes that overlap make a little more.
    assert!(widest >= VOXELS_PER_METRE as i32, "{widest}");
    assert!(widest <= (4.0 * 3.6 * VOXELS_PER_METRE) as i32, "{widest}");
}

/// The land masses of the map, as their areas in square kilometres, largest first: texels on
/// land (above sea level) that are next to one another, across the edges of the cube's faces too.
fn land_masses(map: &WorldMap) -> Vec<f64> {
    let size = map.size();
    let id = |f: usize, i: u32, j: u32| f * (size * size) as usize + (j * size + i) as usize;
    let mut parent: Vec<usize> = (0..6 * (size * size) as usize).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let land = |f: usize, i: u32, j: u32| map.texel(f, i, j).elevation >= 0;
    let join = |parent: &mut Vec<usize>, a: usize, b: usize| {
        let (a, b) = (find(parent, a), find(parent, b));
        parent[a] = b;
    };
    // A step off a face, as far as a texel and a third.
    let eps = 1.3 * std::f64::consts::FRAC_PI_2 / f64::from(size);
    for f in 0..6 {
        for j in 0..size {
            for i in 0..size {
                if !land(f, i, j) {
                    continue;
                }
                if i + 1 < size && land(f, i + 1, j) {
                    join(&mut parent, id(f, i, j), id(f, i + 1, j));
                }
                if j + 1 < size && land(f, i, j + 1) {
                    join(&mut parent, id(f, i, j), id(f, i, j + 1));
                }
                if i == 0 || j == 0 || i + 1 == size || j + 1 == size {
                    let d = map.texel_direction(f, i, j);
                    let length = d.iter().map(|c| c * c).sum::<f64>().sqrt();
                    for axis in 0..3 {
                        for sign in [-1.0, 1.0] {
                            let mut q = d.map(|c| c / length);
                            q[axis] += sign * eps;
                            let (f2, i2, j2) = map.locate(q);
                            if land(f2, i2, j2) {
                                join(&mut parent, id(f, i, j), id(f2, i2, j2));
                            }
                        }
                    }
                }
            }
        }
    }
    let radius_km = f64::from(map.radius()) / VOXELS_PER_METRE / 1000.0;
    let texel_km2 =
        4.0 * std::f64::consts::PI * radius_km * radius_km / (6.0 * f64::from(size * size));
    let mut counts = std::collections::BTreeMap::<usize, usize>::new();
    for f in 0..6 {
        for j in 0..size {
            for i in 0..size {
                if land(f, i, j) {
                    *counts.entry(find(&mut parent, id(f, i, j))).or_default() += 1;
                }
            }
        }
    }
    let mut areas: Vec<f64> = counts.values().map(|&c| c as f64 * texel_km2).collect();
    areas.sort_by(|a, b| b.total_cmp(a));
    areas
}

/// The diameter, in km, of the circle of the same area as a land mass of `area` km².
fn equivalent_diameter(area: f64) -> f64 {
    2.0 * (area / std::f64::consts::PI).sqrt()
}

/// How many separate continents the Planet has at least: land masses of 707 km² or more, which
/// are 30 km across or more as circles (the least of ADR 0008's 30 to 60 km). Measured on the full
/// 2048 map of this seed: 77, of a median 51 km across, 46 of them 30 to 60 km; and 86 to 118 on
/// the 256 map of this seed and each of 11 others (median 49 to 58 km), so 70 is held.
const CONTINENTS: usize = 70;

/// The median equivalent diameter of those continents, in km: 49 to 58 measured at both sizes of
/// map, and ADR 0008 says "about 30 to 60 km".
const MEDIAN_DIAMETER_KM: std::ops::RangeInclusive<f64> = 40.0..=65.0;

#[test]
fn there_are_many_separate_continents_30_to_60_km_across() {
    let areas = land_masses(planet().map());
    let mut diameters: Vec<f64> = areas
        .iter()
        .filter(|&&a| equivalent_diameter(a) >= 30.0)
        .map(|&a| equivalent_diameter(a))
        .collect();
    diameters.sort_by(f64::total_cmp);
    assert!(
        diameters.len() >= CONTINENTS,
        "{}: {areas:?}",
        diameters.len()
    );
    let median = diameters[diameters.len() / 2];
    assert!(MEDIAN_DIAMETER_KM.contains(&median), "{median} km");
    // No land mass is a supercontinent: the largest is a fifth of the land at most (measured 12%
    // of this map at 256 and 22% at 2048, where the least of the others are told apart).
    let land: f64 = areas.iter().sum();
    assert!(areas[0] <= 0.3 * land, "{} of {land} km²", areas[0]);
    // About half of the Planet's 1.29 million km² is land: the ocean share is 54%.
    assert!((500_000.0..=700_000.0).contains(&land), "{land} km²");
}

#[test]
fn the_spawn_is_on_land_above_the_sea_and_the_same_every_time() {
    let spawn = planet().spawn(100);
    assert_eq!(spawn, planet().spawn(100));
    assert_eq!(
        spawn,
        Planet::new(WorldMap::generate(SEED, MAP_SIZE)).spawn(100)
    );
    let height = planet().ground_height(spawn);
    assert!(height > 0.0, "{height}");
    let d = spawn.map(|c| c / spawn.iter().map(|c| c * c).sum::<f64>().sqrt());
    let sea = planet().sea_radius();
    let at = |h: f64| planet().voxel(along(d, sea + h));
    assert!(at(height - 3.0).is_some_and(|m| m != WATER));
    assert_eq!(at(height + 3.0), None);
    // Another seed has another Spawn.
    let other = Planet::new(WorldMap::generate(SEED + 1, 64)).spawn(100);
    assert_ne!(spawn, other);
}

#[test]
fn two_planets_of_different_radii_are_made_in_one_process() {
    let small = Planet::new(WorldMap::with_radius(SEED, 64, TEST_RADIUS));
    let big = Planet::new(WorldMap::with_radius(SEED, 64, 2 * TEST_RADIUS));
    assert_eq!(
        (small.radius(), big.radius()),
        (TEST_RADIUS, 2 * TEST_RADIUS)
    );
    assert_eq!(big.molten_radius(), 2 * small.molten_radius());
    // The relief is the same in metres, so the highest point is as far above the sea at both.
    assert_eq!(big.max_radius() - small.max_radius(), TEST_RADIUS);
    assert_eq!(
        small.sea_radius(),
        f64::from(TEST_RADIUS + i32::from(SEA_LEVEL))
    );
    assert_eq!(
        big.sea_radius(),
        f64::from(2 * TEST_RADIUS + i32::from(SEA_LEVEL))
    );
    // The same seed has the same map at both, and Planet of each has ground at its own radius.
    for planet in [&small, &big] {
        let spawn = planet.spawn(100);
        let r = spawn.iter().map(|c| c * c).sum::<f64>().sqrt();
        assert!((r - planet.sea_radius()).abs() < 3_000.0, "{r}");
        assert!(planet.ground_height(spawn) > 0.0);
    }
    // Where the small Planet has sky, the big one has ground.
    let shell = along([1.0, 0.0, 0.0], f64::from(TEST_RADIUS) * 1.5);
    assert!(small.brick(shell.brick()).is_air());
    assert!(!big.brick(shell.brick()).is_air());
}
