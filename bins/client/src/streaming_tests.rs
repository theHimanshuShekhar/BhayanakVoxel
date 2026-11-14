//! The ring loader at work (#86): the loader, the world that takes in what it makes, and the scene
//! that is drawn from it, on the test world map. Bands of `d0` = 64 Voxels make a few levels fit in a
//! test; the Guard is levels 11 to 13.
//!
//! Most of these tests run **stepped** (#180, docs/agents/ci.md): [`Run::Stepped`] has no threads,
//! no sleeps and no machine clock. The loader's jobs and the scene thread's rounds are done on the
//! test's thread, a step each time the camera is told, by the code the threads run, and time is a
//! clock that the test moves ([`pace`]). They give the same run every time, so they gate merges.
//! The tests that are about real threads or real time ([`Run::Threads`]: how long the frame thread
//! waits, a thread that panics or is busy, the full-size runs) are listed in
//! `.github/real-thread-tests.txt` and run in the job that does not gate.

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
    PLANET.get_or_init(|| Planet::new(WorldMap::for_tests(SEED)))
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
        radius: planet().max_radius(),
    }
}

/// The ring and the Guard: what the client has right after the spawn.
fn spawned_store() -> BrickStore {
    let mut store = planet().crust_around(ground(), RING, LEVELS - 1);
    hold_guard(&mut store, planet(), GUARD_FIRST);
    store
}

/// How the loader and the scene thread run in a test: on the test's thread a step at a time, with
/// a clock the test moves (see [`World::with_stepped_loader`]), or on real threads, `n` of them for
/// the loader.
#[derive(Clone, Copy)]
enum Run {
    Stepped,
    Threads(usize),
}

/// The world right after the spawn, with a loader that runs as `run` says.
fn spawned_world(run: Run) -> (World, ServerEnd, Camera) {
    let store = spawned_store();
    let camera = crate::spawn_camera(ground());
    let scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    let (world, server) = streamed(store, scene, camera.position, run);
    (world, server, camera)
}

/// A world that streams `store`, `scene` being the packing of it, with a loader that runs as `run`
/// says.
fn streamed(store: BrickStore, scene: Scene, camera: [f64; 3], run: Run) -> (World, ServerEnd) {
    let source = store.source().unwrap();
    let (client, server) = loopback();
    let world = World::new(scene, Predictor::new(store), client);
    let world = match run {
        Run::Stepped => world.with_stepped_loader(Loader::stepped(source, camera, bands())),
        Run::Threads(workers) => world.with_loader(Loader::new(source, camera, bands(), workers)),
    };
    (world, server)
}

/// A frame at 60 a second.
const FRAME: Duration = Duration::from_micros(16_667);

/// Lets `time` go by: a stepped world's clock is moved on by it, and a world with threads sleeps.
fn pace(world: &World, time: Duration) {
    if world.is_stepped() {
        world.advance(time);
    } else {
        std::thread::sleep(time);
    }
}

/// The most steps that a test of a stepped world takes before it says that something never came.
const MOST_STEPS: usize = 50_000;

/// How long a test waits for something to happen, a wait at a time: a number of steps (each a frame
/// of the world's clock) for a stepped world, which has no wall clock to run out, and seconds of the
/// machine's time for one with threads.
struct Patience {
    steps: usize,
    began: Instant,
    limit: Duration,
}

impl Patience {
    fn new(limit: Duration) -> Self {
        Self {
            steps: 0,
            began: Instant::now(),
            limit,
        }
    }

    /// Lets a frame go by, or fails with `what` if the wait has gone on too long.
    fn wait(&mut self, world: &World, what: impl FnOnce() -> String) {
        self.steps += 1;
        if world.is_stepped() {
            assert!(self.steps < MOST_STEPS, "{}", what());
            pace(world, FRAME);
        } else {
            assert!(self.began.elapsed() < self.limit, "{}", what());
            pace(world, Duration::from_millis(1));
        }
    }
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
    let mut patience = Patience::new(Duration::from_secs(180));
    for frame in 0.. {
        stream(world, camera);
        if idle(world) {
            return frame;
        }
        patience.wait(world, || "the resident set does not fill".into());
    }
    unreachable!()
}

#[test]
fn with_the_camera_still_the_loader_makes_the_resident_set_complete_and_then_has_no_work() {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
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
        pace(&world, Duration::from_millis(10));
    }
    assert_eq!(world.loader().unwrap().generated, generated);
    assert_eq!(world.loader().unwrap().pending, 0);
    assert_eq!(world.waiting(), 0);
}

/// The scene thread's counts of what it did (`Done`) after `frames` frames of a camera that creeps
/// by a tenth of a Voxel a frame once the resident set is complete: it follows the camera every
/// frame and changes nothing that is drawn.
fn work_after_creeping(exactly: bool, frames: usize) -> (crate::scene_worker::Done, usize) {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    if exactly {
        world.count_exactly();
    }
    fill(&mut world, camera.position);
    // Let it publish what it had left (its sizes are looked at by the second), then stand.
    pace(&world, Duration::from_secs(1));
    for _ in 0..5 {
        stream(&mut world, camera.position);
        pace(&world, FRAME);
    }
    let before = world.stats().work;
    let mut changed = 0;
    for i in 1..=frames {
        let mut at = camera.position;
        at[0] += 0.1 * i as f64;
        changed += stream(&mut world, at).len();
        pace(&world, FRAME);
    }
    let after = world.stats().work;
    assert!(after.follow_steps >= before.follow_steps);
    (
        crate::scene_worker::Done {
            follow_steps: after.follow_steps - before.follow_steps,
            ..Default::default()
        },
        changed,
    )
}

#[test]
fn a_round_that_changes_nothing_is_not_published_in_the_games_mode_but_is_when_counting_exactly() {
    // The game counts nothing exactly, so the scene thread's counts of what it did (which rise on
    // every round) must not make it publish: it publishes as it did before #181. The bench counts
    // exactly, and publishes for them.
    let (game, changed) = work_after_creeping(false, 20);
    assert_eq!(changed, 0, "the scene does not change in this creep");
    assert_eq!(
        game.follow_steps, 0,
        "a round that changed nothing was published in the game's mode"
    );
    let (bench, changed) = work_after_creeping(true, 20);
    assert_eq!(changed, 0, "the scene does not change in this creep");
    assert!(
        bench.follow_steps > 0,
        "the creep is rounds that change nothing, and the bench's counts see them"
    );
}

