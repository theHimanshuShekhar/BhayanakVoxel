//! How the places of the shot list were found: the seeded Planet's own ground, caves and map,
//! searched deterministically within 30 km of the Spawn (where the player starts, so the places
//! are the ones the dev reaches first). Test-only: the result is recorded in `shots.rs`.
//!
//! `cargo test --release -p client --bin client -- --ignored --nocapture record_the_look_dev_sites`
//! prints `shots.rs`; `the_recorded_places_are_the_ones_the_finder_picks` checks that it is
//! current, and `the_recorded_shots_are_where_they_say` that every camera is in the open, the caves and the
//! descent are underground, and the caves are roofed (all three are `#[ignore]`: they make the
//! 320 km world map, 25 s).

use brick_store::VOXELS_PER_METRE as VPM;
use planet_gen::Tube;
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::{Biome, Fields, Regions, Texel, WORLD_MAP_SIZE, WorldMap};

use super::{CavePalette, Hue, Key, Place, SHOTS, Shot, Stamp};

/// How far from the Spawn the search goes, in metres either way.
const HALF_M: f64 = 30_000.0;
/// The distance between the points the map is looked at on, in metres: half a texel.
const STEP_M: f64 = 120.0;

pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

pub fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

pub fn unit(a: [f64; 3]) -> [f64; 3] {
    a.map(|c| c / length(a))
}

