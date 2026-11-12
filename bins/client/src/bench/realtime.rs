//! The real-time mode of the streaming bench (#162): the full-size Planet with the real loader
//! workers and the real scene thread, on the machine's clock, along paths of the flights of ADR
//! 0004 and #143, and what the frame thread and the scene thread cost on them.
//!
//! The stepped mode (`stream.rs`, #181) is the opposite: the test Planet, no threads, a clock that a
//! driver moves, and counts that are the same on every run. This one is **not deterministic**. A
//! frame is at 60 Hz by the machine's clock, the loader's workers and the scene thread run when the
//! operating system lets them, and so what they have done by each frame differs from run to run.
//! Everything in [`RealTimeReport`] is made by this mode, and none of it is in the stepped mode's
//! counts or its hash. Because the counts vary, the paths are run [`--runs`](super::args) times
//! (three by default), the report keeps every run and the minimum and the maximum of each number
//! over them, and the gates are held to the worst run.
//!
//! One run: the world is made as the client makes it ([`crate::load`]), and the **cold start** is
//! the frames at the Spawn from the first until the resident set is complete. The sizing constants
//! are measured there. Then each path starts at the Spawn, with the resident set complete and the
//! camera still, and ends with the camera back at it and the loader caught up. Each frame is the
//! frame thread's work, in the order the game does it: [`World::recentre`], the copy to the GPU, and
//! the draw. **Frame time** is all of that, up to the GPU having finished (the offscreen draw
//! waits for it). **Frame-thread CPU time** is the part that is the CPU's: the recentring, the copy
//! of the update to the GPU's buffers, and the encoding and submitting of the draw, without the wait.
//!
//! The scene thread cannot count holes for every frame at full size (every split brick is looked
//! at, 0.3 s with the store's read lock held), so it does it every few seconds
//! ([`crate::scene_worker::SceneWorker::count_holes`]) and the report says how many looks there
//! were and what they found: **holes are sampled, not counted per frame**, as the stepped mode's are.

use std::error::Error;
use std::time::{Duration, Instant};

use brick_store::{Budget, VOXELS_PER_METRE};
use net::{ServerEnd, loopback};
use planet_gen::planet::tangents;
use planet_gen::world_map::{WORLD_MAP_SIZE, WorldMap};
use render::reference::{Ray, trace};
use render::{Camera, Offscreen, Resolution, Scene, SceneUpdate, camera};
use serde::Serialize;
use serde_json::Value;
use sim::Predictor;

use super::budget::{EXIT_OVER_BUDGET, StreamingBudgets};
use super::process;
use super::stream::{
    Constants, Series, Sizing, Start, TestPlanet, measure_sizes, over_the_ground, test_planet,
};
use crate::loader::{Loader, workers_for};
use crate::scene_worker::Spent;
use crate::world::World;

/// A frame at 60 a second.
const FRAME: Duration = Duration::from_micros(16_667);
/// The camera's speed on the flights: 50 m/s, as #143's.
const FLIGHT_METRES_PER_SECOND: f64 = 50.0;
/// How often the store's size is looked at, in frames: it takes the store's read lock.
const STORE_EVERY: u64 = 6;
/// Metres above the 2 m flight that the high one flies.
const HIGH_METRES: f64 = 48.0;
/// What the frame thread had to say of the edit latency, which no path measures.
const EDIT_LATENCY: &str = "not measured: no path of the real-time mode makes an Edit (the versioned M1a path with a dig is #168)";

/// How long each path goes on for, and how long the waits are held to.
#[derive(Clone, Copy, Debug)]
pub struct Script {
    pub flight: Duration,
    pub take_off: Duration,
    pub turn: Duration,
    /// How long the flight before the stop goes on.
    pub flight_before_stop: Duration,
    /// The most that the camera is stopped for before the soft misses are said not to settle.
    pub settle_cap: Duration,
    /// The most that the cold start and the catching up between paths wait for the loader.
    pub fill_cap: Duration,
    pub between_cap: Duration,
}

impl Script {
    /// The measurement, at full size: the paths of #143.
    pub fn full() -> Self {
        Self {
            flight: Duration::from_secs(10),
            take_off: Duration::from_secs(6),
            turn: Duration::from_secs(2),
            flight_before_stop: Duration::from_secs(5),
            settle_cap: Duration::from_secs(120),
            fill_cap: Duration::from_secs(5400),
            between_cap: Duration::from_secs(1200),
        }
    }

    /// The smoke test, on the test Planet: the same paths for a second or two each.
    pub fn short() -> Self {
        Self {
            flight: Duration::from_secs(1),
            take_off: Duration::from_secs(1),
            turn: Duration::from_secs(1),
            flight_before_stop: Duration::from_secs(1),
            settle_cap: Duration::from_secs(20),
            fill_cap: Duration::from_secs(600),
            between_cap: Duration::from_secs(300),
        }
    }
}

/// What the frame thread does with a frame's update and its drawing: the copy to a GPU and a draw
/// there, or nothing in a test with no GPU.
pub trait Gpu {
    /// Copies `update` (how `scene` changed), if there is one, to the GPU and draws a frame from
    /// `camera`. Returns the CPU time of that, without the wait for the GPU.
    fn frame(
        &mut self,
        camera: &Camera,
        scene: &Scene,
        update: Option<&SceneUpdate>,
    ) -> Result<Duration, Box<dyn Error>>;
}

/// The offscreen GPU of the bench.
pub struct OffscreenGpu<'a>(pub &'a mut Offscreen);

impl Gpu for OffscreenGpu<'_> {
    fn frame(
        &mut self,
        camera: &Camera,
        scene: &Scene,
        update: Option<&SceneUpdate>,
    ) -> Result<Duration, Box<dyn Error>> {
        let began = Instant::now();
        if let Some(update) = update {
            self.0.update_scene(scene, update)?;
        }
        let copied = began.elapsed();
        Ok(copied + self.0.draw_upscaled_timed(camera))
    }
}