#[test]
fn a_brick_the_camera_has_left_has_its_children_dropped_not_generated() {
    let store = spawned_store();
    let camera = crate::spawn_camera(ground()).position;
    let mut loader = Loader::stepped(store.source().unwrap(), camera, bands());
    // A brick of level 5 is split within 64 * 32 = 2048 Voxels; this one is 100 times that away.
    let far = (5, brick_store::BrickCoord([5000, 0, 0]));
    let near = (5, brick_store::BrickCoord(camera.map(|c| c as i32 >> 8)));
    loader.request(&store, [far, near], camera);
    loader.request(&store, [far], camera);
    assert_eq!(loader.pending(), 2, "asked for once");
    loader.sync(camera, bands());
    let mut got = Vec::new();
    let mut steps = 0;
    while got.len() < 2 {
        loader.step(10);
        got.extend(loader.receive(10));
        steps += 1;
        assert!(steps < MOST_STEPS, "{} of 2 came", got.len());
        loader.sync(camera, bands());
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
    let (mut world, _server, spawn) = spawned_world(Run::Stepped);
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
fn after_walking_at_5_m_per_s_the_loader_fills_in_and_the_ground_under_the_camera_is_level_0() {
    let (mut world, _server, mut camera) = spawned_world(Run::Stepped);
    let (ahead, _) = tangents(camera.horizon_up);
    let step = 50.0 / 60.0;
    let mut walked = camera.position;
    for _ in 0..360 {
        walked = std::array::from_fn(|a| walked[a] + ahead[a] * step);
        let position = over_the_ground(&world.store(), walked);
        camera.position = position;
        camera.carry_up(position);
        world.recentre(position).unwrap();
        stream(&mut world, position);
        pace(&world, FRAME);
    }
    // Left alone it fills in, and the ground under the camera is level 0.
    fill(&mut world, camera.position);
    let down = ray_through(
        camera.position,
        std::array::from_fn(|a| camera.position[a] - camera.horizon_up[a]),
    );
    let hit = trace(world.scene(), &down).expect("ground under the camera");
    assert_eq!(world.scene().level_at(hit.voxel), Some(0));
}

#[test]
fn walking_at_5_m_per_s_while_the_loader_runs_reports_the_scene_work_of_every_frame() {
    let (mut world, _server, mut camera) = spawned_world(Run::Threads(2));
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
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    let down = ray_through(
        camera.position,
        std::array::from_fn(|a| camera.position[a] - camera.horizon_up[a]),
    );
    // The seconds are the world's clock, which a frame moves on by 1/60 s.
    let mut frames = 0u32;
    let level_0_at = loop {
        stream(&mut world, camera.position);
        let hit = trace(world.scene(), &down).expect("ground under the camera");
        if world.scene().level_at(hit.voxel) == Some(0) {
            break FRAME * frames;
        }
        assert!(
            FRAME * frames < Duration::from_secs(20),
            "still level {:?} under the camera",
            world.scene().level_at(hit.voxel)
        );
        pace(&world, FRAME);
        frames += 1;
    };
    // The ground under the camera comes before the set is complete: nearest first, end to end.
    assert!(
        world.waiting() > 100,
        "{} waiting after {level_0_at:?}",
        world.waiting()
    );
    eprintln!("level 0 under the camera after {level_0_at:?} of the clock");
}

/// Frames until the loader's workers have taken `MAX_AHEAD` jobs and finished every one they had
/// in flight, which leaves them nothing to do until something is taken in. None of the frames takes
/// anything in. Then nothing more is generated, however long it waits: this does not depend on how
/// fast the machine is.
fn let_the_workers_run_ahead(world: &mut World, camera: [f64; 3]) {
    // What the scene thread was taking in when loading was paused is done by now.
    world.sync().unwrap();
    let mut patience = Patience::new(Duration::from_secs(60));
    loop {
        assert!(stream(world, camera).is_empty());
        let loader = world.loader().unwrap();
        // `ahead` only goes up while nothing is taken in, so once it is at the most, the jobs in
        // flight can only finish.
        if loader.ahead == crate::loader::MAX_AHEAD && loader.in_flight == 0 {
            return;
        }
        patience.wait(world, || "no work ahead".into());
    }
}

#[test]
fn while_loading_is_held_nothing_is_taken_in_and_what_waits_is_bounded() {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
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
    // Frames go by, with every chance for a worker that has not stopped to generate.
    for _ in 0..20 {
        assert!(stream(&mut world, camera.position).is_empty());
        pace(&world, Duration::from_millis(5));
    }
    assert_eq!(world.loader().unwrap().ahead, crate::loader::MAX_AHEAD);
    assert_eq!(world.loader().unwrap().generated, generated);
}

#[test]
fn letting_go_takes_in_what_was_held_and_the_bands_fill_as_before() {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    world.pause_loading(true);
    let_the_workers_run_ahead(&mut world, camera.position);
    world.pause_loading(false);
    assert!(!world.loading_paused());
    // What was held is waiting, so the first frame to see a change has some taken in.
    let mut first = Vec::new();
    let mut patience = Patience::new(Duration::from_secs(60));
    while first.is_empty() {
        first = stream(&mut world, camera.position);
        patience.wait(&world, || "nothing was taken in".into());
    }
    // The scene thread takes in what was held a slice at a time, not all of it at once.
    assert!(world.waiting() > 0, "not all in one slice");
    fill(&mut world, camera.position);
    assert_eq!(world.waiting(), 0);
    assert_eq!(world.loader().unwrap().pending, 0);
}

#[test]
fn an_edit_while_loading_is_held_is_shown() {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
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
        pace(&world, Duration::from_millis(5));
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
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
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
        pace(&world, FRAME);
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
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
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
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    let (ahead, _) = tangents(camera.horizon_up);
    let mut still = Stillness::default();
    let mut patience = Patience::new(Duration::from_secs(60));
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
        patience.wait(&world, || {
            format!(
                "still level {:?} under the camera",
                world.scene().level_at(hit.voxel)
            )
        });
    }
    assert!(taken > 0, "arrivals were taken in");
}

#[test]
fn still_then_moving_then_still_pauses_loading_resumes_it_the_frame_it_moves_and_pauses_again() {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
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
    let mut patience = Patience::new(Duration::from_secs(60));
    loop {
        at = step(at);
        let updates = reference(&mut world, &mut still, at);
        assert!(!world.loading_paused());
        if !updates.is_empty() {
            break;
        }
        patience.wait(&world, || "nothing taken in".into());
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
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    // Part way through the fill, and when it is done.
    for _ in 0..10 {
        stream(&mut world, camera.position);
        pace(&world, Duration::from_millis(3));
    }
    assert!(world.waiting() > 0, "the fill is not done yet");
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
    let (mut world, server, camera) = spawned_world(Run::Stepped);
    fill(&mut world, camera.position);
    let aim = up_close(&mut world, &camera);
    let depth = |world: &World| {
        crate::world::centre_hit(world.scene(), &aim)
            .expect("ground")
            .t
    };
    let whole = depth(&world);

    // The camera is on the move, so the scene thread has the rings to follow when the Edit is
    // made.
    let (ahead, _) = tangents(camera.horizon_up);
    let at = |n: i32| -> [f64; 3] {
        std::array::from_fn(|a| aim.position[a] + 2.0 * f64::from(n) * ahead[a])
    };
    for n in 1..=5 {
        world.recentre(at(n)).unwrap();
        pace(&world, FRAME);
    }
    world.edit(&aim, &crate::tool::Tool::default()).unwrap();
    // The frame of the click does not show it: the scene thread has not been looked at since.
    assert!(
        (depth(&world) - whole).abs() < 0.5,
        "the click does not show its own Edit"
    );
    pace(&world, FRAME);
    // The frame of the click goes on and looks at the scene, which has the Edit by then.
    world.recentre(at(6)).unwrap();
    pace(&world, FRAME);
    // And the frame after has it, in any case.
    world.recentre(at(7)).unwrap();
    let dug = depth(&world);
    assert!(dug > whole + 1.0, "the hole shows: {whole}, then {dug}");

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
fn an_edit_the_frame_after_does_not_wait_for_slow_coarse_levels_and_reads_of_the_store_wait_a_step_at_most()
 {
    let (mut world, _server, camera) = spawned_world(Run::Threads(2));
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
}

#[test]
fn an_edit_the_scene_thread_is_too_busy_to_pack_shows_when_it_has_and_the_frame_does_not_wait() {
    let (mut world, _server, camera) = spawned_world(Run::Threads(2));
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

/// ADR 0006's bound on the stepped clock: the frame thread waits `EDIT_WAIT` for an Edit and no
/// more, and the Edit is in the first look after the scene thread has packed it, with the result
/// kept until it is taken. (`an_edit_the_scene_thread_is_too_busy_to_pack_shows_when_it_has_and_
/// the_frame_does_not_wait` is the same on real threads, where the wait is a condition variable's.)
#[test]
fn an_edit_a_busy_scene_thread_has_not_packed_is_waited_for_edit_wait_and_shows_in_the_first_look_after_it_has()
 {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    fill(&mut world, camera.position);
    let aim = up_close(&mut world, &camera);
    let depth = |world: &World| {
        crate::world::centre_hit(world.scene(), &aim)
            .expect("ground")
            .t
    };
    let whole = depth(&world);
    // The thread is busy for 300 ms of the world's time from now, as a frame's worth of the camera
    // and the arrivals would keep it: it does nothing until then, the Edit's pack included.
    let busy = Duration::from_millis(300);
    let began = world.time();
    world.worker().busy_for(busy);
    world.edit(&aim, &crate::tool::Tool::default()).unwrap();

    // The frame of the click looks without waiting. The frame after waits for the Edit, gives up
    // at `EDIT_WAIT` on the clock with the thread still busy, and has taken nothing.
    let waited = world.time();
    assert!(
        world.recentre(aim.position).unwrap().is_none(),
        "nothing is made while the thread is busy"
    );
    assert_eq!(world.time(), waited, "the first look does not wait");
    assert!(
        world.recentre(aim.position).unwrap().is_none(),
        "nothing is taken while the thread is busy"
    );
    assert_eq!(
        world.time() - waited,
        world.edit_wait_time(),
        "the second look waits for the Edit, no more"
    );
    assert!((depth(&world) - whole).abs() < 0.5, "not shown yet");

    // Frames go by. Every look until the thread is free takes nothing (never an empty result that
    // the Edit would be lost to), and the first look after it has packed the Edit shows it.
    let mut shown = None;
    for frame in 1..=40 {
        pace(&world, FRAME);
        let looked_at = world.time();
        let update = world.recentre(aim.position).unwrap();
        assert_eq!(world.time(), looked_at, "a later look does not wait");
        if (depth(&world) - whole).abs() >= 0.5 {
            assert!(update.is_some(), "the Edit comes in an update");
            shown = Some((frame, looked_at));
            break;
        }
        assert!(update.is_none(), "nothing is taken before the Edit is");
        assert!(
            looked_at < began + busy,
            "a thread that is free has packed it by the look after: {:?} of {busy:?}",
            looked_at - began
        );
    }
    let (frame, looked_at) = shown.expect("the Edit never showed");
    assert!(
        looked_at >= began + busy,
        "it shows only once the thread was free: {:?}",
        looked_at - began
    );
    assert!(
        looked_at - FRAME < began + busy,
        "and in the first look after: frame {frame}, at {:?}",
        looked_at - began
    );
    let dug = depth(&world);
    assert!(dug > whole + 1.0, "the hole shows: {whole}, then {dug}");
}

#[test]
fn changes_made_while_a_copy_of_the_whole_scene_waits_go_into_the_copy() {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
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
    let (mut world, _server, camera) = spawned_world(Run::Threads(2));
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
    let (world, _server, _camera) = spawned_world(Run::Threads(1));
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
    let (mut world, _server, camera) = spawned_world(Run::Threads(2));
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
fn budgeted_world(run: Run, budget: brick_store::Budget) -> (World, ServerEnd, Camera) {
    let mut store = spawned_store();
    store.set_budget(budget);
    let camera = crate::spawn_camera(ground());
    let scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    let (world, server) = streamed(store, scene, camera.position, run);
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
        pace(world, FRAME);
    }
    position
}

/// Streams with the camera at `camera` until the resident set is complete and the store is within
/// its budget's limit.
fn settle(world: &mut World, camera: [f64; 3]) {
    let mut patience = Patience::new(Duration::from_secs(120));
    loop {
        stream(world, camera);
        world.sync().unwrap();
        if idle(world) && !world.store().over_limit() {
            return;
        }
        patience.wait(world, || {
            format!(
                "does not settle: idle {}, {} held of a limit of {}, {} evicted, {} edited",
                idle(world),
                world.store().streamed_len(),
                world.store().budget().unwrap().limit(),
                world.store().evicted_len(),
                world.store().diverged_len()
            )
        });
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
    let (mut first, _s, spawn) = spawned_world(Run::Stepped);
    fill(&mut first, spawn.position);
    first.sync().unwrap();
    let full = first.store().streamed_len();
    drop(first);
    let budget = brick_store::Budget {
        bricks: full * 107 / 100,
        overflow: 0.01,
    };
    let (mut world, _server, spawn) = budgeted_world(Run::Stepped, budget);
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
fn margined_world(run: Run, budget: Option<brick_store::Budget>) -> (World, ServerEnd, Camera) {
    let mut store = spawned_store();
    if let Some(budget) = budget {
        store.set_budget(budget);
    }
    let camera = crate::spawn_camera(ground());
    let mut scene = Scene::rings(&store, camera.position, bands(), bound()).unwrap();
    scene.set_margins(&store, render::lod::Margins::standard());
    let (world, server) = streamed(store, scene, camera.position, run);
    (world, server, camera)
}

#[test]
fn margins_make_more_of_the_store_resident_and_draw_no_more() {
    let (mut plain, _s, camera) = spawned_world(Run::Stepped);
    fill(&mut plain, camera.position);
    plain.sync().unwrap();
    let (mut margined, _s2, camera) = margined_world(Run::Stepped, None);
    margined.count_soft_misses();
    // The count is live: with the Guard standing in for what the bands split, bricks are drawn
    // coarser than they ask until the fill is done.
    let mut missed = 0;
    let mut patience = Patience::new(Duration::from_secs(180));
    while !idle(&margined) {
        stream(&mut margined, camera.position);
        missed = missed.max(margined.stats().soft_misses);
        patience.wait(&margined, || "the resident set does not fill".into());
    }
    assert!(missed > 0, "the fill has soft misses to count");
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
    // Nothing the bands do not ask for yet is a soft miss. The scene thread counts every so often
    // (the world's clock moves on past that).
    pace(&margined, Duration::from_secs(1));
    stream(&mut margined, camera.position);
    assert_eq!(margined.stats().soft_misses, 0);
}

#[test]
fn flying_with_margins_evicts_the_trail_and_keeps_the_margin_the_camera_is_in() {
    let (mut first, _s, spawn) = margined_world(Run::Stepped, None);
    fill(&mut first, spawn.position);
    first.sync().unwrap();
    let full = first.store().streamed_len();
    drop(first);
    let budget = brick_store::Budget {
        bricks: full * 102 / 100,
        overflow: 0.01,
    };
    let (mut world, _server, spawn) = margined_world(Run::Stepped, Some(budget));
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
    // The scene thread counts every so often: the world's clock moves on past that.
    pace(&world, Duration::from_secs(1));
    stream(&mut world, away);
    world.sync().unwrap();
    assert!(idle(&world));
    assert_eq!(world.stats().soft_misses, 0);
}

#[test]
fn a_look_now_counts_the_soft_misses_and_looks_for_holes_whatever_the_clock_says() {
    // The look for holes is due every 5 s of the world's clock, which steps do not move by that
    // much: only a look that is asked for happens.
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    fill(&mut world, camera.position);
    world.measure_scene_thread();
    let looks = world.stats().work.hole_scans;
    let (ahead, _) = tangents(camera.horizon_up);
    let moved = over_the_ground(
        &world.store(),
        std::array::from_fn(|a| camera.position[a] + ahead[a] * 2.0 * D0),
    );
    stream(&mut world, moved);
    // The soft misses are not counted yet, so what the scene thread said of them stands at its
    // first value while bricks wait: a count that is not of this camera (a settle time read from it
    // would be about 0).
    assert!(world.waiting() > 0, "the camera moved to where bricks wait");
    assert_eq!(world.stats().soft_misses, 0);
    world.count_soft_misses();
    assert_eq!(world.stats().work.hole_scans, looks, "no look is due");
    // A look for the count alone does not look for holes.
    world.look_now(false).unwrap();
    assert!(world.stats().soft_misses > 0, "the count was made now");
    assert_eq!(world.stats().work.hole_scans, looks);
    // A look for holes does, once, and then it is not due again until the clock says so.
    world.look_now(true).unwrap();
    assert_eq!(world.stats().work.hole_scans, looks + 1);
    stream(&mut world, moved);
    assert_eq!(world.stats().work.hole_scans, looks + 1);
    assert_eq!(world.stats().work.holes_found, 0);
}

#[test]
fn a_prefetch_lead_makes_bricks_ahead_that_a_camera_without_one_does_not() {
    let (mut without, _s, camera) = spawned_world(Run::Stepped);
    fill(&mut without, camera.position);
    let (ahead, _) = tangents(camera.horizon_up);
    let (mut leading, _s2, _) = spawned_world(Run::Stepped);
    fill(&mut leading, camera.position);
    // Both stand at the same place 120 Voxels on, and the one that looks ahead is given a
    // velocity of 7.5 m/s (the test Planet is small, so the places a lead looks at are near its
    // ground only if it is short). The velocity is given, not worked out from the time of the
    // frames (the unit tests in world.rs cover that), and the path is not walked, so that what
    // is held does not depend on which frames the scene thread saw.
    let speed = 7.5 * VOXELS_PER_METRE;
    let velocity = ahead.map(|c| c * speed);
    let end: [f64; 3] = std::array::from_fn(|a| camera.position[a] + ahead[a] * 120.0);
    for (world, velocity) in [(&mut without, [0.0; 3]), (&mut leading, velocity)] {
        let mut patience = Patience::new(Duration::from_secs(180));
        loop {
            world.recentre_moving(end, velocity).unwrap();
            world.sync().unwrap();
            // `lag` is 0 once the scene thread has taken the update that moved it to `end`: the
            // thread can drop one that comes while it is busy, and then the world is idle at the
            // spawn.
            if idle(world) && world.lag(end) == 0.0 {
                break;
            }
            patience.wait(world, || "does not settle".into());
        }
    }
    // The bricks that only the leading world holds are ahead of the camera: the lead made them.
    let (plain, led) = (without.store(), leading.store());
    let mut only_led = Vec::new();
    for level in 0..LEVELS {
        let held = plain.level(level).unwrap().sorted_coords();
        for coord in led.level(level).unwrap().sorted_coords() {
            if held.binary_search(&coord).is_err() {
                only_led.push((level, coord));
            }
        }
    }
    eprintln!(
        "{} bricks are held with a lead and not without",
        only_led.len()
    );
    assert!(!only_led.is_empty(), "the lead makes bricks");
    let ahead_of_end = only_led
        .iter()
        .filter(|(level, coord)| {
            let edge = f64::from(brick_store::BRICK_SIZE << level);
            let along: f64 = (0..3)
                .map(|a| (f64::from(coord.0[a]) + 0.5) * edge - end[a])
                .zip(ahead)
                .map(|(d, t)| d * t)
                .sum();
            along > 0.0
        })
        .count();
    eprintln!("{ahead_of_end} of them are ahead of the camera");
    assert_eq!(
        ahead_of_end,
        only_led.len(),
        "what the lead made is all ahead of the camera: {ahead_of_end} of {}",
        only_led.len()
    );
}

#[test]
fn at_the_cap_the_farthest_bands_are_coarsened_instead_of_a_wanted_brick_dropped() {
    // The budget is a share of the complete set with the margins: less than the bands want.
    let (mut full, _s, camera) = margined_world(Run::Stepped, None);
    fill(&mut full, camera.position);
    full.sync().unwrap();
    let complete = full.store().streamed_len();
    let drawn: usize = (0..LEVELS).map(|l| full.level_bricks(l)).sum();
    drop(full);
    let budget = brick_store::Budget {
        bricks: complete * 90 / 100,
        overflow: 0.02,
    };
    let (mut world, _server, camera) = margined_world(Run::Stepped, Some(budget));
    world.count_soft_misses();
    let mut patience = Patience::new(Duration::from_secs(120));
    loop {
        stream(&mut world, camera.position);
        world.sync().unwrap();
        if idle(&world) && !world.store().over_limit() && world.stats().coarsening > 0 {
            break;
        }
        patience.wait(&world, || {
            format!(
                "does not settle: {} counted of {}, coarsening {}",
                world.store().streamed_len(),
                budget.limit(),
                world.stats().coarsening
            )
        });
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
    let (mut full, _s, camera) = margined_world(Run::Stepped, None);
    fill(&mut full, camera.position);
    full.sync().unwrap();
    let complete = full.store().streamed_len();
    drop(full);
    let budget = |share: usize| brick_store::Budget {
        bricks: complete * share / 100,
        overflow: 0.02,
    };
    let (mut world, _server, camera) = margined_world(Run::Stepped, Some(budget(90)));
    let mut patience = Patience::new(Duration::from_secs(240));
    let mut settle = |world: &mut World, done: &dyn Fn(&World) -> bool, what: &str| loop {
        stream(world, camera.position);
        world.sync().unwrap();
        if idle(world) && done(world) {
            return;
        }
        patience.wait(world, || {
            format!(
                "{what}: coarsening {}, {} held of a limit of {}",
                world.stats().coarsening,
                world.store().streamed_len(),
                world.store().budget().unwrap().limit()
            )
        });
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
        pace(&world, Duration::from_millis(100));
        stream(&mut world, camera.position);
        world.sync().unwrap();
    }
    assert_eq!(world.stats().coarsening, steps, "the steps are not stable");
    eprintln!("stands at {steps} steps");
    // Room for it (what the camera wants fell, or the budget grew): the steps are taken back,
    // one each hold, to none, and it is not coarsened again.
    world.set_budget(budget(140));
    let mut patience = Patience::new(Duration::from_secs(60));
    while world.stats().coarsening > 0 {
        stream(&mut world, camera.position);
        patience.wait(&world, || {
            format!(
                "the steps are never taken back: {} steps",
                world.stats().coarsening
            )
        });
    }
    pace(&world, Duration::from_millis(1000));
    stream(&mut world, camera.position);
    world.sync().unwrap();
    assert_eq!(world.stats().coarsening, 0);
    assert!(!world.store().over_limit());
}

/// A scene thread that is parked (nothing asked of it, the camera exactly still) wakes by itself
/// when a coarsening hold is over, and takes the steps back: that is the thread's wait, which a
/// stepped scene thread does not have, so this one runs with real threads.
#[test]
fn a_parked_scene_thread_takes_the_coarsening_steps_back_when_the_hold_is_over() {
    let (mut full, _s, camera) = margined_world(Run::Threads(2), None);
    fill(&mut full, camera.position);
    full.sync().unwrap();
    let complete = full.store().streamed_len();
    drop(full);
    let budget = |share: usize| brick_store::Budget {
        bricks: complete * share / 100,
        overflow: 0.02,
    };
    let (mut world, _server, camera) = margined_world(Run::Threads(2), Some(budget(90)));
    let mut patience = Patience::new(Duration::from_secs(240));
    loop {
        stream(&mut world, camera.position);
        world.sync().unwrap();
        if idle(&world) && world.stats().coarsening > 0 && !world.store().over_limit() {
            break;
        }
        patience.wait(&world, || {
            format!("does not coarsen: {} steps", world.stats().coarsening)
        });
    }
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

/// What the world was after each frame of [`scenario`], and what was resident and drawn at its end.
#[derive(Debug, PartialEq)]
struct Trace {
    /// After each frame: the bricks the store counts, the bricks drawn at each level, the soft misses
    /// the scene thread reported, the bricks waiting, and what the loader had generated.
    frames: Vec<(usize, [usize; LEVELS], usize, usize, u64)>,
    /// A hash of every brick the store holds, level by level, and of the words that the drawn scene
    /// is made of.
    resident: u64,
    drawn: u64,
}

/// The camera standing at the spawn for a while and then walking, a frame at a time, with the soft
/// misses counted, and an Edit in the middle of it.
fn scenario(run: Run) -> Trace {
    use std::hash::{Hash, Hasher};
    let (mut world, _server, spawn) = spawned_world(run);
    world.count_soft_misses();
    let (ahead, _) = tangents(spawn.horizon_up);
    let mut frames = Vec::new();
    let mut at = spawn.position;
    for frame in 0..240 {
        if frame >= 60 {
            at = over_the_ground(
                &world.store(),
                std::array::from_fn(|a| at[a] + ahead[a] * 0.4 * VOXELS_PER_METRE),
            );
        }
        if frame == 150 {
            world
                .edit(&within_reach(&world, &spawn), &crate::tool::Tool::default())
                .unwrap();
        }
        stream(&mut world, at);
        pace(&world, FRAME);
        let stats = world.stats();
        frames.push((
            world.store().streamed_len(),
            stats.level_bricks,
            stats.soft_misses,
            stats.waiting,
            stats.loader.generated,
        ));
    }
    world.sync().unwrap();
    let mut resident = std::collections::hash_map::DefaultHasher::new();
    let store = world.store();
    for level in 0..LEVELS {
        store
            .level(level)
            .unwrap()
            .sorted_coords()
            .hash(&mut resident);
    }
    let mut drawn = std::collections::hash_map::DefaultHasher::new();
    world.scene().table.hash(&mut drawn);
    world.scene().regions.hash(&mut drawn);
    Trace {
        frames,
        resident: resident.finish(),
        drawn: drawn.finish(),
    }
}

#[test]
fn two_stepped_runs_of_one_scenario_give_the_same_residency_drawn_sets_and_soft_misses() {
    let (first, second) = (scenario(Run::Stepped), scenario(Run::Stepped));
    // The scenario is not still: bricks come, the soft misses are counted, an Edit is made.
    assert!(
        first.frames.iter().any(|f| f.2 > 0),
        "soft misses are counted"
    );
    assert!(
        first.frames.last().unwrap().0 > first.frames[0].0,
        "bricks come in"
    );
    assert_eq!(first, second);
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
    /// The distance of the scene that is drawn from the camera, at each frame (see
    /// [`World::lag`]), in Voxels.
    lag: Vec<f64>,
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
        lag: Vec::new(),
    };
    while began.elapsed().as_secs_f64() < seconds {
        let tick = Instant::now();
        let position = position_at(world, began.elapsed().as_secs_f64());
        let soft = path_frame(world, position);
        misses.frames += 1;
        misses.with_misses += usize::from(soft > 0);
        misses.total += soft;
        misses.peak = misses.peak.max(soft);
        misses.lag.push(world.lag(position));
        std::thread::sleep(frame.saturating_sub(tick.elapsed()));
    }
    let mut lags = misses.lag.clone();
    lags.sort_by(f64::total_cmp);
    let p99 = lags
        .get((lags.len() * 99).div_ceil(100).saturating_sub(1))
        .copied()
        .unwrap_or(0.0);
    eprintln!(
        "  {name}: {} frames, {} with soft misses, {} bricks missed in all, {} at most at once; \
         {} waiting at the end; the drawn scene was {:.0} Voxels behind the camera at most, {p99:.0} \
         at the 99th percentile, {:.0} at the end",
        misses.frames,
        misses.with_misses,
        misses.total,
        misses.peak,
        world.waiting(),
        lags.last().copied().unwrap_or(0.0),
        misses.lag.last().copied().unwrap_or(0.0),
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

/// A budget small enough that the fill of the test world's shell takes many frames to upload.
const SMALL_BUDGET: render::UploadCost = render::UploadCost {
    bytes: 48 << 10,
    writes: 12,
};

/// What a GPU copy of the scene holds, written to as the tracer writes (`Tracer::update_scene`):
/// the buffers that grew are made longer with what they had, and then the copies of a patch are
/// made from the frame thread's scene (words in the gaps between them as they are there), and the
/// header (the levels) if it changed.
struct GpuCopy {
    levels: Vec<render::scene::Level>,
    table: Vec<u32>,
    regions: Vec<u32>,
    brick_refs: Vec<u32>,
    bricks: Vec<u32>,
}

impl GpuCopy {
    fn of(scene: &Scene) -> Self {
        Self {
            levels: scene.levels.clone(),
            table: scene.table.clone(),
            regions: scene.regions.clone(),
            brick_refs: scene.brick_refs.clone(),
            bricks: scene.bricks.clone(),
        }
    }

    fn write(&mut self, scene: &Scene, update: &SceneUpdate) {
        match update {
            SceneUpdate::Rebuilt => *self = Self::of(scene),
            SceneUpdate::Rings(patch) => {
                if let Some(sizes) = patch.sizes {
                    self.table.resize(sizes.table, 0);
                    self.regions.resize(sizes.regions, 0);
                    self.brick_refs.resize(sizes.brick_refs, 0);
                    self.bricks.resize(sizes.bricks, 0);
                }
                for (part, range) in patch.copies() {
                    let (to, from) = match part {
                        render::scene::Part::Table => (&mut self.table, &scene.table),
                        render::scene::Part::Regions => (&mut self.regions, &scene.regions),
                        render::scene::Part::BrickRefs => (&mut self.brick_refs, &scene.brick_refs),
                        render::scene::Part::Bricks => (&mut self.bricks, &scene.bricks),
                    };
                    to[range.clone()].copy_from_slice(&from[range]);
                }
                if patch.levels {
                    self.levels.clone_from(&scene.levels);
                }
            }
            SceneUpdate::Patched { .. } => unreachable!("rings make rings"),
        }
    }

    /// That the GPU has every word the scene that is drawn has: so every brick that scene draws
    /// has its words (and the table, the records and the header that find them) in the GPU's
    /// buffers, and none is drawn from words that have not been copied.
    fn assert_is(&self, scene: &Scene, frame: usize) {
        assert_eq!(self.levels, scene.levels, "frame {frame}: the header");
        assert!(self.table == scene.table, "frame {frame}: the table");
        assert!(self.regions == scene.regions, "frame {frame}: the regions");
        assert!(
            self.brick_refs == scene.brick_refs,
            "frame {frame}: the brick refs"
        );
        assert!(self.bricks == scene.bricks, "frame {frame}: the bricks");
    }
}

/// What an update costs to upload (`upload_cost`, which is the tracer's count: see
/// `docs/agents/benchmarking.md`), and of it what the patch lists once each (the words and the
/// header) and the words that joining ranges put between them.
struct Uploaded {
    cost: render::UploadCost,
    listed: u64,
    gaps: u64,
}

impl Uploaded {
    fn of(scene: &Scene, update: &SceneUpdate) -> Self {
        let SceneUpdate::Rings(patch) = update else {
            panic!("a rebuild where a patch was expected");
        };
        let cost = render::tracer::upload_cost(scene, update);
        let copied: u64 = patch.copies().iter().map(|(_, r)| r.len() as u64 * 4).sum();
        let header = cost.bytes - copied;
        let listed = patch.words() as u64 * 4 + header;
        Self {
            cost,
            listed,
            gaps: cost.bytes - listed,
        }
    }
}

/// What a fill with a budget came to (see [`fill_checked`]).
#[derive(Debug, Default)]
struct Filled {
    frames: usize,
    patch_frames: usize,
    /// Frames in which work was held back, and the most in a row.
    cut: usize,
    longest_backlog: usize,
    /// Frames in which a buffer grew with a frame before it and one after it that were cut.
    growths_in_a_backlog: usize,
    /// The most that one frame's upload passed the budget by, in bytes and in copies (the words
    /// that joining copies put between them not counted), and the frames that passed it.
    over: render::UploadCost,
    frames_over: usize,
    /// The most that one operation added.
    largest_op: render::UploadCost,
}

/// Streams a world with the camera still until the resident set is complete, with `budget` for
/// what is uploaded, and on every frame checks that a GPU copy that is written as the tracer
/// writes it (see [`GpuCopy`]) is what the frame thread's scene is, so that no brick drawn is
/// drawn from words that were not copied, whatever waits and whatever grew; that no frame has a
/// hole; and that the frame's upload is the budget and the operations that were not inside it
/// (`Stats::excess_ops`), and the words that joining copies puts between them, and no more.
///
/// **What the bound on `excess_ops` can and cannot catch:** `excess_ops` is worked out by the
/// scene thread (`Engine::note`) from what it saw when each operation began, so the bound holds
/// whatever the throttle decides: it catches an operation that adds to a patch without going
/// through `note()`, or a patch that is bigger than the operations that made it (merging adding
/// words), and it does not say that the throttle holds back when it should. That is what the
/// frames cut, `with_a_budget_well_over_one_operation…` (a still camera: the test that fails if the
/// throttle holds nothing back) and the flight's bound from patch sizes are for.
fn fill_checked(budget: render::UploadCost) -> Filled {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    world.count_exactly();
    world.worker().set_upload_budget(budget);
    let mut gpu = GpuCopy::of(world.scene());
    let mut filled = Filled::default();
    // Whether each frame was cut, and whether a buffer grew in it.
    let mut frames: Vec<(bool, bool)> = Vec::new();
    let mut patience = Patience::new(Duration::from_secs(180));
    loop {
        let update = world.recentre(camera.position).unwrap();
        let frame = frames.len();
        let mut grew = false;
        if let Some(update) = &update {
            gpu.write(world.scene(), update);
            filled.patch_frames += 1;
            let SceneUpdate::Rings(patch) = update else {
                panic!("frame {frame}: a rebuild in a fill");
            };
            grew = patch.sizes.is_some();
            // What the patch lists, once each: the bytes before words are put between copies.
            let Uploaded { cost, listed, gaps } = Uploaded::of(world.scene(), update);
            let excess = world.stats().excess_ops;
            assert!(
                listed <= budget.bytes + excess.bytes,
                "frame {frame}: {listed} bytes listed, over the budget of {} and the {} bytes of \
                 the operations that were not inside it",
                budget.bytes,
                excess.bytes
            );
            assert!(
                cost.writes <= budget.writes + excess.writes,
                "frame {frame}: {} copies, over the budget of {} and the {} copies of the \
                 operations that were not inside it",
                cost.writes,
                budget.writes,
                excess.writes
            );
            assert!(
                gaps <= u64::from(cost.writes) * render::scene::COPY_GAP as u64 * 4,
                "frame {frame}: {gaps} bytes between copies"
            );
            if cost.bytes > budget.bytes || cost.writes > budget.writes {
                filled.frames_over += 1;
                filled.over.bytes = filled
                    .over
                    .bytes
                    .max(cost.bytes.saturating_sub(budget.bytes));
                filled.over.writes = filled
                    .over
                    .writes
                    .max(cost.writes.saturating_sub(budget.writes));
            }
        }
        gpu.assert_is(world.scene(), frame);
        assert_eq!(world.stats().holes, 0, "frame {frame}: a hole");
        frames.push((world.stats().throttled, grew));
        if idle(&world) && !world.stats().throttled {
            break;
        }
        patience.wait(&world, || "the resident set does not fill".into());
    }
    filled.frames = frames.len();
    filled.cut = frames.iter().filter(|f| f.0).count();
    filled.longest_backlog = frames
        .split(|f| !f.0)
        .map(<[_]>::len)
        .max()
        .unwrap_or_default();
    filled.growths_in_a_backlog = (1..frames.len().saturating_sub(1))
        .filter(|&i| frames[i].1 && frames[i - 1].0 && frames[i + 1].0)
        .count();
    filled.largest_op = world.stats().largest_op;
    filled
}

#[test]
fn with_a_small_budget_no_frame_draws_a_brick_whose_words_have_not_been_copied_and_a_frame_passes_the_budget_by_the_operations_that_were_not_inside_it()
 {
    let filled = fill_checked(SMALL_BUDGET);
    eprintln!("{filled:?}");
    // The upload spans many frames, and a buffer grew with the backlog standing.
    assert!(filled.cut > 10 && filled.longest_backlog > 3, "{filled:?}");
    assert!(
        filled.growths_in_a_backlog > 0,
        "no buffer grew in the middle of a backlog: {filled:?}"
    );
    // The budget is so small that one operation is over it: the frames pass it, by what the
    // operations that were not inside it add, which `fill_checked` holds each of them to.
    assert!(filled.frames_over > 0, "{filled:?}");
    assert!(filled.largest_op.bytes > SMALL_BUDGET.bytes, "{filled:?}");
}

#[test]
fn with_a_budget_well_over_one_operation_the_budget_holds_in_every_frame_and_still_holds_work_back()
{
    // Twice the most that one operation added, found out with a budget that holds nothing back.
    let one = fill_checked(render::UploadCost {
        bytes: 64 << 20,
        writes: 100_000,
    })
    .largest_op;
    assert!(one.bytes > 0 && one.writes > 0);
    let budget = render::UploadCost {
        bytes: one.bytes * 2,
        writes: one.writes * 2,
    };
    let filled = fill_checked(budget);
    eprintln!("one operation {one:?}, budget {budget:?}: {filled:?}");
    // It holds work back, but no frame is over it: what is added is checked against it before
    // each batch, with the most that a batch has added, so it is not taken over the budget.
    assert!(filled.cut > 0, "the budget holds nothing back: {filled:?}");
    assert_eq!(filled.frames_over, 0, "{filled:?}");
}

/// A stepped world with a camera still at the spawn that has filled in, an Edit in view, and the
/// frames that the Edit takes to be taken: made when there is a backlog (`budget`: a small one) or
/// when there is none (`None`). Returns the frames from the click to the frame that had it, whether
/// there was a backlog in the frame of the click, and whether work was still held back, and bricks
/// were still waiting, in the frame that had it.
fn edit_with_backlog(budget: Option<render::UploadCost>) -> (i32, bool, bool, bool) {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    fill(&mut world, camera.position);
    let aim = up_close(&mut world, &camera);
    let depth = |world: &World| {
        crate::world::centre_hit(world.scene(), &aim)
            .expect("ground")
            .t
    };
    let whole = depth(&world);
    let (ahead, _) = tangents(camera.horizon_up);
    let at = |n: i32| -> [f64; 3] {
        std::array::from_fn(|a| aim.position[a] + 8.0 * f64::from(n.min(6)) * ahead[a])
    };
    let mut frame = 0i32;
    if let Some(budget) = budget {
        // The camera is on the move, with a budget so small that what the loader makes for the
        // new places waits for frames: a backlog, which the Edit is made in the middle of.
        world.worker().set_upload_budget(budget);
        while !(world.stats().throttled && world.waiting() > 0) {
            frame += 1;
            assert!(frame <= 200, "no backlog formed in 200 frames");
            world.recentre(at(frame)).unwrap();
            pace(&world, FRAME);
        }
    }
    let backlog = world.stats().throttled && world.waiting() > 0;
    world.edit(&aim, &crate::tool::Tool::default()).unwrap();
    // The frame of the click looks at the scene and does not have the Edit.
    assert!((depth(&world) - whole).abs() < 0.5);
    let mut taken_in = 0;
    while !world.edit_taken() {
        taken_in += 1;
        assert!(taken_in <= 3, "the Edit was not taken in 3 frames");
        pace(&world, FRAME);
        world.recentre(at(frame + taken_in)).unwrap();
    }
    assert!(depth(&world) > whole + 1.0, "the hole shows");
    (
        taken_in,
        backlog,
        world.stats().throttled,
        world.waiting() > 0,
    )
}

#[test]
fn an_edit_made_while_a_backlog_waits_is_taken_as_soon_as_one_made_without_a_backlog_is() {
    let (without, had_backlog, ..) = edit_with_backlog(None);
    assert!(!had_backlog, "the control had a backlog");
    let (with, had_backlog, still_held, still_waiting) = edit_with_backlog(Some(SMALL_BUDGET));
    // A backlog stood when the Edit was made, and when it was taken: work was held back for the
    // budget and bricks were waiting, so the Edit did not wait for the backlog to be gone.
    assert!(had_backlog, "no backlog stood when the Edit was made");
    assert!(
        still_held && still_waiting,
        "the backlog was gone by the frame that took the Edit"
    );
    // ADR 0006's bound: the frame after the click, as without a backlog.
    assert_eq!(
        without, 1,
        "the control takes the Edit in the frame after the click"
    );
    assert_eq!(with, without, "the Edit took longer behind a backlog");
}

/// What each update of a flight cost: what the patch lists (bytes), its copies, and the bytes
/// between joined ranges. Printed as a count.
#[derive(Default)]
struct Uploads(Vec<(u64, u32, u64)>);

impl std::fmt::Debug for Uploads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} updates", self.0.len())
    }
}

/// What a flight along the ground came to (see [`fly_counting`]).
#[derive(Debug, Default)]
struct Flown {
    frames: usize,
    /// Updates that were a whole new scene (a jump).
    rebuilds: usize,
    /// Frames in which work was held back, and the longest run of them in a row.
    cut: usize,
    longest_backlog: usize,
    /// What the scene thread did over the flight.
    follow_steps: u64,
    pack_steps: u64,
    arrived_bricks: u64,
    /// The most bricks that waited for the bricks under them, and the most that the loader had on
    /// its way, in a frame of the flight.
    waiting_peak: usize,
    pending_peak: usize,
    /// The largest upload of a frame that was not a rebuild.
    worst: render::UploadCost,
    /// Frames, with the camera still at the end, until nothing waits.
    frames_to_drain: usize,
    holes: usize,
    /// What each update of the flight (not a rebuild) cost, and the most that one follow step and
    /// one batch of arrivals added over the run.
    uploads: Uploads,
    largest_follow: render::UploadCost,
    largest_batch: render::UploadCost,
    /// The most that the scene (as of the last update taken) was from the camera, in Voxels, in a
    /// frame of the flight.
    max_lag: f64,
}

/// The camera flown `frames` frames of `step` Voxels along the ground from the spawn, in a stepped
/// world that has filled in, with `budget` for what the scene thread lets wait to be uploaded
/// (the default one, if none is given), and then left still until nothing waits.
fn fly_counting(
    budget: Option<render::UploadCost>,
    step: f64,
    frames: usize,
    filled: bool,
) -> Flown {
    fly_counting_busy(budget, step, frames, filled, Duration::ZERO)
}

/// [`fly_counting`] with the scene thread busy for `busy` of the flight (inside one long
/// operation, as the 4090's was for most of a second at a time), so that the camera is far ahead
/// of the scene when it looks again.
fn fly_counting_busy(
    budget: Option<render::UploadCost>,
    step: f64,
    frames: usize,
    filled: bool,
    busy: Duration,
) -> Flown {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    world.count_exactly();
    if filled {
        fill(&mut world, camera.position);
    }
    if let Some(budget) = budget {
        world.worker().set_upload_budget(budget);
    }
    // The largest operations are those of the flight (and the drain after it), not of the fill.
    world.worker().forget_largest();
    if !busy.is_zero() {
        world.worker().busy_for(busy);
    }
    let before = world.stats().work;
    let (ahead, _) = tangents(camera.horizon_up);
    let mut flown = Flown::default();
    let mut run = 0;
    let mut moved = camera.position;
    let mut position = camera.position;
    let take = |world: &mut World, position: [f64; 3], flown: &mut Flown| {
        let update = world.recentre(position).unwrap();
        match &update {
            Some(SceneUpdate::Rebuilt) => flown.rebuilds += 1,
            Some(update) => {
                let Uploaded { cost, listed, gaps } = Uploaded::of(world.scene(), update);
                flown.worst.bytes = flown.worst.bytes.max(cost.bytes);
                flown.worst.writes = flown.worst.writes.max(cost.writes);
                flown.uploads.0.push((listed, cost.writes, gaps));
            }
            None => {}
        }
        flown.holes += world.stats().holes;
        if let Some(at) = world.stats().camera {
            let lag = (0..3)
                .map(|a| (at[a] - position[a]).powi(2))
                .sum::<f64>()
                .sqrt();
            flown.max_lag = flown.max_lag.max(lag);
        }
        flown.waiting_peak = flown.waiting_peak.max(world.waiting());
        flown.pending_peak = flown
            .pending_peak
            .max(world.loader().map_or(0, |l| l.pending));
        world.stats().throttled
    };
    for _ in 0..frames {
        moved = std::array::from_fn(|a| moved[a] + ahead[a] * step);
        position = over_the_ground(&world.store(), moved);
        let cut = take(&mut world, position, &mut flown);
        run = if cut { run + 1 } else { 0 };
        flown.cut += usize::from(cut);
        flown.longest_backlog = flown.longest_backlog.max(run);
        pace(&world, FRAME);
        flown.frames += 1;
    }
    let after = world.stats().work;
    flown.follow_steps = after.follow_steps - before.follow_steps;
    flown.pack_steps = after.pack_steps - before.pack_steps;
    flown.arrived_bricks = after.arrived_bricks - before.arrived_bricks;
    let mut patience = Patience::new(Duration::from_secs(180));
    loop {
        let cut = take(&mut world, position, &mut flown);
        if idle(&world) && !cut {
            break;
        }
        flown.frames_to_drain += 1;
        patience.wait(&world, || "the backlog does not drain".into());
    }
    flown.largest_follow = world.stats().largest_follow;
    flown.largest_batch = world.stats().largest_batch;
    flown
}

#[test]
fn flying_with_more_work_than_the_budget_drains_the_backlog_takes_in_arrivals_and_does_not_jump() {
    // 8 Voxels a frame is 24 m/s, and a jump is 2 `d0`: 128 Voxels on these bands, 16 frames of
    // it. The flight is 240 frames, so a camera that was not followed for that long would jump.
    let (step, frames) = (8.0, 240);
    let control = fly_counting(None, step, frames, true);
    assert_eq!((control.cut, control.rebuilds), (0, 0), "{control:?}");
    for budget in [
        SMALL_BUDGET,
        render::UploadCost {
            bytes: 256 << 10,
            writes: 32,
        },
    ] {
        let flown = fly_counting(Some(budget), step, frames, true);
        eprintln!("{budget:?}: {flown:?}\n  control {control:?}");
        // Work is held back for the whole flight: the budget is under what following costs.
        assert!(flown.cut > frames / 2, "{flown:?}");
        // The camera is followed every frame, and no throttle makes it jump.
        assert_eq!(flown.rebuilds, 0, "{flown:?}");
        assert_eq!(flown.follow_steps, control.follow_steps, "{flown:?}");
        assert_eq!(flown.holes, 0, "{flown:?}");
        // Arrivals are taken in every frame that the loader has some (a batch each, at least), not
        // starved by following the camera, which comes first.
        assert!(
            flown.pack_steps as usize >= frames / 2,
            "{} batches of arrivals in {frames} frames: {flown:?}",
            flown.pack_steps
        );
        // And the backlog is bounded and drains: what waits in the flight is no more than a
        // few times what waits when nothing is held back, and is gone soon after it ends.
        assert!(
            flown.waiting_peak <= control.waiting_peak * 4 + 1000,
            "{flown:?} against {control:?}"
        );
        assert!(
            flown.pending_peak <= control.pending_peak * 4 + 1000,
            "{flown:?} against {control:?}"
        );
        // (1 036 frames, 4.3 times the flight, on the 40 km test Planet; it was 932, 3.9 times,
        // before the continents and the shaped detail. Both are stepped rounds, not a clock: the
        // same 1 036 with eight busy loops competing for the machine. The drain is what the
        // 12 copies a frame of `SMALL_BUDGET` leave to be made: the flight lists 18 877 copies
        // against 16 853 on the earlier ground, +12%, as the drain is +11%. Rougher ground
        // breaks the surface into more pieces; the generator's speed is not in it.)
        assert!(flown.frames_to_drain <= 5 * frames, "{flown:?}");
    }
}

/// The most that one update of a flight lists (bytes, once each) and copies.
fn most_listed(flown: &Flown) -> (u64, u32) {
    let bytes = flown.uploads.0.iter().map(|u| u.0).max().unwrap_or(0);
    let writes = flown.uploads.0.iter().map(|u| u.1).max().unwrap_or(0);
    (bytes, writes)
}

/// A flight over ground that is still loading, held to a budget of one batch of arrivals.
///
/// **What it can catch:** the throttle in `stream()` (between batches) taken out or loosened, and
/// the follow steps not being cut to the budget (`Engine::next_place`). The unthrottled flight's
/// worst update is 866 324 bytes in 69 copies and its largest follow step 515 964 bytes; the
/// throttled flight's follow steps are held to the budget, and its worst update to under a third
/// of the unthrottled worst in bytes and under half in copies, in messages that name the
/// throttled flight. A partly broken throttle that lets through fewer and smaller operations than
/// the unthrottled flight, and stays under both margins, is not caught by this test, and then
/// nothing in it says it is wrong.
///
/// **What it cannot catch:** the throttle in `follow()` (the flight's numbers are the same without
/// it: `stream` stops before the outbox is over the budget when `follow` looks; see
/// `the_camera_is_not_followed_while_what_waits_is_over_the_budget_and_is_when_the_frame_thread_has_taken_it`),
/// and the bound's terms for the crossing operation and the round's first batch: the update
/// bound below (the budget and two batches, from the *unthrottled* flight's batch, so that a
/// broken throttle cannot widen it) is looser than the unthrottled flight's own worst update, so it
/// states the design's bound and has no power here. The terms are exercised by
/// `with_a_small_budget…`, where a budget under one operation puts every frame over it by the
/// operations that were not inside it, checked against `Stats::excess_ops` (the scene thread's own
/// count, which holds whatever the throttle decides).
#[test]
fn a_flight_over_ground_that_is_still_loading_has_follow_steps_within_the_budget_and_updates_far_under_the_unthrottled_flights()
 {
    // A flight of 24 Voxels a frame from the spawn with the resident set not yet filled in, so
    // that the loader has batches for every round, with nothing held back (the default budget).
    let (step, frames) = (24.0, 120);
    let control = fly_counting(None, step, frames, false);
    eprintln!(
        "control {control:?}, most listed {:?}",
        most_listed(&control)
    );
    assert_eq!(
        (control.cut, control.rebuilds, control.holes),
        (0, 0, 0),
        "the unthrottled flight held work back, jumped, or had a hole: {control:?}"
    );
    // The budget: what the largest batch of arrivals of the unthrottled flight costs.
    let budget = control.largest_batch;
    assert!(budget.bytes > 0 && budget.writes > 0, "{control:?}");
    let flown = fly_counting(Some(budget), step, frames, false);
    let (bytes, writes) = most_listed(&flown);
    let (control_bytes, control_writes) = most_listed(&control);
    eprintln!("{budget:?}: {flown:?}, most listed {:?}", (bytes, writes));
    assert_eq!(
        (flown.rebuilds, flown.holes),
        (0, 0),
        "the throttled flight jumped or had a hole: {flown:?}"
    );
    assert!(
        flown.cut > frames / 2,
        "the throttled flight held work back in {} of {frames} frames: {flown:?}",
        flown.cut
    );
    // The follow steps are cut to the budget: none costs more than it (the unthrottled flight's
    // largest is several times that), where the steps are the scene thread's, not the budget's.
    assert!(
        flown.largest_follow.bytes <= budget.bytes && flown.largest_follow.writes <= budget.writes,
        "the throttled flight has a follow step of {:?}, over the budget of {budget:?} (the \
         unthrottled flight's is {:?}): {flown:?}",
        flown.largest_follow,
        control.largest_follow
    );
    // And well under the unthrottled flight's worst update, in bytes and in copies.
    assert!(
        bytes * 3 < control_bytes && writes * 2 < control_writes,
        "the throttled flight's worst update ({bytes} bytes, {writes} copies) is not well under \
         the unthrottled flight's ({control_bytes} bytes, {control_writes} copies)"
    );
    // The design's bound on an update: the budget, the one operation (a follow step, cut to the
    // budget, or a batch) that took what waits to it, and the first batch of the round, from the
    // largest batch of the unthrottled flight. It is looser than that flight's own worst update, so
    // it does not say the throttle works; it says nothing is more than the design allows.
    let crossing = control.largest_batch;
    let first = control.largest_batch;
    let bound = (
        budget.bytes + crossing.bytes + first.bytes,
        budget.writes + crossing.writes + first.writes,
    );
    for (frame, &(listed, writes, gaps)) in flown.uploads.0.iter().enumerate() {
        assert!(
            listed <= bound.0 && writes <= bound.1,
            "the throttled flight's update {frame}: {listed} bytes, {writes} copies, over {bound:?} \
             (the budget {budget:?}, and the operations {crossing:?}, {first:?})"
        );
        assert!(
            gaps <= u64::from(writes) * render::scene::COPY_GAP as u64 * 4,
            "the throttled flight's update {frame}: {gaps} bytes between copies"
        );
    }
}

/// What `follow_with_an_outbox_that_is_not_taken` saw.
#[derive(Debug)]
struct Unseen {
    /// The five places the camera was put at, in order.
    path: [[f64; 3]; 5],
    /// Follow steps the scene thread took, and where it had moved the scene to, as of the frame
    /// after two rounds that the frame thread did not take what they published, and as of the
    /// frame after that. Follow steps are counted from the frame before the rounds.
    held: (u64, Option<[f64; 3]>),
    resumed: (u64, Option<[f64; 3]>),
    /// Where the scene was once nothing waited.
    end: Option<[f64; 3]>,
    /// The bricks drawn at each level at the end, as the table lists them, once nothing waits.
    listed: Vec<usize>,
}

/// A stepped world that has filled in, with the camera moved along a path in five places: the first
/// as a frame (the scene thread follows, the frame thread takes what it publishes), then two
/// rounds in which the scene thread is given the next places and the frame thread does not take
/// (a slow frame: what the scene thread published waits in the outbox), then two frames. Each frame
/// checks, as the tracer writes, that the GPU copy is the scene and that there is no hole. With
/// `budget`, a budget under any one patch, what waits in the outbox is over it from the first of
/// those rounds on, until a frame takes it; with `None`, the default one, it is not.
fn follow_with_an_outbox_that_is_not_taken(budget: Option<render::UploadCost>) -> Unseen {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    world.count_exactly();
    fill(&mut world, camera.position);
    if let Some(budget) = budget {
        world.worker().set_upload_budget(budget);
    }
    let mut gpu = GpuCopy::of(world.scene());
    let (ahead, _) = tangents(camera.horizon_up);
    let path: [[f64; 3]; 5] = std::array::from_fn(|n| {
        let moved = std::array::from_fn(|a| camera.position[a] + ahead[a] * 8.0 * (n + 1) as f64);
        over_the_ground(&world.store(), moved)
    });
    let mut frames = 0;
    let mut frame = |world: &mut World, position: [f64; 3]| {
        if let Some(update) = world.recentre(position).unwrap() {
            gpu.write(world.scene(), &update);
        }
        gpu.assert_is(world.scene(), frames);
        assert_eq!(world.stats().holes, 0, "frame {frames}: a hole");
        frames += 1;
    };
    frame(&mut world, path[0]);
    let before = world.stats().work.follow_steps;
    // The scene thread is given the next place twice, and publishes what following to the first
    // costs: that waits in the outbox, with nothing to take it.
    for place in &path[1..3] {
        world.worker().follow_moving(*place, [0.0; 3]);
        world.worker().step();
    }
    // Its round in this frame has the outbox as the two left it, then the frame takes it.
    frame(&mut world, path[3]);
    let stats = world.stats();
    let held = (stats.work.follow_steps - before, stats.camera);
    frame(&mut world, path[4]);
    let stats = world.stats();
    let resumed = (stats.work.follow_steps - before, stats.camera);
    let mut patience = Patience::new(Duration::from_secs(180));
    loop {
        frame(&mut world, path[4]);
        if idle(&world) && !world.stats().throttled {
            break;
        }
        patience.wait(&world, || "the backlog does not drain".into());
    }
    Unseen {
        path,
        held,
        resumed,
        end: world.stats().camera,
        listed: listed(world.scene()),
    }
}

/// The throttle in `follow()`: while what the scene thread has published (or packed since) is over
/// the budget, the camera is not followed, and it is when the frame thread has taken it, from where
/// the scene is, by a step each round, with no hole and the scene that following in every round
/// comes to.
///
/// Nothing else tests it: in the flights `stream` stops before the outbox is over the budget, so it
/// never is when `follow` looks, and the flight's numbers are the same without the early return.
/// Here the frame thread is slow on purpose, so the outbox is over the budget (any patch is over
/// one byte) when the next round's `follow` looks: removing only `return Ok(false)` from it makes
/// the held frame's counts those of the control and fails the first assertion on the throttled
/// run.
#[test]
fn the_camera_is_not_followed_while_what_waits_is_over_the_budget_and_is_when_the_frame_thread_has_taken_it()
 {
    let control = follow_with_an_outbox_that_is_not_taken(None);
    // The control follows in every round: the two that were not taken, and the frame after them.
    assert_eq!(
        (control.held.0, control.held.1),
        (3, Some(control.path[3])),
        "the unthrottled run did not follow in every round: {control:?}"
    );
    assert_eq!(
        (control.resumed.0, control.resumed.1),
        (4, Some(control.path[4])),
        "{control:?}"
    );
    // A budget of one byte: a round follows by one step of the least size (a Voxel, see
    // `Engine::next_place`) and is over the budget after it.
    let budget = render::UploadCost {
        bytes: 1,
        writes: 1,
    };
    let held = follow_with_an_outbox_that_is_not_taken(Some(budget));
    assert_eq!(held.path, control.path, "the runs' paths differ");
    // The first round followed (nothing waited), as far as a step that makes a change; the
    // second, and the frame's, found the outbox over the budget and did not: one follow step, and
    // the scene short of the camera. (The unthrottled run follows to the camera in all three.)
    assert_eq!(
        held.held.0, 1,
        "the camera was followed while what waited was over the budget and the frame thread had \
         not taken it: {held:?}"
    );
    assert_ne!(held.held.1, Some(held.path[3]), "{held:?}");
    // The frame after the one that took it follows again, and is not yet at the camera.
    assert_eq!(
        held.resumed.0, 2,
        "the camera was not followed once the frame thread had taken what waited: {held:?}"
    );
    assert_ne!(held.resumed.1, Some(held.path[4]), "{held:?}");
    // Nothing was lost to the hold: the scene walks on to the camera, with no hole in any frame
    // (`follow_with_an_outbox_that_is_not_taken` checks each), and draws what following in every
    // round comes to, within the hysteresis of the selection: no level has a percent more or
    // fewer bricks drawn.
    assert_eq!(held.end, Some(held.path[4]), "{held:?}");
    for (level, (&held, &control)) in held.listed.iter().zip(&control.listed).enumerate() {
        assert!(
            held.abs_diff(control) * 100 <= control,
            "level {level}: {held} bricks drawn against the unthrottled run's {control}"
        );
    }
}

/// A stepped world that has filled in, with `budget`, whose camera is put `far` Voxels along the
/// ground in one move (a teleport), or gets there in moves of 16 Voxels that the scene thread does
/// not look at until the last (it fell behind). Returns the rebuilds and the patches, the largest
/// follow step, the most that one update listed, the frames until nothing waits, and whether the
/// scene ended at the camera. Every frame checks the GPU copy against the scene and that there is
/// no hole.
fn put_the_camera_far_away(
    budget: render::UploadCost,
    far: f64,
    teleport: bool,
) -> (usize, usize, render::UploadCost, u64, usize, bool) {
    let (rebuilds, patches, follow, listed, frames, there, _) =
        put_the_camera_far_away_after(Before::Nothing, budget, far, teleport);
    (rebuilds, patches, follow, listed, frames, there)
}

/// What the scene thread has been through before the camera is put far away (see
/// [`put_the_camera_far_away_after`]), that the length of its follow steps learns from.
#[derive(Clone, Copy, Debug)]
enum Before {
    Nothing,
    /// 40 rounds in which the camera moves a Voxel (to and fro within one bucket of the
    /// selection) and nothing changes: each is a step that moved and cost nothing.
    QuietSteps,
    /// 20 teleports (to the far place and back), each a rebuild, which no patch's cost is of.
    Teleports,
}

/// [`put_the_camera_far_away`] after `before`, and the largest lag of the scene behind the camera
/// that a frame saw ([`World::lag`]) as the last of the tuple.
fn put_the_camera_far_away_after(
    before: Before,
    budget: render::UploadCost,
    far: f64,
    teleport: bool,
) -> (usize, usize, render::UploadCost, u64, usize, bool, f64) {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    world.count_exactly();
    fill(&mut world, camera.position);
    {
        let (ahead, _) = tangents(camera.horizon_up);
        let at = |metres: f64| -> [f64; 3] {
            std::array::from_fn(|a| camera.position[a] + ahead[a] * metres)
        };
        match before {
            Before::Nothing => {}
            Before::QuietSteps => {
                let patches_before = world.stats().work.follow_bricks;
                for n in 0..40 {
                    world.recentre(at((n % 2) as f64)).unwrap();
                }
                let changed = world.stats().work.follow_bricks - patches_before;
                // The steps were quiet: the bricks that changed are those of the first (the
                // selection settling on a camera that is a Voxel off the one it was filled at).
                assert!(changed < 200, "{changed} bricks changed in the quiet steps");
            }
            Before::Teleports => {
                let there = over_the_ground(&world.store(), at(far));
                for _ in 0..20 {
                    world.recentre(there).unwrap();
                    world.recentre(camera.position).unwrap();
                }
            }
        }
        world.recentre(camera.position).unwrap();
        fill(&mut world, camera.position);
    }
    world.worker().set_upload_budget(budget);
    world.worker().forget_largest();
    let mut gpu = GpuCopy::of(world.scene());
    let (ahead, _) = tangents(camera.horizon_up);
    let along = |world: &World, metres: f64| {
        let moved = std::array::from_fn(|a| camera.position[a] + ahead[a] * metres);
        over_the_ground(&world.store(), moved)
    };
    let target = along(&world, far);
    if !teleport {
        for n in 1..=(far / 16.0) as usize {
            let at = along(&world, 16.0 * n as f64);
            world.worker().follow_moving(at, [0.0; 3]);
        }
    }
    let (mut rebuilds, mut patches, mut listed_most) = (0, 0, 0);
    let (mut frames, mut lag) = (0, 0.0f64);
    let mut patience = Patience::new(Duration::from_secs(180));
    loop {
        let update = world.recentre(target).unwrap();
        lag = lag.max(world.lag(target));
        match update {
            Some(SceneUpdate::Rebuilt) => {
                rebuilds += 1;
                gpu.write(world.scene(), &SceneUpdate::Rebuilt);
            }
            Some(update) => {
                patches += 1;
                listed_most = listed_most.max(Uploaded::of(world.scene(), &update).listed);
                gpu.write(world.scene(), &update);
            }
            None => {}
        }
        gpu.assert_is(world.scene(), frames);
        assert_eq!(world.stats().holes, 0, "frame {frames}: a hole");
        frames += 1;
        if idle(&world) && !world.stats().throttled && world.stats().camera == Some(target) {
            break;
        }
        patience.wait(&world, || "the backlog does not drain".into());
    }
    (
        rebuilds,
        patches,
        world.stats().largest_follow,
        listed_most,
        frames,
        world.stats().camera == Some(target),
        lag,
    )
}

/// A budget over what a follow step of the least size costs here (a bucket of 4 Voxels of the
/// camera's travel: about 100 KB), and under what a walk of hundreds of Voxels does.
const WALKING_BUDGET: render::UploadCost = render::UploadCost {
    bytes: 256 << 10,
    writes: 32,
};

#[test]
fn a_camera_that_the_scene_fell_far_behind_is_walked_to_in_steps_within_the_budget_and_one_that_was_put_there_is_jumped_to()
 {
    // Four jumps (a jump is 2 `d0`: 128 Voxels on these bands) away.
    let far = 4.0 * render::scene::JUMP * D0;
    let (rebuilds, patches, follow, listed, frames, there) =
        put_the_camera_far_away(WALKING_BUDGET, far, false);
    eprintln!("walked: {rebuilds} rebuilds, {patches} patches, {follow:?}, {listed}, {frames}");
    assert!(there, "the scene did not reach the camera");
    // Never made again (which would upload the whole scene: 871 MB at full size), though it was
    // more than a jump behind.
    assert_eq!(
        rebuilds, 0,
        "the scene was made again: it fell behind by more than a jump"
    );
    // In many patches, each step of the follow within the budget: not in one operation of many
    // times it (the walk of 512 Voxels costs several MB here).
    assert!(
        patches > 8 && frames > 8,
        "{patches} patches in {frames} frames"
    );
    assert!(
        follow.bytes <= WALKING_BUDGET.bytes && follow.writes <= WALKING_BUDGET.writes,
        "a follow step of {follow:?} is over the budget of {WALKING_BUDGET:?}"
    );
    // A camera that was put there in one move is still jumped to: the scene is made again, as that
    // is cheaper than walking.
    let (rebuilds, ..) = put_the_camera_far_away(WALKING_BUDGET, far, true);
    assert_eq!(rebuilds, 1, "a teleport is not made again");
}

/// A small budget, so that the follow steps of a stepped scene thread are short (a Voxel or two:
/// the step that a share of this budget over the least cost of a Voxel comes to), and a camera
/// that goes at 24 Voxels a frame is not kept up with. A step costs 100 to 145 KB here, whatever its
/// length (the regions that it rewrites are written whole), which is within the budget. The stepped
/// runs are the same every time, so the margins hold. (On this Planet a step's cost does not depend
/// on its length enough for the limit on it to be what keeps it in the budget: at full size it
/// is, 34 KB a Voxel against 34 MB for half a jump; see
/// `a_follow_step_is_sized_by_how_its_cost_compared_to_its_share_and_is_never_over_the_limit`.)
const STEP_BUDGET: render::UploadCost = render::UploadCost {
    bytes: 192 << 10,
    writes: 32,
};

/// What a follow step costs after steps that cost nothing (a run of quiet ones, and teleports,
/// which are rebuilds and no patch's cost) and after none: it is within the budget whatever came
/// before. The length of a step is limited by [`crate::scene_worker`]'s `FOLLOW_MIN_COST`, and was
/// by a floor under a rate; the first 4090 run followed in one step of 17 to 34 MB.
#[test]
fn a_follow_step_is_within_the_budget_after_a_run_of_quiet_steps_and_after_teleports() {
    let far = 4.0 * render::scene::JUMP * D0;
    for before in [Before::Nothing, Before::QuietSteps, Before::Teleports] {
        let (rebuilds, patches, follow, _, frames, there, lag) =
            put_the_camera_far_away_after(before, STEP_BUDGET, far, false);
        eprintln!(
            "{before:?}: {rebuilds} rebuilds, {patches} patches, {follow:?}, {frames} frames, lag {lag}"
        );
        assert!(there, "{before:?}: the scene did not reach the camera");
        assert_eq!(rebuilds, 0, "{before:?}: the scene was made again");
        // Every step costs what the budget lets it, though the cap would let it be 64 Voxels
        // (over the budget here): the budget sized it.
        assert!(
            follow.bytes <= STEP_BUDGET.bytes && follow.writes <= STEP_BUDGET.writes,
            "{before:?}: a follow step of {follow:?} is over the budget of {STEP_BUDGET:?}"
        );
        assert!(patches > 8, "{before:?}: {patches} patches");
        // The scene was behind by about all of the way when it first published: what `World::lag`
        // is for, and it is a distance and not a count.
        assert!(
            lag > far * 0.5 && lag <= far + 1.0,
            "{before:?}: the scene was at most {lag} Voxels behind a camera {far} away"
        );
    }
}

/// What it costs the scene thread to follow a camera that it cannot keep up with is in
/// proportion to how far it walked the scene, not to how far the camera went. The selection looks
/// at the nodes that the camera's travel (and that of the places it looks ahead to) can have changed;
/// the places were the camera's plus its lead, so each round that was told a camera that had gone 24
/// Voxels counted 24 more travel, however far the round walked the scene: every call looked at what
/// 1 300 Voxels at full size (about 3 million nodes, 0.75 s) had made due, for a step of 64.
/// Now the places are the scene's own plus the lead. The test is the model of that: the selection's
/// travel (see [`render::Scene::travelled`]) is at most 2.5 times what the scene walked (2.0
/// times, against 3.5 with the places from the camera: the camera goes 2.4 times as far as the
/// scene walks here, and about 8 times at full size, where the difference is the 3 million nodes
/// of each call); it cannot show the time, since the stepped clock does not measure the work.
#[test]
fn the_selection_travels_as_far_as_the_scene_walks_when_the_camera_goes_faster() {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    world.count_exactly();
    fill(&mut world, camera.position);
    world.worker().set_upload_budget(STEP_BUDGET);
    let (ahead, _) = tangents(camera.horizon_up);
    let along = |world: &World, voxels: f64| {
        let moved = std::array::from_fn(|a| camera.position[a] + ahead[a] * voxels);
        over_the_ground(&world.store(), moved)
    };
    // 40 Voxels a frame, 2 400 a second: the places looked ahead to are 2 400 and 4 800 Voxels on.
    let velocity: [f64; 3] = std::array::from_fn(|a| ahead[a] * 40.0 * 60.0);
    // The first look ahead counts the places' whole distance, once: it is not what is measured.
    world
        .recentre_moving(along(&world, 40.0), velocity)
        .unwrap();
    let (began_at, began) = (world.stats().camera.unwrap(), world.stats().travelled);
    for frame in 2..=60 {
        let there = along(&world, 40.0 * f64::from(frame));
        world.recentre_moving(there, velocity).unwrap();
    }
    let now = world.stats().camera.unwrap();
    let walked = (0..3)
        .map(|a| (now[a] - began_at[a]).powi(2))
        .sum::<f64>()
        .sqrt();
    let travelled = world.stats().travelled - began;
    let went = 40.0 * 59.0;
    eprintln!("walked {walked:.0} Voxels, travelled {travelled:.0}, the camera went {went}");
    assert!(
        walked > 60.0 && walked < went * 0.75,
        "the scene walked {walked} Voxels: it is to be well behind a camera that went {went}"
    );
    assert!(
        travelled <= 2.5 * walked,
        "the selection travelled {travelled} Voxels for a scene that walked {walked}"
    );
}

/// A camera that is put somewhere in one move (a teleport) but ends within a jump of where the
/// scene is (the scene was behind it, and it was put back toward it) is walked to, in steps
/// within the budget: it is not patched in one operation of up to two `d0`, outside the budget
/// (here 250 KB in one, against steps of 100 to 130 KB, and a budget of 192 KB between).
#[test]
fn a_teleport_that_ends_within_a_jump_of_the_scene_is_walked_to_within_the_budget() {
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    world.count_exactly();
    fill(&mut world, camera.position);
    world.worker().set_upload_budget(STEP_BUDGET);
    world.worker().forget_largest();
    let mut gpu = GpuCopy::of(world.scene());
    let (ahead, _) = tangents(camera.horizon_up);
    let jump = render::scene::JUMP * D0;
    let along = |world: &World, voxels: f64| {
        let moved = std::array::from_fn(|a| camera.position[a] + ahead[a] * voxels);
        over_the_ground(&world.store(), moved)
    };
    // The camera is told of a place a jump behind the scene, then put 0.9 of a jump ahead of it:
    // a move of 1.9 jumps, ending 0.9 jump from the scene, which has not looked.
    let behind = along(&world, -jump * 0.9);
    let target = along(&world, jump * 0.9);
    world.worker().follow_moving(behind, [0.0; 3]);
    world.worker().follow_moving(target, [0.0; 3]);
    let (mut rebuilds, mut patches, mut frames) = (0, 0, 0);
    let mut patience = Patience::new(Duration::from_secs(180));
    loop {
        match world.recentre(target).unwrap() {
            Some(SceneUpdate::Rebuilt) => {
                rebuilds += 1;
                gpu.write(world.scene(), &SceneUpdate::Rebuilt);
            }
            Some(update) => {
                patches += 1;
                gpu.write(world.scene(), &update);
            }
            None => {}
        }
        gpu.assert_is(world.scene(), frames);
        assert_eq!(world.stats().holes, 0, "frame {frames}: a hole");
        frames += 1;
        if idle(&world) && !world.stats().throttled && world.stats().camera == Some(target) {
            break;
        }
        patience.wait(&world, || "the backlog does not drain".into());
    }
    let follow = world.stats().largest_follow;
    eprintln!("{rebuilds} rebuilds, {patches} patches, {follow:?}");
    assert_eq!(
        rebuilds, 0,
        "the scene was made again for a move within a jump"
    );
    assert!(
        follow.bytes <= STEP_BUDGET.bytes && follow.writes <= STEP_BUDGET.writes,
        "a follow step of {follow:?} is over the budget of {STEP_BUDGET:?}"
    );
    assert!(patches > 2, "{patches} patches: it was not walked");
}

#[test]
fn a_scene_thread_that_is_busy_while_the_camera_flies_on_walks_to_it_after_and_does_not_end_in_a_rebuild()
 {
    // 24 Voxels a frame for 120 frames (2 s), with the scene thread inside one operation for the
    // first 1.5 s, as the 4090's were: the camera is 2 000 Voxels ahead of the scene when it
    // looks again (a jump is 128 here), and the scene is walked to it, not made again.
    let flown = fly_counting_busy(None, 24.0, 120, true, Duration::from_millis(1500));
    eprintln!("{flown:?}");
    assert!(
        flown.max_lag > render::scene::JUMP * D0,
        "the scene was only {} Voxels behind at the most, which is no more than a jump: {flown:?}",
        flown.max_lag
    );
    assert_eq!(
        flown.rebuilds, 0,
        "a flight that the scene fell {} Voxels behind in ended in a rebuild: {flown:?}",
        flown.max_lag
    );
    assert_eq!(flown.holes, 0, "{flown:?}");
}

#[test]
fn the_default_budget_is_over_what_a_baseline_fill_frame_uploaded_so_it_is_not_what_bounds_a_fill()
{
    // The #162 baseline's fill frames did about 2 MB in about 1 400 copies on the 4090 (frame time
    // p99 4.9 ms). A budget under that bounds the fill, which the first one (128 copies) did: the
    // 4090's fill took 1 035 s against 254 to 265 s. The stepped test planet cannot show it (its
    // frames are a few dozen copies), so the constants are held to the measurement.
    let budget = crate::scene_worker::UPLOAD_BUDGET;
    assert!(budget.writes >= 1_400, "{budget:?}");
    assert!(budget.bytes >= 2 << 20, "{budget:?}");
}

#[test]
fn with_the_default_budget_a_fill_takes_about_as_many_frames_as_one_with_no_budget() {
    let unbudgeted = fill_checked(render::UploadCost {
        bytes: u64::MAX,
        writes: u32::MAX,
    });
    let default = fill_checked(crate::scene_worker::UPLOAD_BUDGET);
    eprintln!("unbudgeted {unbudgeted:?}\ndefault {default:?}");
    assert!(
        default.frames * 5 <= unbudgeted.frames * 6,
        "a fill with the default budget takes {} frames, against {} with none",
        default.frames,
        unbudgeted.frames
    );
}

/// A scene thread that holds work back for the budget goes on as soon as the frame thread has
/// taken what waited, not when its own sleep ends. With a budget of one byte every round ends with
/// the thread holding work back and waiting for the frame thread, and a frame every 2 ms takes what
/// it published: the camera is still, so nothing but the take can wake it. The measure is how the
/// thread's waits ended (`SceneWorker::throttled_waits`): by the wake or by the timeout
/// (`THROTTLED_WAIT`, 20 ms). The share of frames that had an update, which this test counted
/// before, depends on how much CPU the scene thread gets (5 of 250 in the real-thread job, where
/// the other tests are running, and 270 of 287 alone), and a thread that is woken late looks like
/// one that slept out its wait. A wait that ends by the timeout is one that nothing woke in 20 ms,
/// which a frame every 2 ms does not allow unless the frame thread itself was stalled, so a loaded
/// machine adds few; the allowance is half. The loop goes on until the thread has waited 40 times
/// (or 8 s), as a starved thread waits few times in a short run. With the `notify_one` of
/// `SceneWorker::take` removed every wait ends by the timeout (10 of 10, 22 of 22 in scratch
/// runs). Real threads, so it is in `.github/real-thread-tests.txt`.
#[test]
fn a_throttled_scene_thread_goes_on_when_the_frame_thread_takes_what_waited() {
    let (mut world, _server, camera) = spawned_world(Run::Threads(2));
    world.worker().set_upload_budget(render::UploadCost {
        bytes: 1,
        writes: 1,
    });
    let (mut frames, mut with_update) = (0, 0);
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(8)
        && world.worker().throttled_waits().0 < 40
        && !idle(&world)
    {
        // Updates counted only while the loader still has work: a world that has filled in
        // publishes nothing, whatever wakes the thread.
        let loading = world.loader().is_some_and(|l| l.pending > 0);
        let updated = world.recentre(camera.position).unwrap().is_some();
        if loading {
            frames += 1;
            with_update += usize::from(updated);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let (waits, timeouts) = world.worker().throttled_waits();
    eprintln!(
        "{with_update} of {frames} frames had an update; {timeouts} of {waits} waits timed out"
    );
    // (39 on the 40 km test Planet, whose ground has its detail; the smooth sphere's was 50 or
    // more. A slower generator would give more frames, not fewer: the fill here is smaller.)
    assert!(frames >= 30, "only {frames} frames while the loader worked");
    assert!(
        waits >= 10,
        "the thread waited {waits} times in {:?}: it did not hold work back, or was starved",
        began.elapsed()
    );
    assert!(
        timeouts * 2 <= waits,
        "{timeouts} of {waits} waits of the scene thread ended by the timeout: it slept on after \
         the frame thread took what waited"
    );
}

#[test]
fn after_one_batch_over_the_budget_further_batches_are_taken_again_once_the_backlog_has_drained() {
    // A budget under what the fill's biggest batch costs, and over what a batch of a flight at a
    // walking pace does: the fill goes over it once or more, and the flight after is no reason
    // to take one batch a round. (The estimate that one more batch needs room for was the most a
    // batch had ever cost, so after the fill it was over the budget with nothing waiting, and
    // every round took the one batch that it is given and no more.)
    let budget = render::UploadCost {
        bytes: 100 << 10,
        writes: 200,
    };
    let (mut world, _server, camera) = spawned_world(Run::Stepped);
    world.count_exactly();
    world.worker().set_upload_budget(budget);
    let mut patience = Patience::new(Duration::from_secs(180));
    loop {
        world.recentre(camera.position).unwrap();
        if idle(&world) && !world.stats().throttled {
            break;
        }
        patience.wait(&world, || "the resident set does not fill".into());
    }
    let big = world.stats().largest_batch;
    assert!(
        big.bytes > budget.bytes,
        "no batch was over the budget: {big:?}"
    );
    // The backlog is gone: nothing waits, and nothing is held back.
    assert!(idle(&world) && !world.stats().throttled);
    let (ahead, _) = tangents(camera.horizon_up);
    let (mut moved, frames) = (camera.position, 120);
    let before = world.stats().work;
    for _ in 0..frames {
        moved = std::array::from_fn(|a| moved[a] + ahead[a] * 2.0);
        let position = over_the_ground(&world.store(), moved);
        world.recentre(position).unwrap();
        pace(&world, FRAME);
    }
    let batches = world.stats().work.pack_steps - before.pack_steps;
    eprintln!("{batches} batches in {frames} frames, after a batch of {big:?}");
    // More than the one a round is always given. (139 on the 40 km test Planet, whose ground has
    // its detail: the flight over it has fewer arrivals a frame than the smooth sphere's, which
    // had 150 or more. The batches are counted in stepped rounds, not by a clock.)
    assert!(
        batches as usize >= frames + frames / 10,
        "{batches} batches in {frames} frames: one a round, as if the budget were still over"
    );
}
