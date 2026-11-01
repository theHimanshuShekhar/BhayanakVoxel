//! The caves (#46), tested on the test Planet: the criteria of the ticket, measured on the fixed
//! seed. The statistics are over tubes picked at a stride through the table, which is the same
//! every run.

use super::*;
use crate::caves::{self, Tube};

/// The brick at `coord` of level 0 without its caves: the ground alone.
fn ground_brick(coord: BrickCoord) -> Brick {
    planet().evaluate_with(&Samples::new(0, coord), &[])
}

/// The Voxels of the brick at `coord` of level 0 that are caves: ground that the caves have made
/// air.
fn cave_voxels(coord: BrickCoord) -> Vec<[u8; 3]> {
    let (brick, ground) = (planet().brick(coord), ground_brick(coord));
    locals()
        .filter(|&l| ground.get(l).is_some() && brick.get(l).is_none())
        .collect()
}

/// The point `along` (0 to 1) the way from one end of `tube` to the other.
fn on(tube: &Tube, along: f64) -> [f64; 3] {
    std::array::from_fn(|a| tube.a[a] + (tube.b[a] - tube.a[a]) * along)
}

fn brick_of_point(point: [f64; 3]) -> BrickCoord {
    VoxelCoord(point.map(|c| c.round() as i32)).brick()
}

/// How far below the ground `point` is, and the ground's height above sea level there.
fn depth_and_elevation(point: [f64; 3]) -> (f64, f64) {
    let elevation = planet().map().elevation_at(point);
    (SEA_RADIUS + elevation - distance(point), elevation)
}

/// Every `step`th tube of the Planet's caves, which the statistics below are taken over.
fn tubes_stepping(step: usize) -> impl Iterator<Item = &'static Tube> {
    planet().caves.tubes().iter().step_by(step)
}

/// The tube's middle, if it is in the band of the caves under land.
fn middle_in_the_band(tube: &Tube) -> Option<[f64; 3]> {
    let at = on(tube, 0.5);
    let (depth, elevation) = depth_and_elevation(at);
    (elevation >= 0.0 && (0.0..caves::MAX_DEPTH).contains(&depth)).then_some(at)
}

#[test]
fn the_planet_has_caves_under_a_good_part_of_its_land() {
    // Systems are about 17 tubes, and half the Planet is ocean, which has none.
    let tubes = planet().caves.tubes();
    eprintln!(
        "{} tubes, {} mouths",
        tubes.len(),
        planet().cave_mouths().len()
    );
    assert!(tubes.len() > 20_000, "{}", tubes.len());
    assert!(planet().cave_mouths().len() > 500);
    let again = Planet::new(WorldMap::generate(SEED, SIZE));
    assert_eq!(planet().caves_hash(), again.caves_hash());
    let other = Planet::new(WorldMap::generate(SEED + 1, SIZE));
    assert_ne!(planet().caves_hash(), other.caves_hash());
}

#[test]
fn caves_are_only_under_land_less_than_300_m_below_the_ground() {
    // Every Voxel that a tube makes air, in the bricks at two places along every 23rd tube: all
    // are under land and between 0 and 300 m below the ground.
    let (mut carved, mut bricks) = (0, 0);
    let mut by_depth = [0usize; 6];
    for tube in tubes_stepping(23) {
        for along in [0.25, 0.75] {
            let coord = brick_of_point(on(tube, along));
            let found = cave_voxels(coord);
            bricks += usize::from(!found.is_empty());
            for local in found {
                let point = coord.voxel(local).0.map(f64::from);
                let (depth, elevation) = depth_and_elevation(point);
                assert!(elevation >= 0.0, "{coord:?} {local:?} is under the sea");
                assert!(
                    0.0 < depth && depth < caves::MAX_DEPTH,
                    "{depth} Voxels deep"
                );
                by_depth[(depth / 500.0) as usize] += 1;
                carved += 1;
            }
        }
    }
    eprintln!("{carved} cave Voxels in {bricks} bricks, by depth in 50 m: {by_depth:?}");
    assert!(carved > 20_000 && bricks > 200, "{carved} in {bricks}");
    // The caves go all the way to the bottom of the band, not only the top of it.
    assert!(by_depth.iter().all(|&n| n > 0), "{by_depth:?}");

    // And none are below it: bricks at places along tubes that are 300 m to 340 m below the
    // ground, which are the ground's own.
    let mut deep = 0;
    for tube in tubes_stepping(5) {
        for k in 0..=8 {
            let at = on(tube, f64::from(k) / 8.0);
            let (depth, elevation) = depth_and_elevation(at);
            let wall = caves::MAX_DEPTH + 2.0 * caves::MAX_TUBE_RADIUS;
            if elevation >= 0.0 && (wall..wall + 400.0).contains(&depth) {
                let coord = brick_of_point(at);
                assert_eq!(planet().brick(coord), ground_brick(coord), "{coord:?}");
                deep += 1;
            }
        }
    }
    assert!(deep > 100, "{deep}");
}