/// A run with no GPU: only the frame thread's work on the world.
#[cfg(test)]
pub struct NoGpu;

#[cfg(test)]
impl Gpu for NoGpu {
    fn frame(
        &mut self,
        _: &Camera,
        _: &Scene,
        _: Option<&SceneUpdate>,
    ) -> Result<Duration, Box<dyn Error>> {
        Ok(Duration::ZERO)
    }
}

/// Times in milliseconds of the frames of a path: the 99th percentile (nearest rank), the worst
/// and the mean.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Spread {
    pub samples: usize,
    pub mean_ms: f64,
    pub p99_ms: f64,
    pub worst_ms: f64,
}

impl Spread {
    pub fn of(times: &[f64]) -> Self {
        if times.is_empty() {
            return Self::default();
        }
        let mut sorted = times.to_vec();
        sorted.sort_by(f64::total_cmp);
        let rank = (sorted.len() * 99).div_ceil(100);
        Self {
            samples: sorted.len(),
            mean_ms: sorted.iter().sum::<f64>() / sorted.len() as f64,
            p99_ms: sorted[rank - 1],
            worst_ms: sorted[sorted.len() - 1],
        }
    }
}

/// How long the scene thread spent on each part of its rounds, in milliseconds, over a path or a
/// run (`Spent`): following the camera (selecting again and packing what changed), packing what the
/// loader made, publishing (copying what changed out for the frame thread, which copies it to the
/// GPU: the upload), eviction, remaking the coarse levels over an Edit, and the looks for holes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct SceneThread {
    pub follow_ms: f64,
    pub pack_ms: f64,
    pub upload_ms: f64,
    pub evict_ms: f64,
    pub coarse_ms: f64,
    pub hole_scan_ms: f64,
}

impl SceneThread {
    /// What was spent from `then` to `now`.
    fn between(then: Spent, now: Spent) -> Self {
        let ms = |now: Duration, then: Duration| now.saturating_sub(then).as_secs_f64() * 1e3;
        Self {
            follow_ms: ms(now.follow, then.follow),
            pack_ms: ms(now.pack, then.pack),
            upload_ms: ms(now.publish, then.publish),
            evict_ms: ms(now.evict, then.evict),
            coarse_ms: ms(now.coarse, then.coarse),
            hole_scan_ms: ms(now.hole_scan, then.hole_scan),
        }
    }
}

/// How long the camera stood after a flight before the scene had no soft misses.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Settle {
    /// From the frame the camera stopped in, to the first frame with no soft misses: or to the cap,
    /// if there were still some.
    pub seconds: f64,
    pub settled: bool,
}

/// One path of the camera, with what the frames of it cost.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PathReport {
    pub name: &'static str,
    pub frames: usize,
    pub seconds: f64,
    /// Frame time, up to the GPU being done, and the frame thread's CPU time (see the module).
    pub frame_time: Spread,
    pub frame_cpu: Spread,
    /// Soft misses a frame reported (counted by the scene thread a few times a second, so a frame
    /// reports the last count).
    pub soft_misses: Series,
    pub soft_misses_per_frame: f64,
    /// Looks of the scene thread at every split brick (every few seconds), the holes they found in
    /// all, and those per frame of the path.
    pub hole_scans: u64,
    pub holes: u64,
    pub holes_per_frame: f64,
    pub scene_thread: SceneThread,
    /// For the stop after a flight only.
    pub settle: Option<Settle>,
}

/// The cold start: the world made, and the frames at the Spawn until the resident set is complete.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ColdStart {
    /// Generating the world map, which the client does before it loads the Planet: not in the time
    /// to control. Null for the test Planet's.
    pub world_map_s: Option<f64>,
    /// [`crate::load`]: the spawn ring, the Guard, and the first scene.
    pub load_s: f64,
    /// **Time to control**: from the start of the load to the first frame with level 0 under the
    /// camera, the ground the character stands on. The bench has no server, so the character is not
    /// in it.
    pub time_to_control_s: f64,
    /// From the start of the load to the resident set being complete.
    pub fill_s: f64,
}

/// What the run held, at the most.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Memory {
    /// The process's peak resident memory since it began (it only grows over the runs), where the
    /// system says.
    pub process_peak_mib: Option<f64>,
    /// The scene's buffers in use, at the most over the run's frames, as the scene thread said.
    pub scene_peak_mib: f64,
    /// The store's size as its budget counts it, looked at every few frames, and the most it may
    /// hold before eviction starts.
    pub store_peak_bricks: u64,
    pub store_limit_bricks: u64,
}

/// One run of the paths.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunReport {
    pub cold_start: ColdStart,
    /// The cold start, then each path.
    pub paths: Vec<PathReport>,
    /// The scene thread's time over the whole run.
    pub scene_thread: SceneThread,
    pub memory: Memory,
    /// The client's sizing constants against what was measured at full size, with the resident set
    /// complete. Reported; the check of it is not a gate.
    pub sizing: Sizing,
}

/// A measure against what it is held to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Limit {
    /// The worst over the runs (and the paths).
    pub worst: f64,
    pub limit: f64,
}

impl Limit {
    fn over(&self) -> bool {
        self.worst > self.limit
    }
}

/// The four gates of the real-time mode (the budgets' `[streaming]`), each held to the worst run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gates {
    /// The 99th percentile of frame time, of the worst path of the worst run.
    pub frame_time_p99_ms: Limit,
    pub frame_cpu_p99_ms: Limit,
    /// Holes found by the scene thread's looks, summed over every path and run.
    pub holes: Limit,
    /// The memory gate has two measures: the scene's buffers and the store.
    pub scene_mib: Limit,
    pub store_bricks: Limit,
    /// The gates that are over: `frame_time`, `frame_cpu`, `holes`, `memory`.
    pub over: Vec<&'static str>,
}

