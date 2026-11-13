//! A Planet whose bricks are streamed in (`BrickStore::stream_from`) is drawn from what is resident,
//! with a coarser level standing in for what is not, and never a hole. A CPU oracle, the same
//! Planet with everything resident, says what a ray should hit, and the Planet's own Voxels say
//! how deep a ray may go before it does. No GPU needed.

use std::sync::OnceLock;

use brick_store::{BrickCoord, BrickStore, VOXELS_PER_METRE, VoxelCoord, children_from};
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::WorldMap;
use render::lod::{Bands, brick_distance};
use render::reference::{Ray, trace};
use render::scene::Bound;
use render::{Camera, Scene, camera};

const SEED: u64 = 0xB4A7_A9AC;
/// Levels above level 0 that the pyramid has: levels 0 to 13.
const PYRAMID: usize = 13;
/// The Guard of these tests is levels 11 to 13; the real one is 9 to 13.
const GUARD: std::ops::RangeInclusive<usize> = 11..=13;
const FLOOR: usize = 11;
/// Voxels around the spawn that the ring has: 8 m.
const RING: i32 = 160;
/// Level 1 from 128 Voxels, level 2 from 256, and so on: a scale that fits in a test.
const D0: f64 = 64.0;

struct World {
    planet: Planet,
    ground: [f64; 3],
}

fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let planet = Planet::new(WorldMap::for_tests(SEED));
        let ground = planet.spawn(100);
        World { planet, ground }
    })
}

fn bands() -> Bands {
    Bands::with_d0(D0)
}

fn bound() -> Bound {
    Bound {
        centre: [0; 3],
        radius: world().planet.max_radius(),
    }
}

/// The Planet with everything resident: the ring, and every other brick read as the Planet gives it
/// when it is asked for.
fn oracle_store() -> BrickStore {
    let w = world();
    w.planet.crust_around(w.ground, RING, PYRAMID)
}

/// The Planet as the client has it right after the spawn: the ring and the Guard, and nothing else.
fn spawned_store() -> BrickStore {
    static GUARD_BRICKS: OnceLock<Vec<(usize, BrickCoord, brick_store::Brick, bool)>> =
        OnceLock::new();
    let w = world();
    let mut store = oracle_store();
    for (level, coord, brick, solid) in GUARD_BRICKS.get_or_init(|| w.planet.guard(GUARD)) {
        store.hold(*level, *coord, brick.clone(), *solid);
    }
    store.stream_from(FLOOR);
    store
}

/// Loads, a few at a time, what the scene is waiting for: what a loader does, with nothing but
/// the calling thread.
#[derive(Default)]
struct Loader {
    pending: Vec<(usize, BrickCoord)>,
    resident: Vec<(usize, BrickCoord)>,
    generated: usize,
}

impl Loader {
    /// Holds the children of the `most` nearest bricks `camera` has asked for. Returns how many
    /// bricks it did that for.
    fn pump(
        &mut self,
        store: &mut BrickStore,
        scene: &mut Scene,
        camera: [f64; 3],
        most: usize,
    ) -> usize {
        self.pending.extend(scene.take_wanted());
        let near = |&(l, c): &(usize, BrickCoord)| brick_distance(camera, l, c);
        self.pending.sort_by(|a, b| near(b).total_cmp(&near(a)));
        let source = store.source().expect("a source");
        let mut done = Vec::new();
        for _ in 0..most {
            let Some((level, coord)) = self.pending.pop() else {
                break;
            };
            let bricks = children_from(&*source, level, coord, |c| store.holds(level - 1, c));
            self.generated += 8;
            store.hold_children(level, coord, bricks);
            done.push((level, coord));
        }
        scene.arrive(store, &done).unwrap();
        self.resident.extend(&done);
        done.len()
    }

    fn idle(&self, scene: &Scene) -> bool {
        self.pending.is_empty() && scene.waiting() == 0
    }
}