/// Whether the brick at `coord` of level 0 has a cave Voxel with sky beside it: the cave is open
/// to the surface there.
fn open_to_the_sky(coord: BrickCoord) -> bool {
    cave_voxels(coord).into_iter().any(|local| {
        let at = coord.voxel(local).0.map(f64::from);
        (0..6).any(|face| {
            let mut next = at;
            next[face / 2] += if face % 2 == 0 { -1.0 } else { 1.0 };
            planet().ground_at(next).is_none()
        })
    })
}

#[test]
fn some_caves_open_to_the_surface() {
    let mouths = planet().cave_mouths();
    let (mut open, mut tried) = (0, 0);
    for mouth in mouths.iter().step_by(11) {
        let up = mouth.map(|c| c / distance(*mouth));
        // At the ground there and a little below it, in the bricks around, since the tube may
        // open to one side of the point.
        let found = [0.0, -15.0, -30.0].into_iter().any(|dr| {
            let at: [f64; 3] = std::array::from_fn(|a| mouth[a] + up[a] * dr);
            let coord = brick_of_point(at);
            (-1..=1).any(|dx| {
                (-1..=1).any(|dy| {
                    (-1..=1).any(|dz| {
                        let [x, y, z] = coord.0;
                        open_to_the_sky(BrickCoord([x + dx, y + dy, z + dz]))
                    })
                })
            })
        });
        open += usize::from(found);
        tried += 1;
    }
    eprintln!(
        "{open} of {tried} sampled mouths ({} in all) are open to the sky",
        mouths.len()
    );
    assert!(tried > 50 && open > tried / 4, "{open} of {tried}");
}

#[test]
fn some_surfaces_overhang_with_air_under_solid_along_a_radial_column() {
    // Going up a radial column by a cave's mouth, from 30 m below the ground to the sky, there is
    // air with solid above it: the roof of a cave, near the surface.
    let (mut columns, mut roofs, mut near) = (0, 0, 0);
    for mouth in planet().cave_mouths().iter().step_by(13).take(120) {
        let up = mouth.map(|c| c / distance(*mouth));
        let (ahead, aside) = tangents(up);
        for (s, t) in [
            (0, 0),
            (20, 0),
            (-20, 0),
            (0, 20),
            (0, -20),
            (35, 35),
            (-35, -35),
        ] {
            columns += 1;
            let base: [f64; 3] = std::array::from_fn(|a| {
                mouth[a] + ahead[a] * f64::from(s) + aside[a] * f64::from(t)
            });
            let dir = base.map(|c| c / distance(base));
            let ground = SEA_RADIUS + planet().map().elevation_at(dir);
            let column: Vec<_> = (-300..=40)
                .map(|dr| planet().voxel(along(dir, ground + f64::from(dr))))
                .collect();
            let roof = column
                .windows(2)
                .position(|w| w[0].is_none() && w[1].is_some());
            if let Some(roof) = roof {
                roofs += 1;
                // `roof` is the index of the air, so its height is from -300 up.
                near += usize::from(roof as i32 - 300 > -200);
            }
        }
    }
    eprintln!(
        "{roofs} of {columns} radial columns by cave mouths have air under solid \
         ({near} with the roof within 20 m of the ground)"
    );
    assert!(roofs > 50 && near > 20, "{roofs} {near}");
}

/// The radius of the largest ball of air centred at `centre`, in Voxels, as far as the Planet's
/// bricks 56 Voxels round it say: the distance to the nearest solid Voxel.
fn air_ball_radius(centre: [f64; 3]) -> f64 {
    let at = brick_of_point(centre);
    let mut nearest = f64::INFINITY;
    for dx in -7..=7 {
        for dy in -7..=7 {
            for dz in -7..=7 {
                let coord = BrickCoord([at.0[0] + dx, at.0[1] + dy, at.0[2] + dz]);
                let brick = planet().brick(coord);
                if brick.is_air() {
                    continue;
                }
                for local in locals().filter(|&l| brick.get(l).is_some()) {
                    let v = coord.voxel(local).0.map(f64::from);
                    nearest = nearest.min(distance(std::array::from_fn(|a| v[a] - centre[a])));
                }
            }
        }
    }
    nearest
}