/// Holds `runs` to `budgets`: the worst of the runs for each gate.
pub fn gates(runs: &[RunReport], budgets: &StreamingBudgets) -> Gates {
    let paths = || runs.iter().flat_map(|r| &r.paths);
    let worst = |values: &mut dyn Iterator<Item = f64>| values.fold(0.0, f64::max);
    let frame_time_p99_ms = Limit {
        worst: worst(&mut paths().map(|p| p.frame_time.p99_ms)),
        limit: budgets.frame_time_p99_ms,
    };
    let frame_cpu_p99_ms = Limit {
        worst: worst(&mut paths().map(|p| p.frame_cpu.p99_ms)),
        limit: budgets.frame_cpu_p99_ms,
    };
    let holes = Limit {
        worst: paths().map(|p| p.holes).sum::<u64>() as f64,
        limit: budgets.holes as f64,
    };
    let scene_mib = Limit {
        worst: worst(&mut runs.iter().map(|r| r.memory.scene_peak_mib)),
        limit: budgets.scene_mib,
    };
    let store_bricks = Limit {
        worst: worst(&mut runs.iter().map(|r| r.memory.store_peak_bricks as f64)),
        limit: runs
            .iter()
            .map(|r| r.memory.store_limit_bricks as f64)
            .fold(f64::INFINITY, f64::min),
    };
    let over = [
        ("frame_time", frame_time_p99_ms.over()),
        ("frame_cpu", frame_cpu_p99_ms.over()),
        ("holes", holes.over()),
        ("memory", scene_mib.over() || store_bricks.over()),
    ]
    .into_iter()
    .filter_map(|(name, over)| over.then_some(name))
    .collect();
    Gates {
        frame_time_p99_ms,
        frame_cpu_p99_ms,
        holes,
        scene_mib,
        store_bricks,
        over,
    }
}

/// The report of the real-time mode. Everything in it is made by that mode and is not
/// deterministic.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RealTimeReport {
    /// `real-time`, as opposed to the stepped mode that `streaming` is.
    pub mode: &'static str,
    pub deterministic: bool,
    pub planet: String,
    pub d0: f64,
    pub runs: Vec<RunReport>,
    /// Every number of a run (the same shape as one of `runs`), at its least and its most over the
    /// runs. What is not a number is the first run's.
    pub min: Value,
    pub max: Value,
    pub gates: Gates,
    /// Whether the Edit latency is measured in this mode (see [`EDIT_LATENCY`]).
    pub edit_latency: &'static str,
}

impl RealTimeReport {
    pub fn new(planet: String, d0: f64, runs: Vec<RunReport>, budgets: &StreamingBudgets) -> Self {
        let values: Vec<Value> = runs
            .iter()
            .map(|r| serde_json::to_value(r).expect("a run serializes"))
            .collect();
        let (min, max) = envelope(&values.iter().collect::<Vec<_>>());
        Self {
            mode: "real-time",
            deterministic: false,
            planet,
            d0,
            gates: gates(&runs, budgets),
            runs,
            min,
            max,
            edit_latency: EDIT_LATENCY,
        }
    }
}

/// Each number of `values`, which are of one shape, at its least and its most. Objects and arrays
/// of one length are done member by member; anything else is the first value's.
fn envelope(values: &[&Value]) -> (Value, Value) {
    let first = values[0];
    match first {
        Value::Object(fields) => {
            let (mut least, mut most) = (serde_json::Map::new(), serde_json::Map::new());
            for name in fields.keys() {
                let members: Option<Vec<&Value>> = values.iter().map(|v| v.get(name)).collect();
                let (low, high) =
                    members.map_or((first[name].clone(), first[name].clone()), |m| envelope(&m));
                least.insert(name.clone(), low);
                most.insert(name.clone(), high);
            }
            (Value::Object(least), Value::Object(most))
        }
        Value::Array(items)
            if values
                .iter()
                .all(|v| v.as_array().is_some_and(|a| a.len() == items.len())) =>
        {
            let (mut least, mut most) = (Vec::new(), Vec::new());
            for i in 0..items.len() {
                let members: Vec<&Value> = values.iter().map(|v| &v[i]).collect();
                let (low, high) = envelope(&members);
                least.push(low);
                most.push(high);
            }
            (Value::Array(least), Value::Array(most))
        }
        Value::Number(_) if values.iter().all(|v| v.is_number()) => {
            let by_value = |a: &&&Value, b: &&&Value| {
                a.as_f64()
                    .unwrap_or(f64::NAN)
                    .total_cmp(&b.as_f64().unwrap_or(f64::NAN))
            };
            let low = values.iter().min_by(by_value).expect("there is a value");
            let high = values.iter().max_by(by_value).expect("there is a value");
            ((**low).clone(), (**high).clone())
        }
        _ => (first.clone(), first.clone()),
    }
}

/// The exit status of the mode: [`EXIT_OVER_BUDGET`] if a gate is over, unless `no_fail`.
pub fn exit_code(report: &RealTimeReport, no_fail: bool) -> u8 {
    if report.gates.over.is_empty() || no_fail {
        0
    } else {
        EXIT_OVER_BUDGET
    }
}

/// A world right after the spawn: the Guard held and the rest waiting for the loader, with real
/// workers and a real scene thread, and what is known of it.
struct Built {
    world: World,
    /// The server's end of the world's link: held so that it stays connected, and never read.
    _server: ServerEnd,
    setup: Setup,
}

/// What a run needs to know of the world it is on.
struct Setup {
    spawn: Camera,
    d0: f64,
    start: Start,
    /// What the store may hold before eviction starts.
    store_limit: usize,
    constants: Constants,
    world_map_s: Option<f64>,
    load: Duration,
}