/// A camera 4 m over the ground, `along` Voxels ahead of the spawn.
fn camera_at(along: f64) -> Camera {
    let ground = world().ground;
    let length = ground.iter().map(|c| c * c).sum::<f64>().sqrt();
    let up = ground.map(|c| c / length);
    let (ahead, _) = tangents(up);
    let position =
        std::array::from_fn(|a| ground[a] + up[a] * 4.0 * VOXELS_PER_METRE + ahead[a] * along);
    let mut camera = Camera::looking_at(position, std::array::from_fn(|a| position[a] + ahead[a]));
    camera.carry_up(position);
    camera
}

/// Rays from `camera` all round, from steeply down to a little above the horizon.
fn fan(camera: &Camera) -> Vec<Ray> {
    let up = camera.horizon_up;
    let (ahead, aside) = tangents(up);
    let (cell, origin) = camera::split(camera.position);
    let mut rays = Vec::new();
    for azimuth in (0..360).step_by(20) {
        for pitch in [-70.0f64, -35.0, -15.0, -6.0, -2.5, -1.0, 0.0, 2.0] {
            let (a, p) = (f64::from(azimuth).to_radians(), pitch.to_radians());
            let dir = std::array::from_fn(|k| {
                p.cos() * (a.cos() * ahead[k] + a.sin() * aside[k]) + p.sin() * up[k]
            });
            rays.push(Ray {
                cell,
                origin,
                dir: dir.map(|c: f64| c as f32),
            });
        }
    }
    rays
}

/// Rays that hit further than this many Voxels of the level they hit in beyond the first solid
/// Voxel of the Planet along them have gone through ground: a coarse stand-in may be that fat
/// (up to half a coarse Voxel of ground is missed where it is sampled at the middle), but not
/// more.
const DEEPER: f64 = 4.0;

/// Rays further than this many Voxels are not checked against the Planet's Voxels, which takes a
/// step for each Voxel along them.
const TRUTH_REACH: f64 = 1500.0;

/// The distance along the ray from its origin to the first solid Voxel of level 0 of the Planet
/// (`Planet::voxel`) that it passes through, by stepping Voxel to Voxel, if it is within `max`.
fn truth(ray: &Ray, max: f64) -> Option<f64> {
    let planet = &world().planet;
    let origin: [f64; 3] =
        std::array::from_fn(|a| f64::from(ray.cell[a]) + f64::from(ray.origin[a]));
    let dir = ray.dir.map(f64::from);
    let mut cell = origin.map(|c| c.floor() as i32);
    let step = dir.map(|d| if d > 0.0 { 1 } else { -1 });
    let mut next: [f64; 3] = std::array::from_fn(|a| {
        if dir[a].abs() < 1e-12 {
            f64::INFINITY
        } else {
            (f64::from(cell[a] + i32::from(dir[a] > 0.0)) - origin[a]) / dir[a]
        }
    });
    let delta = dir.map(|d| {
        if d.abs() < 1e-12 {
            f64::INFINITY
        } else {
            1.0 / d.abs()
        }
    });
    let mut t = 0.0;
    while t <= max {
        if planet.voxel(VoxelCoord(cell)).is_some() {
            return Some(t);
        }
        let axis = (0..3).min_by(|&i, &j| next[i].total_cmp(&next[j])).unwrap();
        t = next[axis];
        cell[axis] += step[axis];
        next[axis] += delta[axis];
    }
    None
}

