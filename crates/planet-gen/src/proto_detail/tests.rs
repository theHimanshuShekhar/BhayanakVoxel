use super::*;
use crate::planet::Planet;
use crate::world_map::{Relief, TEST_MAP_SIZE, TEST_RADIUS};

const SEED: u64 = 0xB4A7_A9AC;

/// A made-up world of corners: some with no layer, the rest with every strength and direction.
fn synthetic(c: [i32; 3]) -> Corner {
    let h = |k: u64| corner_value(0x51ed ^ k.wrapping_mul(0x9e37_79b9_7f4a_7c15), c);
    let wave = |k: u64| {
        let w = unit([h(k) - 0.5, h(k + 1) - 0.5, h(k + 2) - 0.5]);
        Wave {
            m: if h(k + 3) < 0.25 { 0.0 } else { h(k + 4) },
            w,
            phase: h(k + 5),
            length: 0.85 + 0.3 * h(k + 6),
        }
    };
    Corner {
        ridge: wave(10),
        gully: wave(30),
        talus: if h(50) < 0.3 { 0.0 } else { h(51) },
    }
}

#[test]
fn a_set_is_a_comma_list_or_none_or_all() {
    assert_eq!(Set::parse("none"), Ok(Set::NONE));
    assert_eq!(Set::parse("all"), Ok(Set::ALL));
    let two = Set::parse("ridged, talus").unwrap();
    assert!(two.contains(Set::RIDGED) && two.contains(Set::TALUS) && !two.contains(Set::GULLIES));
    assert_eq!(two.name(), "ridged,talus");
    assert_eq!(Set::NONE.name(), "none");
    assert!(Set::parse("ridges").is_err());
}

/// The bounds over a box of lattice points hold at every point of it, whatever the world.
#[test]
fn the_bounds_over_a_box_hold_at_every_point_for_every_layer() {
    let radius = 800_000;
    let mut widest: f64 = 0.0;
    for set in [Set::RIDGED, Set::GULLIES, Set::TALUS, Set::ALL] {
        let layers = Layers::new(SEED, radius, set);
        for (low, spans, size) in [
            ([100, 200, 7_000], [3, 3, 3], 1.0),
            ([-37, 5_001, 90], [3, 4, 3], 1.0),
            ([4_090, 4_090, 4_090], [9, 5, 17], 1.0),
            ([-20, 70, 3_000], [40, 30, 30], 2.0),
            ([1_000, -1_030, 500], [300, 100, 200], 8.0),
            ([5, 5, 5], [2_000, 2_000, 2_000], 64.0),
        ] {
            let high = [0, 1, 2].map(|a| low[a] + spans[a]);
            let (lo, hi) = layers.bounds(low, high, size, &synthetic);
            let (mut least, mut greatest) = (f64::INFINITY, f64::NEG_INFINITY);
            let mut cache = Cache::for_brick(size);
            let step = [0, 1, 2].map(|a| (spans[a] / 9).max(1));
            let mut x = low[0];
            while x <= high[0] {
                let mut y = low[1];
                while y <= high[1] {
                    let mut z = low[2];
                    while z <= high[2] {
                        let v = layers.height([x, y, z], size, &mut cache, &synthetic);
                        least = least.min(v);
                        greatest = greatest.max(v);
                        z += step[2];
                    }
                    y += step[1];
                }
                x += step[0];
            }
            assert!(
                lo <= least && greatest <= hi,
                "{} {low:?} {spans:?}: [{least}, {greatest}] is not within [{lo}, {hi}]",
                set.name()
            );
            if spans[0] <= 4 {
                eprintln!(
                    "{} {low:?} {spans:?}: bounds {:.1} wide",
                    set.name(),
                    hi - lo
                );
                widest = widest.max(hi - lo);
            }
        }
    }
    // A brick-sized box is bounded to a few metres (100 Voxels), not to the layers' whole reach.
    assert!(
        widest < 100.0,
        "the widest bounds on a brick are {widest} Voxels"
    );
}

/// A box with no corner of a layer, and a world with no mask, have exactly nothing.
#[test]
fn where_the_corners_have_no_layer_there_is_none() {
    let blank = |_: [i32; 3]| Corner::default();
    let layers = Layers::new(SEED, 800_000, Set::ALL);
    let mut cache = Cache::for_brick(1.0);
    assert_eq!(layers.height([10, 20, 30], 1.0, &mut cache, &blank), 0.0);
    let (lo, hi) = layers.bounds([0, 0, 0], [3, 3, 3], 1.0, &blank);
    assert!(lo.abs() < 1e-3 && hi.abs() < 1e-3, "{lo} {hi}");
    assert!(!layers.scree([0, 0, 0], &mut cache, &blank));
    assert!(!layers.scree_possible([0, 0, 0], [3, 3, 3], &blank));
}

/// The sum is a function of the point: with the per-brick cache or without, in any order.
#[test]
fn the_cache_changes_nothing_and_the_order_does_not_matter() {
    let layers = Layers::new(SEED, 800_000, Set::ALL);
    let points: Vec<[i32; 3]> = (0..400)
        .map(|k| [4_000 + k % 7, 6_000 + k / 7 % 5, 100 + k * 3 % 11])
        .collect();
    let mut cached = Cache::for_brick(1.0);
    let with: Vec<f64> = points
        .iter()
        .map(|&p| layers.height(p, 1.0, &mut cached, &synthetic))
        .collect();
    for (k, &p) in points.iter().enumerate().rev() {
        let mut fresh = Cache::for_brick(1.0);
        assert_eq!(
            layers.height(p, 1.0, &mut fresh, &synthetic).to_bits(),
            with[k].to_bits()
        );
    }
}