/// The Planet of the client, at full size: made as `main.rs` makes it.
fn build_full() -> Result<Built, Box<dyn Error>> {
    eprintln!("real time: generating the world map...");
    let began = Instant::now();
    let map = WorldMap::generate(crate::SEED, WORLD_MAP_SIZE);
    let world_map = began.elapsed();
    eprintln!("real time: loading the Planet...");
    let began = Instant::now();
    let loaded = crate::load(map, crate::SPAWN_REACH)
        .map_err(|_| "the ground around the spawn does not fit the scene's buffers")?;
    let load = began.elapsed();
    let start = Start::of(&loaded.store);
    let d0 = loaded.scene.bands().ok_or("the scene has no bands")?.d0();
    let store_limit = loaded
        .store
        .budget()
        .ok_or("the client's store has no budget")?
        .limit();
    let (client, server) = loopback();
    let world =
        World::new(loaded.scene, Predictor::new(loaded.store), client).with_loader(loaded.loader);
    Ok(Built {
        world,
        _server: server,
        setup: Setup {
            spawn: loaded.camera,
            d0,
            start,
            store_limit,
            // Level 0 beyond the spawn ring is in what is measured here, so none of it is taken
            // out.
            constants: Constants {
                streamed_level_0_beyond_the_ring_per_d0_squared: 0.0,
                ..Constants::of_the_client()
            },
            world_map_s: Some(world_map.as_secs_f64()),
            load,
        },
    })
}

/// The test Planet, with threads: what `--short` runs, as a smoke test of the mode.
fn build_test() -> Result<Built, Box<dyn Error>> {
    let began = Instant::now();
    let TestPlanet {
        mut store,
        scene,
        spawn,
        bands,
        start,
    } = test_planet();
    // The budget of the client's constants, for this `d0`, over the spawn ring that is held as well
    // (it is not scaled with `d0`).
    let budget = Budget {
        bricks: (crate::STREAMED_PER_D0_SQUARED * bands.d0() * bands.d0()) as usize
            + start.streamed,
        overflow: crate::BUDGET_OVERFLOW,
    };
    store.set_budget(budget);
    let source = store
        .source()
        .ok_or("the test Planet's store has a source")?;
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let loader = Loader::new(source, spawn.position, bands, workers_for(cores));
    let (client, server) = loopback();
    let world = World::new(scene, Predictor::new(store), client).with_loader(loader);
    Ok(Built {
        world,
        _server: server,
        setup: Setup {
            spawn,
            d0: bands.d0(),
            start,
            store_limit: budget.limit(),
            constants: Constants::of_the_client(),
            world_map_s: None,
            load: began.elapsed(),
        },
    })
}

/// What one frame cost, and the soft misses it reported.
struct Frame {
    time_ms: f64,
    cpu_ms: f64,
    soft_misses: usize,
}

/// What a path has seen so far.
struct Capture {
    name: &'static str,
    began: Instant,
    spent: Spent,
    scans: u64,
    holes: u64,
    frame_ms: Vec<f64>,
    cpu_ms: Vec<f64>,
    soft_misses: Series,
}

impl Capture {
    fn start(name: &'static str, world: &World) -> Self {
        let work = world.stats().work;
        Self {
            name,
            began: Instant::now(),
            spent: world.scene_thread_time(),
            scans: work.hole_scans,
            holes: work.holes_found,
            frame_ms: Vec::new(),
            cpu_ms: Vec::new(),
            soft_misses: Series::default(),
        }
    }

    fn add(&mut self, frame: &Frame) {
        self.frame_ms.push(frame.time_ms);
        self.cpu_ms.push(frame.cpu_ms);
        self.soft_misses.add(frame.soft_misses as u64);
    }

    fn finish(self, world: &World, settle: Option<Settle>) -> PathReport {
        let work = world.stats().work;
        let frames = self.frame_ms.len();
        let per_frame = |count: u64| count as f64 / frames.max(1) as f64;
        let holes = work.holes_found - self.holes;
        PathReport {
            name: self.name,
            frames,
            seconds: self.began.elapsed().as_secs_f64(),
            frame_time: Spread::of(&self.frame_ms),
            frame_cpu: Spread::of(&self.cpu_ms),
            soft_misses_per_frame: per_frame(self.soft_misses.total),
            soft_misses: self.soft_misses,
            hole_scans: work.hole_scans - self.scans,
            holes,
            holes_per_frame: per_frame(holes),
            scene_thread: SceneThread::between(self.spent, world.scene_thread_time()),
            settle,
        }
    }
}

/// The paths' geometry: the Spawn, and the directions the flights go in.
struct Course {
    spawn: Camera,
    ahead: [f64; 3],
    right: [f64; 3],
    radius: f64,
}

fn norm(v: [f64; 3]) -> f64 {
    v.iter().map(|c| c * c).sum::<f64>().sqrt()
}

impl Course {
    fn of(spawn: &Camera) -> Self {
        let (ahead, right) = tangents(spawn.horizon_up);
        Self {
            spawn: *spawn,
            ahead,
            right,
            radius: norm(spawn.position),
        }
    }

    /// The camera `metres` along `direction` from the Spawn, over the sphere the Spawn is on, at
    /// the height of a 2 m flight and `extra` Voxels above that.
    fn along(&self, world: &World, direction: [f64; 3], metres: f64, extra: f64) -> Camera {
        let flat: [f64; 3] = std::array::from_fn(|a| {
            self.spawn.position[a] + direction[a] * metres * VOXELS_PER_METRE
        });
        let on = flat.map(|c| c / norm(flat) * self.radius);
        let at = over_the_ground(&world.store(), on);
        let length = norm(at);
        Camera {
            position: std::array::from_fn(|a| at[a] + at[a] / length * extra),
            ..self.spawn
        }
    }
}