/// Panics on a ray that the scene hits deeper than the first solid Voxel of the Planet along it
/// (see [`DEEPER`]): ground that is not drawn, a hole in the surface that shows what is under it.
/// A ray that grazes a slope can go a long way on a coarse stand-in, so this is for the rays of a
/// camera over the ground, as the walk's and the flight's are. Returns how many rays it compared
/// with the Planet: those that hit within reach, from a camera that is not inside the ground.
fn not_deeper_than_the_planet(scene: &Scene, rays: &[Ray], label: &str) -> usize {
    let mut compared = 0;
    for ray in rays {
        let Some(got) = trace(scene, ray) else {
            continue;
        };
        let t = f64::from(got.t);
        if t > TRUTH_REACH {
            continue;
        }
        // A camera inside the ground has no surface to hit: the first Voxel is its own.
        let Some(first) = truth(ray, t + 1.0).filter(|&first| first > 0.0) else {
            continue;
        };
        compared += 1;
        let level = scene
            .level_at(got.voxel)
            .expect("a hit is in a drawn brick");
        let allowed = DEEPER * f64::from(1u32 << level);
        assert!(
            t - first <= allowed,
            "{label}: a hole: the scene hits level {level} at {t:.1}, the Planet's first Voxel along the ray is at {first:.1} ({ray:?})"
        );
    }
    compared
}

/// What the scene hits that the oracle does, ray by ray. Panics on a ray that the oracle hits and
/// the scene does not (a hole), or that the scene hits in a brick of a finer level than the
/// oracle draws there (the scene may hit a coarse brick where the oracle's finer ones are air, a
/// stand-in that is too fat, but never a finer one than the oracle).
/// Nor may the scene hit deeper than the Planet's own first Voxel along the ray (see
/// [`not_deeper_than_the_planet`]), which the oracle's picture might be wrong about too.
/// Returns (rays that hit in the oracle, rays that the scene hits in a coarser brick than the
/// oracle's, rays that were compared with the Planet's Voxels).
fn compare(scene: &Scene, oracle: &Scene, camera: &Camera, label: &str) -> (usize, usize, usize) {
    let (mut hits, mut coarser) = (0, 0);
    let rays = fan(camera);
    let compared = not_deeper_than_the_planet(scene, &rays, label);
    for ray in rays {
        let Some(want) = trace(oracle, &ray) else {
            continue;
        };
        hits += 1;
        let got = trace(scene, &ray).unwrap_or_else(|| {
            panic!("{label}: a hole: the oracle hits at {want:?} and the scene is sky ({ray:?})")
        });
        let level = scene
            .level_at(got.voxel)
            .expect("a hit is in a drawn brick");
        // Where the oracle draws nothing, the scene's brick has no finer one to be compared to.
        if let Some(there) = oracle.level_at(got.voxel) {
            assert!(
                level >= there,
                "{label}: the scene hits level {level}, finer than the oracle's {there} there ({ray:?})"
            );
            coarser += usize::from(level > there);
        }
    }
    (hits, coarser, compared)
}

#[test]
fn the_horizon_has_no_sky_gaps_right_after_spawn() {
    let store = spawned_store();
    let camera = camera_at(0.0);
    let scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    let oracle_store = oracle_store();
    let oracle = Scene::rings(&oracle_store, camera.position, bands(), bound()).unwrap();
    // Nothing but the Guard is refined: the rays are drawn from coarse bricks.
    assert!(scene.waiting() > 0);
    let (hits, coarser, _) = compare(&scene, &oracle, &camera, "after spawn");
    // The rays that go on to the horizon are among them: the ground is hit hundreds of metres
    // away, and the oracle is not looking at sky either.
    let far = fan(&camera)
        .into_iter()
        .filter_map(|ray| trace(&oracle, &ray))
        .filter(|hit| f64::from(hit.t) > 100.0 * VOXELS_PER_METRE)
        .count();
    assert!(far >= 10, "{far} rays reach a hundred metres");
    assert!(hits >= 120, "{hits} rays hit the ground");
    assert!(coarser > 0, "{coarser} of {hits} are stand-ins");
}

