//! The ring loader at work (#86): the loader's threads, the world that takes in what they make, and
//! the scene that is drawn from it, on the test world map. Bands of `d0` = 64 Voxels make a few
//! levels fit in a test; the Guard is levels 11 to 13.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use brick_store::{BrickStore, TableSize, VOXELS_PER_METRE, VoxelCoord};
use net::{ClientMessage, EditOp, RejectReason, ServerEnd, ServerMessage, loopback};
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::WorldMap;
use render::lod::{Bands, LEVELS};
use render::reference::{Ray, trace};
use render::scene::{Bound, REGION_WORDS};
use render::{Camera, Mode, RingsFull, Scene, SceneUpdate, camera};
use sim::Predictor;

use crate::loader::{Arrival, Loader, hold_guard};
use crate::world::{STILL_FRAMES, SceneWork, Stillness, World, loading_paused_for};

const SEED: u64 = 0xB4A7_A9AC;
const RING: i32 = 160;
const D0: f64 = 64.0;
/// Two levels above the real Guard's, so that a test's Guard is quick to make.
const GUARD_FIRST: usize = crate::loader::GUARD_FIRST + 2;

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
    let mut store = planet().crust_around(ground(), RING, LEVELS - 1);
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

/// [`World::stream`] with no GPU to copy to, and the updates it made.
fn stream(world: &mut World, camera: [f64; 3]) -> Vec<SceneUpdate> {
    let mut updates = Vec::new();
    world
        .stream::<RingsFull>(camera, |_, update| {
            updates.push(update.clone());
            Ok(())
        })
        .unwrap();
    updates
}

fn idle(world: &World) -> bool {
    world.waiting() == 0 && world.loader().is_some_and(|l| l.pending == 0)
}

