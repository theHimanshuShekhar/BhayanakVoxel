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
//! Outside both: the camera's own computation before the frame starts (it reads the store, so it
//! can wait for its lock; see [`SlowFrame::camera_ms`]), and the frame's look at the memory after
//! its time is taken (the store's lock again; [`SlowFrame::after_ms`]). A frame over the limit is
//! reported with its parts ([`SlowFrame`]), so that the time can be placed.
//!
//! The scene thread cannot count holes for every frame at full size (every split brick is looked
//! at, 0.3 s with the store's read lock held), so it looks every few seconds
//! ([`crate::scene_worker::SceneWorker::count_holes`]), and a look is **forced** at the end of every
//! path, when the camera has stood still between paths (the loader's catching up), after the sizing
//! and at the end of the run ([`crate::scene_worker::SceneWorker::look_now`]), so that every path
//! has at least one. The report says how many looks there were, per path and per run, and what they
//! found. The holes gate is on what the looks of the **whole run** found, from the moment the
//! measuring began, and fails if a run or a path had no look at all. **Holes are sampled, not
//! counted per frame** as the stepped mode's are: a hole that is made and filled between two looks is
//! not seen.

use std::any::Any;
use std::error::Error;
use std::panic::{AssertUnwindSafe, catch_unwind};
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
    within_reach,
};
use crate::loader::{Loader, workers_for};
use crate::scene_worker::{Done, Spent};
use crate::tool::Tool;
use crate::world::World;

/// A frame at 60 a second.
const FRAME: Duration = Duration::from_micros(16_667);
/// The camera's speed on the flights: 50 m/s, as #143's.
const FLIGHT_METRES_PER_SECOND: f64 = 50.0;
/// How often the store's size is looked at, in frames: it takes the store's read lock.
const STORE_EVERY: u64 = 6;
/// Metres above the 2 m flight that the high one flies.
const HIGH_METRES: f64 = 48.0;
/// How often the process's resident memory is looked at, in frames (every half second at 60 a
/// second).
const RESIDENT_EVERY: u64 = 30;
/// Frames that the Edit may take to show before it is said not to have: ten seconds.
const EDIT_FRAMES_CAP: u64 = 600;

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
    /// `camera`. Returns the parts of the time that took, the wait for the GPU last.
    fn frame(
        &mut self,
        camera: &Camera,
        scene: &Scene,
        update: Option<&SceneUpdate>,
    ) -> Result<GpuFrame, Box<dyn Error>>;
}

/// What a frame's copy to the GPU and draw took, by part.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuFrame {
    /// Queuing the update's writes (`write_buffer`), or for a rebuild making the new buffers.
    pub write: Duration,
    /// The submit that sends them.
    pub flush: Duration,
    /// Calls of `write_buffer`: each is a copy on the GPU with a barrier of its own.
    pub writes: u32,
    /// Encoding the draw, and submitting it.
    pub encode: Duration,
    pub submit: Duration,
    /// Waiting for the GPU to be done: the update's copies and the draw, and whatever else the GPU
    /// was doing.
    pub wait: Duration,
}

impl GpuFrame {
    /// The CPU's part: all but the wait.
    fn cpu(&self) -> Duration {
        self.write + self.flush + self.encode + self.submit
    }
}

/// The offscreen GPU of the bench.
pub struct OffscreenGpu<'a>(pub &'a mut Offscreen);

impl Gpu for OffscreenGpu<'_> {
    fn frame(
        &mut self,
        camera: &Camera,
        scene: &Scene,
        update: Option<&SceneUpdate>,
    ) -> Result<GpuFrame, Box<dyn Error>> {
        let mut frame = GpuFrame::default();
        if let Some(update) = update {
            self.0.update_scene(scene, update)?;
            let times = self.0.last_update_times();
            frame.write = times.write;
            frame.flush = times.flush;
            frame.writes = times.writes;
        }
        self.0.draw_upscaled_timed(camera);
        let drawn = self.0.last_draw_times();
        frame.encode = drawn.encode;
        frame.submit = drawn.submit;
        frame.wait = drawn.wait;
        Ok(frame)
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
    ) -> Result<GpuFrame, Box<dyn Error>> {
        Ok(GpuFrame::default())
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
    /// The looks for holes' share of the scene thread's **busy** time above (0 to 1): the sum of
    /// the parts, not wall time, so a thread that is idle most of the time can still read high. A
    /// path's figure is of its frames only: the look that is forced after them is not in it (see
    /// [`PathReport::end_look_ms`]); a run's figure has every look of the run in it.
    pub hole_scan_share: f64,
}

impl SceneThread {
    /// What was spent from `then` to `now`.
    fn between(then: Spent, now: Spent) -> Self {
        let ms = |now: Duration, then: Duration| now.saturating_sub(then).as_secs_f64() * 1e3;
        let parts = Self {
            follow_ms: ms(now.follow, then.follow),
            pack_ms: ms(now.pack, then.pack),
            upload_ms: ms(now.publish, then.publish),
            evict_ms: ms(now.evict, then.evict),
            coarse_ms: ms(now.coarse, then.coarse),
            hole_scan_ms: ms(now.hole_scan, then.hole_scan),
            hole_scan_share: 0.0,
        };
        let all = parts.follow_ms
            + parts.pack_ms
            + parts.upload_ms
            + parts.evict_ms
            + parts.coarse_ms
            + parts.hole_scan_ms;
        Self {
            hole_scan_share: if all > 0.0 {
                parts.hole_scan_ms / all
            } else {
                0.0
            },
            ..parts
        }
    }
}

/// How many of the slowest frames of a path are kept.
const SLOWEST: usize = 5;

/// One frame over the limit, or among the slowest of its path, with where its time went. The parts
/// are in milliseconds, in the order the frame does them: [`SlowFrame::recentre_ms`], then the
/// update's copy to the GPU, then the draw's encoding and submitting, then the wait for the GPU;
/// `other_ms` is what none of them says (`time_ms` less all five). `camera_ms` and `after_ms` are
/// outside `time_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SlowFrame {
    /// The frame's place in its path (from 0), and the milliseconds from the path's start to the
    /// frame's start.
    pub index: usize,
    pub at_ms: f64,
    /// The frame's time, as the gate measures it, and its CPU part (the recentre, the update's
    /// write and flush, the encode and the submit).
    pub time_ms: f64,
    pub cpu_ms: f64,
    /// Working out the camera, before the frame started: the flights read the store for the
    /// ground, and wait for its lock. Not in the frame's time, but it delays the frame's start.
    pub camera_ms: f64,
    /// `World::recentre`: telling the scene thread the camera and taking what it published.
    pub recentre_ms: f64,
    /// The update's copy to the GPU: the writes queued and the submit that sends them, in all and
    /// each. Zero with no update.
    pub update_ms: f64,
    pub update_write_ms: f64,
    pub update_flush_ms: f64,
    /// Calls of `write_buffer` (each a copy on the GPU with a barrier of its own).
    pub writes: u32,
    pub encode_ms: f64,
    pub submit_ms: f64,
    /// Waiting for the GPU: after the CPU's clock has stopped.
    pub wait_ms: f64,
    pub other_ms: f64,
    /// The frame's look at the memory after its time was taken: the store's lock. Not in
    /// `time_ms`.
    pub after_ms: f64,
    /// What the update copied: `none`, `patch` (the words it lists) or `rebuild` (new buffers, all
    /// of the scene written), the bytes in all (header and tables included) and those of a patch's
    /// words.
    pub update: &'static str,
    pub upload_bytes: u64,
    pub patch_bytes: u64,
    /// Whether a look for holes was going on in the scene thread at the start or the end of the
    /// frame (it holds the store's read lock and publishes nothing meanwhile).
    pub hole_look: bool,
}

/// The slowest frames of a path, in a list of fixed size: no allocation while the frames come.
struct Worst([Option<SlowFrame>; SLOWEST]);