#[test]
fn no_cave_is_wider_than_8_m() {
    // By construction a tube is at most 7.2 m across.
    for tube in planet().caves.tubes() {
        assert!(tube.ra.max(tube.rb) + caves::ROUGHNESS <= caves::MAX_TUBE_RADIUS);
    }
    const { assert!(2.0 * caves::MAX_TUBE_RADIUS <= 80.0) };
    // And measured: the biggest ball of air in the cave, at the ends of tubes that are deep
    // enough under the ground that no ball reaches the sky, where tubes meet and the cave is
    // widest. None is 8 m across.
    let mut widest: f64 = 0.0;
    let (mut measured, mut sum) = (0, 0.0);
    for tube in tubes_stepping(97) {
        for (end, radius) in [(tube.a, tube.ra), (tube.b, tube.rb)] {
            let (depth, elevation) = depth_and_elevation(end);
            if radius < 20.0 || elevation < 100.0 || !(1000.0..2500.0).contains(&depth) {
                continue;
            }
            let width = 2.0 * (air_ball_radius(end) - 0.5);
            widest = widest.max(width);
            sum += width;
            measured += 1;
        }
    }
    eprintln!(
        "{measured} chambers: widest {widest:.1} Voxels, mean {:.1}",
        sum / f64::from(measured)
    );
    assert!(measured > 30, "{measured}");
    assert!(widest <= 80.0, "{widest} Voxels across");
    assert!(
        widest >= 40.0,
        "{widest}: the chambers are not much of a cave"
    );
}

#[test]
fn no_cave_reaches_further_than_a_kilometre() {
    // The tubes of a system share the ends that they meet at, so a system is the tubes joined by
    // their ends. A cave is connected, so it is within one of these, or of those of a cube that
    // meet.
    let tubes = planet().caves.tubes();
    let key = |p: [f64; 3]| p.map(f64::to_bits);
    let mut system_of = std::collections::BTreeMap::new();
    let mut systems: Vec<Vec<[f64; 3]>> = Vec::new();
    for tube in tubes {
        let known = [tube.a, tube.b]
            .iter()
            .find_map(|&p| system_of.get(&key(p)).copied());
        let id = known.unwrap_or_else(|| {
            systems.push(Vec::new());
            systems.len() - 1
        });
        for end in [tube.a, tube.b] {
            system_of.insert(key(end), id);
            systems[id].push(end);
        }
    }
    let (mut longest, mut over_half) = (0.0_f64, 0);
    let mut reaches = Vec::new();
    for ends in &systems {
        let reach = ends
            .iter()
            .flat_map(|a| {
                ends.iter()
                    .map(move |b| distance(std::array::from_fn(|k| a[k] - b[k])))
            })
            .fold(0.0, f64::max);
        reaches.push(reach);
        longest = longest.max(reach);
        over_half += usize::from(reach > 5000.0);
    }
    reaches.sort_by(f64::total_cmp);
    eprintln!(
        "{} systems: longest reach {:.0} m, median {:.0} m, 90th percentile {:.0} m; \
         {over_half} reach over 500 m",
        systems.len(),
        longest / 10.0,
        reaches[reaches.len() / 2] / 10.0,
        reaches[reaches.len() * 9 / 10] / 10.0,
    );
    assert!(longest <= 2.0 * caves::SPAN, "{longest}");
    assert!(
        longest > 7_000.0,
        "the longest should come close to a kilometre: {longest}"
    );
    // Every end is within the ball of its cube, which is wholly inside the cube.
    for tube in tubes {
        let cube = tube.a.map(|c| (c / f64::from(caves::CELL)).floor());
        let middle = cube.map(|c| (c + 0.5) * f64::from(caves::CELL));
        for end in [tube.a, tube.b] {
            let off = distance(std::array::from_fn(|k| end[k] - middle[k]));
            assert!(off <= caves::SPAN - caves::MAX_TUBE_RADIUS + 1e-6, "{off}");
        }
    }
}