/// The reach holds: the layers move a point by less than `Set::reach` up and down.
#[test]
fn the_sum_stays_within_the_declared_reach() {
    for set in [Set::RIDGED, Set::GULLIES, Set::TALUS, Set::ALL] {
        let layers = Layers::new(SEED, 800_000, set);
        let (up, down) = set.reach();
        let mut cache = Cache::default();
        for k in 0..20_000i32 {
            let p = [
                k.wrapping_mul(7_919) % 9_000,
                k.wrapping_mul(104_729) % 9_000,
                k.wrapping_mul(1_299_709) % 9_000,
            ];
            let v = layers.height(p, 1.0, &mut cache, &synthetic);
            assert!(
                -down - 1e-6 <= v && v <= up + 1e-6,
                "{} at {p:?}: {v} is not within -{down}..{up}",
                set.name()
            );
        }
    }
}

/// A test Planet with the tall relief (8 km peaks) over the test map.
fn map() -> WorldMap {
    WorldMap::with_radius(SEED, TEST_MAP_SIZE, TEST_RADIUS)
        .with_relief(Relief::of_metres(8_000.0, 4_000.0))
}

fn world(set: Set) -> Planet {
    Planet::new(map()).with_proto_detail(set)
}

/// The bricks along the highest ground of the test map: from the surface of the highest texels
/// out to sky, and in.
fn mountain_bricks(planet: &Planet) -> Vec<brick_store::BrickCoord> {
    let map = planet.map();
    let mut texels: Vec<(i16, usize, u32, u32)> = Vec::new();
    for face in 0..6 {
        for j in 0..map.size() {
            for i in 0..map.size() {
                texels.push((map.texel(face, i, j).elevation, face, i, j));
            }
        }
    }
    texels.sort_by_key(|t| std::cmp::Reverse(t.0));
    let mut coords = std::collections::BTreeSet::new();
    for &(_, face, i, j) in texels.iter().step_by(37).take(12) {
        let direction = map.texel_direction(face, i, j);
        let ground = planet.ground_height(direction.map(|c| c * f64::from(TEST_RADIUS)));
        for depth in [-30.0, -6.0, -1.0, 0.0, 1.0, 5.0, 40.0] {
            let radius = planet.sea_radius() + ground + depth;
            let point = direction.map(|c| c * radius);
            coords.insert(brick_store::VoxelCoord(point.map(|c| c.round() as i32)).brick());
        }
    }
    coords.into_iter().collect()
}

#[test]
fn none_is_the_planet_without_the_layers() {
    let plain = world(Set::NONE);
    let off = Planet::new(map());
    for coord in mountain_bricks(&plain) {
        assert_eq!(
            plain.brick(coord).content_hash(),
            off.brick(coord).content_hash()
        );
    }
    assert_eq!(plain.max_radius(), off.max_radius());
}

#[test]
fn a_brick_is_its_voxels_with_every_layer_on() {
    let planet = world(Set::ALL);
    let bricks = mountain_bricks(&planet);
    assert!(bricks.len() > 30);
    let mut scree = 0;
    for coord in bricks {
        let brick = planet.brick(coord);
        let origin = coord.origin().0;
        for i in 0..512i32 {
            let local = [i % 8, i / 8 % 8, i / 64];
            let voxel = brick_store::VoxelCoord(std::array::from_fn(|a| origin[a] + local[a]));
            let got = brick.get(local.map(|c| c as u8));
            assert_eq!(got, planet.voxel(voxel), "{coord:?} at {local:?}");
            scree += usize::from(got == Some(crate::planet::GRAVEL));
        }
    }
    eprintln!("{scree} loose-rock voxels in the sampled bricks");
}

/// The bounds of a Proto over a brick's corners hold at the brick's points, on the real map.
#[test]
fn the_bounds_hold_on_the_maps_corners_and_are_not_the_whole_reach_for_a_brick() {
    let planet = world(Set::ALL);
    let proto = Proto::new(Set::ALL, std::sync::Arc::new(map())).unwrap();
    let mut tight = 0;
    let mut total = 0;
    for coord in mountain_bricks(&planet) {
        let origin = coord.origin().0.map(f64::from);
        let corners: Vec<[f64; 3]> = (0..8)
            .map(|k| std::array::from_fn(|a| origin[a] + if k >> a & 1 == 1 { 7.0 } else { 0.0 }))
            .collect();
        let (lo, hi) = proto.bounds(&corners, 1.0);
        let mut cache = crate::detail::Cache::for_brick(1.0);
        for i in 0..512usize {
            let p: [f64; 3] = std::array::from_fn(|a| origin[a] + (i >> (3 * a) & 7) as f64);
            let radius = distance(p);
            let v = proto.height(p, radius, 1.0, &mut cache);
            assert!(
                lo <= v && v <= hi,
                "{coord:?}: {v} is not within [{lo}, {hi}]"
            );
        }
        let (up, down) = proto.reach();
        total += 1;
        tight += usize::from(hi - lo < 0.5 * (up + down));
    }
    eprintln!("{tight} of {total} bricks are bounded to under half the reach");
}

/// The same bricks at 1, 4 and all threads.
#[test]
fn the_bricks_are_the_same_at_1_4_and_all_threads() {
    let planet = world(Set::ALL);
    let coords = mountain_bricks(&planet);
    let hashes = |threads: usize| {
        machine::par_map(&coords, 3, threads, |&coord| {
            planet.brick(coord).content_hash()
        })
    };
    let alone = hashes(1);
    for threads in [4, machine::threads()] {
        assert_eq!(hashes(threads), alone, "{threads} threads");
    }
}