impl Worst {
    fn new() -> Self {
        Self([None; SLOWEST])
    }

    fn offer(&mut self, frame: SlowFrame) {
        let time = |slot: &Option<SlowFrame>| slot.map_or(f64::NEG_INFINITY, |f| f.time_ms);
        let least = self
            .0
            .iter_mut()
            .min_by(|a, b| time(a).total_cmp(&time(b)))
            .expect("the list has room");
        if frame.time_ms > time(least) {
            *least = Some(frame);
        }
    }

    /// The slowest first.
    fn sorted(&self) -> Vec<SlowFrame> {
        let mut frames: Vec<SlowFrame> = self.0.iter().flatten().copied().collect();
        frames.sort_by(|a, b| b.time_ms.total_cmp(&a.time_ms));
        frames
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
    /// Frames whose time was over the frame-time limit (the gate's: the 99th percentile is held to
    /// it, and this is how many frames there were above it), and the slowest few, worst first, with
    /// their parts. In the min and the max the i-th slowest of each run is taken with the i-th of
    /// the others, member by member, and null if the runs did not all keep as many.
    pub frames_over_limit: usize,
    pub slowest: Vec<SlowFrame>,
    /// Soft misses a frame reported (counted by the scene thread a few times a second, so a frame
    /// reports the last count).
    pub soft_misses: Series,
    pub soft_misses_per_frame: f64,
    /// Looks of the scene thread at every split brick (every few seconds, and one forced at the end
    /// of the path), the holes they found in all, and those per frame of the path. Never 0 looks.
    pub hole_looks: u64,
    pub holes: u64,
    pub holes_per_frame: f64,
    /// The scene thread's time in the frames of the path: without the look for holes that is forced
    /// after them.
    pub scene_thread: SceneThread,
    /// What the forced look at the end of the path took in the scene thread, in milliseconds (the
    /// scan itself): a look after the frames, not in them.
    pub end_look_ms: f64,
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
    /// From the start of the load to the first frame with level 0 under the camera: one ray from
    /// the camera straight down hits the ground at level 0. This is **not** the spec's time to
    /// control (the ring of 20 m round the Spawn at full detail, and the character standing in it:
    /// the bench has no server). The spawn ring is made inside [`crate::load`], so this is the
    /// load time and about a frame.
    pub time_to_level_0_under_camera_s: f64,
    /// From the start of the load to the resident set being complete.
    pub fill_s: f64,
}

/// What the run held, at the most.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Memory {
    /// The largest resident set of the samples taken during the run, where the system says: the
    /// working set on Windows, `VmRSS` on Linux. A sample is taken every 30 frames of the paths and
    /// the cold start, and at the start and the end of the run; not during the sizing, the looks
    /// for holes or the waits between paths. The samples are of this process, so the allocator may
    /// still hold memory from the normal bench and from earlier runs: the first run's figure is
    /// the cleanest reading of what a run needs.
    pub resident_peak_mib: Option<f64>,
    /// The OS's own peak for the **whole process** since it began, which includes the normal
    /// bench's generation of the Planet and every earlier run (the peak working set cannot be reset
    /// on Windows): only an upper bound of this run's.
    pub whole_process_peak_mib: Option<f64>,
    /// The scene's buffers in use (`Scene::used_bytes`), at the most of what the scene thread said,
    /// which it works out every 250 ms: not a value per frame. **The memory gate is on this**, not
    /// on what the GPU reserves.
    pub scene_used_peak_mib: f64,
    /// The scene's buffers as the GPU reserves them, used or not (`Scene::reserved_bytes`): this
    /// is the VRAM the scene takes, and it is allocated whole, so it does not change over the run
    /// (the most of the frames' is given). Reported; not gated.
    pub scene_reserved_mib: f64,
    /// The store's size as its budget counts it, looked at every few frames, and the most it may
    /// hold before eviction starts.
    pub store_peak_bricks: u64,
    pub store_limit_bricks: u64,
}

/// What the dig of a run came to.
enum Edit {
    Taken(EditLatency),
    /// There was no Edit to time, and the reason.
    NotMade(&'static str),
}

/// The Edit of the run: a dig of the default Tool at the Spawn after the stop, the one measure
/// that the stepped mode cannot make (ADR 0006's bound, which needs the real scene thread). Reported,
/// not gated.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct EditLatency {
    /// Frames from the frame the Edit was made in (it is made before that frame's look at the
    /// scene, where the game makes it) to the frame whose look took it in: 1 is the bound (an Edit
    /// made in frame N shows in N+1), 0 the frame that made it.
    pub frames: u64,
    /// The same in milliseconds: from before the Edit's call to the end of the look that took it in.
    pub ms: f64,
    /// The Edit's own call on the frame thread: planning and writing the bricks.
    pub call_ms: f64,
}

/// One run of the paths.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunReport {
    pub cold_start: ColdStart,
    /// The cold start, then each path.
    pub paths: Vec<PathReport>,
    /// The scene thread's time over the whole run.
    pub scene_thread: SceneThread,
    /// Looks for holes over the whole run, from the moment the measuring began (the paths, the
    /// catching up between them, the sizing and the Edit), and what they found: **this is what the
    /// holes gate is held to**, not the sum of the paths'.
    pub hole_looks: u64,
    pub holes: u64,
    /// Null if the dig could not be made: see `edit_not_made`.
    pub edit_latency: Option<EditLatency>,
    /// Why there was no Edit to time: `nothing in reach` (the camera at the Spawn looks at no
    /// Voxel within reach) or `refused` (the client knew the server would turn it down). Null when
    /// the Edit was made. An Edit that was made and never taken in is an error of the run, not this.
    pub edit_not_made: Option<&'static str>,
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
    /// Holes found by the scene thread's looks over each whole run, summed over the runs.
    pub holes: Limit,
    /// The fewest looks of any run, and of any path of any run. The holes gate fails on 0 of
    /// either: a gate that looked at nothing has not passed.
    pub hole_looks_fewest_in_a_run: u64,
    pub hole_looks_fewest_in_a_path: u64,
    /// The memory gate has two measures: the scene's buffers in use (not what the GPU reserves) and
    /// the store.
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
        worst: runs.iter().map(|r| r.holes).sum::<u64>() as f64,
        limit: budgets.holes as f64,
    };
    let hole_looks_fewest_in_a_run = runs.iter().map(|r| r.hole_looks).min().unwrap_or(0);
    let hole_looks_fewest_in_a_path = paths().map(|p| p.hole_looks).min().unwrap_or(0);
    // No run at all (the first failed) is an error of the report, not a pass or a failure of this.
    let looked =
        runs.is_empty() || (hole_looks_fewest_in_a_run > 0 && hole_looks_fewest_in_a_path > 0);
    let scene_mib = Limit {
        worst: worst(&mut runs.iter().map(|r| r.memory.scene_used_peak_mib)),
        limit: budgets.scene_mib,
    };
    let store_bricks = Limit {
        worst: worst(&mut runs.iter().map(|r| r.memory.store_peak_bricks as f64)),
        limit: runs
            .iter()
            .map(|r| r.memory.store_limit_bricks as f64)
            .reduce(f64::min)
            .unwrap_or(0.0),
    };
    let over = [
        ("frame_time", frame_time_p99_ms.over()),
        ("frame_cpu", frame_cpu_p99_ms.over()),
        ("holes", holes.over() || !looked),
        ("memory", scene_mib.over() || store_bricks.over()),
    ]
    .into_iter()
    .filter_map(|(name, over)| over.then_some(name))
    .collect();
    Gates {
        frame_time_p99_ms,
        frame_cpu_p99_ms,
        holes,
        hole_looks_fewest_in_a_run,
        hole_looks_fewest_in_a_path,
        scene_mib,
        store_bricks,
        over,
    }
}