/// Whether the ground straight below `camera` is drawn at level 0.
fn level_0_under(scene: &Scene, camera: &Camera) -> bool {
    let (cell, origin) = camera::split(camera.position);
    let dir = camera.horizon_up.map(|c| -(c as f32));
    trace(scene, &Ray { cell, origin, dir }).is_some_and(|hit| scene.level_at(hit.voxel) == Some(0))
}

/// What the frame thread has on the world and the GPU, and the peaks over the frames.
struct Session<'a> {
    world: World,
    gpu: &'a mut dyn Gpu,
    scene_peak_bytes: u64,
    store_peak: usize,
    frames: u64,
}

impl Session<'_> {
    /// One frame, paced to 60 a second: the camera is told (with its velocity measured, as the
    /// game's is, if `lead`), what the scene thread made is taken, copied to the GPU and drawn.
    fn frame(&mut self, camera: &Camera, lead: bool) -> Result<Frame, Box<dyn Error>> {
        let began = Instant::now();
        let update = if lead {
            self.world.recentre(camera.position)?
        } else {
            self.world.recentre_moving(camera.position, [0.0; 3])?
        };
        let followed = began.elapsed();
        let drawn = self
            .gpu
            .frame(camera, self.world.scene(), update.as_ref())?;
        let time = began.elapsed();
        self.look_at_memory();
        std::thread::sleep(FRAME.saturating_sub(began.elapsed()));
        Ok(Frame {
            time_ms: time.as_secs_f64() * 1e3,
            cpu_ms: (followed + drawn).as_secs_f64() * 1e3,
            soft_misses: self.world.stats().soft_misses,
        })
    }

    fn look_at_memory(&mut self) {
        self.scene_peak_bytes = self.scene_peak_bytes.max(self.world.used_bytes());
        self.frames += 1;
        if self.frames.is_multiple_of(STORE_EVERY) {
            self.store_peak = self.store_peak.max(self.world.store().streamed_len());
        }
    }

    /// Whether the resident set is complete: nothing waits, and the loader has nothing out.
    fn idle(&self) -> bool {
        self.world.waiting() == 0 && self.world.loader().is_some_and(|l| l.pending == 0)
    }

    /// Frames at `camera`, not recorded, until the loader has caught up. The camera's jump to it
    /// is not a flight: the scene thread is told it is not moving.
    fn settle(&mut self, camera: &Camera, cap: Duration) -> Result<(), Box<dyn Error>> {
        let began = Instant::now();
        while !self.idle() {
            self.frame(camera, false)?;
            if began.elapsed() > cap {
                return Err(format!(
                    "the loader has not caught up after {:.0?}: {} bricks wait",
                    cap,
                    self.world.waiting()
                )
                .into());
            }
        }
        Ok(())
    }

    /// The cold start: frames at the Spawn from the first until the resident set is complete.
    fn cold_start(
        &mut self,
        setup: &Setup,
        script: &Script,
    ) -> Result<(ColdStart, PathReport), Box<dyn Error>> {
        let began = Instant::now();
        let mut capture = Capture::start("cold start", &self.world);
        let mut control = None;
        while !self.idle() || control.is_none() {
            let frame = self.frame(&setup.spawn, true)?;
            capture.add(&frame);
            if control.is_none() && level_0_under(self.world.scene(), &setup.spawn) {
                control = Some(setup.load + began.elapsed());
            }
            if began.elapsed() > script.fill_cap {
                return Err(format!(
                    "the resident set is not complete after {:.0?}",
                    script.fill_cap
                )
                .into());
            }
        }
        let cold = ColdStart {
            world_map_s: setup.world_map_s,
            load_s: setup.load.as_secs_f64(),
            time_to_control_s: control.expect("the loop ends with control").as_secs_f64(),
            fill_s: (setup.load + began.elapsed()).as_secs_f64(),
        };
        Ok((cold, capture.finish(&self.world, None)))
    }

    /// The camera at `at(world, t)` for `seconds`, `t` the seconds since the path began.
    fn path(
        &mut self,
        name: &'static str,
        seconds: Duration,
        mut at: impl FnMut(&World, f64) -> Camera,
    ) -> Result<PathReport, Box<dyn Error>> {
        let mut capture = Capture::start(name, &self.world);
        let began = Instant::now();
        loop {
            let camera = at(&self.world, began.elapsed().as_secs_f64());
            let frame = self.frame(&camera, true)?;
            capture.add(&frame);
            if began.elapsed() >= seconds {
                break;
            }
        }
        Ok(capture.finish(&self.world, None))
    }

    /// A flight, and then the camera standing where it stopped until no brick is drawn coarser than
    /// the bands ask (the **settle time**), or the cap.
    fn stop_after_a_flight(
        &mut self,
        course: &Course,
        script: &Script,
    ) -> Result<PathReport, Box<dyn Error>> {
        let mut capture = Capture::start("stop after a flight", &self.world);
        let began = Instant::now();
        let camera = loop {
            let t = began.elapsed().as_secs_f64();
            let camera = course.along(&self.world, course.ahead, FLIGHT_METRES_PER_SECOND * t, 0.0);
            let frame = self.frame(&camera, true)?;
            capture.add(&frame);
            if began.elapsed() >= script.flight_before_stop {
                break camera;
            }
        };
        let stopped = Instant::now();
        let settle = loop {
            let frame = self.frame(&camera, true)?;
            capture.add(&frame);
            if frame.soft_misses == 0 {
                break Settle {
                    seconds: stopped.elapsed().as_secs_f64(),
                    settled: true,
                };
            }
            if stopped.elapsed() >= script.settle_cap {
                break Settle {
                    seconds: stopped.elapsed().as_secs_f64(),
                    settled: false,
                };
            }
        };
        Ok(capture.finish(&self.world, Some(settle)))
    }

    /// The client's sizing constants against what the complete resident set measures: the scene
    /// thread counts exactly once, here, and not in every publish (see
    /// [`World::count_once_exactly`]).
    fn sizing(&mut self, setup: &Setup) -> Result<Sizing, Box<dyn Error>> {
        let update = self.world.count_once_exactly()?;
        // What the count published is copied to the GPU like any update.
        self.gpu
            .frame(&setup.spawn, self.world.scene(), update.as_ref())?;
        let streamed = self.world.store().streamed_len();
        let measured = measure_sizes(&self.world, setup.start, setup.d0, streamed)?;
        Ok(Sizing::check(measured, &setup.constants))
    }
}