#[test]
fn walking_at_5_m_per_s_while_the_loader_keeps_up_a_little_at_a_time_leaves_no_hole() {
    let mut store = spawned_store();
    let oracle_store = oracle_store();
    let mut camera = camera_at(0.0);
    let mut scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    let mut oracle = Scene::rings(&oracle_store, camera.position, bands(), bound()).unwrap();
    let mut loader = Loader::default();
    let ahead = tangents(camera.horizon_up).0;
    // 5 m/s at 60 frames a second; 8 seconds, past the ring and its 8 m.
    let step = 5.0 * VOXELS_PER_METRE / 60.0;
    let (mut hits, mut coarser, mut compared, mut checks) = (0, 0, 0, 0);
    for frame in 0..480 {
        // Along the ground, 4 m over it, as a walker is: a camera that goes straight on is in
        // the hill ahead of it before the walk is over.
        let ahead_of =
            |p: [f64; 3]| -> [f64; 3] { std::array::from_fn(|a| p[a] + ahead[a] * step) };
        let moved = ahead_of(camera.position);
        let length = moved.iter().map(|c| c * c).sum::<f64>().sqrt();
        let target = ground_radius(moved) + 4.0 * VOXELS_PER_METRE;
        let position = moved.map(|c| c * target / length);
        camera.position = position;
        camera.carry_up(position);
        scene.recentre(&store, position).unwrap();
        oracle.recentre(&oracle_store, position).unwrap();
        loader.pump(&mut store, &mut scene, position, 6);
        if frame % 40 == 0 {
            let (h, c, n) = compare(&scene, &oracle, &camera, &format!("frame {frame}"));
            (hits, coarser, compared, checks) = (hits + h, coarser + c, compared + n, checks + 1);
        }
    }
    assert!(checks == 12 && hits > 1000, "{checks} checks, {hits} hits");
    // (381 of the 480 frames' 12 checks on the 40 km Planet's ground, which has its detail: fewer
    // of the rays meet the ground than on the smooth one.)
    assert!(compared > 300, "{compared} rays compared with the Planet");
    // The loader is slow enough that some of the rays are on a stand-in, and fast enough that
    // not all of them are.
    assert!(coarser > 0 && coarser < hits, "{coarser} of {hits}");
    // Left alone it finishes, and then the picture is the oracle's.
    while !loader.idle(&scene) {
        loader.pump(&mut store, &mut scene, camera.position, 200);
    }
    // The live scene keeps a brick at the level it had within the hysteresis of a boundary, which
    // a walk over hills leaves finer than the oracle's: it is a scene made afresh from what is
    // resident that is the oracle's picture.
    let mut scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    let mut loader = Loader::default();
    while !loader.idle(&scene) {
        loader.pump(&mut store, &mut scene, camera.position, 200);
    }
    let oracle = Scene::rings(&oracle_store, camera.position, bands(), bound()).unwrap();
    let (hits, coarser, compared) = compare(&scene, &oracle, &camera, "done");
    assert!(
        hits > 100 && compared > 20,
        "{hits} hits, {compared} compared"
    );
    assert_eq!(coarser, 0, "every ray is at the level the bands say");
}

#[test]
fn with_the_camera_still_the_loader_fills_the_bands_and_then_has_nothing_to_do() {
    let mut store = spawned_store();
    let camera = camera_at(0.0);
    let mut scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    let mut loader = Loader::default();
    let mut rounds = 0;
    while !loader.idle(&scene) {
        loader.pump(&mut store, &mut scene, camera.position, 50);
        rounds += 1;
        assert!(rounds < 10_000, "it does not end");
    }
    let generated = loader.generated;
    assert!(generated > 1000, "{generated}");
    // The bands that filled in draw about what a scene made whole draws: a few more, where the
    // hysteresis keeps a brick as it was, and where a brick's neighbours were not all there when
    // it was looked at.
    let fresh = Scene::rings(&oracle_store(), camera.position, bands(), bound()).unwrap();
    for l in 0..13 {
        let (got, whole) = (scene.level_bricks(l), fresh.level_bricks(l));
        assert!(
            got * 10 <= whole * 12 + 20 && got * 10 + 200 >= whole * 8,
            "level {l}: {got} bricks drawn, {whole} in a scene made whole"
        );
    }
    // Nothing is wanted, and asking again generates nothing.
    assert_eq!(scene.waiting(), 0);
    for _ in 0..5 {
        assert_eq!(loader.pump(&mut store, &mut scene, camera.position, 50), 0);
        assert!(scene.take_wanted().is_empty());
    }
    assert_eq!(loader.generated, generated);
    // It never went beyond the bands: every brick whose children were made resident is one that the
    // bands split, nearer than its level is used from.
    let b = bands();
    assert!(!loader.resident.is_empty());
    for &(level, coord) in &loader.resident {
        let d = brick_distance(camera.position, level, coord);
        assert!(d < b.floor(level), "level {level} {coord:?} at {d}");
    }
}