/// Why the mode stopped before its last run was done.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Failure {
    /// The run (from 1) that was going.
    pub run: u32,
    /// What it was doing: building the world, a path, a catch-up between paths, the sizing, the
    /// Edit.
    pub step: String,
    pub message: String,
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
    /// The runs that finished.
    pub runs: Vec<RunReport>,
    /// Every number of a run (the same shape as one of `runs`), at its least and its most over the
    /// runs. A boolean is the least (all) and the most (any) of the runs'; what is not a number
    /// or a boolean is kept if the runs agree, and null if they do not. Null when no run finished.
    pub min: Value,
    pub max: Value,
    pub gates: Gates,
    /// Set when a run did not finish: the runs above are those that did (and the gates are held to
    /// those only), and the exit status is 1.
    pub error: Option<Failure>,
}

impl RealTimeReport {
    pub fn new(planet: String, d0: f64, runs: Vec<RunReport>, budgets: &StreamingBudgets) -> Self {
        let values: Vec<Value> = runs
            .iter()
            .map(|r| serde_json::to_value(r).expect("a run serializes"))
            .collect();
        let (min, max) = if values.is_empty() {
            (Value::Null, Value::Null)
        } else {
            envelope(&values.iter().collect::<Vec<_>>())
        };
        Self {
            mode: "real-time",
            deterministic: false,
            planet,
            d0,
            gates: gates(&runs, budgets),
            runs,
            min,
            max,
            error: None,
        }
    }

    /// The report of a mode that stopped on `failure`.
    pub fn failed(mut self, failure: Failure) -> Self {
        self.error = Some(failure);
        self
    }
}