/// Streams with the camera at `camera` until the resident set is complete for the bands. Returns
/// how many frames that took.
fn fill(world: &mut World, camera: [f64; 3]) -> usize {
    let started = Instant::now();
    for frame in 0.. {
        stream(world, camera);
        if idle(world) {
            return frame;
        }
        assert!(
            started.elapsed() < Duration::from_secs(180),
            "the resident set does not fill"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    unreachable!()
}

#[test]
fn with_the_camera_still_the_loader_makes_the_resident_set_complete_and_then_has_no_work() {
    let (mut world, _server, camera) = spawned_world(2);
    assert!(world.waiting() > 0, "the Guard stands in for everything");
    let frames = fill(&mut world, camera.position);
    assert!(frames > 3, "it takes frames, not one: {frames}");
    let loader = world.loader().unwrap();
    assert_eq!(loader.pending, 0);
    assert_eq!(world.waiting(), 0);
    let generated = loader.generated;
    assert!(generated > 1000, "{generated}");
    // Standing still, nothing more is asked for and nothing more is generated.
    for _ in 0..20 {
        assert!(stream(&mut world, camera.position).is_empty());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(world.loader().unwrap().generated, generated);
    assert_eq!(world.loader().unwrap().pending, 0);
    assert_eq!(world.waiting(), 0);
}

#[test]
fn a_brick_the_camera_has_left_has_its_children_dropped_not_generated() {
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
            .any(|a| matches!(a, Arrival::Children { key, .. } if *key == near))
    );
    assert!(
        loader.generated() <= 8,
        "only the near brick's children were generated"
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

/// `camera` pulled forward along its view to a point within reach of the ground it looks at
/// (the spawn is 4 m up, and the ground it looks at is further than 7.5 m), for a click.
/// `camera` pulled on to 8 Voxels from the ground it looks at, where everything that is drawn is
/// of level 0, as the ground in reach is in the game (its d0 is 51 m); the bands of these tests
/// are shorter. The rings are made for it.
fn up_close(world: &mut World, camera: &Camera) -> Camera {
    let reach = within_reach(world, camera);
    let distance = crate::world::centre_hit(world.scene(), &reach)
        .expect("ground")
        .t;
    let forward = reach.forward();
    let close = Camera {
        position: std::array::from_fn(|a| {
            reach.position[a] + forward[a] * f64::from(distance - 8.0)
        }),
        ..reach
    };
    // The scene thread follows the camera when it has the time: wait for it to have, and for the
    // bricks that come of it, which are now near.
    world.recentre(close.position).unwrap();
    world.sync().unwrap();
    fill(world, close.position);
    world.sync().unwrap();
    assert!(
        world.level_bricks(0) > 0,
        "the ground is drawn at level 0 where the camera is"
    );
    close
}

/// A dig where `camera` looks, as a click makes it, and the update that shows it once the scene
/// thread has published it (the click itself does not wait: see [`World::edit`]).
fn dig_and_wait(world: &mut World, camera: &Camera) -> SceneUpdate {
    world.edit(camera, &crate::tool::Tool::default()).unwrap();
    world.sync().unwrap().expect("there is ground to dig")
}

fn within_reach(world: &World, camera: &Camera) -> Camera {
    let forward = camera.forward();
    let distance = crate::world::centre_hit(world.scene(), camera)
        .expect("the camera looks at the ground")
        .t;
    Camera {
        position: std::array::from_fn(|a| {
            camera.position[a] + forward[a] * f64::from(distance - 40.0)
        }),
        ..*camera
    }
}

#[test]
fn a_dig_near_the_camera_shows_at_every_band_that_covers_it() {
    let (mut world, _server, spawn) = spawned_world(2);
    fill(&mut world, spawn.position);

    // Dig where the camera looks, as a click does.
    let aim = within_reach(&world, &spawn);
    let before = crate::world::centre_hit(world.scene(), &aim);
    dig_and_wait(&mut world, &aim);
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

        // The Planet with everything resident, dug and not: the Planet's own bricks, with the
        // same Edit applied to one.
        let mut dug = planet().crust_around(ground(), RING, LEVELS - 1);
        sim::apply(
            &mut dug,
            &EditOp::dig_sphere(VoxelCoord(at), crate::tool::DEFAULT_RADIUS),
        );
        let dug_scene = Scene::rings(&dug, from, bands(), bound()).unwrap();
        let whole = planet().crust_around(ground(), RING, LEVELS - 1);
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

#[test]
fn walking_at_5_m_per_s_while_the_loader_runs_reports_the_scene_work_of_every_frame() {
    let (mut world, _server, mut camera) = spawned_world(2);
    let (ahead, _) = tangents(camera.horizon_up);
    let step = 50.0 / 60.0;
    let mut work = SceneWork::default();
    let mut walked = camera.position;
    for _ in 0..360 {
        walked = std::array::from_fn(|a| walked[a] + ahead[a] * step);
        let position = over_the_ground(&world.store(), walked);
        camera.position = position;
        camera.carry_up(position);
        let started = Instant::now();
        world.recentre(position).unwrap();
        stream(&mut world, position);
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
    /// How far the scene the world draws was behind the camera at each frame, in Voxels, and how
    /// long an Edit (that changes nothing) waited every so often, in milliseconds.
    lag: Vec<f64>,
    edit_waits: Vec<f64>,
    /// The most the store counted against its budget at the end of any frame, and the most bytes
    /// of words the scene used.
    held_peak: usize,
    scene_peak: u64,
}

/// The median, 99th percentile and largest of `values`, for a line of a report.
fn spread(values: &[f64]) -> String {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |share: usize| sorted[(sorted.len() * share / 100).min(sorted.len() - 1)];
    format!("p50 {:.1}, p99 {:.1}, max {:.1}", at(50), at(99), at(100))
}

impl Updates {
    /// A line on how far behind the camera the drawn rings were and what an Edit waited, with
    /// the camera moving `step` Voxels a frame.
    fn report(&self, step: f64) -> String {
        format!(
            "  rings behind the camera, in Voxels ({step:.1} a frame): {}; an Edit waited, in ms: {}",
            spread(&self.lag),
            spread(&self.edit_waits)
        )
    }
}

impl Updates {
    /// Adds what `later` came to, which came after.
    fn merge(&mut self, later: Updates) {
        self.recentre.merge(later.recentre);
        self.stream.merge(later.stream);
        self.rebuilt += later.rebuilt;
        self.patched += later.patched;
        self.worst_words = self.worst_words.max(later.worst_words);
        self.lag.extend(later.lag);
        self.edit_waits.extend(later.edit_waits);
        self.held_peak = self.held_peak.max(later.held_peak);
        self.scene_peak = self.scene_peak.max(later.scene_peak);
    }

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

/// The tables of the client, from the first frame: the room each was reserved with, the most
/// entries each held, and the growths and rebuilds there were. The tables are made roomy up front
/// (see `room_for` in `main.rs`), so none should grow.
struct Tables {
    /// As they were at the first frame.
    reserved: Vec<TableSize>,
    /// The most room each has had, as of the last frame (see `crate::growth::raised`).
    highest_room: Vec<TableSize>,
    /// The most entries each held, by name.
    peak: HashMap<String, usize>,
    /// `(frame, table, was, is)` for each growth.
    grown: Vec<(usize, String, usize, usize)>,
    /// The frames in which the scene was rebuilt.
    rebuilt: Vec<usize>,
    frames: usize,
}

impl Tables {
    fn new(world: &World) -> Self {
        let sizes = world.table_sizes();
        Self {
            peak: sizes.iter().map(|t| (t.name.clone(), t.len)).collect(),
            reserved: sizes.clone(),
            highest_room: sizes,
            grown: Vec::new(),
            rebuilt: Vec::new(),
            frames: 0,
        }
    }

    /// Looks at the tables after a frame, and returns the names of the ones that grew in it.
    fn after_frame(&mut self, world: &World, rebuilt: bool) -> Vec<String> {
        self.frames += 1;
        let sizes = world.table_sizes();
        let grown = crate::growth::grown(&self.highest_room, &sizes);
        for (name, was, is) in &grown {
            self.grown.push((self.frames, name.clone(), *was, *is));
        }
        for size in &sizes {
            let peak = self.peak.entry(size.name.clone()).or_insert(0);
            *peak = (*peak).max(size.len);
        }
        if rebuilt {
            // The scene made its tables again: they are the ones to compare with from now on.
            self.rebuilt.push(self.frames);
            self.highest_room = sizes;
        } else {
            self.highest_room = crate::growth::raised(&self.highest_room, sizes);
        }
        grown.into_iter().map(|(name, ..)| name).collect()
    }

    /// Prints the room each table was reserved with against the most it held.
    fn report(&self) {
        eprintln!("tables: reserved room, peak entries, most room at the end");
        for reserved in &self.reserved {
            let room = self
                .highest_room
                .iter()
                .find(|t| t.name == reserved.name)
                .map_or(0, |t| t.capacity);
            eprintln!(
                "  {:<28} {:>11} {:>11} {:>11}",
                reserved.name,
                reserved.capacity,
                self.peak.get(&reserved.name).copied().unwrap_or(0),
                room
            );
        }
    }

    /// Fails if a table grew or the scene was rebuilt since the first frame.
    fn assert_none_grew(&self) {
        assert!(
            self.grown.is_empty(),
            "tables grew in the middle of a frame, so their reservations are too small: {:?}",
            self.grown
        );
        assert!(
            self.rebuilt.is_empty(),
            "the scene was rebuilt in frames {:?}: its buffers or regions ran out",
            self.rebuilt
        );
    }
}

/// The calling thread's time on a CPU and its time waiting for one while it could run, in
/// nanoseconds, from the kernel's scheduler (`/proc/thread-self/schedstat`), on Linux. A frame
/// that took long in wall time but little of either is a thread that was not scheduled (the
/// 11.4 ms frame of #142's flight).
fn schedstat() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    let mut fields = text.split_whitespace();
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

/// One frame's scene work, timed, as the client does it.
fn scene_frame(
    world: &mut World,
    position: [f64; 3],
    work: &mut SceneWork,
    updates: &mut Updates,
    tables: &mut Tables,
) {
    let sched_before = schedstat();
    let started = Instant::now();
    let moved = world.recentre(position).unwrap();
    let recentred = started.elapsed();
    let streaming = Instant::now();
    let mut streamed = Vec::new();
    world
        .stream::<RingsFull>(position, |_, update| {
            streamed.push(update.clone());
            Ok(())
        })
        .unwrap();
    updates.recentre.record(recentred);
    updates.stream.record(streaming.elapsed());
    work.record(started.elapsed());
    let took = started.elapsed();
    let sched = schedstat()
        .zip(sched_before)
        .map(|((run, wait), (run0, wait0))| {
            ((run - run0) as f64 / 1e6, (wait - wait0) as f64 / 1e6)
        });
    let rebuilt = moved
        .as_ref()
        .is_some_and(|u| matches!(u, SceneUpdate::Rebuilt))
        || streamed.iter().any(|u| matches!(u, SceneUpdate::Rebuilt));
    let grown = tables.after_frame(world, rebuilt);
    // Over the 8 ms that the frame thread is held to (HELD), or `FULLSIZE_SLOW_MS` to look at the
    // tail of a run: where the time went. The 11.4 ms frame of #142's flight was one of these.
    static SLOW: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    let slow = *SLOW.get_or_init(|| {
        std::env::var("FULLSIZE_SLOW_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map_or(Duration::from_millis(8), Duration::from_millis)
    });
    if took > slow {
        let words = |u: &Option<SceneUpdate>| match u {
            Some(SceneUpdate::Rings(patch)) => patch.words(),
            _ => 0,
        };
        let moved_words = words(&moved);
        let streamed_words: usize = streamed.iter().map(|u| words(&Some(u.clone()))).sum();
        eprintln!(
            "  slow frame: {took:.1?} (recentre {recentred:.1?}, stream {:.1?}), patch words {moved_words} + \
             {streamed_words}, rebuilt {rebuilt}, grew {grown:?}, {} batches shown, load {}; the \
             thread ran {:.1} ms and waited for a core {:.1} ms (ms, from the scheduler)",
            streaming.elapsed(),
            streamed.len(),
            std::fs::read_to_string("/proc/loadavg")
                .unwrap_or_default()
                .split_whitespace()
                .next()
                .unwrap_or("?"),
            sched.map_or(f64::NAN, |s| s.0),
            sched.map_or(f64::NAN, |s| s.1),
        );
    }
    updates.add(moved);
    for update in streamed {
        updates.add(Some(update));
    }
    updates.held_peak = updates.held_peak.max(world.store().streamed_len());
    updates.scene_peak = updates.scene_peak.max(world.used_bytes());
}

fn norm(v: [f64; 3]) -> f64 {
    v.iter().map(|c| c * c).sum::<f64>().sqrt()
}

/// The camera moving over the ground at `metres_per_s` for `frames` frames at
/// 60 a second, with the loader running: the scene work of each frame.
fn travel(
    world: &mut World,
    camera: &mut Camera,
    tables: &mut Tables,
    metres_per_s: f64,
    frames: usize,
) -> (SceneWork, Updates) {
    let frame = Duration::from_micros(16_667);
    let (mut work, mut updates) = (SceneWork::default(), Updates::default());
    let (ahead, _) = tangents(camera.horizon_up);
    let step = metres_per_s * VOXELS_PER_METRE / 60.0;
    let mut moved = camera.position;
    for _ in 0..frames {
        let tick = Instant::now();
        moved = std::array::from_fn(|a| moved[a] + ahead[a] * step);
        // On the sphere of the ground the camera is over, which a straight line leaves: a flight
        // of kilometres is far above the ground by its end.
        let (radius, length) = (norm(camera.position), norm(moved));
        moved = moved.map(|c| c / length * radius);
        camera.position = over_the_ground(&world.store(), moved);
        camera.carry_up(camera.position);
        scene_frame(world, camera.position, &mut work, &mut updates, tables);
        updates.lag.push(world.lag(camera.position));
        if updates.lag.len().is_multiple_of(60) {
            updates
                .edit_waits
                .push(world.edit_wait().unwrap().as_secs_f64() * 1e3);
        }
        std::thread::sleep(frame.saturating_sub(tick.elapsed()));
    }
    (work, updates)
}

/// A line on the host store and the process's resident memory, for the full-size test.
fn memory(world: &World, when: &str) -> String {
    let rss_mib = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:").map(str::to_owned))
        })
        .and_then(|line| line.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(f64::NAN, |kib| kib / 1024.0);
    let store = world.store();
    let bricks: usize = (0..store.level_count())
        .map(|l| store.level(l).map_or(0, |l| l.len()))
        .sum();
    format!(
        "memory {when}: store {bricks} bricks held, {} counted against the budget, {} evicted, {} \
         edited; scene {:.0} MiB used; process resident {rss_mib:.0} MiB",
        store.streamed_len(),
        store.evicted_len(),
        store.diverged_len(),
        world.used_bytes() as f64 / (1024.0 * 1024.0)
    )
}

/// What the full-size test holds the frame thread's scene work to, in milliseconds: the 99th
/// percentile and the worst frame. These are the targets of #127: 2 ms at the 99th percentile for
/// following the camera, 4 ms in all, and no frame over 8 ms, whether the camera is still (the
/// fill), walking at 5 m/s or flying at 48 m/s. The scene thread does the rest (see
/// [`crate::scene_worker`]); the 8-core dev box does it in a few tenths of a millisecond at the
/// 99th percentile, with a frame now and then of a few milliseconds when other programs are
/// running.
struct Held {
    /// Scene work in all, and following the camera alone, as (p99, worst).
    total: (f64, f64),
    following: (f64, f64),
}

const HELD: Held = Held {
    total: (4.0, 8.0),
    following: (2.0, 8.0),
};
const HELD_FILL: Held = HELD;
const HELD_WALK: Held = HELD;
const HELD_FLIGHT: Held = HELD;
/// The longest a full fill may take with the camera still, in seconds (it took 309 s before #113's
/// bricks were added, 411 to 424 s after with the frame thread taking them in, and 146 to 190 s
/// with the scene thread).
const HELD_FILL_SECS: f64 = 300.0;
/// The most the drawn scene may use per copy (the words of the bricks it draws, in MiB), near the
/// ground at the full bands: measured 474 MiB for 3.44 million drawn bricks at about 144 bytes
/// each (the words of the bricks and their regions). The prefetch simulator's shell at rest, 227 to
/// 259 MB (#49), is about half of that because it stops at the horizon, which the client does not
/// yet: it draws the whole Planet (#143 re-checked it, `prefetch-sim`'s `built_size`). The bands
/// bound the scene whatever the store holds, and the margins are not drawn, so this is a ceiling
/// on the client, with some room over what was measured.
const SCENE_MIB_MOST: f64 = 512.0;

impl Held {
    fn assert(&self, what: &str, work: &SceneWork, updates: &Updates) {
        let check = |label: &str, work: &SceneWork, (p99, worst): (f64, f64)| {
            let (got99, got_worst) = (work.p99().unwrap(), work.worst().unwrap());
            assert!(
                got99 <= p99 && got_worst <= worst,
                "{what}, {label}: p99 {got99:.2} ms (held to {p99}), worst {got_worst:.2} ms                  (held to {worst})"
            );
        };
        check("scene work in all", work, self.total);
        check("following the camera", &updates.recentre, self.following);
    }
}

/// The Planet at full size, streamed in as the client does it: the spawn ring and the Guard are
/// timed by `load`, then the resident set fills with the camera still (timed), and then the camera
/// walks at 5 m/s and then flies at 48 m/s with the loader running (the frame thread's scene work
/// per frame, worst and p99). Takes about four minutes (the fill is capped by
/// `FULLSIZE_FILL_CAP_SECS`, 5400 by default) and a lot of memory in a release build:
/// `cargo test --release -p client -- --ignored --nocapture full_size`. It prints each table's
/// reserved room against its peak (`room_for` in `main.rs` is sized from these) and fails if a
/// table grew or the scene was rebuilt in the fill or the walk (the flight asks for far more than
/// the loader makes, and only reports what grew), if the cap cut the fill short (a capped run is
/// for trying things out, and reports before it fails), or if the fill, the walk or the flight
/// is slower than [`Held`] says.
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
    let mut loaded = crate::load_planet();
    // For comparing with a store that is not evicted from: `FULLSIZE_NO_BUDGET=1`.
    if std::env::var_os("FULLSIZE_NO_BUDGET").is_some() {
        loaded.store.set_budget(brick_store::Budget {
            bricks: usize::MAX / 4,
            overflow: 0.0,
        });
    }
    let (client, _server) = loopback();
    let mut world =
        World::new(loaded.scene, Predictor::new(loaded.store), client).with_loader(loaded.loader);
    let mut camera = loaded.camera;
    let mut tables = Tables::new(&world);
    let frame = Duration::from_micros(16_667);

    // The resident set fills with the camera still. Reported on the way: when the ground under the
    // camera is first drawn at level 0, which the spawn ring has held since it was generated.
    let mut level_0_at = None;
    let (mut fill_work, mut fill_updates) = (SceneWork::default(), Updates::default());
    let began = Instant::now();
    let mut frames = 0;
    let mut last_report = Instant::now();
    // For trying things out: stop filling after this many seconds and go on to walking.
    let cap = std::env::var("FULLSIZE_FILL_CAP_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5400);
    // Whether the loader went idle, which is a full fill.
    let mut filled = false;
    loop {
        let tick = Instant::now();
        scene_frame(
            &mut world,
            camera.position,
            &mut fill_work,
            &mut fill_updates,
            &mut tables,
        );
        frames += 1;
        if level_0_at.is_none() {
            let down = ray_through(
                camera.position,
                std::array::from_fn(|a| camera.position[a] - camera.horizon_up[a]),
            );
            if let Some(hit) = trace(world.scene(), &down)
                && world.scene().level_at(hit.voxel) == Some(0)
            {
                level_0_at = Some(began.elapsed());
                eprintln!(
                    "  the ground under the camera is level 0 after {:.2?}",
                    began.elapsed()
                );
            }
        }
        if idle(&world) {
            filled = true;
            break;
        }
        if began.elapsed() > Duration::from_secs(cap) {
            eprintln!("  (filling stopped after {cap} s)");
            break;
        }
        if last_report.elapsed() > Duration::from_secs(30) {
            last_report = Instant::now();
            let drawn: usize = (0..LEVELS).map(|l| world.level_bricks(l)).sum();
            eprintln!(
                "  {:.0?}: {frames} frames, {} waiting, {} pending, {} generated, {drawn} drawn",
                began.elapsed(),
                world.waiting(),
                world.loader().unwrap().pending,
                world.loader().unwrap().generated
            );
        }
        std::thread::sleep(frame.saturating_sub(tick.elapsed()));
    }
    let fill_took = began.elapsed();
    let bricks: Vec<usize> = (0..LEVELS).map(|l| world.level_bricks(l)).collect();
    eprintln!(
        "resident set complete for the bands {:.1?} after the first frame ({frames} frames): {} \
         bricks generated, {} drawn (per level {bricks:?}), scene {:.0} MiB used of {:.0} MiB \
         reserved; {} patches, {} rebuilds, the largest patch {} words; level 0 under the camera \
         at {level_0_at:?}",
        began.elapsed(),
        world.loader().unwrap().generated,
        bricks.iter().sum::<usize>(),
        world.used_bytes() as f64 / (1024.0 * 1024.0),
        world.scene().reserved_bytes() as f64 / (1024.0 * 1024.0),
        fill_updates.patched,
        fill_updates.rebuilt,
        fill_updates.worst_words,
    );
    tables.report();
    eprintln!("{}", memory(&world, "after the fill"));
    eprintln!("filling: {}", fill_work.summary().unwrap());
    eprintln!(
        "  of which following the camera: {}; taking in bricks: {}",
        fill_updates.recentre.summary().unwrap(),
        fill_updates.stream.summary().unwrap()
    );

    // Then it walks, 5 m/s over the ground.
    let (walk_work, walk_updates) = travel(&mut world, &mut camera, &mut tables, 5.0, 1800);
    eprintln!(
        "walking 150 m: {}; {} patches, {} rebuilds, the largest patch {} words; {} waiting, {} \
         pending, {} bricks generated in all",
        walk_work.summary().unwrap(),
        walk_updates.patched,
        walk_updates.rebuilt,
        walk_updates.worst_words,
        world.waiting(),
        world.loader().unwrap().pending,
        world.loader().unwrap().generated,
    );
    eprintln!(
        "  of which following the camera: {}; taking in bricks: {}",
        walk_updates.recentre.summary().unwrap(),
        walk_updates.stream.summary().unwrap()
    );
    eprintln!("{}", walk_updates.report(5.0 * VOXELS_PER_METRE / 60.0));
    tables.report();
    // The reservations hold over the fill and the walk: no table grew and nothing was packed again.
    tables.assert_none_grew();
    HELD_FILL.assert("the fill", &fill_work, &fill_updates);
    HELD_WALK.assert("the walk", &walk_work, &walk_updates);
    assert!(
        fill_took.as_secs_f64() <= HELD_FILL_SECS,
        "the fill took {fill_took:.1?}, held to {HELD_FILL_SECS} s"
    );

    // Then it flies, 48 m/s over the ground: ten times the walk's distance a frame. The
    // reservations are for the fill and the walk, so the flight is not held to them. It goes on
    // for 5 km (#141), in legs, with the host store and the resident set reported after each.
    let (mut fly_work, mut fly_updates) = (SceneWork::default(), Updates::default());
    let scene_after_fill = world.used_bytes();
    let generated_before_flight = world.loader().unwrap().generated;
    for leg in 1..=6 {
        let (work, updates) = travel(&mut world, &mut camera, &mut tables, 48.0, 1050);
        eprintln!("{}", memory(&world, &format!("flight, {} m", leg * 840)));
        fly_work.merge(work);
        fly_updates.merge(updates);
    }
    eprintln!(
        "flying 5 km: {}; {} patches, {} rebuilds, the largest patch {} words",
        fly_work.summary().unwrap(),
        fly_updates.patched,
        fly_updates.rebuilt,
        fly_updates.worst_words,
    );
    eprintln!(
        "  of which following the camera: {}; taking in bricks: {}",
        fly_updates.recentre.summary().unwrap(),
        fly_updates.stream.summary().unwrap()
    );
    eprintln!(
        "  the flight generated {} bricks, {} drawn at the end",
        world.loader().unwrap().generated - generated_before_flight,
        (0..LEVELS).map(|l| world.level_bricks(l)).sum::<usize>()
    );
    eprintln!("{}", fly_updates.report(48.0 * VOXELS_PER_METRE / 60.0));
    tables.report();
    eprintln!(
        "  tables that grew in the flight (it asks for far more than the loader makes in a frame): {:?}",
        tables.grown
    );
    HELD_FLIGHT.assert("the flight", &fly_work, &fly_updates);
    // The host store stays within its budget and the overflow (#141): with the flight asking for
    // far more than the loader makes, the most it counted at the end of any frame is bounded by
    // the limit, not growing with the distance; and the scene, which the bands bound, does not
    // grow either.
    let store = world.store();
    let budget = store.budget().unwrap();
    let mib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
    let eviction = world.stats().eviction;
    eprintln!(
        "the most the store counted at the end of a frame in the flight: {}; the budget {} with its \
         overflow {}; {} bricks evicted in {} passes",
        fly_updates.held_peak,
        budget.bricks,
        budget.limit(),
        store.evicted_len(),
        eviction.passes
    );
    eprintln!(
        "eviction: the last pass took {:.1?} to list the units, the longest {:.1?}; the store's read \
         lock was held {} times to look at units, {:.2?} at a time on average and {:.2?} at the \
         longest; its write lock {:.2?} at the longest",
        eviction.listing,
        eviction.longest_listing,
        eviction.reads,
        eviction.read_total / eviction.reads.max(1) as u32,
        eviction.longest_read,
        eviction.longest_write
    );
    eprintln!(
        "the scene: {:.0} MiB used after the fill, at most {:.0} MiB in the flight (the prefetch \
         simulator's horizon-capped shell at rest near the ground is 227 to 259 MB; the client \
         draws the whole Planet)",
        mib(scene_after_fill),
        mib(fly_updates.scene_peak)
    );
    assert!(
        fly_updates.scene_peak <= scene_after_fill + scene_after_fill / 20,
        "the scene grew in the flight"
    );
    assert!(
        mib(scene_after_fill.max(fly_updates.scene_peak)) <= SCENE_MIB_MOST,
        "the scene used {:.0} MiB, held to {SCENE_MIB_MOST}",
        mib(scene_after_fill.max(fly_updates.scene_peak))
    );
    eprintln!(
        "steps of coarsening at the end: {}",
        world.stats().coarsening
    );
    if std::env::var_os("FULLSIZE_NO_BUDGET").is_none() {
        // The budget is the complete set with the margins: the bands are not coarsened to fit it.
        assert_eq!(world.stats().coarsening, 0);
        assert!(
            fly_updates.held_peak <= budget.limit(),
            "the store counted {} against a limit of {}",
            fly_updates.held_peak,
            budget.limit()
        );
        assert!(
            store.evicted_len() > 0,
            "the flight was long enough to evict"
        );
    }
    drop(store);
    // Last, so that a capped run reports the walk and the flight too.
    assert!(
        filled,
        "the fill stopped at the {cap} s cap before the loader went idle, so no growth over a \
         full fill was checked"
    );
}

#[test]
fn the_spawn_ring_is_drawn_at_level_0_within_seconds_and_before_the_far_bricks_are_resident() {
    let (mut world, _server, camera) = spawned_world(2);
    let down = ray_through(
        camera.position,
        std::array::from_fn(|a| camera.position[a] - camera.horizon_up[a]),
    );
    let begun = Instant::now();
    let level_0_at = loop {
        stream(&mut world, camera.position);
        let hit = trace(world.scene(), &down).expect("ground under the camera");
        if world.scene().level_at(hit.voxel) == Some(0) {
            break begun.elapsed();
        }
        assert!(
            begun.elapsed() < Duration::from_secs(20),
            "still level {:?} under the camera",
            world.scene().level_at(hit.voxel)
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    // The ground under the camera comes before the set is complete: nearest first, end to end.
    assert!(
        world.waiting() > 100,
        "{} waiting after {level_0_at:?}",
        world.waiting()
    );
    eprintln!("level 0 under the camera after {level_0_at:?} (debug build)");
}

/// Frames, a few milliseconds apart, until the loader's threads have taken `MAX_AHEAD` jobs and
/// finished every one they had in flight, which leaves them nothing to do until something is
/// taken in. None of the frames takes anything in. Then nothing more is generated, however long
/// it waits: this does not depend on how fast the machine is.
fn let_the_workers_run_ahead(world: &mut World, camera: [f64; 3]) {
    // What the scene thread was taking in when loading was paused is done by now.
    world.sync().unwrap();
    let started = Instant::now();
    loop {
        assert!(stream(world, camera).is_empty());
        let loader = world.loader().unwrap();
        // `ahead` only goes up while nothing is taken in, so once it is at the most, the jobs in
        // flight can only finish.
        if loader.ahead == crate::loader::MAX_AHEAD && loader.in_flight == 0 {
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "no work ahead");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn while_loading_is_held_nothing_is_taken_in_and_what_waits_is_bounded() {
    let (mut world, _server, camera) = spawned_world(2);
    let waiting = world.waiting();
    assert!(waiting > 0, "the Guard stands in for everything");
    world.pause_loading(true);
    assert!(world.loading_paused());
    let_the_workers_run_ahead(&mut world, camera.position);

    // The scene did not change, and with the camera still nor did the rings.
    assert_eq!(world.waiting(), waiting);
    assert!(world.recentre(camera.position).unwrap().is_none());
    // What was generated and not taken in is what the workers are ahead by, and they have
    // stopped: nothing more is generated however many frames pass.
    let loader = world.loader().unwrap();
    assert_eq!(loader.ahead, crate::loader::MAX_AHEAD);
    let generated = loader.generated;
    // The wait is only for there to be time for a worker that has not stopped to generate: that
    // none has is not a matter of how long the wait is, so no runner can outlast it.
    for _ in 0..20 {
        assert!(stream(&mut world, camera.position).is_empty());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(world.loader().unwrap().ahead, crate::loader::MAX_AHEAD);
    assert_eq!(world.loader().unwrap().generated, generated);
}

#[test]
fn letting_go_takes_in_what_was_held_and_the_bands_fill_as_before() {
    let (mut world, _server, camera) = spawned_world(2);
    world.pause_loading(true);
    let_the_workers_run_ahead(&mut world, camera.position);
    world.pause_loading(false);
    assert!(!world.loading_paused());
    // What was held is waiting, so the first frame to see a change has some taken in.
    let mut first = Vec::new();
    while first.is_empty() {
        first = stream(&mut world, camera.position);
        std::thread::sleep(Duration::from_millis(1));
    }
    // The scene thread takes in what was held a slice at a time, not all of it at once.
    assert!(world.waiting() > 0, "not all in one slice");
    fill(&mut world, camera.position);
    assert_eq!(world.waiting(), 0);
    assert_eq!(world.loader().unwrap().pending, 0);
}

#[test]
fn an_edit_while_loading_is_held_is_shown() {
    let (mut world, _server, camera) = spawned_world(2);
    fill(&mut world, camera.position);
    let aim = within_reach(&world, &camera);
    let before = crate::world::centre_hit(world.scene(), &aim).expect("ground to dig");
    // The camera moves on, which gives the loader work again, and loading is paused.
    let (ahead, _) = tangents(camera.horizon_up);
    let moved: [f64; 3] = std::array::from_fn(|a| camera.position[a] + 100.0 * ahead[a]);
    world.recentre(moved).unwrap();
    world.pause_loading(true);
    world.sync().unwrap();
    for _ in 0..20 {
        assert!(stream(&mut world, moved).is_empty());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(world.loader().unwrap().pending > 0, "the loader has work");
    let update = dig_and_wait(&mut world, &aim);
    assert!(matches!(update, SceneUpdate::Rings(_)));
    // The Voxels that were dug are gone from the scene: the view hits something further on.
    let after = crate::world::centre_hit(world.scene(), &aim);
    assert!(
        after.is_none_or(|hit| hit.t > before.t),
        "{:?} then {:?}",
        before.t,
        after.map(|h| h.t)
    );
}

#[test]
fn flying_far_while_loading_is_paused_does_not_grow_the_queue_without_bound() {
    let (mut world, _server, camera) = spawned_world(2);
    fill(&mut world, camera.position);
    world.pause_loading(true);
    let (ahead, _) = tangents(camera.horizon_up);
    let radius = camera.position.iter().map(|c| c * c).sum::<f64>().sqrt();
    // Out and back over several bands' worth, in steps that make the rings follow.
    let steps = 60;
    for step in 0..2 * steps {
        let along = if step < steps { step } else { 2 * steps - step };
        // Round the Planet, at the height of the spawn.
        let flat: [f64; 3] =
            std::array::from_fn(|a| camera.position[a] + 0.5 * D0 * along as f64 * ahead[a]);
        let scale = radius / flat.iter().map(|c| c * c).sum::<f64>().sqrt();
        let at = flat.map(|c| c * scale);
        world.recentre(at).unwrap();
        assert!(stream(&mut world, at).is_empty());
        let loader = world.loader().unwrap();
        let (queued, cancelled) = (loader.queued, loader.cancelled);
        // Jobs given up on are not kept past a share of the queue (with some room for a frame
        // that could not hand its cancels over).
        assert!(
            cancelled <= 2 * (queued / 4).max(256),
            "step {step}: {queued} queued, {cancelled} cancelled"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// One frame of the App's, with no GPU: loading pauses as [`loading_paused_for`] says for how
/// long the view (here the camera, which is all that moves) has been still, and what the
/// loader has generated is taken in, or not.
fn frame(
    world: &mut World,
    still: &mut Stillness<[f64; 3]>,
    mode: Mode,
    camera: [f64; 3],
) -> Vec<SceneUpdate> {
    world.pause_loading(loading_paused_for(mode, still.observe(camera)));
    stream(world, camera)
}

#[test]
fn loading_pauses_only_for_a_reference_that_has_a_still_view() {
    assert!(!loading_paused_for(Mode::Reference, 0));
    assert!(!loading_paused_for(Mode::Reference, STILL_FRAMES - 1));
    assert!(loading_paused_for(Mode::Reference, STILL_FRAMES));
    assert!(loading_paused_for(Mode::Reference, 1000));
    // Primary is the playable view: it loads whatever the view does.
    assert!(!loading_paused_for(Mode::Primary, 1000));
}

#[test]
fn a_still_reference_takes_in_nothing_once_it_has_been_still_for_a_while() {
    let (mut world, _server, camera) = spawned_world(2);
    let mut still = Stillness::default();
    // The first frames of a view are not paused.
    for _ in 0..=STILL_FRAMES {
        frame(&mut world, &mut still, Mode::Reference, camera.position);
    }
    assert!(world.loading_paused());
    let_the_workers_run_ahead(&mut world, camera.position);
    for _ in 0..20 {
        let updates = frame(&mut world, &mut still, Mode::Reference, camera.position);
        assert!(updates.is_empty());
        assert!(world.loading_paused());
    }
    assert!(world.loader().unwrap().pending > 0, "the loader has work");
}

#[test]
fn a_moving_reference_takes_in_arrivals_and_the_ground_under_it_reaches_level_0() {
    let (mut world, _server, camera) = spawned_world(2);
    let (ahead, _) = tangents(camera.horizon_up);
    let mut still = Stillness::default();
    let begun = Instant::now();
    let mut taken = 0;
    for step in 0u32.. {
        // Back and forth over a few metres: never the same view in two frames.
        let along = 3.0 * (f64::from(step) * 0.05).sin();
        let position = over_the_ground(
            &world.store(),
            std::array::from_fn(|a| camera.position[a] + along * ahead[a]),
        );
        taken += usize::from(world.recentre(position).unwrap().is_some());
        taken += frame(&mut world, &mut still, Mode::Reference, position).len();
        assert!(!world.loading_paused(), "step {step}");
        let down = ray_through(
            position,
            std::array::from_fn(|a| position[a] - camera.horizon_up[a]),
        );
        let hit = trace(world.scene(), &down).expect("ground under the camera");
        if world.scene().level_at(hit.voxel) == Some(0) {
            break;
        }
        assert!(
            begun.elapsed() < Duration::from_secs(60),
            "still level {:?} under the camera",
            world.scene().level_at(hit.voxel)
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(taken > 0, "arrivals were taken in");
}

#[test]
fn still_then_moving_then_still_pauses_loading_resumes_it_the_frame_it_moves_and_pauses_again() {
    let (mut world, _server, camera) = spawned_world(2);
    let (ahead, _) = tangents(camera.horizon_up);
    let mut still = Stillness::default();
    let reference = |world: &mut World, still: &mut Stillness<[f64; 3]>, at: [f64; 3]| {
        frame(world, still, Mode::Reference, at)
    };

    // Still: paused after the first frames, with the workers run ahead.
    for _ in 0..=STILL_FRAMES {
        reference(&mut world, &mut still, camera.position);
    }
    let_the_workers_run_ahead(&mut world, camera.position);
    assert!(reference(&mut world, &mut still, camera.position).is_empty());
    assert!(world.loading_paused());

    // Moving: from the frame the camera moves, what was held is taken in (in the first frames,
    // not in the first if the thread is kept from running for a while).
    let step = |at: [f64; 3]| -> [f64; 3] { std::array::from_fn(|a| at[a] + 0.5 * ahead[a]) };
    let mut at = camera.position;
    let started = Instant::now();
    loop {
        at = step(at);
        let updates = reference(&mut world, &mut still, at);
        assert!(!world.loading_paused());
        if !updates.is_empty() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "nothing taken in"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let moved_again = at;

    // Still again: loading goes on for the first frames of the new view, then pauses.
    for _ in 0..STILL_FRAMES {
        assert!(!world.loading_paused());
        reference(&mut world, &mut still, moved_again);
    }
    assert!(world.loading_paused());
    let_the_workers_run_ahead(&mut world, moved_again);
    for _ in 0..10 {
        assert!(reference(&mut world, &mut still, moved_again).is_empty());
    }
}

#[test]
fn stillness_counts_frames_with_an_equal_key_and_starts_over_for_another() {
    let mut still = Stillness::default();
    assert_eq!(still.observe(1), 0);
    assert_eq!(still.observe(1), 1);
    assert_eq!(still.observe(1), 2);
    assert_eq!(still.observe(2), 0);
    assert_eq!(still.observe(2), 1);
    assert_eq!(still.observe(1), 0);
}

/// Bricks that the copy of the scene lists at each level, from its regions.
fn listed(scene: &Scene) -> Vec<usize> {
    scene
        .levels
        .iter()
        .map(|l| {
            (l.table_base..=l.table_base + l.table_mask)
                .map(|slot| {
                    let entry = &scene.table[slot as usize * 4..][..4];
                    if entry[3] == 0 {
                        return 0;
                    }
                    let record = &scene.regions[(entry[3] as usize - 1) * REGION_WORDS..][..16];
                    record
                        .iter()
                        .map(|w| w.count_ones() as usize)
                        .sum::<usize>()
                })
                .sum()
        })
        .collect()
}

#[test]
fn what_the_scene_thread_counts_as_drawn_is_what_the_frame_threads_copy_lists() {
    let (mut world, _server, camera) = spawned_world(2);
    // Part way through the fill, and when it is done.
    for _ in 0..30 {
        stream(&mut world, camera.position);
        std::thread::sleep(Duration::from_millis(3));
    }
    world.sync().unwrap();
    assert_eq!(listed(world.scene()), world.stats().level_bricks[..LEVELS]);
    fill(&mut world, camera.position);
    assert_eq!(listed(world.scene()), world.stats().level_bricks[..LEVELS]);
    // And after the rings have followed the camera, and the bands have filled again.
    let (ahead, _) = tangents(camera.horizon_up);
    let moved: [f64; 3] = std::array::from_fn(|a| camera.position[a] + 150.0 * ahead[a]);
    world.recentre(moved).unwrap();
    fill(&mut world, moved);
    assert_eq!(listed(world.scene()), world.stats().level_bricks[..LEVELS]);
    assert!(world.stats().level_bricks.iter().sum::<usize>() > 1000);
}

#[test]
fn an_edit_is_in_the_scene_the_frame_after_the_one_that_makes_it_while_the_camera_is_followed() {
    let (mut world, server, camera) = spawned_world(2);
    fill(&mut world, camera.position);
    let aim = up_close(&mut world, &camera);
    let depth = |world: &World| {
        crate::world::centre_hit(world.scene(), &aim)
            .expect("ground")
            .t
    };
    let whole = depth(&world);
    // A build that is not optimised takes longer than a few milliseconds to pack the Edit.
    world.set_edit_wait(Duration::from_secs(20));

    // The camera is on the move, so the scene thread has the rings to follow when the Edit is
    // made, and it is busy for a while with that before it comes to the Edit.
    let (ahead, _) = tangents(camera.horizon_up);
    let at = |n: i32| -> [f64; 3] {
        std::array::from_fn(|a| aim.position[a] + 2.0 * f64::from(n) * ahead[a])
    };
    for n in 1..=5 {
        world.recentre(at(n)).unwrap();
    }
    // The coarse levels are slow to remake (each step of them sleeps), which must not hold up the
    // Edit that is seen, nor the frame thread's reads of the store. And the thread is busy for a
    // little when the Edit is made, so that it is not done in the frame that makes it, however
    // quick the machine: the frame after waits a few milliseconds for it, and has it.
    world.worker().slow_coarse(Duration::from_millis(40));
    world.worker().stall(Duration::from_millis(2));
    world.edit(&aim, &crate::tool::Tool::default()).unwrap();
    // The frame of the click goes on and looks at the scene, which may have the Edit already.
    world.recentre(at(6)).unwrap();
    // And the frame after has it, waiting for the thread to have packed it and not for the coarse
    // levels, which take far longer here.
    let looking = Instant::now();
    world.recentre(at(7)).unwrap();
    assert!(
        looking.elapsed() < Duration::from_secs(2),
        "the coarse levels take seconds here: {:?}",
        looking.elapsed()
    );
    let dug = depth(&world);
    assert!(dug > whole + 1.0, "the hole shows: {whole}, then {dug}");
    // The frame thread reads the store (the character, the ghost) while the coarse levels are
    // remade in steps, and waits for no more than a step.
    let mut longest = Duration::ZERO;
    for _ in 0..40 {
        let asked = Instant::now();
        drop(world.store());
        longest = longest.max(asked.elapsed());
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(longest < Duration::from_millis(25), "{longest:?}");
    world.worker().slow_coarse(Duration::ZERO);

    // The server turns it down: the ground is back in the call that hears of it.
    let Ok(ClientMessage::Edit { .. }) = server.try_recv() else {
        panic!("no Edit was sent");
    };
    server
        .send(ServerMessage::Rejected {
            client_seq: 0,
            reason: RejectReason::BrushTooLarge,
        })
        .unwrap();
    assert!(world.poll().unwrap().is_some());
    assert!(
        (depth(&world) - whole).abs() < 0.5,
        "{whole} before, {} after the undo",
        depth(&world)
    );
}

#[test]
fn an_edit_the_scene_thread_is_too_busy_to_pack_shows_when_it_has_and_the_frame_does_not_wait() {
    let (mut world, _server, camera) = spawned_world(2);
    fill(&mut world, camera.position);
    let aim = up_close(&mut world, &camera);
    let depth = |world: &World| {
        crate::world::centre_hit(world.scene(), &aim)
            .expect("ground")
            .t
    };
    let whole = depth(&world);
    world.worker().stall(Duration::from_millis(300));
    world.edit(&aim, &crate::tool::Tool::default()).unwrap();
    // The two looks of the frame and the frame after: the thread is busy, and the frame thread
    // waits a few milliseconds for it and goes on with the Edit not shown.
    for _ in 0..2 {
        let looking = Instant::now();
        world.recentre(aim.position).unwrap();
        assert!(looking.elapsed() < Duration::from_millis(30));
    }
    assert!((depth(&world) - whole).abs() < 0.5, "not shown yet");
    // It shows once the thread has packed it.
    let started = Instant::now();
    while (depth(&world) - whole).abs() < 0.5 {
        assert!(started.elapsed() < Duration::from_secs(5), "never shown");
        std::thread::sleep(Duration::from_millis(10));
        world.recentre(aim.position).unwrap();
    }
}

#[test]
fn changes_made_while_a_copy_of_the_whole_scene_waits_go_into_the_copy() {
    let (mut world, _server, camera) = spawned_world(2);
    fill(&mut world, camera.position);
    world.sync().unwrap();
    // Other bands pack the scene again, and the camera then moves, with nothing taken in between:
    // what the thread made after the copy is pasted into it, and what is taken is one whole state.
    world.worker().set_bands(Bands::with_d0(D0 * 1.2)).unwrap();
    let (ahead, _) = tangents(camera.horizon_up);
    for n in 1..=3 {
        let at: [f64; 3] = std::array::from_fn(|a| camera.position[a] + 20.0 * n as f64 * ahead[a]);
        world.worker().follow_moving(at, [0.0; 3]);
        world.worker().sync().unwrap();
    }
    let ready = world.worker().take(true).unwrap();
    let copy = ready.mirror.expect("the copy waits");
    assert!(ready.patch.is_none());
    let stats = ready.stats.unwrap();
    assert_eq!(listed(&copy), stats.level_bricks[..LEVELS]);
}

#[test]
fn a_job_that_is_done_is_always_there_to_be_taken_even_as_the_thread_goes_on_publishing() {
    let (mut world, _server, camera) = spawned_world(2);
    let (ahead, _) = tangents(camera.horizon_up);
    for n in 0..30 {
        // The thread has the loader's arrivals and the camera to publish all the time.
        let at: [f64; 3] = std::array::from_fn(|a| camera.position[a] + 3.0 * n as f64 * ahead[a]);
        world.recentre(at).unwrap();
        let bands = Bands::with_d0(if n % 2 == 0 { D0 * 1.1 } else { D0 });
        let update = world.set_bands(bands).unwrap();
        assert!(update.is_some(), "set {n}");
    }
}

#[test]
#[should_panic(expected = "the scene thread panicked")]
fn a_panic_on_the_scene_thread_is_a_panic_on_the_frame_thread_not_a_wait() {
    let (world, _server, _camera) = spawned_world(1);
    world.worker().panic();
}

/// What the frame thread does for the largest Brush at the spawn, with every wait in it, as the
/// client's frame loop does: the click, the reads of the store that the character and the ghost
/// make while the scene thread remakes the coarse levels (which at the spawn are read from the
/// generator, and take tens of milliseconds), and the looks at the scene that wait for the Edit
/// and paste it. In release each is held to a few milliseconds; a build that is not optimised only
/// runs it, with a smaller Brush. `-- --nocapture` prints them. The ghost, which is made again
/// after a click (its plan, and its outline from the masks), is held to the same few milliseconds.
#[test]
fn the_frame_thread_takes_a_few_milliseconds_of_the_largest_edit_at_the_spawn_with_every_wait() {
    use crate::tool::Tool;
    use net::{EditMode, Shape};

    let release = !cfg!(debug_assertions);
    let (mut world, _server, camera) = spawned_world(2);
    fill(&mut world, camera.position);
    let aim = up_close(&mut world, &camera);
    let radius = if release { sim::MAX_BRUSH_RADIUS } else { 12 };
    let tool = |mode| Tool {
        mode,
        shape: Shape::Cube,
        radius,
        ..Tool::default()
    };
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
    // The worst of each, over the Edits, in milliseconds.
    let (mut store, mut ghost, mut look) = (0f64, 0f64, 0f64);
    let mut shown = 0;
    let mut clicks = Vec::new();
    for n in 0..8 {
        let mode = if n % 2 == 0 {
            EditMode::Dig
        } else {
            EditMode::Place
        };
        // The allowance is a tick's credit at a time: a rest of 10 seconds fills it.
        for _ in 0..600 {
            world.walk(net::MoveInput::default()).unwrap();
        }
        world.hide_ghost();
        let before = crate::world::centre_hit(world.scene(), &aim).map(|h| h.t);
        let started = Instant::now();
        world.edit(&aim, &tool(mode)).unwrap();
        clicks.push(ms(started));
        // The frames after: each reads the store as the character does, makes the ghost of the
        // next click, and looks at the scene, as the frame does, with the time of a frame between.
        for _ in 0..6 {
            let asked = Instant::now();
            drop(world.store());
            store = store.max(ms(asked));
            let asked = Instant::now();
            world.show_ghost(&aim, &tool(mode));
            ghost = ghost.max(ms(asked));
            let asked = Instant::now();
            world.recentre(aim.position).unwrap();
            look = look.max(ms(asked));
            std::thread::sleep(Duration::from_millis(16));
        }
        let after = crate::world::centre_hit(world.scene(), &aim).map(|h| h.t);
        shown += usize::from(before != after);
    }
    clicks.sort_by(f64::total_cmp);
    let (median, worst) = (clicks[clicks.len() / 2], clicks[clicks.len() - 1]);
    eprintln!(
        "frame thread, 8 Edits of a cube of {radius}, in ms: click median {median:.2} worst \
         {worst:.2}; the worst read of the store {store:.2}, look at the scene (the wait for the \
         Edit, and the paste) {look:.2}, ghost made after a click {ghost:.2}"
    );
    assert!(shown >= 6, "the Edits show: {shown} of 8");
    if release {
        // The click is the plan, which reads bricks the generator makes the first time (the worst
        // click), and the write. The store is not held for more than a step of the coarse levels,
        // and the looks wait for the Edit's pack for a few milliseconds at the most.
        assert!(median < 5.0 && worst < 12.0, "a hitch in the click");
        assert!(store < 2.0 && look < 6.0, "a wait: {store} and {look}");
        assert!(ghost < 10.0, "the ghost: {ghost}");
    }
}

/// A world like [`spawned_world`] whose store has the `budget`.
fn budgeted_world(workers: usize, budget: brick_store::Budget) -> (World, ServerEnd, Camera) {
    let mut store = spawned_store();
    store.set_budget(budget);
    let camera = crate::spawn_camera(ground());
    let scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    let loader = Loader::new(store.source().unwrap(), camera.position, bands(), workers);
    let (client, server) = loopback();
    let world = World::new(scene, Predictor::new(store), client).with_loader(loader);
    (world, server, camera)
}

/// The camera moved `frames` frames of `step` Voxels along the ground in `direction`, streaming.
fn fly(
    world: &mut World,
    from: [f64; 3],
    direction: [f64; 3],
    step: f64,
    frames: usize,
) -> [f64; 3] {
    let mut moved = from;
    let mut position = from;
    for _ in 0..frames {
        moved = std::array::from_fn(|a| moved[a] + direction[a] * step);
        position = over_the_ground(&world.store(), moved);
        world.recentre(position).unwrap();
        stream(world, position);
        std::thread::sleep(Duration::from_millis(2));
    }
    position
}

/// Streams with the camera at `camera` until the resident set is complete and the store is within
/// its budget's limit.
fn settle(world: &mut World, camera: [f64; 3]) {
    let started = Instant::now();
    loop {
        stream(world, camera);
        world.sync().unwrap();
        if idle(world) && !world.store().over_limit() {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "does not settle: idle {}, {} held of a limit of {}, {} evicted, {} edited",
            idle(world),
            world.store().streamed_len(),
            world.store().budget().unwrap().limit(),
            world.store().evicted_len(),
            world.store().diverged_len()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Frames of 20 Voxels in each leg of the flight away and of the flight back: a shell holds fewer
/// bricks since #142 leaves the air and the buried bricks out of a unit, and what one shell adds to
/// the store beyond the ones before is a few per cent, so two legs take it over its limit.
const FLIGHT_FRAMES: usize = 1500;

#[test]
fn flying_away_and_back_keeps_the_store_within_its_budget_and_leaves_no_hole_or_stale_edit() {
    // The budget is what the complete set holds with the camera at the spawn, and 7 per cent over:
    // the complete set elsewhere is a few per cent more or less, and a store that cannot get under
    // its limit with everything wanted is left alone by eviction for a while (`EVICT_RETRY`), so
    // the margin is the most that the flight takes the store over (3 per cent a shell), less a little.
    let (mut first, _s, spawn) = spawned_world(2);
    fill(&mut first, spawn.position);
    first.sync().unwrap();
    let full = first.store().streamed_len();
    drop(first);
    let budget = brick_store::Budget {
        bricks: full * 107 / 100,
        overflow: 0.01,
    };
    let (mut world, _server, spawn) = budgeted_world(2, budget);
    fill(&mut world, spawn.position);

    // Dig at the spawn, so that there is an Edit to keep.
    let aim = up_close(&mut world, &spawn);
    let at = crate::world::centre_hit(world.scene(), &aim)
        .expect("ground")
        .voxel;
    assert!(world.store().voxel_at(0, VoxelCoord(at)).is_some());
    // The scene shows the crater: a ray at the Voxel goes deeper than it did.
    let ray = ray_through(aim.position, at.map(f64::from));
    let before_t = trace(world.scene(), &ray).expect("ground").t;
    dig_and_wait(&mut world, &aim);
    let dug_t = trace(world.scene(), &ray).expect("ground").t;
    assert!(dug_t > before_t + 2.0, "{before_t} {dug_t}");
    assert!(world.store().voxel_at(0, VoxelCoord(at)).is_none());
    assert!(world.store().diverged_len() > 0);
    // A brick of the ground 60 Voxels along from the crater, well outside what the crater keeps.
    let (ahead, right) = tangents(spawn.horizon_up);
    let probe = {
        let store = world.store();
        let beside = over_the_ground(
            &store,
            std::array::from_fn(|a| at[a] as f64 + ahead[a] * 60.0),
        );
        let up = beside.map(|c| c / norm(beside));
        let brick = (0..80)
            .map(|i| {
                VoxelCoord(std::array::from_fn(|a| {
                    (beside[a] - up[a] * f64::from(i)).round() as i32
                }))
                .brick()
            })
            .find(|&b| store.holds(0, b))
            .expect("the ground beside the crater is held");
        assert!(store.children_resident(1, brick.parent()));
        brick
    };

    // Fly away far enough that the spawn's units are left behind, and then on to the side, so that
    // the shells that it passes through are three and more than the limit holds, and settle there.
    let turn = fly(&mut world, aim.position, ahead, 20.0, FLIGHT_FRAMES);
    let away = fly(&mut world, turn, right, 20.0, FLIGHT_FRAMES);
    settle(&mut world, away);
    {
        let store = world.store();
        eprintln!(
            "complete set {full}; after the flight {} (limit {}), {} bricks evicted",
            store.streamed_len(),
            budget.limit(),
            store.evicted_len()
        );
        assert!(store.streamed_len() <= budget.limit());
        assert!(store.evicted_len() > 0, "the flight left units to evict");
        assert!(store.diverged_len() > 0, "the Edit is still the store's");
        assert!(
            !store.children_resident(1, probe.parent()) && !store.holds(0, probe),
            "a unit near the spawn that nothing keeps was evicted"
        );
    }

    // Back at the spawn the area is streamed in again: ground at level 0 under the camera, and the
    // crater where it was dug.
    let turn = fly(&mut world, away, right.map(|c| -c), 20.0, FLIGHT_FRAMES);
    fly(&mut world, turn, ahead.map(|c| -c), 20.0, FLIGHT_FRAMES);
    let back = aim.position;
    world.recentre(back).unwrap();
    settle(&mut world, back);
    let down = ray_through(back, std::array::from_fn(|a| back[a] - spawn.horizon_up[a]));
    let hit = trace(world.scene(), &down).expect("ground under the camera");
    assert_eq!(world.scene().level_at(hit.voxel), Some(0));
    assert!(world.store().voxel_at(0, VoxelCoord(at)).is_none());
    assert!(
        world.store().holds(0, probe),
        "the unit that was evicted is streamed in again"
    );
    let again = trace(world.scene(), &ray).expect("ground").t;
    assert!(
        (again - dug_t).abs() <= 1.0,
        "the crater is in the scene: the ray hit at {dug_t} when it was dug and at {again} now"
    );
    assert!(world.store().streamed_len() <= budget.limit());
}

/// A world like [`spawned_world`] with the residency margins of ADR 0004 and a `budget` if given.
fn margined_world(
    workers: usize,
    budget: Option<brick_store::Budget>,
) -> (World, ServerEnd, Camera) {
    let mut store = spawned_store();
    if let Some(budget) = budget {
        store.set_budget(budget);
    }
    let camera = crate::spawn_camera(ground());
    let mut scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    scene.set_margins(&store, render::lod::Margins::standard());
    let loader = Loader::new(store.source().unwrap(), camera.position, bands(), workers);
    let (client, server) = loopback();
    let world = World::new(scene, Predictor::new(store), client).with_loader(loader);
    (world, server, camera)
}

#[test]
fn margins_make_more_of_the_store_resident_and_draw_no_more() {
    let (mut plain, _s, camera) = spawned_world(2);
    fill(&mut plain, camera.position);
    plain.sync().unwrap();
    let (mut margined, _s2, camera) = margined_world(2, None);
    fill(&mut margined, camera.position);
    margined.sync().unwrap();
    let drawn = |w: &World| (0..LEVELS).map(|l| w.level_bricks(l)).collect::<Vec<_>>();
    // A margin is not drawn: the bricks that are, are the bands'. It draws fewer, even, since the
    // bricks it makes resident are known to be full beside the ones that are drawn, which buries more.
    let (a, b) = (drawn(&plain), drawn(&margined));
    eprintln!("drawn per level {a:?} without margins, {b:?} with");
    assert!(a.iter().zip(&b).all(|(plain, margin)| margin <= plain));
    let (a, b) = (
        plain.store().streamed_len(),
        margined.store().streamed_len(),
    );
    eprintln!("store counted {a} without margins, {b} with");
    assert!(b > a + a / 50, "the margin adds units: {a} {b}");
    // Nothing the bands do not ask for yet is a soft miss.
    margined.count_soft_misses();
    stream(&mut margined, camera.position);
    assert_eq!(margined.stats().soft_misses, 0);
}

#[test]
fn flying_with_margins_evicts_the_trail_and_keeps_the_margin_the_camera_is_in() {
    let (mut first, _s, spawn) = margined_world(2, None);
    fill(&mut first, spawn.position);
    first.sync().unwrap();
    let full = first.store().streamed_len();
    drop(first);
    let budget = brick_store::Budget {
        bricks: full * 102 / 100,
        overflow: 0.01,
    };
    let (mut world, _server, spawn) = margined_world(2, Some(budget));
    world.count_soft_misses();
    fill(&mut world, spawn.position);
    let (ahead, right) = tangents(spawn.horizon_up);
    let turn = fly(&mut world, spawn.position, ahead, 20.0, FLIGHT_FRAMES);
    let away = fly(&mut world, turn, right, 20.0, FLIGHT_FRAMES);
    settle(&mut world, away);
    world.sync().unwrap();
    let store = world.store();
    eprintln!(
        "complete set {full}; after the flight {} (limit {}), {} bricks evicted",
        store.streamed_len(),
        budget.limit(),
        store.evicted_len()
    );
    assert!(store.evicted_len() > 0, "the flight left units to evict");
    assert!(store.streamed_len() <= budget.limit());
    drop(store);
    // Settled: nothing the margin wants is missing, and eviction left the units it wants alone
    // (else the scene thread would be generating them again, and the loader would not be idle).
    stream(&mut world, away);
    world.sync().unwrap();
    assert!(idle(&world));
    assert_eq!(world.stats().soft_misses, 0);
}

#[test]
fn a_prefetch_lead_makes_bricks_ahead_that_a_camera_without_one_does_not() {
    let (mut without, _s, camera) = spawned_world(2);
    fill(&mut without, camera.position);
    without.set_lead(false);
    let (ahead, _) = tangents(camera.horizon_up);
    let (mut leading, _s2, _) = spawned_world(2);
    fill(&mut leading, camera.position);
    // Both go the same way at 7.5 m/s (0.75 Voxels every 5 ms: the test Planet is small, so the
    // places a lead looks at are near its ground only if it is short), and the frames show it to the one
    // that looks ahead.
    for step in 1..=160 {
        let at: [f64; 3] =
            std::array::from_fn(|a| camera.position[a] + ahead[a] * 0.75 * f64::from(step));
        for world in [&mut without, &mut leading] {
            world.recentre(at).unwrap();
            world.stream::<RingsFull>(at, |_, _| Ok(())).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    without.sync().unwrap();
    leading.sync().unwrap();
    let (a, b) = (
        without.loader().unwrap().generated,
        leading.loader().unwrap().generated,
    );
    eprintln!("generated {a} without a lead, {b} with one");
    assert!(b > a, "the lead makes bricks: {a} {b}");
}

#[test]
fn at_the_cap_the_farthest_bands_are_coarsened_instead_of_a_wanted_brick_dropped() {
    // The budget is a share of the complete set with the margins: less than the bands want.
    let (mut full, _s, camera) = margined_world(2, None);
    fill(&mut full, camera.position);
    full.sync().unwrap();
    let complete = full.store().streamed_len();
    let drawn: usize = (0..LEVELS).map(|l| full.level_bricks(l)).sum();
    drop(full);
    let budget = brick_store::Budget {
        bricks: complete * 90 / 100,
        overflow: 0.02,
    };
    let (mut world, _server, camera) = margined_world(2, Some(budget));
    world.count_soft_misses();
    let began = Instant::now();
    loop {
        stream(&mut world, camera.position);
        world.sync().unwrap();
        if idle(&world) && !world.store().over_limit() && world.stats().coarsening > 0 {
            break;
        }
        assert!(
            began.elapsed() < Duration::from_secs(120),
            "does not settle: {} counted of {}, coarsening {}",
            world.store().streamed_len(),
            budget.limit(),
            world.stats().coarsening
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let now: usize = (0..LEVELS).map(|l| world.level_bricks(l)).sum();
    eprintln!(
        "coarsening {} steps: {} counted of a limit of {}, {} bricks drawn of {drawn}",
        world.stats().coarsening,
        world.store().streamed_len(),
        budget.limit(),
        now
    );
    assert!(now < drawn, "the scene is coarser: {now} of {drawn}");
    // Coarser, not holes: the ground under the camera is still drawn, at level 0.
    let down = ray_through(
        camera.position,
        std::array::from_fn(|a| camera.position[a] - camera.horizon_up[a]),
    );
    let hit = trace(world.scene(), &down).expect("ground under the camera");
    assert_eq!(world.scene().level_at(hit.voxel), Some(0));
}

#[test]
fn coarsening_stands_while_what_is_wanted_does_not_fit_and_is_taken_back_when_it_does() {
    let (mut full, _s, camera) = margined_world(2, None);
    fill(&mut full, camera.position);
    full.sync().unwrap();
    let complete = full.store().streamed_len();
    drop(full);
    let budget = |share: usize| brick_store::Budget {
        bricks: complete * share / 100,
        overflow: 0.02,
    };
    let (mut world, _server, camera) = margined_world(2, Some(budget(90)));
    let began = Instant::now();
    let settle = |world: &mut World, done: &dyn Fn(&World) -> bool, what: &str| loop {
        stream(world, camera.position);
        world.sync().unwrap();
        if idle(world) && done(world) {
            return;
        }
        assert!(
            began.elapsed() < Duration::from_secs(240),
            "{what}: coarsening {}, {} held of a limit of {}",
            world.stats().coarsening,
            world.store().streamed_len(),
            world.store().budget().unwrap().limit()
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    // Less room than the bands want: it coarsens, and settles within the limit.
    settle(
        &mut world,
        &|w| w.stats().coarsening > 0 && !w.store().over_limit(),
        "does not coarsen under a budget it does not fit",
    );
    // It stands: what is wanted at the step before still does not fit, so a few holds later (300 ms
    // each in a test) the steps are what they were, not taken back and made again.
    let steps = world.stats().coarsening;
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        stream(&mut world, camera.position);
        world.sync().unwrap();
    }
    assert_eq!(world.stats().coarsening, steps, "the steps are not stable");
    eprintln!("stands at {steps} steps");
    // Room for it (what the camera wants fell, or the budget grew): the steps are taken back,
    // one each hold, to none, and it is not coarsened again.
    // The camera is exactly still and nothing is waited for with `sync` (which wakes the scene
    // thread): the thread looks at the clock itself, or it stays coarse for good.
    world.set_budget(budget(140));
    let given_back = Instant::now();
    while world.stats().coarsening > 0 {
        stream(&mut world, camera.position);
        assert!(
            given_back.elapsed() < Duration::from_secs(60),
            "a parked scene thread never takes the steps back: {} steps",
            world.stats().coarsening
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(1000));
    stream(&mut world, camera.position);
    world.sync().unwrap();
    assert_eq!(world.stats().coarsening, 0);
    assert!(!world.store().over_limit());
}

/// One frame of a path at 60 a second, in real time: the camera is at `position`, the scene
/// thread is told, and what it made is taken. Returns the soft misses it reports (counted by the
/// scene thread, see [`crate::scene_worker::Stats::soft_misses`]).
fn path_frame(world: &mut World, position: [f64; 3]) -> usize {
    world.recentre(position).unwrap();
    world.stream::<RingsFull>(position, |_, _| Ok(())).unwrap();
    world.stats().soft_misses
}

/// What a path counted: frames with a soft miss, the bricks missed summed over the frames, and the
/// most at once.
struct Misses {
    name: &'static str,
    frames: usize,
    with_misses: usize,
    total: usize,
    peak: usize,
}

/// Runs the camera along `position_at(t)` (seconds) for `seconds` at 60 frames a second, with the
/// loader running, counting soft misses.
fn run_path(
    world: &mut World,
    name: &'static str,
    seconds: f64,
    mut position_at: impl FnMut(&World, f64) -> [f64; 3],
) -> Misses {
    let frame = Duration::from_micros(16_667);
    let began = Instant::now();
    let mut misses = Misses {
        name,
        frames: 0,
        with_misses: 0,
        total: 0,
        peak: 0,
    };
    while began.elapsed().as_secs_f64() < seconds {
        let tick = Instant::now();
        let position = position_at(world, began.elapsed().as_secs_f64());
        let soft = path_frame(world, position);
        misses.frames += 1;
        misses.with_misses += usize::from(soft > 0);
        misses.total += soft;
        misses.peak = misses.peak.max(soft);
        std::thread::sleep(frame.saturating_sub(tick.elapsed()));
    }
    eprintln!(
        "  {name}: {} frames, {} with soft misses, {} bricks missed in all, {} at most at once; \
         {} waiting at the end",
        misses.frames,
        misses.with_misses,
        misses.total,
        misses.peak,
        world.waiting()
    );
    misses
}

/// The soft misses (a drawn brick whose wanted level is not resident, CONTEXT.md) on the paths of
/// #143, in the real client at full size: a 50 m/s flight at 2 m and at 50 m, a 50 m/s vertical
/// take-off, and standing still. Each starts at the spawn, with the shell complete and the
/// camera still; the camera goes back to it and the loader catches up between paths. The runs differ
/// in what is on: `FULLSIZE_MARGINS=0` has no margins, `FULLSIZE_PREFETCH=0` no prefetch (both on
/// by default), and the counts are compared by hand. A 180 degree turn moves nothing the scene
/// reads, since the selection does not look at the view and the whole Planet is drawn (ADR 0004), so
/// that criterion is what it is: 2 s of the camera standing. About ten minutes at full size:
/// `FULLSIZE_MARGINS=0 FULLSIZE_PREFETCH=0 cargo test --release -p client -- --ignored --nocapture soft_misses`.
#[test]
#[ignore = "generates the full-size world map"]
fn soft_misses_on_the_flight_take_off_and_standing_paths() {
    let off = |name: &str| std::env::var_os(name).is_some_and(|v| v == "0");
    let (margins, prefetch) = (!off("FULLSIZE_MARGINS"), !off("FULLSIZE_PREFETCH"));
    eprintln!("margins {margins}, prefetch {prefetch}");
    let loaded = crate::load_planet();
    let (client, _server) = loopback();
    let mut world =
        World::new(loaded.scene, Predictor::new(loaded.store), client).with_loader(loaded.loader);
    world.set_lead(prefetch);
    let spawn = loaded.camera;
    let began = Instant::now();
    wait_idle(&mut world, spawn.position, Duration::from_secs(5400));
    // Counted from here: looking at every brick that waits slows a fill down.
    world.count_soft_misses();
    eprintln!("filled after {:.0?}", began.elapsed());
    eprintln!("{}", memory(&world, "at rest"));

    let (ahead, right) = tangents(spawn.horizon_up);
    let up = spawn.horizon_up;
    let speed = 50.0 * VOXELS_PER_METRE;
    let radius = norm(spawn.position);
    // Along the sphere that the camera is over, `extra` Voxels above the 2 m flight.
    let along = move |world: &World, direction: [f64; 3], metres: f64, extra: f64| {
        let flat: [f64; 3] =
            std::array::from_fn(|a| spawn.position[a] + direction[a] * metres * VOXELS_PER_METRE);
        let on = flat.map(|c| c / norm(flat) * radius);
        let at = over_the_ground(&world.store(), on);
        let length = norm(at);
        std::array::from_fn(|a| at[a] + at[a] / length * extra)
    };
    let mut all = Vec::new();
    all.push(run_path(&mut world, "flight at 2 m", 10.0, |w, t| {
        along(w, ahead, 50.0 * t, 0.0)
    }));
    eprintln!("{}", memory(&world, "after the flight at 2 m"));
    wait_idle(&mut world, spawn.position, Duration::from_secs(1200));
    all.push(run_path(&mut world, "flight at 50 m", 10.0, |w, t| {
        along(w, right, 50.0 * t, 48.0 * VOXELS_PER_METRE)
    }));
    eprintln!("{}", memory(&world, "after the flight at 50 m"));
    wait_idle(&mut world, spawn.position, Duration::from_secs(1200));
    all.push(run_path(&mut world, "vertical take-off", 6.0, |_, t| {
        std::array::from_fn(|a| spawn.position[a] + up[a] * speed * t)
    }));
    eprintln!("{}", memory(&world, "after the take-off"));
    wait_idle(&mut world, spawn.position, Duration::from_secs(1200));
    all.push(run_path(&mut world, "standing still", 2.0, |_, _| {
        spawn.position
    }));
    eprintln!("margins {margins}, prefetch {prefetch}:");
    for m in &all {
        eprintln!(
            "  {:>18}: {:>4} of {} frames with soft misses, {:>8} brick-frames, peak {:>6}",
            m.name, m.with_misses, m.frames, m.total, m.peak
        );
    }
    eprintln!("coarsening steps: {}", world.stats().coarsening);
}

/// Streams with the camera at `camera` until the loader has nothing left (or `cap` is over). A
/// jump back to a camera that no path counts, when it is not already there.
fn wait_idle(world: &mut World, camera: [f64; 3], cap: Duration) {
    let began = Instant::now();
    let mut last = Instant::now();
    loop {
        world.recentre_moving(camera, [0.0; 3]).unwrap();
        world.stream::<RingsFull>(camera, |_, _| Ok(())).unwrap();
        if idle(world) {
            return;
        }
        if last.elapsed() > Duration::from_secs(30) {
            last = Instant::now();
            eprintln!(
                "  {:.0?}: {} waiting, {} pending",
                began.elapsed(),
                world.waiting(),
                world.loader().unwrap().pending
            );
        }
        assert!(began.elapsed() < cap, "does not fill");
        std::thread::sleep(Duration::from_millis(16));
    }
}