pub fn plus(a: [f64; 3], b: [f64; 3], by: f64) -> [f64; 3] {
    std::array::from_fn(|i| a[i] + b[i] * by)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// The ground at the ray from the centre along `direction`.
fn ground(planet: &Planet, direction: [f64; 3]) -> [f64; 3] {
    let up = unit(direction);
    up.map(|u| u * (planet.sea_radius() + planet.ground_height(up)))
}

/// The point `metres` along the ground from `at` towards `way` (a direction along the ground),
/// on the ground.
fn along(planet: &Planet, at: [f64; 3], way: [f64; 3], metres: f64) -> [f64; 3] {
    ground(planet, plus(at, way, metres * VPM))
}

/// How much higher the ground is `metres` along `way` than at `at`, in metres.
fn rise(planet: &Planet, at: [f64; 3], way: [f64; 3], metres: f64) -> f64 {
    (length(along(planet, at, way, metres)) - length(at)) / VPM
}

/// The directions along the ground that are tried: eight, every 45 degrees from `ahead`.
fn headings(at: [f64; 3]) -> Vec<[f64; 3]> {
    let (ahead, aside) = tangents(unit(at));
    (0..8)
        .map(|k| {
            let angle = f64::from(k) * std::f64::consts::FRAC_PI_4;
            // The Planet's generator avoids sin and cos for determinism; a finder need not.
            plus(
                [0.0; 3],
                plus(ahead.map(|c| c * angle.cos()), aside, angle.sin()),
                1.0,
            )
        })
        .collect()
}

/// A point of the search: where, and what the map says there.
struct Spot {
    /// The unit vector from the centre.
    direction: [f64; 3],
    texel: Texel,
    fields: Fields,
}

impl Spot {
    fn metres(&self) -> f64 {
        self.fields.elevation / VPM
    }

    /// The ground at the spot: the detail and all, which costs more than the map's fields.
    fn ground(&self, planet: &Planet) -> [f64; 3] {
        ground(planet, self.direction)
    }
}

/// Every `stride`th texel of the whole map, in order: for the places that the Spawn's country does
/// not have (steep ground).
fn everywhere(planet: &Planet, stride: u32) -> Vec<Spot> {
    let map = planet.map();
    let mut spots = Vec::new();
    for face in 0..6 {
        for j in (0..map.size()).step_by(stride as usize) {
            for i in (0..map.size()).step_by(stride as usize) {
                let direction = map.texel_direction(face, i, j);
                spots.push(Spot {
                    direction,
                    texel: map.texel(face, i, j),
                    fields: map.fields_at(direction),
                });
            }
        }
    }
    spots
}

/// The lattice of the search: the points `STEP_M` apart within `HALF_M` of the Spawn along the
/// ground, in order.
fn lattice(planet: &Planet) -> Vec<Spot> {
    let spawn = planet.spawn(100);
    let (ahead, aside) = tangents(unit(spawn));
    let n = (HALF_M / STEP_M) as i32;
    let mut spots = Vec::new();
    for i in -n..=n {
        for j in -n..=n {
            let p = plus(
                plus(spawn, ahead, f64::from(i) * STEP_M * VPM),
                aside,
                f64::from(j) * STEP_M * VPM,
            );
            let direction = unit(p);
            spots.push(Spot {
                direction,
                texel: planet.map().sample(direction),
                fields: planet.map().fields_at(direction),
            });
        }
    }
    spots
}

fn voxel(p: [f64; 3]) -> [i32; 3] {
    p.map(|c| c.round() as i32)
}

fn key(position: [f64; 3], target: [f64; 3], roll: f64) -> Key {
    Key {
        position,
        target,
        roll,
    }
}

/// A camera `height` metres over the ground at `at`.
fn eye(planet: &Planet, at: [f64; 3], height: f64) -> [f64; 3] {
    let up = unit(at);
    up.map(|u| u * (length(ground(planet, at)) + height * VPM))
}

/// The point `metres` ahead of `from` along `way`, and `down` metres lower: where a camera looks.
fn gaze(from: [f64; 3], way: [f64; 3], metres: f64, down: f64) -> [f64; 3] {
    plus(plus(from, way, metres * VPM), unit(from), -down * VPM)
}

/// The least of the ground's rises along the ways, over `metres`: the way that has the least
/// ground in the way of a low camera.
fn most_open(planet: &Planet, at: [f64; 3], metres: f64) -> [f64; 3] {
    let ways = headings(at);
    *ways
        .iter()
        .min_by(|a, b| {
            let worst = |w: &[f64; 3]| {
                (1..=4)
                    .map(|k| rise(planet, at, *w, metres * f64::from(k) / 4.0))
                    .fold(f64::MIN, f64::max)
            };
            worst(a).total_cmp(&worst(b))
        })
        .expect("eight ways")
}

/// The ground the ways fall to: the way along which the ground's mean height over `from`..`to`
/// metres is lowest.
fn falling_way(planet: &Planet, at: [f64; 3], from: f64, to: f64) -> [f64; 3] {
    let mean = |w: &[f64; 3]| {
        (0..8)
            .map(|k| rise(planet, at, *w, from + (to - from) * f64::from(k) / 7.0))
            .sum::<f64>()
    };
    *headings(at)
        .iter()
        .min_by(|a, b| mean(a).total_cmp(&mean(b)))
        .expect("eight ways")
}

/// The place for a stamp where the ray from `start` along `direction` first meets a solid Voxel,
/// within `reach` metres: the last Voxel of air before it, and the way the face it touches
/// points (an axis, back towards the ray). `None` if the ray meets no solid, or `start` is in
/// solid.
fn surface_place(
    planet: &Planet,
    stamp: Stamp,
    start: [f64; 3],
    direction: [f64; 3],
    reach: f64,
) -> Option<Place> {
    let solid = |v: [i32; 3]| planet.voxel(brick_store::VoxelCoord(v)).is_some();
    let steps = (reach * VPM) as i32;
    let mut air = voxel(start);
    if solid(air) {
        return None;
    }
    for k in 1..=steps {
        let here = voxel(plus(start, direction, f64::from(k)));
        if solid(here) {
            // The face of `air` that touches solid, most against the ray.
            let (axis, sign) = (0..3)
                .flat_map(|a| [(a, 1i32), (a, -1i32)])
                .filter(|&(a, sign)| {
                    let mut n = air;
                    n[a] -= sign;
                    solid(n)
                })
                .max_by(|&(a, sa), &(b, sb)| {
                    (-direction[a] * f64::from(sa)).total_cmp(&(-direction[b] * f64::from(sb)))
                })?;
            let mut face = [0i8; 3];
            face[axis] = sign as i8;
            return Some(Place {
                stamp,
                at: air,
                face,
            });
        }
        air = here;
    }
    None
}

/// Places for stamps on the ground at offsets `(ahead, aside)` metres from `at` in the frame of
/// `way`: the Voxel of air on the ground, the ground facing up.
fn on_ground(
    planet: &Planet,
    at: [f64; 3],
    way: [f64; 3],
    offsets: &[(Stamp, f64, f64)],
) -> Vec<Place> {
    let side = cross(unit(at), way);
    offsets
        .iter()
        .filter_map(|&(stamp, ahead, aside)| {
            let p = ground(planet, plus(plus(at, way, ahead * VPM), side, aside * VPM));
            let up = unit(p);
            surface_place(planet, stamp, plus(p, up, 2.0 * VPM), up.map(|c| -c), 4.0)
        })
        .collect()
}

/// What a shot is made of before it is a `Shot`: the pieces that `Shot` holds as statics.
pub struct Found {
    pub name: &'static str,
    pub subject: &'static str,
    pub keys: Vec<Key>,
    pub frames: u32,
    pub places: Vec<Place>,
    pub cave: Option<CavePalette>,
}

// ---------------------------------------------------------------------------------------------
// The shots.

/// Gentle temperate land with a valley's moisture, 30 to 150 m up, whose ground is level within
/// 6 m over 25 m: forest country.
fn forest_floor(planet: &Planet, spots: &[Spot]) -> (Found, [f64; 3]) {
    let best = spots
        .iter()
        .filter(|s| {
            s.texel.biome == Biome::Temperate
                && (30.0..150.0).contains(&s.metres())
                && (0.3..0.65).contains(&s.fields.rough)
                && s.fields.drainage > 0.12
        })
        .filter(|s| {
            headings(s.ground(planet))
                .iter()
                .all(|w| rise(planet, s.ground(planet), *w, 25.0).abs() < 6.0)
        })
        .max_by(|a, b| a.fields.drainage.total_cmp(&b.fields.drainage))
        .expect("temperate valley country near the Spawn");
    let at = best.ground(planet);
    let way = most_open(planet, at, 20.0);
    let eye0 = eye(planet, at, 1.6);
    let stamps = on_ground(
        planet,
        at,
        way,
        &[
            (Stamp::Tree, 7.0, -4.0),
            (Stamp::Tree, 11.0, 5.0),
            (Stamp::Tree, 16.0, -7.0),
            (Stamp::Tree, 4.0, 8.0),
            (Stamp::Tree, 14.0, 0.5),
            (Stamp::Fern, 3.0, -1.5),
            (Stamp::Fern, 5.5, 2.0),
            (Stamp::Fern, 9.0, -2.0),
            (Stamp::Fern, 12.5, 3.0),
            (Stamp::Flowers, 2.0, 1.0),
            (Stamp::Flowers, 6.0, -1.0),
            (Stamp::Moss, 8.0, 1.0),
        ],
    );
    let forward = most_open(planet, at, 20.0);
    let end = along(planet, at, forward, 6.0);
    (
        Found {
            name: "forest floor",
            subject: "dappled forest floor at 5 cm: how small a Voxel reads, ferns and leaf litter, soft sun through a canopy",
            keys: vec![
                key(eye0, gaze(eye0, way, 20.0, 1.0), 0.0),
                key(
                    eye(planet, end, 1.6),
                    gaze(eye(planet, end, 1.6), way, 20.0, 1.0),
                    0.0,
                ),
            ],
            frames: 90,
            places: stamps,
            cave: None,
        },
        at,
    )
}

/// How far to each side of the floor the walls are measured, in metres.
const CANYON_M: f64 = 60.0;

/// The ground along 4 axes, `CANYON_M` to each side: how walled in `p` is across `axis`, in metres.
fn enclosure(planet: &Planet, p: [f64; 3]) -> (f64, [f64; 3]) {
    let mut best = (f64::MIN, [0.0; 3]);
    for axis in headings(p).into_iter().take(4) {
        let side = cross(unit(p), axis);
        let left = rise(planet, p, side, CANYON_M);
        let right = rise(planet, p, side, -CANYON_M);
        let slope_along = rise(planet, p, axis, CANYON_M)
            .abs()
            .max(rise(planet, p, axis, -CANYON_M).abs());
        let score = left.min(right) - slope_along * 0.5;
        if score > best.0 {
            best = (score, axis);
        }
    }
    best
}

/// A valley that is walled in: of the lattices about the most drained and roughest high ground
/// of the map, the floor with walls `CANYON_M` either side that rise most over it, and level
/// along the way down. The generator makes no canyon in metres (see `canyon`'s print): its
/// deepest valley at this width is the shot.
fn canyon(planet: &Planet, spots: &[Spot]) -> Found {
    let mut candidates: Vec<&Spot> = spots
        .iter()
        .filter(|s| {
            s.texel.biome != Biome::Snow
                && s.metres() > 100.0
                && s.fields.rough > 0.4
                && s.fields.drainage > 0.05
        })
        .collect();
    candidates.sort_by(|a, b| {
        (b.fields.drainage + b.fields.rough).total_cmp(&(a.fields.drainage + a.fields.rough))
    });
    let (mut best, mut best_at) = (f64::MIN, ground_zero());
    for s in candidates.iter().take(3000) {
        // The floor is somewhere on a lattice 24 m apart over 240 m.
        let (ahead, aside) = tangents(unit(s.ground(planet)));
        for i in -5..=5 {
            for j in -5..=5 {
                let p = ground(
                    planet,
                    plus(
                        plus(s.ground(planet), ahead, f64::from(i) * 24.0 * VPM),
                        aside,
                        f64::from(j) * 24.0 * VPM,
                    ),
                );
                let (score, _) = enclosure(planet, p);
                if score > best {
                    (best, best_at) = (score, p);
                }
            }
        }
    }
    eprintln!("canyon: walls {best:.1} m above the floor at {CANYON_M} m");
    let (_, axis) = enclosure(planet, best_at);
    // Down the canyon, the way it falls.
    let way = if rise(planet, best_at, axis, 60.0) < 0.0 {
        axis
    } else {
        axis.map(|c| -c)
    };
    let start = eye(planet, plus(best_at, way, -20.0 * VPM), 2.0);
    let finish = eye(planet, along(planet, best_at, way, 25.0), 2.0);
    Found {
        name: "canyon with haze",
        subject: "a gully with walls of slope and strata: aerial perspective down a long view, the sky seen as a strip, haze in the valley",
        keys: vec![
            key(start, gaze(start, way, 80.0, 4.0), 0.0),
            key(finish, gaze(finish, way, 80.0, 4.0), 0.10),
        ],
        frames: 120,
        places: Vec::new(),
        cave: None,
    }
}

fn ground_zero() -> [f64; 3] {
    [0.0; 3]
}

/// How tall the face is that falls away from `at` along `way`: the drop, in metres, of the run of
/// 1 m steps that fall at least 0.6 m each (a gap of two gentler steps ends it), and where it
/// starts and ends.
fn face(planet: &Planet, at: [f64; 3], way: [f64; 3]) -> (f64, [f64; 3], [f64; 3]) {
    let step = |from: [f64; 3], sign: f64| along(planet, from, way, sign);
    let drop_of = |from: [f64; 3], to: [f64; 3]| (length(from) - length(to)) / VPM;
    // The top: back up the face while it is steep.
    let mut top = at;
    let mut gentle = 0;
    for _ in 0..80 {
        let up_a = step(top, -1.0);
        if drop_of(up_a, top) >= 0.6 {
            top = up_a;
            gentle = 0;
        } else {
            gentle += 1;
            if gentle >= 2 {
                break;
            }
            top = up_a;
        }
    }
    let mut foot = at;
    gentle = 0;
    for _ in 0..80 {
        let down = step(foot, 1.0);
        if drop_of(foot, down) >= 0.6 {
            foot = down;
            gentle = 0;
        } else {
            gentle += 1;
            if gentle >= 2 {
                break;
            }
            foot = down;
        }
    }
    (drop_of(top, foot), top, foot)
}

/// The steep points of the roughest high ground that is not snow: a fall of over 3 m in 5 m, on
/// a lattice 4 m apart about each of the 1500 roughest spots of the map. With the fall in metres,
/// and the way it falls, steepest first.
fn steep_points(planet: &Planet, spots: &[Spot]) -> Vec<(f64, [f64; 3], [f64; 3])> {
    let drop = |p: [f64; 3]| -> (f64, [f64; 3]) {
        headings(p)
            .into_iter()
            .map(|w| (-rise(planet, p, w, 5.0), w))
            .fold((f64::MIN, [0.0; 3]), |a, b| if b.0 > a.0 { b } else { a })
    };
    let mut candidates: Vec<&Spot> = spots
        .iter()
        .filter(|s| s.metres() > 80.0 && s.fields.rough > 0.6 && s.texel.biome != Biome::Snow)
        .collect();
    candidates.sort_by(|a, b| b.fields.rough.total_cmp(&a.fields.rough));
    let mut steep: Vec<(f64, [f64; 3], [f64; 3])> = Vec::new();
    for s in candidates.iter().take(1500) {
        let (ahead, aside) = tangents(unit(s.ground(planet)));
        for i in -10..=10 {
            for j in -10..=10 {
                let p = ground(
                    planet,
                    plus(
                        plus(s.ground(planet), ahead, f64::from(i) * 4.0 * VPM),
                        aside,
                        f64::from(j) * 4.0 * VPM,
                    ),
                );
                let (d, w) = drop(p);
                if d > 3.0 {
                    steep.push((d, p, w));
                }
            }
        }
    }
    steep.sort_by(|a, b| b.0.total_cmp(&a.0));
    steep
}

/// The tallest steep face: of the steepest points (see `steep_points`), the one whose face (see
/// `face`) is tallest.
fn cliff(planet: &Planet, steep: &[(f64, [f64; 3], [f64; 3])]) -> Found {
    let (height, top, foot, way) = steep
        .iter()
        .take(60)
        .map(|&(_, p, w)| {
            let (h, top, foot) = face(planet, p, w);
            (h, top, foot, w)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .expect("steep ground");
    eprintln!("cliff: a face {height:.1} m tall");
    // Stand back by its height and a little, at eye height, looking at the middle of the face.
    let back = way.map(|c| -c);
    let stand = (1.1 * height + 6.0).max(12.0);
    let middle = plus(plus(top, foot, 1.0), [0.0; 3], 0.0).map(|c| c / 2.0);
    let from = eye(planet, along(planet, foot, way, stand), 2.0);
    let nearer = eye(planet, along(planet, foot, way, 0.7 * stand), 2.0);
    Found {
        name: "sunlit cliff",
        subject: "a rock face in the low sun: slope shading, smooth against face normals, strata colour, a long cast shadow",
        keys: vec![key(from, middle, 0.0), key(nearer, middle, 0.0)],
        frames: 90,
        places: on_ground(
            planet,
            top,
            back,
            &[
                (Stamp::Moss, 0.0, 0.0),
                (Stamp::Moss, 0.5, 2.0),
                (Stamp::Moss, -0.5, -2.0),
                (Stamp::Fern, 3.0, -2.0),
                (Stamp::Fern, 4.0, 1.0),
            ],
        )
        .into_iter()
        .chain(on_ground(
            planet,
            foot,
            way,
            &[
                (Stamp::Fern, 2.0, -1.5),
                (Stamp::Fern, 3.5, 2.0),
                (Stamp::Moss, 1.0, 0.0),
            ],
        ))
        .collect(),
        cave: None,
    }
}

/// Open, gentle, low land: the ground within 60 m is level within 3 m.
fn meadow(planet: &Planet, spots: &[Spot], not_near: [f64; 3]) -> (Found, [f64; 3]) {
    let flatness = |s: &Spot| {
        headings(s.ground(planet))
            .iter()
            .flat_map(|w| [20.0, 40.0, 60.0].map(|m| rise(planet, s.ground(planet), *w, m).abs()))
            .fold(0.0, f64::max)
    };
    let best = spots
        .iter()
        .filter(|s| {
            s.texel.biome == Biome::Temperate
                && (10.0..60.0).contains(&s.metres())
                && s.fields.rough < 0.4
                && s.fields.drainage < 0.1
                && length(plus(s.ground(planet), not_near, -1.0)) > 1000.0 * VPM
        })
        .min_by(|a, b| flatness(a).total_cmp(&flatness(b)))
        .expect("a meadow near the Spawn");
    let at = best.ground(planet);
    let way = falling_way(planet, at, 100.0, 500.0);
    let from = eye(planet, at, 1.7);
    let end = along(planet, at, way, 8.0);
    let stamps = on_ground(
        planet,
        at,
        way,
        &[
            (Stamp::Flowers, 3.0, -2.0),
            (Stamp::Flowers, 5.0, 3.0),
            (Stamp::Flowers, 8.0, -4.0),
            (Stamp::Flowers, 10.0, 1.0),
            (Stamp::Flowers, 14.0, 5.0),
            (Stamp::Moss, 2.0, 1.5),
            (Stamp::Moss, 4.0, -1.0),
            (Stamp::Moss, 6.5, 0.5),
            (Stamp::Moss, 9.0, -2.5),
            (Stamp::Moss, 12.0, 2.5),
            (Stamp::Tree, 30.0, -12.0),
            (Stamp::Tree, 36.0, 10.0),
        ],
    );
    (
        Found {
            name: "meadow",
            subject: "open grass under the hero light: flower carpets, long shadows, the sky's gradient to a peach horizon",
            keys: vec![
                key(from, gaze(from, way, 40.0, 0.5), 0.0),
                key(
                    eye(planet, end, 1.7),
                    gaze(eye(planet, end, 1.7), way, 40.0, 0.5),
                    0.0,
                ),
            ],
            frames: 90,
            places: stamps,
            cave: None,
        },
        at,
    )
}

/// The pull-out: straight up from the meadow and out, from the ground to 640 km, keeping the
/// meadow in view and then the Planet.
fn pull_out(at: [f64; 3], way: [f64; 3]) -> Found {
    let up = unit(at);
    let side = cross(up, way);
    let radius = length(at);
    // Altitudes in metres, and how far it has backed off along the ground and aside by then, as a
    // share of the altitude: the camera leaves the meadow on a slant, and so is never exactly over
    // the Planet's centre, where "up" has no heading.
    let steps = [
        (1.7, 0.0),
        (60.0, 0.5),
        (1_500.0, 0.5),
        (30_000.0, 0.45),
        (640_000.0, 0.3),
    ];
    let keys = steps
        .iter()
        .map(|&(height, slant)| {
            let position = plus(
                plus(
                    up.map(|c| c * (radius + height * VPM)),
                    way,
                    -height * slant * VPM,
                ),
                side,
                height * slant * 0.5 * VPM,
            );
            // From the meadow's horizon at first, to the Planet's centre at the last.
            let share = (height / 640_000.0_f64).sqrt().min(1.0);
            let ahead = plus(position, way, 200.0 * VPM);
            let look = std::array::from_fn(|a| ahead[a] * (1.0 - share) + share * 0.0);
            key(position, look, 0.0)
        })
        .collect();
    Found {
        name: "pull-out to the Planet",
        subject: "the Zoom's far end: aerial perspective at every scale, the atmosphere's edge, the Planet as a disc",
        keys,
        frames: 181,
        places: Vec::new(),
        cave: None,
    }
}

// Caves ---------------------------------------------------------------------------------------

/// A way into the ground that a tube makes, as `Planet::cave_mouths` finds one: the first point
/// along it within four fifths of its radius of the ground from below.
struct Mouth {
    tube: Tube,
    at: [f64; 3],
    /// The way into the ground, along the tube.
    inward: [f64; 3],
    /// How much lower (metres, in height above the centre) the tube's far end is than the mouth,
    /// and its length from there.
    descent: f64,
    run: f64,
}

fn tube_mouth(planet: &Planet, tube: Tube) -> Option<Mouth> {
    let span: [f64; 3] = std::array::from_fn(|a| tube.b[a] - tube.a[a]);
    let steps = (length(span) / (2.0 * VPM)).ceil().max(1.0) as u32;
    let at_t = |t: f64| plus(tube.a, span, t);
    let depth_at = |t: f64| {
        let p = plus(tube.a, span, t);
        let r = length(p);
        let ground = planet.ground_height(p);
        (planet.sea_radius() + ground - r, ground)
    };
    for k in 0..=steps {
        let t = f64::from(k) / f64::from(steps);
        let radius = tube.ra + (tube.rb - tube.ra) * t;
        let (depth, ground) = depth_at(t);
        if ground >= 0.0 && (0.0..=0.8 * radius).contains(&depth) {
            // Going into the ground is the way along the tube that gets deeper.
            let (end_depth, _) = depth_at(1.0);
            let (start_depth, _) = depth_at(0.0);
            let forward = end_depth > start_depth;
            let dir = unit(if forward { span } else { span.map(|c| -c) });
            let end_t = if forward { 1.0 } else { 0.0 };
            let run = length(plus(plus(tube.a, span, end_t), plus(tube.a, span, t), -1.0));
            return Some(Mouth {
                tube,
                at: plus(tube.a, span, t),
                inward: dir,
                descent: (length(at_t(t)) - length(at_t(end_t))) / VPM,
                run: run / VPM,
            });
        }
    }
    None
}

fn tubes_near(planet: &Planet, at: [f64; 3]) -> Vec<Tube> {
    let reach = 600.0 * VPM;
    planet.cave_tubes(at.map(|c| c - reach), at.map(|c| c + reach))
}

/// A mouth that leads down: the one whose tube drops most over a run of 40 m or more, wide
/// enough to stand in. Searched near the ground of the spots with the roughest map.
fn descent(planet: &Planet, spots: &[Spot]) -> Found {
    let mut best: Option<(f64, Mouth)> = None;
    let mut seen = std::collections::HashSet::new();
    for s in spots
        .iter()
        .filter(|s| s.metres() > 40.0 && s.texel.biome != Biome::Snow)
        .step_by(97)
        .take(60)
    {
        for tube in tubes_near(planet, s.ground(planet)) {
            if !seen.insert(tube.a.map(|c| c as i64)) {
                continue;
            }
            let Some(mouth) = tube_mouth(planet, tube) else {
                continue;
            };
            let radius = mouth.tube.ra.min(mouth.tube.rb) / VPM;
            // A slope that a camera can follow into the dark: 20 to 40 degrees below the
            // horizon (caves are carved against the world's axes, so on a tilted part of the
            // Planet they run steeper one way than the other).
            let angle = (mouth.descent / mouth.run).asin().to_degrees();
            if mouth.run >= 50.0 && radius >= 1.4 && (20.0..=40.0).contains(&angle) {
                let score = mouth.descent + mouth.tube.ra.min(mouth.tube.rb) / VPM;
                if best.as_ref().is_none_or(|(b, _)| score > *b) {
                    best = Some((score, mouth));
                }
            }
        }
    }
    let (score, mouth) = best.expect("a cave that leads down near the Spawn");
    eprintln!(
        "descent: drops {:.1} m over a run of {:.0} m (score {score:.1})",
        mouth.descent, mouth.run
    );
    let d = mouth.inward;
    let at = |metres: f64| plus(mouth.at, d, metres * VPM);
    let look = |p: [f64; 3]| plus(p, d, 20.0 * VPM);
    let (a, b, c) = (at(-8.0), at(6.0), at(mouth.run.min(34.0)));
    Found {
        name: "dark descent",
        subject: "daylight to dark: the auto-exposure's adaptation rate, the tone curve's toe, how much a cave shows with no light in it",
        keys: vec![
            key(a, look(a), 0.0),
            key(b, look(b), 0.0),
            key(c, look(c), 0.0),
        ],
        frames: 181,
        places: Vec::new(),
        cave: None,
    }
}

/// A chamber (a tube end 2.4 m wide or more) that is deep and closed: its ground at least 25 m
/// over it, the tube a good length.
struct Chamber {
    centre: [f64; 3],
    /// Along the tube, away from the chamber.
    ahead: [f64; 3],
    radius: f64,
    depth: f64,
}

fn chambers(planet: &Planet, near: [f64; 3]) -> Vec<Chamber> {
    let mut found = Vec::new();
    for tube in tubes_near(planet, near) {
        let span: [f64; 3] = std::array::from_fn(|a| tube.b[a] - tube.a[a]);
        if length(span) < 60.0 * VPM {
            continue;
        }
        for (centre, radius, ahead) in [
            (tube.a, tube.ra, unit(span)),
            (tube.b, tube.rb, unit(span.map(|c| -c))),
        ] {
            let depth = (planet.sea_radius() + planet.ground_height(centre) - length(centre)) / VPM;
            if radius >= 2.4 * VPM && depth >= 25.0 && planet.ground_height(centre) > 0.0 {
                found.push(Chamber {
                    centre,
                    ahead,
                    radius: radius / VPM,
                    depth,
                });
            }
        }
    }
    found
}

/// Places for crystals in the chamber that the camera is in: clusters of the dominant hue on the
/// walls and ceiling, beds of the second hue on the floor, the accent hue at a few edges. Each is
/// found by a ray from the tube's axis towards where it is to grow.
fn crystals(planet: &Planet, c: &Chamber, palette: CavePalette) -> Vec<Place> {
    let up = unit(c.centre);
    let side = unit(cross(up, c.ahead));
    // (how far along the tube in metres, which way from the axis (up, side), which hue)
    let layout: [(f64, f64, f64, Stamp); 11] = [
        (1.0, 0.0, 1.0, Stamp::Crystal(palette.dominant)),
        (3.5, 0.7, 0.7, Stamp::Crystal(palette.dominant)),
        (5.0, -0.9, 0.4, Stamp::Crystal(palette.dominant)),
        (7.5, 0.95, -0.2, Stamp::Crystal(palette.dominant)),
        (9.0, -0.6, 0.8, Stamp::Crystal(palette.dominant)),
        (2.0, 0.0, -1.0, Stamp::Crystal(palette.bed)),
        (4.0, 0.4, -0.9, Stamp::Crystal(palette.bed)),
        (6.0, -0.5, -0.85, Stamp::Crystal(palette.bed)),
        (8.0, 0.2, -1.0, Stamp::Crystal(palette.bed)),
        (3.0, -1.0, -0.1, Stamp::Crystal(palette.accent)),
        (6.5, 1.0, 0.1, Stamp::Crystal(palette.accent)),
    ];
    layout
        .iter()
        .filter_map(|&(metres, across, vertical, stamp)| {
            let normal = unit(plus(plus([0.0; 3], side, across), up, vertical));
            // Where the tube is a junction or the wall is far, try a little further along.
            [0.0, 0.8, -0.8, 1.6].into_iter().find_map(|shift| {
                let from = plus(c.centre, c.ahead, (metres + shift) * VPM);
                surface_place(planet, stamp, from, normal, c.radius + 1.0)
            })
        })
        .collect()
}

/// Two crystal caves, in the map's crystal regions, a kilometre or more apart: the chambers
/// with the most room (radius, then depth).
fn crystal_caves(planet: &Planet, spots: &[Spot]) -> [Found; 2] {
    let mut picks: Vec<Chamber> = Vec::new();
    let regions: Vec<&Spot> = spots
        .iter()
        .filter(|s| {
            s.texel.regions.contains(Regions::CRYSTAL_CAVES)
                && s.metres() > 40.0
                && s.texel.biome != Biome::Snow
        })
        .collect();
    let mut all = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in regions.iter().step_by((regions.len() / 80).max(1)).take(80) {
        for chamber in chambers(planet, s.ground(planet)) {
            if seen.insert(chamber.centre.map(|c| c as i64)) {
                all.push(chamber);
            }
        }
    }
    all.sort_by(|a, b| {
        (b.radius, b.depth)
            .partial_cmp(&(a.radius, a.depth))
            .expect("finite")
    });
    for chamber in all {
        if picks.len() == 2 {
            break;
        }
        if picks
            .iter()
            .all(|p| length(plus(p.centre, chamber.centre, -1.0)) > 2_000.0 * VPM)
        {
            picks.push(chamber);
        }
    }
    assert_eq!(
        picks.len(),
        2,
        "two crystal chambers 2 km apart near the Spawn"
    );
    let palettes = [
        CavePalette {
            dominant: Hue::Cyan,
            accent: Hue::White,
            bed: Hue::Violet,
        },
        CavePalette {
            dominant: Hue::Magenta,
            accent: Hue::Red,
            bed: Hue::Amber,
        },
    ];
    let names = ["crystal cave, cyan", "crystal cave, magenta"];
    let mut out = Vec::new();
    for (i, chamber) in picks.iter().enumerate() {
        eprintln!(
            "{}: chamber radius {:.1} m, {:.0} m under the ground",
            names[i], chamber.radius, chamber.depth
        );
        let up = unit(chamber.centre);
        // The camera stands on the floor-ish: a little under the axis, looking along the tube.
        let from = plus(chamber.centre, up, -0.3 * chamber.radius * VPM);
        let to = plus(from, chamber.ahead, 10.0 * VPM);
        out.push(Found {
            name: names[i],
            subject: "a crystal cave lit by its own emissives and nothing else: does the crystal read neon here and pastel by day, bloom, the dark between",
            keys: vec![
                key(from, plus(from, chamber.ahead, 20.0 * VPM), 0.0),
                key(to, plus(to, chamber.ahead, 20.0 * VPM), 0.0),
            ],
            frames: 120,
            places: crystals(planet, chamber, palettes[i]),
            cave: Some(palettes[i]),
        });
    }
    let [a, b]: [Found; 2] = out.try_into().ok().expect("two caves");
    [a, b]
}

// ---------------------------------------------------------------------------------------------

/// The shots, in the order of the flythrough.
pub fn find(planet: &Planet) -> Vec<Found> {
    let spots = lattice(planet);
    let wide = everywhere(planet, 4);
    let (forest, forest_at) = forest_floor(planet, &spots);
    let steep = steep_points(planet, &wide);
    let canyon = canyon(planet, &wide);
    let descent = descent(planet, &spots);
    let [cave_a, cave_b] = crystal_caves(planet, &spots);
    let cliff = cliff(planet, &steep);
    let (meadow, meadow_at) = meadow(planet, &spots, forest_at);
    let way = unit(plus(meadow.keys[0].target, meadow.keys[0].position, -1.0));
    let way = unit(plus(way, unit(meadow_at), -dot(way, unit(meadow_at))));
    let pull = pull_out(meadow_at, way);
    vec![forest, canyon, descent, cave_a, cave_b, cliff, meadow, pull]
}

fn f(v: f64) -> String {
    format!("{v:?}")
}

fn v3(p: [f64; 3]) -> String {
    format!("[{}, {}, {}]", f(p[0]), f(p[1]), f(p[2]))
}

fn hue(h: Hue) -> String {
    format!("Hue::{h:?}")
}

fn stamp(s: Stamp) -> String {
    match s {
        Stamp::Crystal(h) => format!("Stamp::Crystal({})", hue(h)),
        other => format!("Stamp::{other:?}"),
    }
}

/// The source of `shots.rs` for `found`.
pub fn source(planet: &Planet, found: &[Found]) -> String {
    let mut out = String::new();
    out.push_str("//! The recorded places of the shot list. Written by\n//! `finder::record_the_look_dev_sites`; do not edit by hand.\n\n");
    out.push_str("use super::{CavePalette, Hue, Key, Place, Shot, Stamp};\n\n");
    out.push_str(&format!(
        "/// The `GENERATOR_VERSION` the places were found on.\npub const RECORDED_FOR: u32 = {};\n\n",
        planet_gen::GENERATOR_VERSION
    ));
    let _ = planet;
    out.push_str(&format!(
        "/// The seed and radius (Voxels) of the Planet they are on, which a test holds to the client's own.\npub const RECORDED_SEED: u64 = {:#X};\npub const RECORDED_RADIUS: i32 = {};\n\n",
        crate::SEED,
        planet_gen::world_map::WORLD_RADIUS
    ));
    out.push_str("pub const SHOTS: [Shot; ");
    out.push_str(&format!("{}] = [\n", found.len()));
    for s in found {
        out.push_str("    Shot {\n");
        out.push_str(&format!(
            "        name: {:?},\n        subject: {:?},\n",
            s.name, s.subject
        ));
        out.push_str("        keys: &[\n");
        for k in &s.keys {
            out.push_str(&format!(
                "            Key {{ position: {}, target: {}, roll: {} }},\n",
                v3(k.position),
                v3(k.target),
                f(k.roll)
            ));
        }
        out.push_str("        ],\n");
        out.push_str(&format!("        frames: {},\n", s.frames));
        out.push_str("        places: &[\n");
        for p in &s.places {
            out.push_str(&format!(
                "            Place {{ stamp: {}, at: {:?}, face: {:?} }},\n",
                stamp(p.stamp),
                p.at,
                p.face
            ));
        }
        out.push_str("        ],\n");
        match s.cave {
            Some(c) => out.push_str(&format!(
                "        cave: Some(CavePalette {{ dominant: {}, accent: {}, bed: {} }}),\n",
                hue(c.dominant),
                hue(c.accent),
                hue(c.bed)
            )),
            None => out.push_str("        cave: None,\n"),
        }
        out.push_str("    },\n");
    }
    out.push_str("];\n");
    out
}

pub fn full_planet() -> Planet {
    Planet::new(WorldMap::generate(crate::SEED, WORLD_MAP_SIZE))
}

#[test]
#[ignore = "makes the 320 km world map: 25 s"]
fn record_the_look_dev_sites() {
    let planet = full_planet();
    let found = find(&planet);
    let text = source(&planet, &found);
    match std::env::var_os("RECORD_TO") {
        Some(path) => std::fs::write(path, text).expect("write the recorded shots"),
        None => println!("{text}"),
    }
}

/// Whether the finder's `found` is the recorded `shot`, to the bit: the Planet is deterministic.
fn same(found: &Found, shot: &Shot) -> bool {
    found.name == shot.name
        && found.subject == shot.subject
        && found.frames == shot.frames
        && found.keys == shot.keys
        && found.places == shot.places
        && found.cave == shot.cave
}

#[test]
#[ignore = "makes the 320 km world map and searches it: a minute"]
fn the_recorded_places_are_the_ones_the_finder_picks() {
    let found = find(&full_planet());
    assert_eq!(found.len(), SHOTS.len());
    for (found, shot) in found.iter().zip(SHOTS) {
        assert!(
            same(found, &shot),
            "{} is not what the finder picks: run `record_the_look_dev_sites` with RECORD_TO set \
             to the path of shots.rs, and cargo fmt",
            shot.name
        );
    }
}

#[test]
#[ignore = "makes the 320 km world map: 25 s"]
fn the_recorded_shots_are_where_they_say() {
    let planet = full_planet();
    let solid = |v: [i32; 3]| planet.voxel(brick_store::VoxelCoord(v)).is_some();
    for shot in SHOTS {
        // The camera is in the open all along its path.
        let path = shot.path();
        for frame in 0..shot.frames {
            let eye = path.camera_at(frame, shot.frames).position;
            assert!(
                !solid(voxel(eye)),
                "{}: the camera is in rock at frame {frame}",
                shot.name
            );
        }
        // Every place for a stamp is a Voxel of air against a solid Voxel.
        for place in shot.places {
            let against: [i32; 3] = std::array::from_fn(|a| place.at[a] - i32::from(place.face[a]));
            assert!(!solid(place.at), "{}: {place:?} is in rock", shot.name);
            assert!(
                solid(against),
                "{}: {place:?} touches no surface",
                shot.name
            );
        }
        // A cave is under the ground all along its path, with rock overhead where it is recorded.
        if shot.cave.is_some() {
            for key in shot.keys {
                let depth = depth_under_ground(&planet, key.position);
                assert!(
                    depth > 20.0 * VPM,
                    "{}: only {} m under at {:?}",
                    shot.name,
                    depth / VPM,
                    key.position
                );
                assert!(
                    rock_overhead(&solid, key.position),
                    "{}: open to the sky straight up from {:?}",
                    shot.name,
                    key.position
                );
            }
        }
        // The dark descent ends underground, in the dark: 15 m or more under (it records 18.8 m), with
        // rock overhead.
        if shot.name == "dark descent" {
            let end = shot.keys.last().expect("keyframes").position;
            let depth = depth_under_ground(&planet, end);
            assert!(
                depth > 15.0 * VPM,
                "the descent ends only {} m under",
                depth / VPM
            );
            assert!(
                rock_overhead(&solid, end),
                "the descent ends open to the sky"
            );
        }
    }
}

/// How far under the ground (Voxels) `eye` is: negative above it.
fn depth_under_ground(planet: &Planet, eye: [f64; 3]) -> f64 {
    planet.sea_radius() + planet.ground_height(eye) - length(eye)
}

/// Whether there is rock somewhere straight up (away from the Planet's centre) from `eye`, within
/// 400 m, looking every 25 cm: the cheap check that there is no sky overhead. It does not say
/// there is none from the side (a cave's mouth), only that the chamber is roofed.
fn rock_overhead(solid: &impl Fn([i32; 3]) -> bool, eye: [f64; 3]) -> bool {
    let up = unit(eye);
    (1..=(400.0 * 4.0) as u32).any(|step| {
        let at = plus(eye, up, f64::from(step) * 0.25 * VPM);
        solid(voxel(at))
    })
}