/// One run of the paths on `built`, drawing with `gpu`.
fn one_run(built: Built, gpu: &mut dyn Gpu, script: &Script) -> Result<RunReport, Box<dyn Error>> {
    let Built { world, setup, .. } = built;
    world.count_soft_misses();
    world.measure_scene_thread();
    let mut session = Session {
        world,
        gpu,
        scene_peak_bytes: 0,
        store_peak: 0,
        frames: 0,
    };
    let course = Course::of(&setup.spawn);
    let (cold_start, cold) = session.cold_start(&setup, script)?;
    let mut paths = vec![cold];
    let sizing = session.sizing(&setup)?;
    let flight = |height: f64, direction: [f64; 3]| {
        let course = &course;
        move |world: &World, t: f64| {
            course.along(world, direction, FLIGHT_METRES_PER_SECOND * t, height)
        }
    };
    let speed = FLIGHT_METRES_PER_SECOND * VOXELS_PER_METRE;
    let spawn = setup.spawn;
    session.settle(&spawn, script.between_cap)?;
    paths.push(session.path("flight at 2 m", script.flight, flight(0.0, course.ahead))?);
    session.settle(&spawn, script.between_cap)?;
    paths.push(session.path(
        "flight at 50 m",
        script.flight,
        flight(HIGH_METRES * VOXELS_PER_METRE, course.right),
    )?);
    session.settle(&spawn, script.between_cap)?;
    paths.push(
        session.path("vertical take-off", script.take_off, |_, t| Camera {
            position: std::array::from_fn(|a| spawn.position[a] + spawn.horizon_up[a] * speed * t),
            ..spawn
        })?,
    );
    session.settle(&spawn, script.between_cap)?;
    // A half turn in the first half of the path and the view still for the second.
    let half = script.turn.as_secs_f64() / 2.0;
    paths.push(session.path("180 degree turn", script.turn, |_, t| {
        let mut turned = spawn;
        turned.turn(std::f64::consts::PI * (t / half).min(1.0), 0.0);
        turned
    })?);
    session.settle(&spawn, script.between_cap)?;
    paths.push(session.stop_after_a_flight(&course, script)?);

    let memory = Memory {
        process_peak_mib: process::peak_bytes().map(|b| b as f64 / MIB),
        scene_peak_mib: session.scene_peak_bytes as f64 / MIB,
        store_peak_bricks: session.store_peak as u64,
        store_limit_bricks: setup.store_limit as u64,
    };
    Ok(RunReport {
        cold_start,
        paths,
        scene_thread: SceneThread::between(Spent::default(), session.world.scene_thread_time()),
        memory,
        sizing,
    })
}

/// Runs the real-time mode: `runs` runs of the paths, each on a world of its own and a GPU of its
/// own, at full size or (for `--short`) on the test Planet, and the report of them.
pub fn run(
    full: bool,
    runs: u32,
    resolution: Resolution,
    budgets: &StreamingBudgets,
) -> Result<RealTimeReport, Box<dyn Error>> {
    let script = if full {
        Script::full()
    } else {
        Script::short()
    };
    let mut reports = Vec::new();
    let mut d0 = 0.0;
    for run in 1..=runs {
        eprintln!("real time: run {run} of {runs}");
        let began = Instant::now();
        let built = if full { build_full()? } else { build_test()? };
        d0 = built.setup.d0;
        let mut gpu = Offscreen::with_resolution(resolution, built.world.scene())?;
        let report = one_run(built, &mut OffscreenGpu(&mut gpu), &script)?;
        eprintln!("real time: run {run} took {:.0?}", began.elapsed());
        reports.push(report);
    }
    let planet = if full {
        "full-size".to_string()
    } else {
        "test".to_string()
    };
    Ok(RealTimeReport::new(planet, d0, reports, budgets))
}

const MIB: f64 = 1024.0 * 1024.0;

#[cfg(test)]
mod tests {
    use super::super::stream::Measured;
    use super::*;

    const BUDGETS: StreamingBudgets = StreamingBudgets {
        frame_time_p99_ms: 16.7,
        frame_cpu_p99_ms: 8.0,
        holes: 0,
        scene_mib: 512.0,
    };

    fn path(name: &'static str, frame_p99: f64, cpu_p99: f64, holes: u64) -> PathReport {
        PathReport {
            name,
            frames: 600,
            seconds: 10.0,
            frame_time: Spread {
                samples: 600,
                mean_ms: frame_p99 / 2.0,
                p99_ms: frame_p99,
                worst_ms: frame_p99 * 2.0,
            },
            frame_cpu: Spread {
                samples: 600,
                mean_ms: cpu_p99 / 2.0,
                p99_ms: cpu_p99,
                worst_ms: cpu_p99 * 2.0,
            },
            soft_misses: Series::default(),
            soft_misses_per_frame: 0.0,
            hole_scans: 2,
            holes,
            holes_per_frame: 0.0,
            scene_thread: SceneThread::default(),
            settle: None,
        }
    }

    fn measured() -> Measured {
        Measured {
            ring_bricks: 0,
            guard_bricks: 0,
            ring_refs_words: 0,
            ring_bricks_words: 0,
            guard_refs_words: 0,
            guard_bricks_words: 0,
            streamed: 0,
            streamed_at_start: 0,
            level_0_bricks_at_start: 0,
            level_0_bricks_when_complete: 0,
            bricks_per_d0_squared: 0.0,
            refs_per_brick: 0.0,
            words_per_brick: 0.0,
            words_per_guard_brick: 0.0,
            streamed_per_d0_squared: 0.0,
        }
    }