/// Each number of `values`, which are of one shape, at its least and its most. Objects and arrays
/// of one length are done member by member. A boolean is least when every run's is true and most
/// when any is (false below true), so a flag that says "over" says it for the worst run. Anything
/// else is kept if the runs agree and is null if they do not, so that nothing reads as one run's.
fn envelope(values: &[&Value]) -> (Value, Value) {
    let first = values[0];
    match first {
        Value::Object(fields) if values.iter().all(|v| v.is_object()) => {
            let (mut least, mut most) = (serde_json::Map::new(), serde_json::Map::new());
            for name in fields.keys() {
                let members: Option<Vec<&Value>> = values.iter().map(|v| v.get(name)).collect();
                let (low, high) = members.map_or((Value::Null, Value::Null), |m| envelope(&m));
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
        Value::Bool(_) if values.iter().all(|v| v.is_boolean()) => {
            let flags = || values.iter().filter_map(|v| v.as_bool());
            (
                Value::Bool(flags().all(|f| f)),
                Value::Bool(flags().any(|f| f)),
            )
        }
        _ if values.iter().all(|v| *v == first) => (first.clone(), first.clone()),
        _ => (Value::Null, Value::Null),
    }
}

/// The exit status of the mode: 1 if it stopped on an error (whatever `no_fail` says),
/// [`EXIT_OVER_BUDGET`] if a gate is over, unless `no_fail`.
pub fn exit_code(report: &RealTimeReport, no_fail: bool) -> u8 {
    if report.error.is_some() {
        1
    } else if report.gates.over.is_empty() || no_fail {
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
    /// When the frame started.
    began: Instant,
    time_ms: f64,
    cpu_ms: f64,
    /// The parts of the frame; its `index` and `at_ms` are set by the path that takes it in.
    parts: SlowFrame,
    soft_misses: usize,
    /// Whether the look at the scene in this frame took in the newest Edit, and when it was done.
    edit_taken: bool,
    taken_at: Instant,
}

/// What a path has seen so far.
struct Capture {
    name: &'static str,
    began: Instant,
    /// How long the path went on for, set when its frames are done (the forced look after them is
    /// not in it).
    seconds: Option<f64>,
    spent: Spent,
    scans: u64,
    holes: u64,
    /// What the scene thread had spent when the path's frames were done, before the forced look.
    frames_done: Option<Spent>,
    frame_ms: Vec<f64>,
    cpu_ms: Vec<f64>,
    soft_misses: Series,
    /// Frames over the limit, and the slowest few.
    limit_ms: f64,
    over_limit: usize,
    worst: Worst,
}

impl Capture {
    fn start(name: &'static str, world: &World, limit_ms: f64) -> Self {
        let work = world.stats().work;
        Self {
            name,
            began: Instant::now(),
            seconds: None,
            frames_done: None,
            limit_ms,
            over_limit: 0,
            worst: Worst::new(),
            spent: world.scene_thread_time(),
            scans: work.hole_scans,
            holes: work.holes_found,
            frame_ms: Vec::new(),
            cpu_ms: Vec::new(),
            soft_misses: Series::default(),
        }
    }

    fn add(&mut self, frame: &Frame) {
        if frame.time_ms > self.limit_ms {
            self.over_limit += 1;
        }
        self.worst.offer(SlowFrame {
            index: self.frame_ms.len(),
            at_ms: frame
                .began
                .saturating_duration_since(self.began)
                .as_secs_f64()
                * 1e3,
            ..frame.parts
        });
        self.frame_ms.push(frame.time_ms);
        self.cpu_ms.push(frame.cpu_ms);
        self.soft_misses.add(frame.soft_misses as u64);
    }

    /// The path's frames are done: what the scene thread has spent so far is the frames', and the
    /// forced look that follows is not.
    fn frames_done(&mut self, world: &World) {
        self.seconds = Some(self.began.elapsed().as_secs_f64());
        self.frames_done = Some(world.scene_thread_time());
    }

    fn finish(self, world: &World, settle: Option<Settle>) -> PathReport {
        let work = world.stats().work;
        let now = world.scene_thread_time();
        let frames_end = self.frames_done.unwrap_or(now);
        let frames = self.frame_ms.len();
        let per_frame = |count: u64| count as f64 / frames.max(1) as f64;
        let holes = work.holes_found - self.holes;
        PathReport {
            name: self.name,
            frames,
            seconds: self
                .seconds
                .unwrap_or_else(|| self.began.elapsed().as_secs_f64()),
            frame_time: Spread::of(&self.frame_ms),
            frame_cpu: Spread::of(&self.cpu_ms),
            frames_over_limit: self.over_limit,
            slowest: self.worst.sorted(),
            soft_misses_per_frame: per_frame(self.soft_misses.total),
            soft_misses: self.soft_misses,
            hole_looks: work.hole_scans - self.scans,
            holes,
            holes_per_frame: per_frame(holes),
            scene_thread: SceneThread::between(self.spent, frames_end),
            end_look_ms: now
                .hole_scan
                .saturating_sub(frames_end.hole_scan)
                .as_secs_f64()
                * 1e3,
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
    /// What the run was doing when it last started on something: for the error of a run that stops.
    step: String,
    /// What the scene thread had done when the measuring began.
    work_before: Done,
    scene_used_peak_bytes: u64,
    scene_reserved_bytes: u64,
    resident_peak: Option<u64>,
    whole_process_peak: Option<u64>,
    store_peak: usize,
    frames: u64,
    /// The frame-time limit, for counting the frames over it.
    limit_ms: f64,
    /// How long working out the camera of the next frame took (a flight reads the store).
    camera_wait: Duration,
}

/// What an update copies to the GPU: its kind, the bytes in all and those of a patch's words.
fn uploaded(scene: &Scene, update: Option<&SceneUpdate>) -> (&'static str, u64, u64) {
    match update {
        None => ("none", 0, 0),
        Some(update) => {
            let bytes = render::tracer::upload_bytes(scene, update);
            match update {
                SceneUpdate::Rebuilt => ("rebuild", bytes, 0),
                SceneUpdate::Rings(patch) => ("patch", bytes, patch.words() as u64 * 4),
                SceneUpdate::Patched { .. } => ("patch", bytes, bytes),
            }
        }
    }
}

impl<'a> Session<'a> {
    fn new(world: World, gpu: &'a mut dyn Gpu, limit_ms: f64) -> Self {
        let work_before = world.stats().work;
        let mut session = Self {
            world,
            gpu,
            limit_ms,
            camera_wait: Duration::ZERO,
            step: "starting".to_string(),
            work_before,
            scene_used_peak_bytes: 0,
            scene_reserved_bytes: 0,
            resident_peak: None,
            whole_process_peak: None,
            store_peak: 0,
            frames: 0,
        };
        session.look_at_resident_memory();
        session
    }

    /// One frame, paced to 60 a second: the camera is told (with its velocity measured, as the
    /// game's is, if `lead`), what the scene thread made is taken, copied to the GPU and drawn.
    fn frame(&mut self, camera: &Camera, lead: bool) -> Result<Frame, Box<dyn Error>> {
        let camera_wait = std::mem::take(&mut self.camera_wait);
        let began = Instant::now();
        let looking = self.world.looking_for_holes();
        let update = if lead {
            self.world.recentre(camera.position)?
        } else {
            self.world.recentre_moving(camera.position, [0.0; 3])?
        };
        let taken_at = Instant::now();
        let edit_taken = self.world.edit_taken();
        let followed = taken_at - began;
        let gpu = self
            .gpu
            .frame(camera, self.world.scene(), update.as_ref())?;
        let looking = looking || self.world.looking_for_holes();
        let time = began.elapsed();
        self.look_at_memory();
        let after = began.elapsed() - time;
        let (kind, upload_bytes, patch_bytes) = uploaded(self.world.scene(), update.as_ref());
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let parts = SlowFrame {
            index: 0,
            at_ms: 0.0,
            time_ms: ms(time),
            cpu_ms: ms(followed + gpu.cpu()),
            camera_ms: ms(camera_wait),
            recentre_ms: ms(followed),
            update_ms: ms(gpu.write + gpu.flush),
            update_write_ms: ms(gpu.write),
            update_flush_ms: ms(gpu.flush),
            writes: gpu.writes,
            encode_ms: ms(gpu.encode),
            submit_ms: ms(gpu.submit),
            wait_ms: ms(gpu.wait),
            other_ms: ms(time.saturating_sub(followed + gpu.cpu() + gpu.wait)),
            after_ms: ms(after),
            update: kind,
            upload_bytes,
            patch_bytes,
            hole_look: looking,
        };
        std::thread::sleep(FRAME.saturating_sub(began.elapsed()));
        Ok(Frame {
            began,
            time_ms: parts.time_ms,
            cpu_ms: parts.cpu_ms,
            parts,
            soft_misses: self.world.stats().soft_misses,
            edit_taken,
            taken_at,
        })
    }

    fn look_at_memory(&mut self) {
        self.scene_used_peak_bytes = self.scene_used_peak_bytes.max(self.world.used_bytes());
        self.scene_reserved_bytes = self.scene_reserved_bytes.max(self.world.reserved_bytes());
        self.frames += 1;
        if self.frames.is_multiple_of(STORE_EVERY) {
            self.store_peak = self.store_peak.max(self.world.store().streamed_len());
        }
        if self.frames.is_multiple_of(RESIDENT_EVERY) {
            self.look_at_resident_memory();
        }
    }

    fn look_at_resident_memory(&mut self) {
        if let Some(resident) = process::resident() {
            self.resident_peak = self.resident_peak.max(Some(resident.now));
            // The OS's peak only rises, but the largest of what was read is kept in case a reading lags.
            self.whole_process_peak = self.whole_process_peak.max(Some(resident.peak_since_start));
        }
    }

    /// A look of the scene thread at every split brick for a hole now (if `holes`), and a count of
    /// the soft misses now, with what the camera has told it, which is copied to the GPU like any
    /// update (and drawn, but that is no frame of a path).
    fn look(&mut self, camera: &Camera, holes: bool) -> Result<(), Box<dyn Error>> {
        if let Some(update) = self.world.look_now(holes)? {
            self.gpu.frame(camera, self.world.scene(), Some(&update))?;
        }
        Ok(())
    }

    /// Ends a path: a look at every split brick, so that no path has none, and its report.
    fn end_path(
        &mut self,
        mut capture: Capture,
        camera: &Camera,
        settle: Option<Settle>,
    ) -> Result<PathReport, Box<dyn Error>> {
        capture.frames_done(&self.world);
        self.look(camera, true)?;
        Ok(capture.finish(&self.world, settle))
    }

    /// Whether the resident set is complete: nothing waits, and the loader has nothing out.
    fn idle(&self) -> bool {
        self.world.waiting() == 0 && self.world.loader().is_some_and(|l| l.pending == 0)
    }

    /// Frames at `camera`, not recorded, until the loader has caught up, and then a look at every
    /// split brick: whatever the scene thread found between paths is in the run's holes. The
    /// camera's jump to it is not a flight: the scene thread is told it is not moving. `after`
    /// names what came before, for an error.
    fn settle(
        &mut self,
        camera: &Camera,
        cap: Duration,
        after: &str,
    ) -> Result<(), Box<dyn Error>> {
        self.step = format!("the loader's catch-up after {after}");
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
        self.look(camera, true)
    }

    /// The cold start: frames at the Spawn from the first until the resident set is complete.
    fn cold_start(
        &mut self,
        setup: &Setup,
        script: &Script,
    ) -> Result<(ColdStart, PathReport), Box<dyn Error>> {
        self.step = "cold start".to_string();
        let began = Instant::now();
        let mut capture = Capture::start("cold start", &self.world, self.limit_ms);
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
            time_to_level_0_under_camera_s: control
                .expect("the loop ends with control")
                .as_secs_f64(),
            fill_s: (setup.load + began.elapsed()).as_secs_f64(),
        };
        Ok((cold, self.end_path(capture, &setup.spawn, None)?))
    }

    /// The camera at `at(world, t)` for `seconds`, `t` the seconds since the path began.
    fn path(
        &mut self,
        name: &'static str,
        seconds: Duration,
        mut at: impl FnMut(&World, f64) -> Camera,
    ) -> Result<PathReport, Box<dyn Error>> {
        self.step = name.to_string();
        let mut capture = Capture::start(name, &self.world, self.limit_ms);
        let began = Instant::now();
        let camera = loop {
            let t = began.elapsed().as_secs_f64();
            let camera = at(&self.world, t);
            self.camera_wait = began.elapsed() - Duration::from_secs_f64(t);
            let frame = self.frame(&camera, true)?;
            capture.add(&frame);
            if began.elapsed() >= seconds {
                break camera;
            }
        };
        self.end_path(capture, &camera, None)
    }

    /// A flight, and then the camera standing where it stopped until no brick is drawn coarser than
    /// the bands ask (the **settle time**), or the cap.
    ///
    /// The soft misses are counted by the scene thread a few times a second, and not at all while it
    /// sleeps, so the count a frame sees can be from before the stop: a stale 0 would read as
    /// settled at once. So the stop is followed by a count made **now** ([`Session::look`], without
    /// the look for holes, which would be in the time), and the camera is settled when that count
    /// or a later one is 0 and `waiting() == 0`. `waiting` (the bricks the scene still waits for the
    /// loader's to split) is the right "nothing left to arrive" for a view: the loader's `pending`
    /// also holds jobs ahead of the view, which the view does not wait for. A later count is not
    /// stale: the thread counts again at once when nothing waits any more, and every 100 ms while
    /// it works.
    fn stop_after_a_flight(
        &mut self,
        course: &Course,
        script: &Script,
    ) -> Result<PathReport, Box<dyn Error>> {
        self.step = "stop after a flight".to_string();
        let mut capture = Capture::start("stop after a flight", &self.world, self.limit_ms);
        let began = Instant::now();
        let camera = loop {
            let t = began.elapsed().as_secs_f64();
            let camera = course.along(&self.world, course.ahead, FLIGHT_METRES_PER_SECOND * t, 0.0);
            self.camera_wait = began.elapsed() - Duration::from_secs_f64(t);
            let frame = self.frame(&camera, true)?;
            capture.add(&frame);
            if began.elapsed() >= script.flight_before_stop {
                break camera;
            }
        };
        let stopped = Instant::now();
        self.look(&camera, false)?;
        let settle = loop {
            let frame = self.frame(&camera, true)?;
            capture.add(&frame);
            if frame.soft_misses == 0 && self.world.waiting() == 0 {
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
        self.end_path(capture, &camera, Some(settle))
    }

    /// One dig at the Spawn, made before the frame's look at the scene as the game makes it, and
    /// the frames after it until one has taken it in (ADR 0006's bound, which a stepped scene
    /// thread cannot show). The camera is at the Spawn and the loader has caught up. The frames are
    /// not those of a path.
    fn edit(&mut self, spawn: &Camera) -> Result<Edit, Box<dyn Error>> {
        self.step = "edit latency".to_string();
        // A dig that cannot happen is not an error of the run: there is nothing to time.
        let tool = Tool::default();
        let Some(aim) = within_reach(&self.world, spawn) else {
            return Ok(Edit::NotMade("nothing in reach"));
        };
        if self.world.aim(&aim, &tool).is_none() {
            return Ok(Edit::NotMade("nothing in reach"));
        }
        let began = Instant::now();
        let update = self.world.edit(&aim, &tool)?;
        let call = began.elapsed();
        if !self.world.edit_made() {
            return Ok(Edit::NotMade("refused"));
        }
        if let Some(update) = update {
            self.gpu.frame(spawn, self.world.scene(), Some(&update))?;
        }
        for frames in 0..EDIT_FRAMES_CAP {
            let frame = self.frame(spawn, true)?;
            if frame.edit_taken {
                return Ok(Edit::Taken(EditLatency {
                    frames,
                    ms: (frame.taken_at - began).as_secs_f64() * 1e3,
                    call_ms: call.as_secs_f64() * 1e3,
                }));
            }
        }
        Err(format!("the Edit was not taken in after {EDIT_FRAMES_CAP} frames").into())
    }

    /// The client's sizing constants against what the complete resident set measures: the scene
    /// thread counts exactly once, here, and not in every publish (see
    /// [`World::count_once_exactly`]). A look for holes follows, so that what the exact count saw
    /// is in the run's.
    fn sizing(&mut self, setup: &Setup) -> Result<Sizing, Box<dyn Error>> {
        self.step = "sizing".to_string();
        let update = self.world.count_once_exactly()?;
        // What the count published is copied to the GPU like any update.
        self.gpu
            .frame(&setup.spawn, self.world.scene(), update.as_ref())?;
        let streamed = self.world.store().streamed_len();
        let measured = measure_sizes(&self.world, setup.start, setup.d0, streamed)?;
        self.look(&setup.spawn, true)?;
        Ok(Sizing::check(measured, &setup.constants))
    }

    /// The paths, in order, and what they and the run cost.
    fn paths(&mut self, setup: &Setup, script: &Script) -> Result<RunReport, Box<dyn Error>> {
        let course = Course::of(&setup.spawn);
        let (cold_start, cold) = self.cold_start(setup, script)?;
        let mut paths = vec![cold];
        let sizing = self.sizing(setup)?;
        let flight = |height: f64, direction: [f64; 3]| {
            let course = &course;
            move |world: &World, t: f64| {
                course.along(world, direction, FLIGHT_METRES_PER_SECOND * t, height)
            }
        };
        let speed = FLIGHT_METRES_PER_SECOND * VOXELS_PER_METRE;
        let spawn = setup.spawn;
        let cap = script.between_cap;
        self.settle(&spawn, cap, "the cold start")?;
        paths.push(self.path("flight at 2 m", script.flight, flight(0.0, course.ahead))?);
        self.settle(&spawn, cap, "the flight at 2 m")?;
        paths.push(self.path(
            "flight at 50 m",
            script.flight,
            flight(HIGH_METRES * VOXELS_PER_METRE, course.right),
        )?);
        self.settle(&spawn, cap, "the flight at 50 m")?;
        paths.push(
            self.path("vertical take-off", script.take_off, |_, t| Camera {
                position: std::array::from_fn(|a| {
                    spawn.position[a] + spawn.horizon_up[a] * speed * t
                }),
                ..spawn
            })?,
        );
        self.settle(&spawn, cap, "the vertical take-off")?;
        // A half turn in the first half of the path and the view still for the second.
        let half = script.turn.as_secs_f64() / 2.0;
        paths.push(self.path("180 degree turn", script.turn, |_, t| {
            let mut turned = spawn;
            turned.turn(std::f64::consts::PI * (t / half).min(1.0), 0.0);
            turned
        })?);
        self.settle(&spawn, cap, "the 180 degree turn")?;
        paths.push(self.stop_after_a_flight(&course, script)?);
        self.settle(&spawn, cap, "the stop after a flight")?;
        let (edit_latency, edit_not_made) = match self.edit(&spawn)? {
            Edit::Taken(latency) => (Some(latency), None),
            Edit::NotMade(why) => (None, Some(why)),
        };
        // The last look: what happened since the last path's, the Edit's, is in the run's holes.
        self.step = "the last look".to_string();
        self.look(&spawn, true)?;
        self.look_at_resident_memory();

        let work = self.world.stats().work;
        let mib = |bytes: u64| bytes as f64 / MIB;
        let memory = Memory {
            resident_peak_mib: self.resident_peak.map(mib),
            whole_process_peak_mib: self.whole_process_peak.map(mib),
            scene_used_peak_mib: mib(self.scene_used_peak_bytes),
            scene_reserved_mib: mib(self.scene_reserved_bytes),
            store_peak_bricks: self.store_peak as u64,
            store_limit_bricks: setup.store_limit as u64,
        };
        Ok(RunReport {
            cold_start,
            paths,
            scene_thread: SceneThread::between(Spent::default(), self.world.scene_thread_time()),
            hole_looks: work.hole_scans - self.work_before.hole_scans,
            holes: work.holes_found - self.work_before.holes_found,
            edit_latency,
            edit_not_made,
            memory,
            sizing,
        })
    }
}

/// A run that stopped: what it was doing, and why.
struct Stopped {
    step: String,
    error: Box<dyn Error>,
}

/// One run of the paths on `built`, drawing with `gpu`.
fn one_run(
    built: Built,
    gpu: &mut dyn Gpu,
    script: &Script,
    limit_ms: f64,
) -> Result<RunReport, Stopped> {
    let Built { world, setup, .. } = built;
    world.count_soft_misses();
    world.measure_scene_thread();
    // The work the scene thread had done before it was told to measure is the run's baseline.
    let mut session = Session::new(world, gpu, limit_ms);
    // A panic in the paths is the run's error too, with the step it was at, and the report is kept.
    catching(|| session.paths(&setup, script)).map_err(|error| Stopped {
        step: session.step.clone(),
        error,
    })
}

/// What a panic said.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|m| (*m).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_string())
}

/// `work`'s result, or if it panics an error that says so: a panic would end the process with status
/// 101 and no report. The panic's own message is still printed by the hook.
fn catching<T>(work: impl FnOnce() -> Result<T, Box<dyn Error>>) -> Result<T, Box<dyn Error>> {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(result) => result,
        Err(payload) => Err(format!("panicked: {}", panic_message(&*payload)).into()),
    }
}

/// The `run`th run: its world, its GPU, and the paths. Also the Planet's `d0`.
fn whole_run(
    full: bool,
    run: u32,
    resolution: Resolution,
    script: &Script,
    limit_ms: f64,
) -> Result<(RunReport, f64), Failure> {
    // What the paths do is caught where the step is known; this catches the rest (making the
    // world, the GPU, and ending the run, which drops them).
    catch_unwind(AssertUnwindSafe(|| {
        run_once(full, run, resolution, script, limit_ms)
    }))
    .unwrap_or_else(|payload| {
        Err(Failure {
            run,
            step: "building the world, making the GPU, or ending the run".to_string(),
            message: format!("panicked: {}", panic_message(&*payload)),
        })
    })
}

fn run_once(
    full: bool,
    run: u32,
    resolution: Resolution,
    script: &Script,
    limit_ms: f64,
) -> Result<(RunReport, f64), Failure> {
    let failed = |step: &str, error: Box<dyn Error>| Failure {
        run,
        step: step.to_string(),
        message: error.to_string(),
    };
    let built = if full { build_full() } else { build_test() }
        .map_err(|e| failed("building the world", e))?;
    let d0 = built.setup.d0;
    let mut gpu = Offscreen::with_resolution(resolution, built.world.scene())
        .map_err(|e| failed("making the GPU context", e))?;
    let report = one_run(built, &mut OffscreenGpu(&mut gpu), script, limit_ms)
        .map_err(|stopped| failed(&stopped.step, stopped.error))?;
    Ok((report, d0))
}

/// Runs the real-time mode: `runs` runs of the paths, each on a world of its own and a GPU of its
/// own, at full size or (for `--short`) on the test Planet, and the report of them. A run that
/// stops ends the mode, and the report has the runs that finished and the error.
pub fn run(
    full: bool,
    runs: u32,
    resolution: Resolution,
    budgets: &StreamingBudgets,
) -> RealTimeReport {
    let script = if full {
        Script::full()
    } else {
        Script::short()
    };
    let mut reports = Vec::new();
    let mut d0 = 0.0;
    let mut failure = None;
    for run in 1..=runs {
        eprintln!("real time: run {run} of {runs}");
        let began = Instant::now();
        match whole_run(full, run, resolution, &script, budgets.frame_time_p99_ms) {
            Ok((report, d)) => {
                d0 = d;
                eprintln!("real time: run {run} took {:.0?}", began.elapsed());
                reports.push(report);
            }
            Err(f) => {
                eprintln!(
                    "real time: run {run} stopped at `{}` after {:.0?}: {}",
                    f.step,
                    began.elapsed(),
                    f.message
                );
                failure = Some(f);
                break;
            }
        }
    }
    let planet = if full {
        "full-size".to_string()
    } else {
        "test".to_string()
    };
    let report = RealTimeReport::new(planet, d0, reports, budgets);
    match failure {
        Some(failure) => report.failed(failure),
        None => report,
    }
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
            frames_over_limit: 0,
            slowest: vec![slow_frame(0, frame_p99 * 2.0)],
            soft_misses: Series::default(),
            soft_misses_per_frame: 0.0,
            hole_looks: 2,
            holes,
            holes_per_frame: 0.0,
            scene_thread: SceneThread::default(),
            end_look_ms: 0.0,
            settle: None,
        }
    }

    /// A slow frame whose parts add up: 1 ms of recentre, 0.5 of update, 0.2 of encode, 0.1 of
    /// submit, the rest the wait.
    fn slow_frame(index: usize, time_ms: f64) -> SlowFrame {
        SlowFrame {
            index,
            at_ms: index as f64 * 16.7,
            time_ms,
            cpu_ms: 1.8,
            camera_ms: 0.0,
            recentre_ms: 1.0,
            update_ms: 0.5,
            update_write_ms: 0.4,
            update_flush_ms: 0.1,
            writes: 12,
            encode_ms: 0.2,
            submit_ms: 0.1,
            wait_ms: time_ms - 1.8,
            other_ms: 0.0,
            after_ms: 0.0,
            update: "patch",
            upload_bytes: 4096,
            patch_bytes: 4000,
            hole_look: false,
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
                time_to_level_0_under_camera_s: 61.0,
                fill_s: 200.0,
            },
            paths: vec![
                path("cold start", 12.0, 5.0, 0),
                path("flight at 2 m", 11.0, 6.0, 0),
            ],
            scene_thread: SceneThread::default(),
            hole_looks: 12,
            holes: 0,
            edit_latency: Some(EditLatency {
                frames: 1,
                ms: 21.0,
                call_ms: 3.0,
            }),
            edit_not_made: None,
            memory: Memory {
                resident_peak_mib: Some(8000.0),
                whole_process_peak_mib: Some(9000.0),
                scene_used_peak_mib: 474.0,
                scene_reserved_mib: 560.0,
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

    /// A run with `holes` found by the looks of the whole run, `in_a_path` of them in the path.
    fn holes_found(holes: u64, in_a_path: u64) -> RunReport {
        run_with(|r| {
            r.holes = holes;
            r.paths[1].holes = in_a_path;
        })
    }

    #[test]
    fn the_holes_gate_fires_alone_on_one_hole_in_any_path_of_any_run() {
        let hole = holes_found(1, 1);
        assert_eq!(over(std::slice::from_ref(&hole)), ["holes"]);
        // Three runs, one hole in the last: the holes are summed, and one is too many.
        assert_eq!(
            over(&[run_with(|_| {}), run_with(|_| {}), hole.clone()]),
            ["holes"]
        );
        let report = RealTimeReport::new("test".into(), 64.0, vec![hole], &BUDGETS);
        assert_eq!(exit_code(&report, false), 2);
        assert_eq!(exit_code(&report, true), 0, "--no-fail");
    }

    #[test]
    fn holes_found_outside_any_path_fire_the_gate_too() {
        // Between paths, while the loader catches up, at the sizing, or at the last look: the run's
        // total has the hole, and no path does.
        let outside = holes_found(1, 0);
        assert_eq!(outside.paths.iter().map(|p| p.holes).sum::<u64>(), 0);
        assert_eq!(over(std::slice::from_ref(&outside)), ["holes"]);
        let report = RealTimeReport::new("test".into(), 64.0, vec![outside], &BUDGETS);
        assert_eq!(report.gates.holes.worst, 1.0);
        assert_eq!(exit_code(&report, false), 2);
    }

    #[test]
    fn a_holes_gate_that_looked_at_nothing_has_not_passed() {
        // A run, or a path of one, with no look at all: no evidence, so the gate is over, and the
        // report says how few looks there were.
        let none_in_a_run = run_with(|r| r.hole_looks = 0);
        assert_eq!(over(std::slice::from_ref(&none_in_a_run)), ["holes"]);
        let none_in_a_path = run_with(|r| r.paths[1].hole_looks = 0);
        assert_eq!(over(std::slice::from_ref(&none_in_a_path)), ["holes"]);
        let g = gates(std::slice::from_ref(&none_in_a_path), &BUDGETS);
        assert_eq!(g.hole_looks_fewest_in_a_path, 0);
        assert_eq!(g.hole_looks_fewest_in_a_run, 12);
        assert_eq!(
            g.holes.worst, 0.0,
            "no hole was found, and the gate is over"
        );
        // One run of three that looked at nothing is enough.
        assert_eq!(
            over(&[run_with(|_| {}), none_in_a_run, run_with(|_| {})]),
            ["holes"]
        );
        // With a look in each path and each run, and none found, it passes.
        let passing = RealTimeReport::new("test".into(), 64.0, vec![run_with(|_| {})], &BUDGETS);
        assert!(passing.gates.over.is_empty());
        assert_eq!(passing.gates.hole_looks_fewest_in_a_path, 2);
    }

    #[test]
    fn the_memory_gate_fires_alone_for_the_scene_over_its_budget_or_the_store_over_its_limit() {
        let scene = run_with(|r| r.memory.scene_used_peak_mib = 512.5);
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
        // What the GPU reserves, and the process's memory, are reported and not gated.
        assert!(over(&[run_with(|r| r.memory.scene_reserved_mib = 1e6)]).is_empty());
        assert!(over(&[run_with(|r| r.memory.resident_peak_mib = Some(1e9))]).is_empty());
        assert!(over(&[run_with(|r| r.memory.whole_process_peak_mib = Some(1e9))]).is_empty());
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
            r.cold_start.time_to_level_0_under_camera_s = 600.0;
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
                r.memory.scene_used_peak_mib = 470.0;
            }),
            run_with(|r| {
                r.paths[1].frame_time.p99_ms = 12.5;
                r.paths[1].holes = 0;
                r.memory.scene_used_peak_mib = 480.0;
            }),
            run_with(|r| {
                r.paths[1].frame_time.p99_ms = 7.0;
                r.paths[1].holes = 0;
                r.memory.scene_used_peak_mib = 475.0;
            }),
        ];
        let report = RealTimeReport::new("full-size".into(), 1028.2, runs, &BUDGETS);
        let p99 = |v: &Value| v["paths"][1]["frame_time"]["p99_ms"].as_f64().unwrap();
        assert_eq!((p99(&report.min), p99(&report.max)), (7.0, 12.5));
        assert_eq!(report.min["memory"]["scene_used_peak_mib"], 470.0);
        assert_eq!(report.max["memory"]["scene_used_peak_mib"], 480.0);
        // Integers stay integers, and what the runs agree on is kept.
        assert_eq!(report.max["paths"][1]["frames"], 600);
        assert_eq!(report.min["paths"][1]["name"], "flight at 2 m");
        // The min and the max have the shape of a run, so a field added to a run is in them.
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        let run = serde_json::to_value(&report.runs[0]).unwrap();
        assert_eq!(keys(&report.min), keys(&run));
        assert_eq!(keys(&report.max), keys(&run));
    }

    #[test]
    fn a_flag_is_the_all_and_the_any_of_the_runs_and_a_word_is_kept_only_if_they_agree() {
        use serde_json::json;
        let one =
            json!({"over": false, "settled": true, "name": "a", "note": "x", "n": 1, "gone": null});
        let two =
            json!({"over": true, "settled": false, "name": "a", "note": "y", "n": 3, "gone": null});
        let (least, most) = envelope(&[&one, &two]);
        // A flag that says "over" says it for the worst run, whichever run it was.
        assert_eq!(
            (&least["over"], &most["over"]),
            (&json!(false), &json!(true))
        );
        assert_eq!(
            (&least["settled"], &most["settled"]),
            (&json!(false), &json!(true))
        );
        assert_eq!((&least["name"], &most["name"]), (&json!("a"), &json!("a")));
        // Runs that disagree on a word do not make one of theirs the report's.
        assert_eq!(
            (&least["note"], &most["note"]),
            (&json!(null), &json!(null))
        );
        assert_eq!((&least["n"], &most["n"]), (&json!(1), &json!(3)));
        let (least, most) = envelope(&[&two, &two]);
        assert_eq!(
            (&least["over"], &most["over"]),
            (&json!(true), &json!(true))
        );
        // The sizing's own checks are in the runs, and a check that one run is over is over in the max.
        let over_in_run_2 = {
            let mut run = serde_json::to_value(run_with(|_| {})).unwrap();
            run["sizing"]["checks"][0]["over"] = json!(true);
            run
        };
        let fine = serde_json::to_value(run_with(|_| {})).unwrap();
        let (least, most) = envelope(&[&fine, &over_in_run_2]);
        assert_eq!(least["sizing"]["checks"][0]["over"], false);
        assert_eq!(most["sizing"]["checks"][0]["over"], true);
    }

    #[test]
    fn a_report_of_a_run_that_stopped_has_the_runs_that_finished_and_the_error_and_exits_1() {
        let stopped = Failure {
            run: 3,
            step: "flight at 50 m".into(),
            message: "the loader has not caught up after 1200s".into(),
        };
        let report = RealTimeReport::new(
            "full-size".into(),
            1028.2,
            vec![run_with(|_| {}), run_with(|_| {})],
            &BUDGETS,
        )
        .failed(stopped.clone());
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["runs"].as_array().unwrap().len(), 2);
        assert_eq!(json["error"]["run"], 3);
        assert_eq!(json["error"]["step"], "flight at 50 m");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("caught up")
        );
        // The min and the max are of the runs that finished.
        assert_eq!(json["min"]["paths"][0]["name"], "cold start");
        // An error is status 1 whatever the gates say, and `--no-fail` does not hide it, and a
        // gate that is over is status 2 and not an error.
        assert_eq!(exit_code(&report, false), 1);
        assert_eq!(exit_code(&report, true), 1);
        let slow = run_with(|r| r.paths[1].frame_time.p99_ms = 20.0);
        let over_only = RealTimeReport::new("test".into(), 64.0, vec![slow], &BUDGETS);
        assert_eq!(exit_code(&over_only, false), 2);
        assert!(serde_json::to_value(&over_only).unwrap()["error"].is_null());
        // No run finished at all: still a report, with no min and max, and the same status.
        let none =
            RealTimeReport::new("full-size".into(), 0.0, Vec::new(), &BUDGETS).failed(stopped);
        let json = serde_json::to_value(&none).unwrap();
        assert!(json["min"].is_null() && json["max"].is_null());
        assert_eq!(exit_code(&none, false), 1);
        assert!(
            none.gates.over.is_empty(),
            "no run is not a run that failed a gate"
        );
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
                "error",
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
        assert!(json["error"].is_null());
        assert_eq!(
            keys(&json["runs"][0]),
            [
                "cold_start",
                "edit_latency",
                "edit_not_made",
                "hole_looks",
                "holes",
                "memory",
                "paths",
                "scene_thread",
                "sizing"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["edit_latency"]),
            ["call_ms", "frames", "ms"]
        );
        assert_eq!(
            keys(&json["runs"][0]["memory"]),
            [
                "resident_peak_mib",
                "scene_reserved_mib",
                "scene_used_peak_mib",
                "store_limit_bricks",
                "store_peak_bricks",
                "whole_process_peak_mib"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]),
            [
                "end_look_ms",
                "frame_cpu",
                "frame_time",
                "frames",
                "frames_over_limit",
                "hole_looks",
                "holes",
                "holes_per_frame",
                "name",
                "scene_thread",
                "seconds",
                "settle",
                "slowest",
                "soft_misses",
                "soft_misses_per_frame"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]["slowest"][0]),
            [
                "after_ms",
                "at_ms",
                "camera_ms",
                "cpu_ms",
                "encode_ms",
                "hole_look",
                "index",
                "other_ms",
                "patch_bytes",
                "recentre_ms",
                "submit_ms",
                "time_ms",
                "update",
                "update_flush_ms",
                "update_ms",
                "update_write_ms",
                "upload_bytes",
                "wait_ms",
                "writes"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]["scene_thread"]),
            [
                "coarse_ms",
                "evict_ms",
                "follow_ms",
                "hole_scan_ms",
                "hole_scan_share",
                "pack_ms",
                "upload_ms"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["cold_start"]),
            [
                "fill_s",
                "load_s",
                "time_to_level_0_under_camera_s",
                "world_map_s"
            ]
        );
        assert_eq!(
            keys(&json["gates"]),
            [
                "frame_cpu_p99_ms",
                "frame_time_p99_ms",
                "hole_looks_fewest_in_a_path",
                "hole_looks_fewest_in_a_run",
                "holes",
                "over",
                "scene_mib",
                "store_bricks"
            ]
        );
    }

    #[test]
    fn the_list_of_slowest_frames_keeps_the_five_longest_worst_first_and_never_grows() {
        let mut worst = Worst::new();
        assert!(worst.sorted().is_empty());
        for (index, time) in [4.0, 30.0, 9.0, 2.0, 17.0, 31.5, 8.0, 40.0, 1.0, 17.5]
            .into_iter()
            .enumerate()
        {
            worst.offer(slow_frame(index, time));
        }
        let kept: Vec<(usize, f64)> = worst
            .sorted()
            .iter()
            .map(|f| (f.index, f.time_ms))
            .collect();
        assert_eq!(
            kept,
            [(7, 40.0), (5, 31.5), (1, 30.0), (9, 17.5), (4, 17.0)]
        );
        // A frame that is not slower than the fifth is not kept; a tie does not push one out.
        worst.offer(slow_frame(10, 17.0));
        assert_eq!(worst.sorted().len(), SLOWEST);
        assert_eq!(worst.sorted()[4].index, 4);
        // Fewer frames than the list holds are all there.
        let mut few = Worst::new();
        few.offer(slow_frame(0, 5.0));
        few.offer(slow_frame(1, 6.0));
        assert_eq!(
            few.sorted().iter().map(|f| f.index).collect::<Vec<_>>(),
            [1, 0]
        );
    }

    #[test]
    fn a_gpu_frames_cpu_part_is_all_but_the_wait() {
        let ms = Duration::from_millis;
        let frame = GpuFrame {
            write: ms(1),
            flush: ms(2),
            writes: 7,
            encode: ms(4),
            submit: ms(8),
            wait: ms(100),
        };
        assert_eq!(frame.cpu(), ms(15));
        assert_eq!(GpuFrame::default().cpu(), Duration::ZERO);
    }

    #[test]
    fn an_update_is_told_by_its_kind_and_bytes() {
        let TestPlanet { scene, .. } = test_planet();
        assert_eq!(uploaded(&scene, None), ("none", 0, 0));
        let (kind, bytes, patch) = uploaded(&scene, Some(&SceneUpdate::Rebuilt));
        assert_eq!((kind, patch), ("rebuild", 0));
        assert!(bytes > 0);
    }

    #[test]
    fn a_dig_with_nothing_in_reach_is_no_edit_and_not_an_error() {
        // No loader and no threads: the world of the test Planet, with the camera at the sky.
        let TestPlanet {
            store,
            scene,
            spawn,
            ..
        } = test_planet();
        let (client, _server) = loopback();
        let world = World::new(scene, Predictor::new(store), client);
        let mut gpu = NoGpu;
        let mut session = Session::new(world, &mut gpu, BUDGETS.frame_time_p99_ms);
        let mut sky = spawn;
        sky.turn(0.0, 10.0);
        match session
            .edit(&sky)
            .expect("a dig that cannot happen is no error")
        {
            Edit::NotMade(why) => assert_eq!(why, "nothing in reach"),
            Edit::Taken(_) => panic!("there is nothing to dig in the sky"),
        }
        assert!(!session.world.edit_made());
    }

    #[test]
    fn a_report_with_no_edit_says_why_and_the_min_and_max_keep_the_reason_the_runs_agree_on() {
        let none = || {
            run_with(|r| {
                r.edit_latency = None;
                r.edit_not_made = Some("nothing in reach");
            })
        };
        let report = RealTimeReport::new(
            "test".into(),
            64.0,
            vec![none(), none(), run_with(|_| {})],
            &BUDGETS,
        );
        let json = serde_json::to_value(&report).unwrap();
        assert!(json["runs"][0]["edit_latency"].is_null());
        assert_eq!(json["runs"][0]["edit_not_made"], "nothing in reach");
        // One run made the Edit and two did not: the envelope does not make one run's the report's.
        assert!(json["min"]["edit_latency"].is_null() && json["max"]["edit_latency"].is_null());
        assert!(json["min"]["edit_not_made"].is_null());
        // All the runs agreeing keeps the reason, and no Edit is no failure of a gate.
        let all = RealTimeReport::new("test".into(), 64.0, vec![none(), none()], &BUDGETS);
        let json = serde_json::to_value(&all).unwrap();
        assert_eq!(json["max"]["edit_not_made"], "nothing in reach");
        assert!(json["max"]["edit_latency"].is_null());
        assert!(all.gates.over.is_empty());
        assert_eq!(exit_code(&all, false), 0);
    }

    #[test]
    fn a_panic_in_a_run_is_an_error_with_its_message_and_not_a_dead_process() {
        let caught: Result<(), _> = catching(|| panic!("the scene thread died: {}", 7));
        let message = caught.unwrap_err().to_string();
        assert_eq!(message, "panicked: the scene thread died: 7");
        let caught: Result<(), _> = catching(|| panic!("a plain message"));
        assert_eq!(caught.unwrap_err().to_string(), "panicked: a plain message");
        // What does not panic comes through as it was.
        assert_eq!(catching(|| Ok(3)).unwrap(), 3);
        assert_eq!(
            catching::<()>(|| Err("no".into())).unwrap_err().to_string(),
            "no"
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
        let run = one_run(
            built,
            &mut NoGpu,
            &Script::short(),
            BUDGETS.frame_time_p99_ms,
        )
        .unwrap_or_else(|stopped| panic!("the run stopped at {}: {}", stopped.step, stopped.error));
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
            // Every path has a look at the holes: one is forced at its end.
            assert!(path.hole_looks >= 1, "{}", path.name);
        }
        // The run's looks are all of the paths' and those between and after them.
        assert!(run.hole_looks >= run.paths.iter().map(|p| p.hole_looks).sum::<u64>());
        assert!(run.holes >= run.paths.iter().map(|p| p.holes).sum::<u64>());
        // The Edit was taken in by a look at the scene: no later than the cap, with times.
        let edit = run.edit_latency.expect("the Edit at the Spawn was made");
        assert!(run.edit_not_made.is_none());
        assert!(edit.frames < EDIT_FRAMES_CAP && edit.ms > 0.0);
        assert!(edit.call_ms > 0.0);
        let stop = run.paths.last().unwrap();
        assert!(stop.settle.is_some() && run.paths[..5].iter().all(|p| p.settle.is_none()));
        let cold = &run.cold_start;
        assert!(
            cold.time_to_level_0_under_camera_s >= cold.load_s
                && cold.fill_s >= cold.time_to_level_0_under_camera_s
        );
        // The thread did the work of a fill, which is timed by category.
        assert!(run.scene_thread.pack_ms > 0.0 && run.scene_thread.follow_ms > 0.0);
        assert!(run.memory.scene_used_peak_mib > 0.0 && run.memory.store_peak_bricks > 0);
        // The GPU reserves the buffers whole, which is at least what is in use.
        assert!(run.memory.scene_reserved_mib >= run.memory.scene_used_peak_mib);
        // Where the system says, the run's resident peak is within the process's.
        if let (Some(run_peak), Some(whole)) = (
            run.memory.resident_peak_mib,
            run.memory.whole_process_peak_mib,
        ) {
            assert!(run_peak > 0.0 && run_peak <= whole);
        }
        assert!(run.memory.store_limit_bricks >= run.memory.store_peak_bricks);
        // The sizing is measured at the complete set, with the counts made once and exactly.
        assert!(run.sizing.measured.ring_bricks > 0 && run.sizing.measured.guard_bricks > 0);
        assert_eq!(
            run.sizing.measured.ring_refs_words,
            run.sizing.measured.ring_bricks
        );
        // What the looks found is the gate, and each had a look, so that the gate looked.
        let report = RealTimeReport::new("test".into(), d0, vec![run], &BUDGETS);
        assert_eq!(report.gates.holes.worst, 0.0, "{:?}", report.gates);
        assert!(report.gates.hole_looks_fewest_in_a_path >= 1);
        assert!(!report.gates.over.contains(&"holes"), "{:?}", report.gates);
        serde_json::to_string(&report).expect("the report serializes");
    }

    /// A run that cannot finish says at which step, and why: here the cold start, with no time to
    /// fill. It is in `.github/real-thread-tests.txt` with the smoke test (it has threads).
    #[test]
    fn a_run_that_stops_names_the_step_it_was_at() {
        let built = build_test().expect("the test Planet builds");
        let script = Script {
            fill_cap: Duration::ZERO,
            ..Script::short()
        };
        let stopped = match one_run(built, &mut NoGpu, &script, BUDGETS.frame_time_p99_ms) {
            Ok(_) => panic!("a cold start with no time cannot finish"),
            Err(stopped) => stopped,
        };
        assert_eq!(stopped.step, "cold start");
        assert!(stopped.error.to_string().contains("not complete"));
    }
}
