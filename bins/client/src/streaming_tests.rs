//! The ring loader at work (#86): the loader's threads, the world that takes in what they make, and
//! the scene that is drawn from it, on the test world map. Bands of `d0` = 64 Voxels make a few
//! levels fit in a test; the Guard is levels 10 to 12.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use brick_store::{BrickStore, VoxelCoord};
use net::{ServerEnd, loopback};
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::WorldMap;
use render::lod::Bands;
use render::reference::{Ray, trace};
use render::scene::Bound;
use render::{Camera, Scene, camera};
use sim::Predictor;

use crate::loader::{Arrival, Loader, hold_guard};
use crate::world::{SceneWork, World};

const SEED: u64 = 0xB4A7_A9AC;
const RING: i32 = 160;
const D0: f64 = 64.0;
const GUARD_FIRST: usize = 10;

fn planet() -> &'static Planet {
    static PLANET: OnceLock<Planet> = OnceLock::new();
    PLANET.get_or_init(|| Planet::new(WorldMap::generate(SEED, 512)))
}

fn ground() -> [f64; 3] {
    static GROUND: OnceLock<[f64; 3]> = OnceLock::new();
    *GROUND.get_or_init(|| planet().spawn(100))
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

/// The ring and the Guard: what the client has right after the spawn.
fn spawned_store() -> BrickStore {
    let mut store = planet().crust_around(ground(), RING, 12);
    hold_guard(&mut store, planet(), GUARD_FIRST);
    store
}

/// The world right after the spawn, with a loader of `workers` threads.
fn spawned_world(workers: usize) -> (World, ServerEnd, Camera) {
    let store = spawned_store();
    let camera = crate::spawn_camera(ground());
    let scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    let loader = Loader::new(store.source().unwrap(), camera.position, bands(), workers);
    let (client, server) = loopback();
    let world = World::new(scene, Predictor::new(store), client).with_loader(loader);
    (world, server, camera)
}

fn idle(world: &World) -> bool {
    world.waiting() == 0 && world.loader().is_some_and(|l| l.pending() == 0)
}

/// Streams with the camera at `camera` until the bands are complete. Returns how many frames
/// that took.
fn fill(world: &mut World, camera: [f64; 3]) -> usize {
    let started = Instant::now();
    for frame in 0.. {
        world.stream(camera).unwrap();
        if idle(world) {
            return frame;
        }
        assert!(
            started.elapsed() < Duration::from_secs(180),
            "the bands do not fill"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    unreachable!()
}

#[test]
fn with_the_camera_still_the_loader_fills_the_bands_and_then_has_no_work() {
    let (mut world, _server, camera) = spawned_world(2);
    assert!(world.waiting() > 0, "the Guard stands in for everything");
    let frames = fill(&mut world, camera.position);
    assert!(frames > 3, "it takes frames, not one: {frames}");
    let loader = world.loader().unwrap();
    assert_eq!(loader.pending(), 0);
    assert_eq!(world.waiting(), 0);
    let generated = loader.generated();
    assert!(generated > 1000, "{generated}");
    // Standing still, nothing more is asked for and nothing more is generated.
    for _ in 0..20 {
        assert!(world.stream(camera.position).unwrap().is_none());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(world.loader().unwrap().generated(), generated);
    assert_eq!(world.loader().unwrap().pending(), 0);
    assert_eq!(world.waiting(), 0);
}

#[test]
fn a_family_the_camera_has_left_is_dropped_not_generated() {
    let store = spawned_store();
    let camera = crate::spawn_camera(ground()).position;
    let mut loader = Loader::new(store.source().unwrap(), camera, bands(), 1);
    // A brick of level 5 is split within 64 * 32 = 2048 Voxels; this one is 100 times that away.
    let far = (5, brick_store::BrickCoord([5000, 0, 0]));
    let near = (5, brick_store::BrickCoord(camera.map(|c| c as i32 >> 8)));
    loader.request(&store, [far, near], camera);
    loader.request(&store, [far], camera);
    assert_eq!(loader.pending(), 2, "asked for once");
    loader.sync(camera, bands());
    let mut got = Vec::new();
    let started = Instant::now();
    while got.len() < 2 {
        got.extend(loader.receive(10));
        assert!(started.elapsed() < Duration::from_secs(20));
        loader.sync(camera, bands());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(loader.pending(), 0);
    assert!(
        got.iter()
            .any(|a| matches!(a, Arrival::Dropped(k) if *k == far))
    );
    assert!(
        got.iter()
            .any(|a| matches!(a, Arrival::Family { key, .. } if *key == near))
    );
    assert!(
        loader.generated() <= 8,
        "only the near family was generated"
    );
}

/// A ray from `from` through the point `to`, both in Voxels.
fn ray_through(from: [f64; 3], to: [f64; 3]) -> Ray {
    let d: [f64; 3] = std::array::from_fn(|a| to[a] - from[a]);
    let length = d.iter().map(|c| c * c).sum::<f64>().sqrt();
    let (cell, origin) = camera::split(from);
    Ray {
        cell,
        origin,
        dir: d.map(|c| (c / length) as f32),
    }
}

#[test]
fn a_dig_near_the_camera_shows_at_every_band_that_covers_it() {
    let (mut world, _server, spawn) = spawned_world(2);
    fill(&mut world, spawn.position);

    // Dig where the camera looks, as a click does.
    let before = crate::world::centre_hit(world.scene(), &spawn);
    world.dig(&spawn).unwrap().expect("there is ground to dig");
    let at = before.expect("the camera looks at the ground").voxel;
    let target = at.map(f64::from);

    // Camera positions at 1.5 times the start of the band of level 0, 1, 2 and 3 from the
    // brick dug, in the direction up and away along the ground.
    let up = spawn.horizon_up;
    let (ahead, _) = tangents(up);
    let towards: [f64; 3] = std::array::from_fn(|a| 0.5 * up[a] - 0.87 * ahead[a]);
    let mut seen = 0;
    for level in 0..=3usize {
        let distance = if level == 0 {
            90.0
        } else {
            1.5 * D0 * f64::from(1u32 << level)
        };
        let from: [f64; 3] = std::array::from_fn(|a| target[a] + towards[a] * distance);
        world.recentre(from).unwrap();
        fill(&mut world, from);

        let ray = ray_through(from, target);
        let shown = trace(world.scene(), &ray);

        // The Planet with everything loaded, dug and not: the dug store's bricks, and the rest
        // as the Planet gives them.
        let mut dug = world_store(&world);
        dug.end_streaming();
        let dug_scene = Scene::rings(&dug, from, bands(), bound()).unwrap();
        let whole = planet().crust_around(ground(), RING, 12);
        let whole_scene = Scene::rings(&whole, from, bands(), bound()).unwrap();
        let (hole, flat) = (trace(&dug_scene, &ray), trace(&whole_scene, &ray));
        let (hole, flat, shown) = (hole.unwrap(), flat.unwrap(), shown.unwrap());
        assert_eq!(
            whole_scene.level_at(flat.voxel),
            Some(level),
            "the camera is placed so that the ground at the dig is in the band of level {level}"
        );

        // The size of a Voxel here: the hole is 10 deep, which a Voxel of this level may blur.
        let voxel = f32::from(1u16 << level);
        assert!(
            (shown.t - hole.t).abs() <= voxel + 1.0,
            "level {level}: the scene hits at {} and the dug Planet at {}",
            shown.t,
            hole.t
        );
        if hole.t > flat.t + 2.0 * voxel {
            seen += 1;
            assert!(
                shown.t > flat.t + voxel,
                "level {level}: the hole is in the dug Planet ({} past the ground) and not in the \
                 scene ({})",
                hole.t - flat.t,
                shown.t - flat.t
            );
        }
    }
    assert!(seen >= 3, "the hole shows at {seen} levels");
}

/// The point 4 m above the ground, along the radial through `point`, as `store` reads the Planet.
fn over_the_ground(store: &BrickStore, point: [f64; 3]) -> [f64; 3] {
    let length = point.iter().map(|c| c * c).sum::<f64>().sqrt();
    let up = point.map(|c| c / length);
    let at = |r: f64| -> [f64; 3] { up.map(|u| u * r) };
    let solid = |r: f64| {
        let p = at(r).map(|c| c.round() as i32);
        store.voxel_at(0, VoxelCoord(p)).is_some()
    };
    // The ground is within 60 m of where `point` is, which it is walked along.
    let (mut below, mut above) = (length - 600.0, length + 800.0);
    while above - below > 1.0 {
        let middle = (below + above) / 2.0;
        if solid(middle) {
            below = middle;
        } else {
            above = middle;
        }
    }
    at(below + 40.0)
}

fn world_store(world: &World) -> BrickStore {
    world.store().clone()
}

#[test]
fn walking_at_5_m_per_s_while_the_loader_runs_reports_the_scene_work_of_every_frame() {
    let (mut world, _server, mut camera) = spawned_world(2);
    let (ahead, _) = tangents(camera.horizon_up);
    let step = 50.0 / 60.0;
    let mut work = SceneWork::default();
    let mut walked = camera.position;
    for _ in 0..360 {
        walked = std::array::from_fn(|a| walked[a] + ahead[a] * step);
        let position = over_the_ground(world.store(), walked);
        camera.position = position;
        camera.carry_up(position);
        let started = Instant::now();
        world.recentre(position).unwrap();
        world.stream(position).unwrap();
        work.record(started.elapsed());
        std::thread::sleep(Duration::from_millis(3));
    }
    let (p99, worst) = (work.p99().unwrap(), work.worst().unwrap());
    eprintln!("{}", work.summary().unwrap());
    assert!(p99 <= worst && worst.is_finite());
    // Left alone it fills in, and the ground under the camera is level 0.
    fill(&mut world, camera.position);
    let down = ray_through(
        camera.position,
        std::array::from_fn(|a| camera.position[a] - camera.horizon_up[a]),
    );
    let hit = trace(world.scene(), &down).expect("ground under the camera");
    assert_eq!(world.scene().level_at(hit.voxel), Some(0));
}

/// What the scene changes came to over a run of frames.
#[derive(Default)]
struct Updates {
    /// The time following the camera took, and that taking in bricks took, frame by frame.
    recentre: SceneWork,
    stream: SceneWork,
    rebuilt: usize,
    patched: usize,
    worst_words: usize,
}

impl Updates {
    fn add(&mut self, update: Option<render::SceneUpdate>) {
        match update {
            Some(render::SceneUpdate::Rebuilt) => self.rebuilt += 1,
            Some(render::SceneUpdate::Rings(patch)) => {
                self.patched += 1;
                self.worst_words = self.worst_words.max(patch.words());
            }
            _ => {}
        }
    }
}

/// One frame's scene work, timed, as the client does it.
fn scene_frame(world: &mut World, position: [f64; 3], work: &mut SceneWork, updates: &mut Updates) {
    let started = Instant::now();
    let moved = world.recentre(position).unwrap();
    let recentred = started.elapsed();
    let streaming = Instant::now();
    let streamed = world.stream(position).unwrap();
    updates.recentre.record(recentred);
    updates.stream.record(streaming.elapsed());
    work.record(started.elapsed());
    if started.elapsed() > Duration::from_millis(250) {
        eprintln!(
            "  slow frame: {:.0?} (recentre {recentred:.0?}), {:?} {:?}",
            started.elapsed(),
            moved
                .as_ref()
                .map(|u| matches!(u, render::SceneUpdate::Rebuilt)),
            streamed
                .as_ref()
                .map(|u| matches!(u, render::SceneUpdate::Rebuilt))
        );
    }
    updates.add(moved);
    updates.add(streamed);
}

/// The Planet at full size, streamed in as the client does it: the spawn ring and the Guard are
/// timed by `load`, then the bands fill with the camera still (timed), and then the camera
/// walks at 5 m/s with the loader running (scene work per frame, worst and p99). Takes about two
/// minutes and a lot of memory in a release build:
/// `cargo test --release -p client -- --ignored --nocapture full_size`.
#[test]
#[ignore = "generates the full-size world map"]
fn the_full_size_planet_streams_in_and_walking_over_it_does_not_stall() {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    eprintln!(
        "{cores} cores, {} workers, load average at the start: {}",
        crate::loader::workers_for(cores),
        std::fs::read_to_string("/proc/loadavg")
            .unwrap_or_default()
            .trim()
    );
    let loaded = crate::load_planet();
    let (client, _server) = loopback();
    let mut world =
        World::new(loaded.scene, Predictor::new(loaded.store), client).with_loader(loaded.loader);
    let mut camera = loaded.camera;
    let frame = Duration::from_micros(16_667);

    // The bands fill with the camera still.
    let (mut fill_work, mut fill_updates) = (SceneWork::default(), Updates::default());
    let began = Instant::now();
    let mut frames = 0;
    let mut last_report = Instant::now();
    loop {
        let tick = Instant::now();
        scene_frame(
            &mut world,
            camera.position,
            &mut fill_work,
            &mut fill_updates,
        );
        frames += 1;
        if idle(&world) {
            break;
        }
        // For trying things out: stop filling after this many seconds and go on to walking.
        let cap = std::env::var("FULLSIZE_FILL_CAP_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5400);
        if began.elapsed() > Duration::from_secs(cap) {
            eprintln!("  (filling stopped after {cap} s)");
            break;
        }
        if last_report.elapsed() > Duration::from_secs(30) {
            last_report = Instant::now();
            let drawn: usize = (0..13).map(|l| world.scene().level_bricks(l)).sum();
            eprintln!(
                "  {:.0?}: {frames} frames, {} waiting, {} pending, {} generated, {drawn} drawn",
                began.elapsed(),
                world.waiting(),
                world.loader().unwrap().pending(),
                world.loader().unwrap().generated()
            );
        }
        std::thread::sleep(frame.saturating_sub(tick.elapsed()));
    }
    let bricks: Vec<usize> = (0..13).map(|l| world.scene().level_bricks(l)).collect();
    eprintln!(
        "bands complete {:.1?} after the first frame ({frames} frames): {} bricks generated, {} \
         drawn (per level {bricks:?}), scene {:.0} MiB; {} patches, {} rebuilds, the largest \
         patch {} words",
        began.elapsed(),
        world.loader().unwrap().generated(),
        bricks.iter().sum::<usize>(),
        world.scene().used_bytes() as f64 / (1024.0 * 1024.0),
        fill_updates.patched,
        fill_updates.rebuilt,
        fill_updates.worst_words,
    );
    eprintln!("filling: {}", fill_work.summary().unwrap());
    eprintln!(
        "  of which following the camera: {}; taking in bricks: {}",
        fill_updates.recentre.summary().unwrap(),
        fill_updates.stream.summary().unwrap()
    );

    // Then it walks, 5 m/s over the ground.
    let (mut walk_work, mut walk_updates) = (SceneWork::default(), Updates::default());
    let (ahead, _) = tangents(camera.horizon_up);
    let mut walked = camera.position;
    for _ in 0..1800 {
        let tick = Instant::now();
        walked = std::array::from_fn(|a| walked[a] + ahead[a] * (50.0 / 60.0));
        camera.position = over_the_ground(world.store(), walked);
        camera.carry_up(camera.position);
        scene_frame(
            &mut world,
            camera.position,
            &mut walk_work,
            &mut walk_updates,
        );
        std::thread::sleep(frame.saturating_sub(tick.elapsed()));
    }
    eprintln!(
        "walking 150 m: {}; {} patches, {} rebuilds, the largest patch {} words; {} waiting, {} \
         pending, {} bricks generated in all",
        walk_work.summary().unwrap(),
        walk_updates.patched,
        walk_updates.rebuilt,
        walk_updates.worst_words,
        world.waiting(),
        world.loader().unwrap().pending(),
        world.loader().unwrap().generated(),
    );
    eprintln!(
        "  of which following the camera: {}; taking in bricks: {}",
        walk_updates.recentre.summary().unwrap(),
        walk_updates.stream.summary().unwrap()
    );
}