/// Rays in front of a camera that looks along `heading` over the ground: a grid from 45 degrees
/// either side and from steeply down to a little above the horizon.
fn ahead_fan(position: [f64; 3], heading: [f64; 3]) -> Vec<Ray> {
    let up = unit(position);
    let aside = unit(cross(heading, up));
    let (cell, origin) = camera::split(position);
    let mut rays = Vec::new();
    for azimuth in (-30..=30).step_by(2) {
        for pitch in (-40..=2).step_by(2) {
            let (a, p) = (
                (f64::from(azimuth) * 1.5).to_radians(),
                (f64::from(pitch) * 0.75).to_radians(),
            );
            let dir = unit(std::array::from_fn(|k| {
                p.cos() * (a.cos() * heading[k] + a.sin() * aside[k]) + p.sin() * up[k]
            }));
            rays.push(Ray {
                cell,
                origin,
                dir: dir.map(|c| c as f32),
            });
        }
    }
    rays
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let length = v.iter().map(|c| c * c).sum::<f64>().sqrt();
    v.map(|c| c / length)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// The radius of the ground under `position`: where the Voxels on the line to the centre are
/// first solid, looking down from a little above.
fn ground_radius(position: [f64; 3]) -> f64 {
    let planet = &world().planet;
    let up = unit(position);
    let length = position.iter().map(|c| c * c).sum::<f64>().sqrt();
    let at = |r: f64| VoxelCoord(up.map(|u| (u * r).floor() as i32));
    let mut r = length + 8.0 * VOXELS_PER_METRE;
    while planet.voxel(at(r)).is_none() {
        r -= 1.0;
    }
    r + 1.0
}

#[test]
fn flying_at_20_m_per_s_leaves_no_hole_in_the_ground_ahead_of_the_camera() {
    // 4 m over the ground and 20 m/s, with the loader reading six
    // bricks' children a frame. A coarse brick just over a slope can be air at the middle of its
    // Voxels with ground under it that pokes into it: if it is not drawn, a ray goes through the
    // ground and hits what is under it.
    let mut store = spawned_store();
    let start = camera_at(0.0);
    let mut position = start.position;
    let (ahead, _) = tangents(start.horizon_up);
    let mut scene = Scene::rings(&store, position, bands(), bound()).unwrap();
    let mut loader = Loader::default();
    let mut checked = 0;
    for frame in 0..300 {
        let up = unit(position);
        let along: f64 = ahead.iter().zip(&up).map(|(f, u)| f * u).sum();
        let heading = unit(std::array::from_fn(|a| ahead[a] - up[a] * along));
        position =
            std::array::from_fn(|a| position[a] + heading[a] * 20.0 * VOXELS_PER_METRE / 60.0);
        let length = position.iter().map(|c| c * c).sum::<f64>().sqrt();
        let target = ground_radius(position) + 4.0 * VOXELS_PER_METRE;
        position = position.map(|c| c * target / length);
        scene.recentre(&store, position).unwrap();
        loader.pump(&mut store, &mut scene, position, 6);
        if frame % 30 == 29 {
            let rays = ahead_fan(position, heading);
            checked += not_deeper_than_the_planet(&scene, &rays, &format!("frame {frame}"));
        }
    }
    // (1 376 on the 40 km Planet, whose ground is rougher than the smooth one it had.)
    assert!(checked > 1000, "{checked} rays compared with the Planet");
}
