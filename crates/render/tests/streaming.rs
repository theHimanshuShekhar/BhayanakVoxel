//! A Planet whose bricks are streamed in (`BrickStore::stream_from`) is drawn from what is loaded,
//! with a coarser level standing in for what is not, and never a hole. A CPU oracle, the same
//! Planet with everything loaded, says what a ray should hit. No GPU needed.

use std::sync::OnceLock;

use brick_store::{BrickCoord, BrickStore, children_from};
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::WorldMap;
use render::lod::{Bands, brick_distance};
use render::reference::{Ray, trace};
use render::scene::Bound;
use render::{Camera, Scene, camera};

const SEED: u64 = 0xB4A7_A9AC;
/// Levels above level 0 that the pyramid has: levels 0 to 12.
const PYRAMID: usize = 12;
/// The Guard of these tests is levels 10 to 12; the real one is 8 to 12.
const GUARD: std::ops::RangeInclusive<usize> = 10..=12;
const FLOOR: usize = 10;
/// Voxels around the spawn that the ring has: 16 m.
const RING: i32 = 160;
/// Level 1 from 128 Voxels, level 2 from 256, and so on: a scale that fits in a test.
const D0: f64 = 64.0;
/// A Voxel is 10 cm.
const VOXELS_PER_METRE: f64 = 10.0;

struct World {
    planet: Planet,
    ground: [f64; 3],
}

fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let planet = Planet::new(WorldMap::generate(SEED, 512));
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
        radius: planet_gen::planet::MAX_RADIUS,
    }
}

/// The Planet with everything loaded: the ring, and every other brick read as the Planet gives it
/// when it is asked for.
fn oracle_store() -> BrickStore {
    let w = world();
    w.planet.crust_around(w.ground, RING, PYRAMID)
}

/// The Planet as the client has it right after the spawn: the ring and the Guard, and nothing else.
fn spawned_store() -> BrickStore {
    static GUARD_BRICKS: OnceLock<Vec<(usize, BrickCoord, brick_store::Brick)>> = OnceLock::new();
    let w = world();
    let mut store = oracle_store();
    for (level, coord, brick) in GUARD_BRICKS.get_or_init(|| w.planet.guard(GUARD)) {
        store.insert_at(*level, *coord, brick.clone());
    }
    store.stream_from(FLOOR);
    store
}

/// Loads, a few at a time, what the scene is waiting for: what a loader does, with nothing but
/// the calling thread.
#[derive(Default)]
struct Loader {
    pending: Vec<(usize, BrickCoord)>,
    loaded: Vec<(usize, BrickCoord)>,
    generated: usize,
}

impl Loader {
    /// Loads the `most` nearest families `camera` has asked for. Returns how many it loaded.
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
        let mut loaded = Vec::new();
        for _ in 0..most {
            let Some((level, coord)) = self.pending.pop() else {
                break;
            };
            let bricks = children_from(&*source, level, coord, |c| store.holds(level - 1, c));
            self.generated += 8;
            store.load_children(level, coord, bricks);
            loaded.push((level, coord));
        }
        scene.arrive(store, &loaded).unwrap();
        self.loaded.extend(&loaded);
        loaded.len()
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
    let position = std::array::from_fn(|a| ground[a] + up[a] * 40.0 + ahead[a] * along);
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

/// What the scene hits that the oracle does, ray by ray. Panics on a ray that the oracle hits and
/// the scene does not (a hole), or that the scene hits in a brick of a finer level than the
/// oracle draws there (the scene may hit a coarse brick where the oracle's finer ones are air, a
/// stand-in that is too fat, but never a finer one than the oracle).
/// Returns (rays that hit in the oracle, rays that the scene hits in a coarser brick than the
/// oracle's).
fn compare(scene: &Scene, oracle: &Scene, camera: &Camera, label: &str) -> (usize, usize) {
    let (mut hits, mut coarser) = (0, 0);
    for ray in fan(camera) {
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
    (hits, coarser)
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
    let (hits, coarser) = compare(&scene, &oracle, &camera, "after spawn");
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
    // 5 m/s is 50 Voxels a second, at 60 frames a second; 8 seconds, past the ring and its 16 m.
    let step = 50.0 / 60.0;
    let (mut hits, mut coarser, mut checks) = (0, 0, 0);
    for frame in 0..480 {
        let position: [f64; 3] = std::array::from_fn(|a| camera.position[a] + ahead[a] * step);
        camera.position = position;
        camera.carry_up(position);
        scene.recentre(&store, position).unwrap();
        oracle.recentre(&oracle_store, position).unwrap();
        loader.pump(&mut store, &mut scene, position, 6);
        if frame % 40 == 0 {
            let (h, c) = compare(&scene, &oracle, &camera, &format!("frame {frame}"));
            (hits, coarser, checks) = (hits + h, coarser + c, checks + 1);
        }
    }
    assert!(checks == 12 && hits > 1000, "{checks} checks, {hits} hits");
    // The loader is slow enough that some of the rays are on a stand-in, and fast enough that
    // not all of them are.
    assert!(coarser > 0 && coarser < hits, "{coarser} of {hits}");
    // Left alone it finishes, and then the picture is the oracle's.
    while !loader.idle(&scene) {
        loader.pump(&mut store, &mut scene, camera.position, 200);
    }
    let (hits, coarser) = compare(&scene, &oracle, &camera, "done");
    assert!(hits > 100);
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
    // It never loaded beyond the bands: every family that was loaded is of a brick that the
    // bands split, nearer than its level is used from.
    let b = bands();
    assert!(!loader.loaded.is_empty());
    for &(level, coord) in &loader.loaded {
        let d = brick_distance(camera.position, level, coord);
        assert!(d < b.floor(level), "level {level} {coord:?} at {d}");
    }
}