    /// A run that is within every gate, but for what `change` does to it.
    fn run_with(change: impl FnOnce(&mut RunReport)) -> RunReport {
        let mut run = RunReport {
            cold_start: ColdStart {
                world_map_s: Some(25.0),
                load_s: 60.0,
                time_to_control_s: 61.0,
                fill_s: 200.0,
            },
            paths: vec![
                path("cold start", 12.0, 5.0, 0),
                path("flight at 2 m", 11.0, 6.0, 0),
            ],
            scene_thread: SceneThread::default(),
            memory: Memory {
                process_peak_mib: Some(9000.0),
                scene_peak_mib: 474.0,
                store_peak_bricks: 20_000_000,
                store_limit_bricks: 23_000_000,
            },
            sizing: Sizing::check(measured(), &Constants::of_the_client()),
        };
        change(&mut run);
        run
    }

    fn over(runs: &[RunReport]) -> Vec<&'static str> {
        gates(runs, &BUDGETS).over
    }

    #[test]
    fn a_run_within_every_gate_passes_with_status_0() {
        let report =
            RealTimeReport::new("full-size".into(), 1028.2, vec![run_with(|_| {})], &BUDGETS);
        assert!(report.gates.over.is_empty(), "{:?}", report.gates);
        assert_eq!(exit_code(&report, false), 0);
    }

    #[test]
    fn the_frame_time_gate_fires_alone_when_a_paths_p99_is_over_16_7_ms() {
        let slow = run_with(|r| r.paths[1].frame_time.p99_ms = 16.8);
        assert_eq!(over(std::slice::from_ref(&slow)), ["frame_time"]);
        // Up to the budget is within it.
        assert!(over(&[run_with(|r| r.paths[1].frame_time.p99_ms = 16.7)]).is_empty());
        let report = RealTimeReport::new("test".into(), 64.0, vec![slow], &BUDGETS);
        assert_eq!(exit_code(&report, false), 2);
        assert_eq!(exit_code(&report, true), 0, "--no-fail");
        assert_eq!(EXIT_OVER_BUDGET, 2);
    }

    #[test]
    fn the_frame_thread_cpu_gate_fires_alone_when_a_paths_p99_is_over_8_ms() {
        let slow = run_with(|r| r.paths[0].frame_cpu.p99_ms = 8.1);
        assert_eq!(over(std::slice::from_ref(&slow)), ["frame_cpu"]);
        assert!(over(&[run_with(|r| r.paths[0].frame_cpu.p99_ms = 8.0)]).is_empty());
        let report = RealTimeReport::new("test".into(), 64.0, vec![slow], &BUDGETS);
        assert_eq!(exit_code(&report, false), 2);
    }

    #[test]
    fn the_holes_gate_fires_alone_on_one_hole_in_any_path_of_any_run() {
        let hole = run_with(|r| r.paths[1].holes = 1);
        assert_eq!(over(std::slice::from_ref(&hole)), ["holes"]);
        // Three runs, one hole in the last: the holes are summed, and one is too many.
        assert_eq!(
            over(&[run_with(|_| {}), run_with(|_| {}), hole.clone()]),
            ["holes"]
        );
        let report = RealTimeReport::new("test".into(), 64.0, vec![hole], &BUDGETS);
        assert_eq!(exit_code(&report, false), 2);
    }

    #[test]
    fn the_memory_gate_fires_alone_for_the_scene_over_its_budget_or_the_store_over_its_limit() {
        let scene = run_with(|r| r.memory.scene_peak_mib = 512.5);
        assert_eq!(over(std::slice::from_ref(&scene)), ["memory"]);
        let store = run_with(|r| r.memory.store_peak_bricks = r.memory.store_limit_bricks + 1);
        assert_eq!(over(std::slice::from_ref(&store)), ["memory"]);
        // At the limit is within it, and the process's peak is reported, not gated.
        assert!(
            over(&[run_with(
                |r| r.memory.store_peak_bricks = r.memory.store_limit_bricks
            )])
            .is_empty()
        );
        assert!(over(&[run_with(|r| r.memory.process_peak_mib = Some(1e9))]).is_empty());
        let report = RealTimeReport::new("test".into(), 64.0, vec![store], &BUDGETS);
        assert_eq!(exit_code(&report, false), 2);
    }

    #[test]
    fn the_gates_use_the_worst_of_the_runs_and_the_report_has_every_gate_at_it() {
        let runs = [
            run_with(|r| r.paths[1].frame_time.p99_ms = 9.0),
            run_with(|r| r.paths[1].frame_time.p99_ms = 20.0),
            run_with(|r| r.paths[1].frame_time.p99_ms = 10.0),
        ];
        let gates = gates(&runs, &BUDGETS);
        assert_eq!(gates.frame_time_p99_ms.worst, 20.0);
        assert_eq!(gates.over, ["frame_time"]);
        // Soft misses, time to control and settle time are reported and not gated: a run with
        // many, and a long time to control, is still within.
        let slow_to_settle = run_with(|r| {
            r.cold_start.time_to_control_s = 600.0;
            r.paths[1].soft_misses_per_frame = 5000.0;
            r.paths[1].settle = Some(Settle {
                seconds: 120.0,
                settled: false,
            });
        });
        assert!(over(&[slow_to_settle]).is_empty());
    }

    #[test]
    fn the_least_and_the_most_of_every_number_over_the_runs_are_in_the_report() {
        let runs = vec![
            run_with(|r| {
                r.paths[1].frame_time.p99_ms = 9.0;
                r.paths[1].holes = 0;
                r.memory.scene_peak_mib = 470.0;
            }),
            run_with(|r| {
                r.paths[1].frame_time.p99_ms = 12.5;
                r.paths[1].holes = 0;
                r.memory.scene_peak_mib = 480.0;
            }),
            run_with(|r| {
                r.paths[1].frame_time.p99_ms = 7.0;
                r.paths[1].holes = 0;
                r.memory.scene_peak_mib = 475.0;
            }),
        ];
        let report = RealTimeReport::new("full-size".into(), 1028.2, runs, &BUDGETS);
        let p99 = |v: &Value| v["paths"][1]["frame_time"]["p99_ms"].as_f64().unwrap();
        assert_eq!((p99(&report.min), p99(&report.max)), (7.0, 12.5));
        assert_eq!(report.min["memory"]["scene_peak_mib"], 470.0);
        assert_eq!(report.max["memory"]["scene_peak_mib"], 480.0);
        // Integers stay integers, and what is not a number is the first run's.
        assert_eq!(report.max["paths"][1]["frames"], 600);
        assert_eq!(report.min["paths"][1]["name"], "flight at 2 m");
        // The min and the max have the shape of a run, so a field added to a run is in them.
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        let run = serde_json::to_value(&report.runs[0]).unwrap();
        assert_eq!(keys(&report.min), keys(&run));
        assert_eq!(keys(&report.max), keys(&run));
    }

    #[test]
    fn the_report_serialises_to_the_documented_shape() {
        let report =
            RealTimeReport::new("full-size".into(), 1028.2, vec![run_with(|_| {})], &BUDGETS);
        let json = serde_json::to_value(&report).unwrap();
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            keys(&json),
            [
                "d0",
                "deterministic",
                "edit_latency",
                "gates",
                "max",
                "min",
                "mode",
                "planet",
                "runs"
            ]
        );
        assert_eq!(json["mode"], "real-time");
        assert_eq!(json["deterministic"], false);
        assert!(
            json["edit_latency"]
                .as_str()
                .unwrap()
                .starts_with("not measured")
        );
        assert_eq!(
            keys(&json["runs"][0]),
            ["cold_start", "memory", "paths", "scene_thread", "sizing"]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]),
            [
                "frame_cpu",
                "frame_time",
                "frames",
                "hole_scans",
                "holes",
                "holes_per_frame",
                "name",
                "scene_thread",
                "seconds",
                "settle",
                "soft_misses",
                "soft_misses_per_frame"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]["scene_thread"]),
            [
                "coarse_ms",
                "evict_ms",
                "follow_ms",
                "hole_scan_ms",
                "pack_ms",
                "upload_ms"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["cold_start"]),
            ["fill_s", "load_s", "time_to_control_s", "world_map_s"]
        );
        assert_eq!(
            keys(&json["gates"]),
            [
                "frame_cpu_p99_ms",
                "frame_time_p99_ms",
                "holes",
                "over",
                "scene_mib",
                "store_bricks"
            ]
        );
    }

    #[test]
    fn the_99th_percentile_is_the_nearest_rank_and_the_worst_is_the_longest() {
        let times: Vec<f64> = (1..=200).rev().map(f64::from).collect();
        let spread = Spread::of(&times);
        assert_eq!(spread.samples, 200);
        assert_eq!(spread.p99_ms, 198.0);
        assert_eq!(spread.worst_ms, 200.0);
        assert_eq!(spread.mean_ms, 100.5);
        assert_eq!(Spread::of(&[]), Spread::default());
        assert_eq!(Spread::of(&[3.0]).p99_ms, 3.0);
    }

    /// The real-time mode on the test Planet, with real workers and a real scene thread and no GPU:
    /// every path runs and the report has its fields. It asserts no time, only what was run: how long
    /// anything took is not for a test to hold. It is in `.github/real-thread-tests.txt`, and does
    /// not gate.
    #[test]
    fn the_real_time_run_on_the_test_planet_completes_with_every_path_and_field() {
        let built = build_test().expect("the test Planet builds");
        let d0 = built.setup.d0;
        let run = one_run(built, &mut NoGpu, &Script::short()).expect("the run completes");
        let names: Vec<_> = run.paths.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            [
                "cold start",
                "flight at 2 m",
                "flight at 50 m",
                "vertical take-off",
                "180 degree turn",
                "stop after a flight"
            ]
        );
        for path in &run.paths {
            assert!(path.frames > 0, "{}", path.name);
            assert_eq!(path.frame_time.samples, path.frames);
            assert_eq!(path.frame_cpu.samples, path.frames);
            assert_eq!(path.soft_misses.frames, path.frames);
        }
        let stop = run.paths.last().unwrap();
        assert!(stop.settle.is_some() && run.paths[..5].iter().all(|p| p.settle.is_none()));
        let cold = &run.cold_start;
        assert!(cold.time_to_control_s >= cold.load_s && cold.fill_s >= cold.time_to_control_s);
        // The thread did the work of a fill, which is timed by category.
        assert!(run.scene_thread.pack_ms > 0.0 && run.scene_thread.follow_ms > 0.0);
        assert!(run.memory.scene_peak_mib > 0.0 && run.memory.store_peak_bricks > 0);
        assert!(run.memory.store_limit_bricks >= run.memory.store_peak_bricks);
        // The sizing is measured at the complete set, with the counts made once and exactly.
        assert!(run.sizing.measured.ring_bricks > 0 && run.sizing.measured.guard_bricks > 0);
        assert_eq!(
            run.sizing.measured.ring_refs_words,
            run.sizing.measured.ring_bricks
        );
        // The scene thread looked for holes at least in the cold start's few seconds, or not at all
        // if it was quicker: what it found is the gate.
        let report = RealTimeReport::new("test".into(), d0, vec![run], &BUDGETS);
        assert_eq!(report.gates.holes.worst, 0.0, "{:?}", report.gates);
        serde_json::to_string(&report).expect("the report serializes");
    }
}