#[test]
fn a_brick_with_caves_is_what_its_voxels_are() {
    let mut compared = 0;
    for tube in tubes_stepping(41) {
        let coord = brick_of_point(on(tube, 0.5));
        if cave_voxels(coord).is_empty() {
            continue;
        }
        let brick = planet().brick(coord);
        for (local, voxel) in voxels_of(coord) {
            assert_eq!(
                brick.get(local),
                planet().voxel(voxel),
                "{coord:?} {local:?}"
            );
        }
        compared += 1;
    }
    assert!(compared > 20, "{compared}");
}

/// The bricks of level 0 under the brick at `coord` of mip `level`.
fn bricks_under(level: usize, coord: BrickCoord) -> Vec<BrickCoord> {
    let side = 1 << level;
    (0..side * side * side)
        .map(|i| {
            BrickCoord(std::array::from_fn(|a| {
                coord.0[a] * side + (i >> (level * a) & (side - 1))
            }))
        })
        .collect()
}

#[test]
fn solid_below_is_never_claimed_for_a_brick_with_a_cave_in_it() {
    // Bricks round the middles of tubes in the band, at levels 0 to 3: wherever the Planet says
    // that a brick is solid all the way down, every brick of level 0 under it is full (all of
    // them at levels 0 to 2, and every ninth at level 3, where there are 512).
    let (mut claimed, mut taken, mut with_caves) = (0, 0, 0);
    for tube in tubes_stepping(211) {
        let Some(at) = middle_in_the_band(tube) else {
            continue;
        };
        for level in 0..=3 {
            let centre = ancestor(brick_of_point(at), level);
            for (dx, dy, dz) in (0..27).map(|i| (i % 3 - 1, i / 3 % 3 - 1, i / 9 - 1)) {
                let [x, y, z] = centre.0;
                let coord = BrickCoord([x + dx, y + dy, z + dz]);
                let by_ground = planet().solid_below_with(level, coord, false);
                let claim = planet().solid_below(level, coord);
                assert!(!claim || by_ground, "the caves only take a claim away");
                taken += usize::from(by_ground && !claim);
                if !claim {
                    continue;
                }
                claimed += 1;
                let step = if level == 3 { 9 } else { 1 };
                for child in bricks_under(level, coord).into_iter().step_by(step) {
                    assert!(
                        planet().brick(child).is_full(),
                        "level {level} {coord:?} is solid below, but {child:?} is not full"
                    );
                    with_caves += usize::from(!cave_voxels(child).is_empty());
                }
            }
        }
    }
    eprintln!("{claimed} claims, {taken} taken away by the caves");
    assert!(claimed > 1_000 && taken > 100, "{claimed} {taken}");
    assert_eq!(with_caves, 0);
}

#[test]
fn solid_below_is_not_known_for_a_brick_that_a_cave_goes_through() {
    // The other way round: a brick of level 0 that has a cave Voxel does not say so, and at
    // every level the brick over it does not either.
    let mut checked = 0;
    for tube in tubes_stepping(19) {
        let coord = brick_of_point(on(tube, 0.5));
        if cave_voxels(coord).is_empty() {
            continue;
        }
        for level in 0..=8 {
            assert!(
                !planet().solid_below(level, ancestor(coord, level)),
                "level {level} {coord:?}"
            );
        }
        checked += 1;
    }
    assert!(checked > 100, "{checked}");
}

/// The bricks of mip `level` that hold ground with caves in it: those over the middles of every
/// `step`th tube that is in the band, at most `most`.
fn coarse_bricks_with_caves(level: usize, step: usize, most: usize) -> Vec<BrickCoord> {
    let mut coords: Vec<_> = tubes_stepping(step)
        .filter_map(middle_in_the_band)
        .map(|at| ancestor(brick_of_point(at), level))
        .collect();
    coords.sort();
    coords.dedup();
    coords.truncate(most);
    coords
}

/// Most Voxels of the coarse bricks with caves in them that may differ from the pyramid's
/// downsample of the Planet, as a fraction, at levels 1, 2, ...: the numbers measured (printed
/// by the test below) with a margin. A tube of 1 to 7 m is a few Voxels wide at level 1, and
/// less than one at level 4, where the pyramid drops it unless it is a chamber; the sample at
/// the middle of a Voxel and the majority of its children's differ at the walls.
const CAVE_TOLERANCE: [f64; 4] = [0.008, 0.030, 0.026, 0.025];

#[test]
fn a_coarse_brick_with_caves_is_the_planets_downsampled_but_for_a_few_voxels() {
    for level in 1..=4 {
        let most = [150, 100, 60, 20][level - 1];
        let (mut differ, mut total, mut cave_voxels_made, mut cave_voxels_real) = (0, 0, 0, 0);
        for coord in coarse_bricks_with_caves(level, 29, most) {
            let made = planet().coarse_brick(level, coord);
            let real = downsampled(level, coord);
            let ground = planet().evaluate_with(&Samples::new(level, coord), &[]);
            for local in locals() {
                // A Voxel is a cave if the ground there is solid and the brick's is not.
                cave_voxels_made +=
                    usize::from(ground.get(local).is_some() && made.get(local).is_none());
                cave_voxels_real +=
                    usize::from(ground.get(local).is_some() && real.get(local).is_none());
                differ += usize::from(made.get(local) != real.get(local));
                total += 1;
            }
        }
        let fraction = differ as f64 / total as f64;
        eprintln!(
            "level {level}: {differ} of {total} Voxels differ, {fraction:.4}; \
             caves are {cave_voxels_made} Voxels of ours and {cave_voxels_real} of the pyramid's"
        );
        assert!(total >= 512 * 10, "level {level}: {total}");
        assert!(
            fraction <= CAVE_TOLERANCE[level - 1],
            "level {level}: {fraction}"
        );
        // The caves are there at the levels where they are wide enough to be.
        if level <= 2 {
            assert!(cave_voxels_made > 100, "level {level}");
        }
    }
}

#[test]
fn a_coarse_voxel_is_a_cave_only_where_a_tube_holds_most_of_it() {
    // At level 7 and up (12.8 m Voxels and wider) no tube is wide enough: the caves are not in
    // the coarse bricks, and so are never in the Guard.
    for level in [7, 8, 10, 12] {
        for coord in coarse_bricks_with_caves(level, 61, 40) {
            assert_eq!(
                planet().coarse_brick(level, coord),
                planet().evaluate_with(&Samples::new(level, coord), &[]),
                "level {level} {coord:?}"
            );
        }
    }
}

#[test]
fn the_spawn_is_clear_of_the_caves_and_so_is_the_ring_around_it() {
    let spawn = planet().spawn(SPAWN_REACH);
    assert!(planet().clear_of_caves(spawn, SPAWN_REACH));
    // The bricks of the ring are the ground's own.
    for coord in region().sorted_coords() {
        let held = region().brick(coord).unwrap().content_hash();
        assert_eq!(held, ground_brick(coord).content_hash(), "{coord:?}");
    }
    // So are those down to the bottom of the caves, under the middle and the edge of the ring.
    let radius = distance(spawn);
    let up = spawn.map(|c| c / radius);
    let (ahead, _) = tangents(up);
    for across in [0.0, 60.0, f64::from(SPAWN_REACH)] {
        let mut depth = -40.0;
        while depth < caves::MAX_DEPTH + 120.0 {
            let at: [f64; 3] =
                std::array::from_fn(|a| spawn[a] - up[a] * depth + ahead[a] * across);
            let coord = brick_of_point(at);
            assert!(
                cave_voxels(coord).is_empty(),
                "a cave {depth} Voxels under the spawn"
            );
            depth += 8.0;
        }
    }
    // No cave opens to the surface within the ring's reach.
    for mouth in planet().cave_mouths() {
        let along: f64 = (0..3).map(|a| (mouth[a] - spawn[a]) * up[a]).sum();
        let across = distance(std::array::from_fn(|a| mouth[a] - spawn[a] - up[a] * along));
        assert!(
            across > f64::from(SPAWN_REACH),
            "a cave mouth is {across} Voxels from the spawn"
        );
    }
}

#[test]
fn the_spawn_moves_to_clear_ground_when_the_first_spot_is_by_a_cave() {
    // On the map of seed 23, the first gentle, temperate spot is within reach of a cave.
    let planet = Planet::new(WorldMap::generate(23, 128));
    let first = planet.spawn_where(100, |_| true);
    assert!(!planet.clear_of_caves(first, 100));
    let spawn = planet.spawn(100);
    assert!(spawn != first && planet.clear_of_caves(spawn, 100));
    // It is still land with some relief to it of at most 40 m, if not temperate.
    assert!(planet.map().elevation_at(spawn) >= 0.0);
    let (low, high) = planet.heights_near(spawn, 100);
    assert!(high - low <= GENTLE);
}
