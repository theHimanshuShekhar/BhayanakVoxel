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
use render::{Camera, GpuPasses, Mode, Offscreen, Rebuild, Resolution, Scene, SceneUpdate, camera};
use serde::Serialize;
use serde_json::Value;
use sim::Predictor;

use super::budget::{EXIT_OVER_BUDGET, StreamingBudgets};
use super::process;
use super::stream::{
    Constants, Series, Sizing, Start, TestPlanet, measure_sizes, over_the_ground, percentile_99,
    test_planet, within_reach,
};
use crate::loader::{Loader, workers_for};
use crate::scene_worker::{Cause, Done, Longest, Spent, UPLOAD_BUDGET};
use crate::tool::Tool;
use crate::world::{Hold, STILL_FRAMES, StillCuts, Stillness, World};

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
    /// How long the view is held Still after the flight of the still hold (#217).
    pub still_hold: Duration,
    /// The most that the camera is stopped for before the soft misses are said not to settle.
    pub settle_cap: Duration,
    /// The most that the cold start and the catching up between paths wait for the loader.
    pub fill_cap: Duration,
    pub between_cap: Duration,
    /// How long the scene thread takes in what the loader made for while the loader's workers are
    /// as far ahead as they may be ([`crate::scene_worker::BACKED_UP_SLICE`], or what
    /// `--stream-slice-ms` said).
    pub stream_slice: Duration,
}

impl Script {
    /// The measurement, at full size: the paths of #143.
    pub fn full() -> Self {
        Self {
            flight: Duration::from_secs(10),
            take_off: Duration::from_secs(6),
            turn: Duration::from_secs(2),
            flight_before_stop: Duration::from_secs(5),
            still_hold: Duration::from_secs(3),
            settle_cap: Duration::from_secs(120),
            fill_cap: Duration::from_secs(5400),
            between_cap: Duration::from_secs(1200),
            stream_slice: crate::scene_worker::BACKED_UP_SLICE,
        }
    }

    /// The smoke test, on the test Planet: the same paths for a second or two each.
    pub fn short() -> Self {
        Self {
            flight: Duration::from_secs(1),
            take_off: Duration::from_secs(1),
            turn: Duration::from_secs(1),
            flight_before_stop: Duration::from_secs(1),
            still_hold: Duration::from_secs(1),
            settle_cap: Duration::from_secs(20),
            fill_cap: Duration::from_secs(600),
            between_cap: Duration::from_secs(300),
            stream_slice: crate::scene_worker::BACKED_UP_SLICE,
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

    /// The most bytes of GPU memory the scene's buffers have taken at once while a buffer grew
    /// (the old buffer and the new one both counted: `Tracer::peak_growth_bytes`), and while a
    /// rebuilt scene was made (`Tracer::peak_rebuild_bytes`), in all the updates so far. 0 with no
    /// GPU, and 0 for what has not happened.
    fn peak_growth_bytes(&self) -> u64 {
        0
    }

    fn peak_rebuild_bytes(&self) -> u64 {
        0
    }
}

/// What a frame's copy to the GPU and draw took, by part.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuFrame {
    /// Queuing the update's copies to the staging belt (and the new buffers of one that grew), or
    /// for a rebuild making the new buffers.
    pub write: Duration,
    /// The submit that sends them.
    pub flush: Duration,
    /// Copies written to the belt: each is a copy on the GPU with a barrier of its own.
    pub writes: u32,
    /// Encoding the draw, and submitting it.
    pub encode: Duration,
    pub submit: Duration,
    /// Waiting for the GPU to be done: the update's copies and the draw, and whatever else the GPU
    /// was doing.
    pub wait: Duration,
    /// The GPU's own time for this frame's passes, from its timestamps. None where the device has
    /// no timers, or this frame's own times did not come in: never an earlier frame's.
    pub passes: Option<GpuPasses>,
    /// What making the buffers that grew in this frame's update took on the host (new buffers and
    /// the copies into them, recorded; it is inside `write`), and the bytes of the old buffers
    /// that were copied. Zero if no buffer grew.
    pub grow: Duration,
    pub grown_bytes: u64,
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
            frame.grow = self.0.last_grow_time();
            frame.grown_bytes = self.0.last_grown_bytes();
        }
        self.0.draw_upscaled_timed(camera);
        let drawn = self.0.last_draw_times();
        frame.encode = drawn.encode;
        frame.submit = drawn.submit;
        frame.wait = drawn.wait;
        frame.passes = drawn.gpu;
        Ok(frame)
    }

    fn peak_growth_bytes(&self) -> u64 {
        self.0.peak_growth_bytes()
    }

    fn peak_rebuild_bytes(&self) -> u64 {
        self.0.peak_rebuild_bytes()
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

/// What the uploads of a path's frames came to (#145): the frames that had an update, what each cost
/// on the CPU, the bytes and the copies of each, and how often and for how long the scene thread
/// held work back for the upload budget (`scene_worker::UPLOAD_BUDGET`). The gate on frame time is
/// the one this is for; nothing here is gated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct PathUpload {
    /// Frames of the path that had an update.
    pub updates: usize,
    /// The CPU's time of the update's copy to the GPU (the copies made and sent), over those frames.
    pub time: Spread,
    /// The bytes and the copies (each a barrier of its own) of those frames: the 99th percentile
    /// (nearest rank) and the largest. A rebuild makes new buffers and has no copies.
    pub bytes_p99: u64,
    pub bytes_max: u64,
    pub writes_p99: u64,
    pub writes_max: u64,
    /// Frames in which the scene thread held work back for the budget, so that the frame's upload
    /// was cut.
    pub cut_frames: usize,
    /// The longest run of frames in a row that did, in frames and in milliseconds: how long the
    /// longest backlog lasted.
    pub longest_backlog_frames: usize,
    pub longest_backlog_ms: f64,
    /// The frames whose upload was **over the plain budget** (more than `UPLOAD_BUDGET`'s bytes or
    /// more than its copies), by cause. A frame can take up to the budget plus the operation that
    /// took what waits to it plus the round's first batch of arrivals plus the gap words, and some
    /// work is not budgeted at all (see [`OverBudget`]).
    pub over_budget: OverBudget,
    /// Frames in which a buffer of the scene grew, the bytes of the old buffers those growths
    /// copied on the GPU, and the most that one frame spent on the host making a longer buffer: the
    /// frame thread's own copy (`lengthen_ms_max`, inside the recentre) and the GPU buffers and
    /// their copies (`grow_ms_max`, inside the update's write). The slowest frames carry the same
    /// per frame.
    pub growth_frames: usize,
    pub grown_bytes: u64,
    pub lengthen_ms_max: f64,
    pub grow_ms_max: f64,
}

/// The frames of a path whose upload was over the plain budget, and why. A frame is counted under
/// one cause, the first that applies, in this order: a **jump** or **bands** (the scene packed
/// again and uploaded whole), an **edit** (the frame took the newest Edit: Edits and their coarse
/// remakes are not budgeted), a **growth** (a buffer was made longer: the copy of its old words is
/// not budgeted and is not in the bytes), or else an **operation** (the operation that took what
/// waits to the budget, the round's first batch of arrivals that is taken after it, and the gap
/// words of joined ranges). Only a frame whose bytes or copies are over the budget is counted
/// (a rebuild of less is not), under exactly one cause, so the causes add up to `frames`. What
/// is counted is the update's bytes and copies, not a frame time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct OverBudget {
    pub frames: usize,
    pub operation: usize,
    pub edit: usize,
    pub jump: usize,
    pub bands: usize,
    pub growth: usize,
}

impl OverBudget {
    fn add(&mut self, cause: Option<&str>) {
        let Some(cause) = cause else { return };
        self.frames += 1;
        match cause {
            "jump" => self.jump += 1,
            "bands" => self.bands += 1,
            "edit" => self.edit += 1,
            "growth" => self.growth += 1,
            _ => self.operation += 1,
        }
    }
}

/// Why a frame's upload was over the plain budget (see [`OverBudget`]), if it was.
fn over_budget_cause(
    update: &str,
    bytes: u64,
    writes: u32,
    rebuilt: Option<Rebuild>,
    took_edit: bool,
    grown: bool,
) -> Option<&'static str> {
    let budget = UPLOAD_BUDGET;
    if bytes <= budget.bytes && u64::from(writes) <= u64::from(budget.writes) {
        return None;
    }
    Some(if update == "rebuild" {
        // (None is not expected for a rebuild: it is counted as a jump.)
        match rebuilt {
            Some(Rebuild::Bands | Rebuild::Release) => "bands",
            _ => "jump",
        }
    } else if took_edit {
        "edit"
    } else if grown {
        "growth"
    } else {
        "operation"
    })
}

/// How long the scene thread spent on each part of its rounds, in milliseconds, over a path or a
/// run (`Spent`): following the camera (selecting again and packing what changed), packing what the
/// loader made, publishing (copying what changed out for the frame thread, which copies it to the
/// GPU: the upload), eviction, remaking the coarse levels over an Edit, the looks for holes, and
/// the change of the bands for the camera's speed (`reband_ms`: looking again at the nodes that a
/// change of depth listed, which is not part of `follow_ms`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct SceneThread {
    pub follow_ms: f64,
    pub pack_ms: f64,
    pub upload_ms: f64,
    pub evict_ms: f64,
    pub coarse_ms: f64,
    pub hole_scan_ms: f64,
    pub reband_ms: f64,
    /// Making the scene again for a finer depth, at a flight's end (#168, `Rebuild::Release`): the
    /// scene thread's time in the rebuild, which the frame thread does not wait for, against
    /// #217's cap on the catch-up (12 s). The copy for the frame thread is in `upload_ms`.
    pub release_ms: f64,
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
            reband_ms: ms(now.reband, then.reband),
            release_ms: ms(now.release, then.release),
            hole_scan_share: 0.0,
        };
        let all = parts.follow_ms
            + parts.pack_ms
            + parts.upload_ms
            + parts.evict_ms
            + parts.coarse_ms
            + parts.hole_scan_ms
            + parts.reband_ms
            + parts.release_ms;
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

/// How far under sea level the sea-level crossing goes, and how high over it, in metres: the pit of
/// #146's stepped test (the cap goes off about 20 m under, and comes back on 50 m over).
pub const CROSSING_UNDER_METRES: f64 = 30.0;
pub const CROSSING_OVER_METRES: f64 = 80.0;
/// The longest that the crossing waits at the bottom and at the top for the scene to come to the
/// camera.
pub const CROSSING_HOLD_CAP: Duration = Duration::from_secs(90);

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
    /// Copies written to the staging belt (each a copy on the GPU with a barrier of its own).
    pub writes: u32,
    /// Encoding the draw (the passes, the timer queries' resolving, and finishing the command
    /// buffer), and `Queue::submit`.
    pub encode_ms: f64,
    pub submit_ms: f64,
    /// Waiting for the GPU: after the CPU's clock has stopped. It holds the readback of the pass
    /// times, the device poll and what wgpu frees inside it (the belt's chunks that were used) as
    /// well as the GPU's work.
    pub wait_ms: f64,
    /// What the GPU says its passes of **this frame** took, from the timestamps the passes wrote
    /// (the device's clock, not wall time): the primary pass, the upscale pass, and from the start
    /// of the first to the end of the last (gaps included). Null where the device has no timers or
    /// this frame's own times did not come in (never an earlier frame's: the profiler says which
    /// frame a readback is of).
    pub gpu_primary_ms: Option<f64>,
    pub gpu_upscale_ms: Option<f64>,
    pub gpu_total_ms: Option<f64>,
    /// `wait_ms` less `gpu_total_ms`: what the wait was that was not the passes (the update's
    /// copies, which are queued ahead of them, the driver, other GPU clients, our own cleanup in
    /// the poll). Negative if the passes were already running while the CPU was still in the
    /// submit. Null with `gpu_total_ms`.
    pub wait_not_passes_ms: Option<f64>,
    pub other_ms: f64,
    /// The frame's look at the memory after its time was taken: the store's lock, and the read of
    /// the process's resident memory. Not in `time_ms`. `after_lock_ms` is what of it was the wait
    /// for the store's read lock (taken every sixth frame); the rest is the harness's own read of
    /// the resident memory (every thirtieth) and its bookkeeping, which a player's frame does not
    /// do.
    pub after_ms: f64,
    pub after_lock_ms: f64,
    /// The frame's pacing, which `time_ms` leaves out (#167): the milliseconds from the end of the
    /// frame before (its pacing sleep done) to this frame's start (`before_ms`: working out the
    /// camera and the harness's own bookkeeping), the sleep this frame asked for to fill the
    /// 16.667 ms (`sleep_asked_ms`, 0 for a frame that was already longer) and by how much the
    /// sleep ran over what it asked (`sleep_over_ms`: the thread waking late, which on Windows is
    /// the timer's tick and the scheduler), and the frame's cycle from its start to the end of its
    /// sleep (`cycle_ms`, so the cycle from this frame's start to the next frame's is `cycle_ms`
    /// plus the next frame's `before_ms`). A frame whose `time_ms` is long with a `wait_ms` that is
    /// not the GPU's (`wait_not_passes_ms`) and a normal `cpu_ms` is a late wake-up of the frame
    /// thread inside the device poll; a long `sleep_over_ms` is one inside the sleep.
    pub before_ms: f64,
    /// The harness's own work between the look at the memory and the sleep (working out what the
    /// frame uploaded and its parts for this report), so that `time_ms`, `after_ms`, `between_ms`, `sleep_asked_ms` and
    /// `sleep_over_ms` add up to `cycle_ms`.
    pub between_ms: f64,
    pub sleep_asked_ms: f64,
    pub sleep_over_ms: f64,
    pub cycle_ms: f64,
    /// Of `before_ms`, what the harness itself did between the frames: the look it makes at the
    /// stop of a flight (`Session::look`), which waits for the scene thread to finish a round and
    /// copies what that made to the GPU. A player's frame does not do it. The rest of `before_ms`
    /// is working out the camera (`camera_ms`) and the microseconds of the loop.
    pub harness_ms: f64,
    /// What the update copied: `none`, `patch` (the words it lists) or `rebuild` (new buffers, all
    /// of the scene written), the bytes in all (header and tables included) and those of a patch's
    /// words.
    pub update: &'static str,
    pub upload_bytes: u64,
    pub patch_bytes: u64,
    /// What making a buffer longer cost in this frame, in milliseconds, in the two places it is
    /// paid, and the bytes of the old buffers copied on the GPU: the frame thread's own copy of the
    /// scene (inside `recentre_ms`; its `Vec` is lengthened and filled with zeros), and the GPU's
    /// new buffers and their copies (inside `update_write_ms`). All zero if no buffer grew.
    pub lengthen_ms: f64,
    pub grow_ms: f64,
    pub grown_bytes: u64,
    /// Why this frame's upload was over the plain budget (`jump`, `bands`, `edit`, `growth` or
    /// `operation`, see [`OverBudget`]); null if it was not.
    pub over_budget: Option<&'static str>,
    /// Whether a look for holes **overlapped** the frame: the scene thread was in one at the start
    /// or at the end of it (from before it takes the store's read lock to after it lets go). That
    /// is not that it blocked the frame, and a look that began after the frame's start and was done
    /// before its end is missed.
    pub hole_look: bool,
}

/// How many frames of a path were of each kind of update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct UpdateKinds {
    pub none: usize,
    pub patch: usize,
    pub rebuild: usize,
}

impl UpdateKinds {
    fn add(&mut self, kind: &str) {
        match kind {
            "patch" => self.patch += 1,
            "rebuild" => self.rebuild += 1,
            _ => self.none += 1,
        }
    }
}

/// The slowest frames of a path, in a list of fixed size: no allocation while the frames come.
struct Worst {
    frames: [Option<SlowFrame>; SLOWEST],
    /// What the list is of the most of.
    key: fn(&SlowFrame) -> f64,
}

impl Worst {
    /// The slowest frames.
    fn new() -> Self {
        Self::by(|f| f.time_ms)
    }

    /// The frames with the most of `key`.
    fn by(key: fn(&SlowFrame) -> f64) -> Self {
        Self {
            frames: [None; SLOWEST],
            key,
        }
    }

    fn offer(&mut self, frame: SlowFrame) {
        let key = self.key;
        let value = |slot: &Option<SlowFrame>| slot.as_ref().map_or(f64::NEG_INFINITY, key);
        let least = self
            .frames
            .iter_mut()
            .min_by(|a, b| value(a).total_cmp(&value(b)))
            .expect("the list has room");
        if key(&frame) > value(least) {
            *least = Some(frame);
        }
    }

    /// The most first.
    fn sorted(&self) -> Vec<SlowFrame> {
        let key = self.key;
        let mut frames: Vec<SlowFrame> = self.frames.iter().flatten().copied().collect();
        frames.sort_by(|a, b| key(b).total_cmp(&key(a)));
        frames
    }
}

/// How many indexes of frames over the limit a path keeps.
const OVER_LIMIT_INDEXES: usize = 200;

/// How long the camera stood after a flight before the scene had no soft misses.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Settle {
    /// From the frame the camera stopped in, to the first frame in which the scene is at the
    /// camera (the drawn scene's camera is the frame's) and has no soft misses and nothing waiting:
    /// or to the cap, if it was not. `walk_s`, `reband_s` and `fill_s` are its three parts and add
    /// up to it.
    pub seconds: f64,
    pub settled: bool,
    /// The first part: from the stop to the scene's camera being the frame's (the scene thread
    /// walking to where the camera stopped, with the bands as they are then). The whole of
    /// `seconds` if it never was.
    pub walk_s: f64,
    /// The second part: from there until the scene thread has looked at every node that the
    /// change of the bands for the stop listed (the finer bands' bricks being made round the
    /// stop: the scene thread's own work, `SceneThread::reband_ms`), 0 if it was done by then.
    pub reband_s: f64,
    /// The third part: from there to settled (what is left is the loader's bricks being waited
    /// for, and made into the scene).
    pub fill_s: f64,
}

/// How long what a hold had held back took to catch up (#217).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CatchUp {
    pub seconds: f64,
    pub settled: bool,
}

/// Cuts of a Still view by what the scene thread's work was (#217). A cut is an update that the
/// frame thread took in while the view was Still: it restarts the Reference view from sample 0 (two
/// Primary frames and a fade). A cut that several kinds of work made counts under each, so the
/// causes add up to at least `cuts`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StillCutsReport {
    /// Frames of the path that the view was Still for.
    pub still_frames: u64,
    pub cuts: u64,
    pub by_cause: CutCauses,
}

/// How many cuts each kind of work was part of (see [`Cause`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CutCauses {
    pub edit: u64,
    pub bands: u64,
    pub coarse: u64,
    pub follow: u64,
    pub arrival: u64,
    pub coarsen: u64,
    pub reband: u64,
}

impl From<StillCuts> for StillCutsReport {
    fn from(cuts: StillCuts) -> Self {
        Self {
            still_frames: cuts.frames,
            cuts: cuts.cuts,
            by_cause: CutCauses {
                edit: cuts.of(Cause::Edit),
                bands: cuts.of(Cause::Bands),
                coarse: cuts.of(Cause::Coarse),
                follow: cuts.of(Cause::Follow),
                arrival: cuts.of(Cause::Arrival),
                coarsen: cuts.of(Cause::Coarsen),
                reband: cuts.of(Cause::Reband),
            },
        }
    }
}

/// What the scene did between the view going Still and the hold beginning (#217): the game
/// catches the scene up with the camera first, with loading going on and Primary drawn, and
/// holds it then ([`World::hold_still`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BeforeHold {
    /// How far the scene was behind the camera, in Voxels, when the view went Still: what a hold
    /// at once (the first form of #217) would have held it at, and the Reference view converged on.
    pub lag_when_still_voxels: f64,
    /// And when the hold began: under [`crate::world::CAUGHT_UP_LAG`] unless the cap ended the wait.
    pub lag_when_held_voxels: f64,
    /// From the view going Still to the hold beginning: how long the Reference view started
    /// later than it would have.
    pub waited_s: f64,
    /// The wait ran to [`crate::world::CATCH_UP_CAP`] and the hold began with the scene still behind the camera.
    /// **Expected false:** a cap that is hit means the Reference view starts on a lagging scene.
    pub hit_cap: bool,
}

/// The view held Still after a flight, with the game's policy (the scene catches up with the
/// camera, then loading is paused, as `Mode::Reference` does), and then let go (#217): the cuts
/// that the scene thread made while it was held, and the catch-up after.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct StillAfterFlight {
    /// The catch-up of the scene before the hold began.
    pub before_hold: BeforeHold,
    /// How long the camera was held after the hold began, in seconds.
    pub hold_s: f64,
    /// The cuts while it was held. **The acceptance of #217 is at most 1 cut here**, from scene
    /// patches (the Planet has no Edit in this run).
    pub cuts: StillCutsReport,
    /// From the frame that ended the hold (the view changes, loading goes on) to the first frame
    /// with no soft misses and nothing waiting: the catch-up of what was held back.
    pub catch_up: CatchUp,
    /// What the look at every split brick found after the catch-up. Must be 0: nothing held back
    /// leaves a hole.
    pub holes_after_catch_up: u64,
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
    /// The frames' cycle from the start of one to the start of the next (the last frame of the
    /// path has none), and by how much their pacing sleeps ran over what they asked for, over the
    /// frames that slept (#167). A cycle over 16.7 ms that `frame_time` does not show is the
    /// thread waking late from its sleep or the harness between frames; see
    /// [`SlowFrame::sleep_over_ms`].
    pub frame_cycle: Spread,
    pub sleep_overshoot: Spread,
    /// The frames with the longest turn (`before_ms` + `cycle_ms`: from the end of the frame before
    /// to the end of this one's sleep), longest first, with their parts: where a long cycle that
    /// `frame_time` does not show is, and which of `camera_ms`, `harness_ms`, `after_lock_ms`, the
    /// rest of `after_ms` or the sleep it was (#167). Reported; not gated, as a cycle's length
    /// is as much the harness's work between frames as the frame's.
    pub slowest_cycles: Vec<SlowFrame>,
    /// Frames whose time was over the frame-time limit (the gate's: the 99th percentile is held to
    /// it, and this is how many frames there were above it), and the slowest few, worst first, with
    /// their parts. In the min and the max the i-th slowest of each run is taken with the i-th of
    /// the others, member by member, and null if the runs did not all keep as many.
    pub frames_over_limit: usize,
    /// Those frames by what their update was: none, a patch or a rebuild.
    pub frames_over_limit_by_update: UpdateKinds,
    pub slowest: Vec<SlowFrame>,
    /// The places in the path (from 0) of the first frames over the limit, so that where the slow
    /// frames are can be read against where the largest updates are (`largest_updates`): kept up
    /// to a few hundred, `frames_over_limit` says how many there were.
    pub over_limit_at: Vec<usize>,
    /// The frames whose update copied the most bytes, the most first, in the same form as
    /// `slowest`: with `index` and `at_ms` to read against the slow frames, and each one's time,
    /// copies and wait.
    pub largest_updates: Vec<SlowFrame>,
    /// How far behind the camera the scene that was drawn was, at each frame of the path: the
    /// distance between the frame thread's camera and the centre of the scene it draws (the one the
    /// scene thread had published, and the frame thread taken), in Voxels and in `d0` (the
    /// report's, so that it reads against the jump: 2 `d0`), the most and the 99th percentile. A
    /// scene that is always walked to, never made again, falls behind without limit above the
    /// speed it keeps up at; a path that ends with the camera standing ends with the lag the
    /// scene has then (0 if it has caught up).
    pub scene_lag_voxels_max: f64,
    pub scene_lag_d0_max: f64,
    pub scene_lag_voxels_p99: f64,
    pub scene_lag_d0_p99: f64,
    /// The mean of the drawn scene's lag over the frames of the path, in `d0` (#168: how far
    /// behind the camera the scene is for most of a flight, which the maximum and the 99th
    /// percentile do not say). Reported, not gated.
    pub scene_lag_d0_mean: f64,
    /// Rounds in which the scene thread moved the scene to the camera, and in which it held work
    /// back for the upload budget (`Done::follow_steps` and `budget_cuts`), and the frames that
    /// scored a camera soft miss in each second of the path (the flights only; empty for the
    /// others): whether they are at the start of a flight, or all along it.
    pub scene_follow_steps: u64,
    pub scene_budget_cuts: u64,
    /// Times the camera's speed gave a depth back and the scene was made again at it
    /// (`Rebuild::Release`; `SceneThread::release_ms` is the scene thread's time in it) instead of
    /// the nodes being looked at again, and the frame thread's cost of taking one: the longest
    /// frame that had it, and the longest of those frames' update (the copy to the GPU). 0 if
    /// none was in the path's frames.
    pub scene_release_rebuilds: u64,
    pub release_frame_ms_max: f64,
    pub release_update_ms_max: f64,
    /// The frames with a camera soft miss in each second of the path (the second a frame began in,
    /// counted from the path's first frame): one entry for every second the path's frames cover,
    /// so a path of 10 s has 10, and a last entry of 0 is a second without a miss (an array that
    /// ended at the last second with one made a miss at 5 to 6 s look like a path of 6 s).
    pub camera_soft_miss_per_second: Vec<u32>,
    /// The camera misses split by what can have caused them (#168): a frame whose drawn scene is
    /// further from the camera than level 0 reaches (`floor(1)` less the hysteresis: the miss
    /// score asks for level 0 within it of the frame's camera) is a miss by construction, whatever
    /// the loader has made, so the frames over that lag are counted (`lag_over_floor_frames`,
    /// misses or not), and the misses are the **lag side** (the frame was over it: the scene
    /// thread's following, P1) or the **loader side** (it was not: a brick that the bands ask for
    /// is not there, which is decision A's). They add up to `camera_soft_miss_frames`.
    pub camera_soft_miss_lag_side: usize,
    pub camera_soft_miss_loader_side: usize,
    pub lag_over_floor_frames: usize,
    /// The scene's buffers in use and the bricks drawn at each level when the path ended (to be
    /// read against `scene_used_peak_mib`: a peak over it is a transient).
    pub scene_used_end_mib: f64,
    pub scene_level_bricks_end: Vec<usize>,
    /// What the frames' uploads came to, and how often the budget cut them.
    pub upload: PathUpload,
    /// Soft misses a frame reported (counted by the scene thread a few times a second, so a frame
    /// reports the last count).
    pub soft_misses: Series,
    pub soft_misses_per_frame: f64,
    /// Frames in which the ground straight below **the frame's camera** (one ray down from it) was
    /// drawn coarser than the bands ask at its distance, with the bands' hysteresis: the soft miss
    /// of what the player sees, which the scene thread's own count (`soft_misses`, against the
    /// scene's camera, which lags the frame's) does not say. **This is what is gated**, on the
    /// flights (ADR 0009, #168).
    pub camera_soft_miss_frames: usize,
    /// What the loader did in the path (and in the forced look after its frames): the bricks the
    /// generator was asked for, and the bricks of the arrivals that the scene thread took in, so
    /// that a flight that falls short can be read as limited by the generator (few generated, many
    /// waiting) or not. And the loader's queue (the bricks asked for and not yet taken in, as of
    /// each frame): its most and its mean over the frames.
    pub generated_bricks: u64,
    pub arrived_bricks: u64,
    pub loader_queue_max: usize,
    pub loader_queue_mean: f64,
    /// What the loader's workers did with their time in the path, summed over them, in seconds:
    /// generating, and waiting with jobs queued because they were as far ahead as they may be (the
    /// scene thread's taking in is what limits the loader when this is most of it, #168); and how
    /// many times the scene thread took in what the loader made (the time of that is
    /// `scene_thread.pack_ms`), and in how many it stopped because its slice was used up.
    pub loader_generating_s: f64,
    pub loader_held_back_s: f64,
    pub stream_calls: u64,
    pub stream_slices_used: u64,
    /// Looks of the scene thread at every split brick (every few seconds, and one forced at the end
    /// of the path), the holes they found in all, and those per frame of the path. Never 0 looks.
    pub hole_looks: u64,
    pub holes: u64,
    pub holes_per_frame: f64,
    /// The scene thread's time in the frames of the path: without the look for holes that is forced
    /// after them.
    pub scene_thread: SceneThread,
    /// The longest round of the scene thread in the frames of the path, in milliseconds, without
    /// the look for holes (#213), and the longest single update of the scene to a camera in it (a
    /// step of following the camera): the longest that the scene stood still for the frame thread
    /// (`SceneWorker::take_longest`). The sea-level crossing's update is held against the other
    /// paths' worst.
    pub scene_round_max_ms: f64,
    pub scene_update_max_ms: f64,
    /// The updates in which the horizon cap came on or went off, and the longest update that was
    /// one of those or part of absorbing one (the revisiting goes on after the flip, and into the
    /// next path if the camera has stopped: its figures are in the path where it ends).
    pub cap_flips: u64,
    pub cap_flip_max_ms: f64,
    /// The most MiB of the scene in use at the end of one of the path's frames (the run's
    /// `memory.scene_used_peak_mib`, the gate, is the most of these and of the looks between frames).
    pub scene_used_peak_mib: f64,
    /// Absorbing a flip: the most nodes that waited to be revisited at once, the updates that
    /// were absorbing, and the milliseconds from the update that found a flip to the one that
    /// ended its absorbing (summed over the spans that ended in the path; flips that overlap are
    /// one span, and `absorb_updates` counts the same span's updates).
    pub backlog_peak: u64,
    pub absorb_updates: u64,
    pub absorb_ms: f64,
    /// What the forced look at the end of the path took in the scene thread, in milliseconds (the
    /// scan itself): a look after the frames, not in them.
    pub end_look_ms: f64,
    /// For the stop after a flight only.
    pub settle: Option<Settle>,
    /// For the stop after a flight only (#217): the cuts the view would have had, from the frame it
    /// has stood for [`STILL_FRAMES`] frames. Nothing is held back in this path (it measures the
    /// settle time), so this is what a Still view gets when the scene thread works as it does for
    /// the Primary view; [`RunReport::still_after_flight`] is the game's policy.
    pub still_cuts: Option<StillCutsReport>,
}

/// The cold start: the world made, and the frames at the Spawn until the resident set is complete.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ColdStart {
    /// Generating the world map, which the client does before it loads the Planet: not in the time
    /// to control. Null for the test Planet's.
    pub world_map_s: Option<f64>,
    /// [`crate::load`]: the spawn ring, the Guard, and the first scene.
    pub load_s: f64,
    /// The spawn ring (20 m, #168), which is when the player has control (ADR 0009), and the part
    /// of the Guard after it that a camera within 2 km of the Spawn could see (#220: `guard_cells_at_control`
    /// of the `guard_cells` cells of the Guard, as [`crate::loader::plan_guard`] says), which the
    /// window waits for as well so that nothing visible past the ring is sky. Both are in
    /// `load_s`. **The `spawn_ring_s` gate is on the ring.**
    pub spawn_ring_s: f64,
    pub guard_s: f64,
    /// The cells of the Guard, in all (the bricks of the pyramid's top level that hold it, see
    /// `Planet::guard_cells`), and how many of them were made before the window opened (#220). The
    /// rest are made by the loader's workers in the background, a quarter of them at a time while
    /// there is other work. 0 for the test Planet's, whose Guard is whole at the start.
    pub guard_cells: u32,
    pub guard_cells_at_control: u32,
    /// The real time to control: from the world map being ready to the window opening (`load`
    /// returning: the ring, the Guard's visible part and the first scene, which is when `main`
    /// opens it). **The `time_to_control_s` gate is on this**, 2 s since #220.
    pub time_to_control_s: f64,
    /// From the start of the load to the first frame with level 0 under the camera: one ray from
    /// the camera straight down hits the ground at level 0. This is **not** the spec's time to
    /// control (the ring of 20 m round the Spawn at full detail, and the character standing in it:
    /// the bench has no server). The spawn ring is made inside [`crate::load`], so this is the
    /// load time and about a frame.
    pub time_to_level_0_under_camera_s: f64,
    /// From the start of the load to the Guard being complete: the last cell taken in by the scene
    /// thread, seen by the first frame after it (#220). Reported, not gated; the camera's flights
    /// start after the fill, which waits for it, and until then a flying camera is held to 2 km of
    /// the Spawn ([`World::hold_to_guard`]).
    pub guard_complete_s: f64,
    /// From the start of the load to the resident set being complete.
    pub fill_s: f64,
    /// An Edit made at the Spawn while the loader was filling the resident set (#168: a long
    /// slice of taking in is on the scene thread at the time, and the Edit waits for the round to
    /// end or for the slice to give way), timed as the one after the paths is. Null if the fill
    /// was over before one could be made. Reported, not gated.
    pub edit_during_fill: Option<EditLatency>,
    /// The bricks that the loader had been asked for and not taken in when that Edit was made.
    pub edit_during_fill_pending: usize,
}

/// What the run held, at the most.
#[derive(Clone, Debug, PartialEq, Serialize)]
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
    /// What the run was doing when `scene_used_peak_mib` was reached (a path's name, or the wait
    /// between paths, "the loader's catch-up after ..."; #168). Reported.
    pub scene_used_peak_in: String,
    /// The bricks drawn at each level when `scene_used_peak_mib` was reached.
    pub scene_used_peak_level_bricks: Vec<usize>,
    /// The scene's buffers as the GPU reserves them, used or not (`Scene::reserved_bytes`): this
    /// is the VRAM the scene takes, after its growths (a buffer that fills grows, #145: the most
    /// of the frames' is given). Reported; not gated.
    pub scene_reserved_mib: f64,
    /// The peak **during a growth** (ADR 0004's accounting is of the buffers after it): the GPU's
    /// buffers at the most while a buffer grew, the old buffer and the new one both counted
    /// (`Tracer::peak_growth_bytes`: what the buffers held before, plus the new ones; 0 if none
    /// grew); and the frame thread's copy of the scene at the most, its reserved bytes after a growth plus the old
    /// words of the buffers that grew in that frame (a `Vec` that is lengthened may hold the old
    /// allocation and the new at once, which is what the platform's allocator does when it cannot
    /// extend in place: the upper bound; the scene thread's own copy, which grows the same way a
    /// round earlier, is not in it). Reported; not gated, and `scene_used_peak_mib` stays the gate.
    pub scene_gpu_peak_growth_mib: f64,
    pub scene_host_peak_growth_mib: f64,
    /// The GPU's peak while a **rebuilt** scene was made (the old scene's buffers and the new
    /// ones: a jump or other bands), apart from the growths'. 0 if there was none.
    pub scene_gpu_peak_rebuild_mib: f64,
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
    /// The view held Still after a flight (#217).
    pub still_after_flight: StillAfterFlight,
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
    /// Frames on the flights (the flights at 2 m and 50 m, the take-off and the turn) in which the
    /// ground below the frame's camera was drawn coarser than the speed-scaled bands ask
    /// ([`PathReport::camera_soft_miss_frames`]), at the most of a run. The speed-scaled level is
    /// not a soft miss (#168).
    pub camera_soft_miss_frames: Limit,
    /// The scene thread's own soft-miss frames on the same flights (against the scene's camera,
    /// which lags the frame's), at the most of a run: **reported, not gated**.
    pub scene_soft_miss_frames: u64,
    /// The drawn scene's lag behind the camera, in `d0`, at the most over the four flights of the
    /// worst run. Main's value plus a margin for now; it tightens once the follow fixes land.
    pub scene_lag_d0_max: Limit,
    /// The spawn ring's generation (ADR 0009), at the most of the runs.
    pub spawn_ring_s: Limit,
    /// From the world map being ready to the window opening (the ring and the Guard), at the most
    /// of the runs.
    pub time_to_control_s: Limit,
    /// The settle time after the stop, at the most of the runs (the cap's if one did not settle,
    /// which is over whatever the limit): the scene at the camera, no soft miss, nothing waiting.
    pub settle_s: Limit,
    /// The gates that are over: `frame_time`, `frame_cpu`, `holes`, `memory`, `soft_misses`,
    /// `scene_lag`, `spawn_ring`, `time_to_control`, `settle`.
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
    let camera_soft_miss_frames = Limit {
        worst: worst(&mut runs.iter().map(|r| {
            r.paths
                .iter()
                .filter(|p| FLIGHTS.contains(&p.name))
                .map(|p| p.camera_soft_miss_frames as f64)
                .sum::<f64>()
        })),
        limit: budgets.camera_soft_miss_frames as f64,
    };
    let scene_soft_miss_frames = runs
        .iter()
        .map(|r| {
            r.paths
                .iter()
                .filter(|p| FLIGHTS.contains(&p.name))
                .map(|p| p.soft_misses.frames_with as u64)
                .sum::<u64>()
        })
        .max()
        .unwrap_or(0);
    let scene_lag_d0_max = Limit {
        worst: worst(
            &mut paths()
                .filter(|p| FLIGHTS.contains(&p.name))
                .map(|p| p.scene_lag_d0_max),
        ),
        limit: budgets.scene_lag_d0_max,
    };
    let spawn_ring_s = Limit {
        worst: worst(&mut runs.iter().map(|r| r.cold_start.spawn_ring_s)),
        limit: budgets.spawn_ring_s,
    };
    let time_to_control_s = Limit {
        worst: worst(&mut runs.iter().map(|r| r.cold_start.time_to_control_s)),
        limit: budgets.time_to_control_s,
    };
    let unsettled = paths().any(|p| p.settle.is_some_and(|s| !s.settled));
    let settle_s = Limit {
        worst: worst(&mut paths().filter_map(|p| p.settle.map(|s| s.seconds))),
        limit: budgets.settle_s,
    };
    let over = [
        ("frame_time", frame_time_p99_ms.over()),
        ("frame_cpu", frame_cpu_p99_ms.over()),
        ("holes", holes.over() || !looked),
        ("memory", scene_mib.over() || store_bricks.over()),
        ("soft_misses", camera_soft_miss_frames.over()),
        ("scene_lag", scene_lag_d0_max.over()),
        ("spawn_ring", spawn_ring_s.over()),
        ("time_to_control", time_to_control_s.over()),
        // A run that was still unsettled at the cap is over, whatever the cap is.
        ("settle", settle_s.over() || unsettled),
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
        camera_soft_miss_frames,
        scene_soft_miss_frames,
        scene_lag_d0_max,
        spawn_ring_s,
        time_to_control_s,
        settle_s,
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

/// The members that have no least and most over the runs, and are in the runs only: a list of the
/// places of frames (`over_limit_at`) is a different list in each run, and the i-th place of one
/// run is not comparable with the i-th of another.
const NOT_IN_THE_ENVELOPE: &[&str] = &["over_limit_at"];

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
                if NOT_IN_THE_ENVELOPE.contains(&name.as_str()) {
                    continue;
                }
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
    /// Sea level's distance from the centre, in Voxels: the horizon cap's sphere.
    sea: f64,
    constants: Constants,
    world_map_s: Option<f64>,
    load: Duration,
    /// The spawn ring and the Guard's visible part, in `load` ([`ColdStart::spawn_ring_s`]); 0 for
    /// the test Planet's. And the Guard's cells: in all, and made before the window opened.
    ring: Duration,
    guard: Duration,
    guard_cells: (usize, usize),
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
            sea: loaded.sea,
            // Level 0 beyond the spawn ring is in what is measured here, so none of it is taken
            // out.
            constants: Constants {
                streamed_level_0_beyond_the_ring_per_d0_squared: 0.0,
                ..Constants::of_the_client()
            },
            world_map_s: Some(world_map.as_secs_f64()),
            load,
            ring: loaded.ring_took,
            guard: loaded.guard_took,
            guard_cells: (
                loaded.guard_cells.0 + loaded.guard_cells.1,
                loaded.guard_cells.0,
            ),
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
        sea,
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
            sea,
            constants: Constants::of_the_client(),
            world_map_s: None,
            load: began.elapsed(),
            ring: Duration::ZERO,
            guard: Duration::ZERO,
            guard_cells: (0, 0),
        },
    })
}

/// The largest of the `lags` (Voxels) and the 99th percentile, nearest rank; 0 for none.
fn lag_spread(lags: &[f64]) -> (f64, f64) {
    let mut sorted = lags.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (sorted.len() * 99).div_ceil(100);
    (
        sorted.last().copied().unwrap_or(0.0),
        rank.checked_sub(1).map_or(0.0, |at| sorted[at]),
    )
}

/// The frame thread's peak while a buffer grew: `peak`, or if `grown` bytes of old buffers were
/// copied in this frame, the larger of it and what the copy reserves now (`reserved`) plus them.
/// A frame with no growth is not a peak of one: 0 stays 0.
fn host_growth_peak(peak: u64, reserved: u64, grown: u64) -> u64 {
    if grown > 0 {
        peak.max(reserved + grown)
    } else {
        peak
    }
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
    /// Whether the ground below the frame's camera is drawn coarser than the bands ask
    /// ([`PathReport::camera_soft_miss_frames`]).
    camera_soft_miss: bool,
    /// Whether the drawn scene's lag was over what level 0 reaches (see
    /// [`PathReport::lag_over_floor_frames`]); false when nothing is scored.
    lag_over_floor: bool,
    /// Whether the frame's update was the scene made again for a depth that was given back
    /// ([`Rebuild::Release`]).
    release_rebuild: bool,
    /// Whether the scene the frame draws is at the frame's camera ([`Settle`]).
    at_camera: bool,
    /// The words of the scene in use at the end of the frame, in bytes.
    scene_used_bytes: u64,
    /// Whether the scene thread held work back in this frame for the upload budget.
    throttled: bool,
    /// The bricks the loader had been asked for and not yet taken in, as of the frame's look at the
    /// scene (the loader's queue and what its workers have, see [`PathReport::loader_queue_max`]).
    loader_queue: usize,
    /// The drawn scene's lag behind the camera in Voxels (see [`PathReport::scene_lag_voxels_max`]).
    lag: f64,
    /// Whether the look at the scene in this frame has the newest Edit in it (from this frame or an
    /// earlier one), and when it was done.
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
    /// The bricks the generator had been asked for and the arrivals the scene thread had taken in
    /// when the path began.
    generated: u64,
    arrived: u64,
    /// The loader's workers' time (generating, held back, in microseconds) and the scene thread's
    /// calls to take in, when the path began.
    busy: (u64, u64),
    streams: (u64, u64),
    /// The loader's queue at each frame: the most and the sum, for the mean.
    queue_max: usize,
    queue_sum: u64,
    /// The most the scene's buffers in use were, in bytes.
    /// The scene thread's follow rounds and budget cuts when the path began.
    follow: (u64, u64, u64),
    /// Frames with a camera soft miss, by second of the path.
    miss_seconds: Vec<u32>,
    /// What the scene thread had spent when the path's frames were done, before the forced look.
    frames_done: Option<Spent>,
    /// The scene thread's longest round in the path's frames, taken when they were done.
    longest: Longest,
    /// The most bytes of the scene in use at the end of one of the path's frames.
    scene_used_peak_bytes: u64,
    frame_ms: Vec<f64>,
    cpu_ms: Vec<f64>,
    /// The start of each frame, to work the cycles out from, and the overshoot of the sleeps.
    started: Vec<Instant>,
    /// The cycle from the start of each frame to the next's, less what the harness did between
    /// them (the look at a stop).
    cycles: Vec<f64>,
    sleep_over_ms: Vec<f64>,
    soft_misses: Series,
    camera_soft_miss_frames: usize,
    camera_soft_miss_lag_side: usize,
    lag_over_floor_frames: usize,
    release_frame_ms: f64,
    release_update_ms: f64,
    /// The update of each frame that had one: its time, bytes and copies.
    update_ms: Vec<f64>,
    update_bytes: Vec<u64>,
    update_writes: Vec<u64>,
    /// Frames cut by the budget, the run of them that is going on (when it began, and how many),
    /// and the longest so far.
    cut_frames: usize,
    backlog: Option<(Instant, usize)>,
    longest_backlog: (usize, f64),
    /// Frames over the limit, and the slowest few.
    limit_ms: f64,
    over_limit: usize,
    over_by_update: UpdateKinds,
    over_budget: OverBudget,
    growth_frames: usize,
    grown_bytes: u64,
    lengthen_ms_max: f64,
    grow_ms_max: f64,
    worst: Worst,
    /// The frames with the longest turn.
    longest_turns: Worst,
    /// The frames over the limit, by place, as far as the report keeps them.
    over_limit_at: Vec<usize>,
    /// The updates with the most bytes.
    largest: Worst,
    /// The lag of the drawn scene behind the camera at each frame, in Voxels.
    lags: Vec<f64>,
}

impl Capture {
    fn start(name: &'static str, world: &World, limit_ms: f64) -> Self {
        let work = world.stats().work;
        // What an earlier round left is not this path's.
        world.take_longest();
        Self {
            name,
            began: Instant::now(),
            seconds: None,
            frames_done: None,
            longest: Longest::default(),
            scene_used_peak_bytes: 0,
            limit_ms,
            over_limit: 0,
            over_by_update: UpdateKinds::default(),
            over_budget: OverBudget::default(),
            growth_frames: 0,
            grown_bytes: 0,
            lengthen_ms_max: 0.0,
            grow_ms_max: 0.0,
            worst: Worst::new(),
            longest_turns: Worst::by(|f| f.before_ms + f.cycle_ms),
            over_limit_at: Vec::new(),
            largest: Worst::by(|f| f.upload_bytes as f64),
            lags: Vec::new(),
            spent: world.scene_thread_time(),
            scans: work.hole_scans,
            holes: work.holes_found,
            generated: world.stats().loader.generated,
            arrived: work.arrived_bricks,
            busy: (
                world.stats().loader.generating_us,
                world.stats().loader.held_back_us,
            ),
            streams: (work.stream_calls, work.stream_slices_used),
            queue_max: 0,
            queue_sum: 0,
            follow: (work.follow_steps, work.budget_cuts, work.release_rebuilds),
            miss_seconds: Vec::new(),
            frame_ms: Vec::new(),
            cpu_ms: Vec::new(),
            started: Vec::new(),
            cycles: Vec::new(),
            sleep_over_ms: Vec::new(),
            soft_misses: Series::default(),
            camera_soft_miss_frames: 0,
            camera_soft_miss_lag_side: 0,
            lag_over_floor_frames: 0,
            release_frame_ms: 0.0,
            release_update_ms: 0.0,
            update_ms: Vec::new(),
            update_bytes: Vec::new(),
            update_writes: Vec::new(),
            cut_frames: 0,
            backlog: None,
            longest_backlog: (0, 0.0),
        }
    }

    fn add(&mut self, frame: &Frame) {
        self.scene_used_peak_bytes = self.scene_used_peak_bytes.max(frame.scene_used_bytes);
        if frame.time_ms > self.limit_ms {
            if self.over_limit_at.len() < OVER_LIMIT_INDEXES {
                self.over_limit_at.push(self.frame_ms.len());
            }
            self.over_limit += 1;
            self.over_by_update.add(frame.parts.update);
        }
        self.lags.push(frame.lag);
        self.queue_max = self.queue_max.max(frame.loader_queue);
        self.queue_sum += frame.loader_queue as u64;
        self.over_budget.add(frame.parts.over_budget);
        if frame.parts.grown_bytes > 0 {
            self.growth_frames += 1;
            self.grown_bytes += frame.parts.grown_bytes;
        }
        self.lengthen_ms_max = self.lengthen_ms_max.max(frame.parts.lengthen_ms);
        self.grow_ms_max = self.grow_ms_max.max(frame.parts.grow_ms);
        let slow = SlowFrame {
            index: self.frame_ms.len(),
            at_ms: frame
                .began
                .saturating_duration_since(self.began)
                .as_secs_f64()
                * 1e3,
            ..frame.parts
        };
        self.worst.offer(slow);
        self.longest_turns.offer(slow);
        if frame.parts.update != "none" {
            self.largest.offer(slow);
        }
        self.frame_ms.push(frame.time_ms);
        self.cpu_ms.push(frame.cpu_ms);
        if let Some(before) = self.started.last() {
            // The harness's own look between frames is not the frame's pacing.
            let turn = frame.began.duration_since(*before).as_secs_f64() * 1e3;
            self.cycles.push((turn - frame.parts.harness_ms).max(0.0));
        }
        self.started.push(frame.began);
        if frame.parts.sleep_asked_ms > 0.0 {
            self.sleep_over_ms.push(frame.parts.sleep_over_ms);
        }
        self.soft_misses.add(frame.soft_misses as u64);
        self.camera_soft_miss_frames += usize::from(frame.camera_soft_miss);
        self.camera_soft_miss_lag_side +=
            usize::from(frame.camera_soft_miss && frame.lag_over_floor);
        self.lag_over_floor_frames += usize::from(frame.lag_over_floor);
        if frame.release_rebuild {
            self.release_frame_ms = self.release_frame_ms.max(frame.time_ms);
            self.release_update_ms = self.release_update_ms.max(frame.parts.update_ms);
        }
        if frame.camera_soft_miss {
            let second = frame.began.saturating_duration_since(self.began).as_secs() as usize;
            if self.miss_seconds.len() <= second {
                self.miss_seconds.resize(second + 1, 0);
            }
            self.miss_seconds[second] += 1;
        }
        if frame.parts.update != "none" {
            self.update_ms.push(frame.parts.update_ms);
            self.update_bytes.push(frame.parts.upload_bytes);
            self.update_writes.push(u64::from(frame.parts.writes));
        }
        if frame.throttled {
            self.cut_frames += 1;
            let (began, frames) = self.backlog.get_or_insert((frame.began, 0));
            *frames += 1;
            let ms = frame.began.duration_since(*began).as_secs_f64() * 1e3 + frame.time_ms;
            if *frames > self.longest_backlog.0 {
                self.longest_backlog = (*frames, ms);
            }
        } else {
            self.backlog = None;
        }
    }

    /// The path's frames are done: what the scene thread has spent so far is the frames', and the
    /// forced look that follows is not.
    fn frames_done(&mut self, world: &World) {
        self.seconds = Some(self.began.elapsed().as_secs_f64());
        self.frames_done = Some(world.scene_thread_time());
        self.longest = world.take_longest();
    }

    fn finish(
        self,
        world: &World,
        settle: Option<Settle>,
        still_cuts: Option<StillCutsReport>,
    ) -> PathReport {
        let work = world.stats().work;
        let now = world.scene_thread_time();
        let frames_end = self.frames_done.unwrap_or(now);
        let frames = self.frame_ms.len();
        let per_frame = |count: u64| count as f64 / frames.max(1) as f64;
        let holes = work.holes_found - self.holes;
        let d0 = world.stats().bands.map(|bands| bands.d0());
        let (lag_max, lag_p99) = lag_spread(&self.lags);
        PathReport {
            name: self.name,
            frames,
            seconds: self
                .seconds
                .unwrap_or_else(|| self.began.elapsed().as_secs_f64()),
            frame_time: Spread::of(&self.frame_ms),
            frame_cpu: Spread::of(&self.cpu_ms),
            frame_cycle: Spread::of(&self.cycles),
            sleep_overshoot: Spread::of(&self.sleep_over_ms),
            frames_over_limit: self.over_limit,
            frames_over_limit_by_update: self.over_by_update,
            slowest: self.worst.sorted(),
            slowest_cycles: self.longest_turns.sorted(),
            over_limit_at: self.over_limit_at,
            largest_updates: self.largest.sorted(),
            scene_lag_voxels_max: lag_max,
            scene_lag_d0_max: d0.map_or(0.0, |d0| lag_max / d0),
            scene_lag_voxels_p99: lag_p99,
            scene_lag_d0_p99: d0.map_or(0.0, |d0| lag_p99 / d0),
            scene_lag_d0_mean: d0.map_or(0.0, |d0| {
                self.lags.iter().sum::<f64>() / self.lags.len().max(1) as f64 / d0
            }),
            scene_follow_steps: work.follow_steps - self.follow.0,
            scene_budget_cuts: work.budget_cuts - self.follow.1,
            scene_release_rebuilds: work.release_rebuilds - self.follow.2,
            release_frame_ms_max: self.release_frame_ms,
            release_update_ms_max: self.release_update_ms,
            camera_soft_miss_per_second: misses_by_second(
                &self.miss_seconds,
                self.started.last().copied(),
                self.began,
            ),
            camera_soft_miss_lag_side: self.camera_soft_miss_lag_side,
            camera_soft_miss_loader_side: self.camera_soft_miss_frames
                - self.camera_soft_miss_lag_side,
            lag_over_floor_frames: self.lag_over_floor_frames,
            scene_used_end_mib: world.used_bytes() as f64 / MIB,
            scene_level_bricks_end: (0..render::lod::LEVELS)
                .map(|l| world.level_bricks(l))
                .collect(),
            upload: PathUpload {
                updates: self.update_ms.len(),
                time: Spread::of(&self.update_ms),
                bytes_p99: percentile_99(&self.update_bytes),
                bytes_max: self.update_bytes.iter().copied().max().unwrap_or(0),
                writes_p99: percentile_99(&self.update_writes),
                writes_max: self.update_writes.iter().copied().max().unwrap_or(0),
                cut_frames: self.cut_frames,
                longest_backlog_frames: self.longest_backlog.0,
                longest_backlog_ms: self.longest_backlog.1,
                over_budget: self.over_budget,
                growth_frames: self.growth_frames,
                grown_bytes: self.grown_bytes,
                lengthen_ms_max: self.lengthen_ms_max,
                grow_ms_max: self.grow_ms_max,
            },
            soft_misses_per_frame: per_frame(self.soft_misses.total),
            soft_misses: self.soft_misses,
            camera_soft_miss_frames: self.camera_soft_miss_frames,
            generated_bricks: world
                .stats()
                .loader
                .generated
                .saturating_sub(self.generated),
            arrived_bricks: work.arrived_bricks - self.arrived,
            loader_queue_max: self.queue_max,
            loader_queue_mean: self.queue_sum as f64 / frames.max(1) as f64,
            loader_generating_s: world
                .stats()
                .loader
                .generating_us
                .saturating_sub(self.busy.0) as f64
                * 1e-6,
            loader_held_back_s: world
                .stats()
                .loader
                .held_back_us
                .saturating_sub(self.busy.1) as f64
                * 1e-6,
            stream_calls: work.stream_calls - self.streams.0,
            stream_slices_used: work.stream_slices_used - self.streams.1,
            hole_looks: work.hole_scans - self.scans,
            holes,
            holes_per_frame: per_frame(holes),
            scene_thread: SceneThread::between(self.spent, frames_end),
            scene_round_max_ms: self.longest.round.as_secs_f64() * 1e3,
            scene_update_max_ms: self.longest.update.as_secs_f64() * 1e3,
            cap_flips: self.longest.flips,
            cap_flip_max_ms: self.longest.flip.as_secs_f64() * 1e3,
            scene_used_peak_mib: self.scene_used_peak_bytes as f64 / (1024.0 * 1024.0),
            backlog_peak: self.longest.backlog_peak,
            absorb_updates: self.longest.absorb_updates,
            absorb_ms: self.longest.absorb.as_secs_f64() * 1e3,
            end_look_ms: now
                .hole_scan
                .saturating_sub(frames_end.hole_scan)
                .as_secs_f64()
                * 1e3,
            settle,
            still_cuts,
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

// The paths that are flown at speed, whose scene is drawn at the level the speed asks for.
pub const FLIGHTS: [&str; 4] = [
    "flight at 2 m",
    "flight at 50 m",
    "vertical take-off",
    "180 degree turn",
];

/// Whether the ground straight below `camera` is drawn coarser than `bands` ask for it: the brick
/// that one ray down hits is of a level whose band has not begun at the brick's distance (less
/// the hysteresis, which is how far in a brick is kept whole). This is a soft miss of what the
/// frame shows, the speed-scaled level and the camera's own place in it: the scene thread counts
/// against the scene's camera, which is behind the frame's when the scene lags.
///
/// The alternative, re-scoring every brick at the frame's camera, is the same count for what is
/// under the player and a pass over every node a frame; a ray is the player's view of what they
/// stand on, which is what the ticket's "no soft miss" is for.
fn soft_miss_under(scene: &Scene, camera: &Camera, bands: Option<render::lod::Bands>) -> bool {
    let Some(bands) = bands else {
        return false;
    };
    let (cell, origin) = camera::split(camera.position);
    let dir = camera.horizon_up.map(|c| -(c as f32));
    let Some(hit) = trace(scene, &Ray { cell, origin, dir }) else {
        return false;
    };
    let Some(level) = scene.level_at(hit.voxel) else {
        return false;
    };
    let size = 8i64 << level;
    let distance = (0..3)
        .map(|a| {
            let low = i64::from(hit.voxel[a]).div_euclid(size) * size;
            let (c, low, high) = (camera.position[a], low as f64, (low + size) as f64);
            (low - c).max(c - high).max(0.0).powi(2)
        })
        .sum::<f64>()
        .sqrt();
    level > 0 && distance < bands.floor(level) * (1.0 - render::lod::HYSTERESIS)
}

/// What the frame thread has on the world and the GPU, and the peaks over the frames.
struct Session<'a> {
    world: World,
    gpu: &'a mut dyn Gpu,
    /// What the run was doing when it last started on something: for the error of a run that stops.
    step: String,
    /// Whether [`Frame::camera_soft_miss`] is scored: on the flights, where the scene is drawn at
    /// the speed-scaled level (a ray a frame, not worth it on a fill of 19 000 frames).
    score_camera: bool,
    /// What the scene thread had done when the measuring began.
    work_before: Done,
    scene_used_peak_bytes: u64,
    /// What the run was doing when that was reached.
    scene_used_peak_in: String,
    scene_used_peak_levels: Vec<usize>,
    scene_reserved_bytes: u64,
    /// The most the frame thread's copy of the scene reserved, with the old words of a buffer
    /// that grew in that frame added (see [`Memory::scene_host_peak_growth_mib`]).
    scene_host_peak_bytes: u64,
    resident_peak: Option<u64>,
    whole_process_peak: Option<u64>,
    store_peak: usize,
    frames: u64,
    /// The frame-time limit, for counting the frames over it.
    limit_ms: f64,
    /// How long working out the camera of the next frame took (a flight reads the store).
    camera_wait: Duration,
    /// What the harness did between frames (a look), which the next frame says.
    harness_wait: Duration,
    /// When the frame before ended, its sleep done (none before the first).
    last_ended: Option<Instant>,
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
            harness_wait: Duration::ZERO,
            last_ended: None,
            step: "starting".to_string(),
            work_before,
            score_camera: false,
            scene_used_peak_bytes: 0,
            scene_used_peak_in: String::new(),
            scene_used_peak_levels: Vec::new(),
            scene_reserved_bytes: 0,
            scene_host_peak_bytes: 0,
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
        let harness_wait = std::mem::take(&mut self.harness_wait);
        let began = Instant::now();
        let before = self
            .last_ended
            .map_or(Duration::ZERO, |ended| began - ended);
        let looking = self.world.looking_for_holes();
        let edit_before = self.world.edit_taken();
        let update = if lead {
            self.world.recentre(camera.position)?
        } else {
            self.world.recentre_moving(camera.position, [0.0; 3])?
        };
        let taken_at = Instant::now();
        let edit_taken = self.world.edit_taken();
        let took_edit = edit_taken && !edit_before;
        let lengthened = self.world.take_lengthened();
        let followed = taken_at - began;
        let gpu = self
            .gpu
            .frame(camera, self.world.scene(), update.as_ref())?;
        let looking = looking || self.world.looking_for_holes();
        let time = began.elapsed();
        let lock_wait = self.look_at_memory();
        let after_end = began.elapsed();
        let after = after_end - time;
        let (kind, upload_bytes, patch_bytes) = uploaded(self.world.scene(), update.as_ref());
        self.scene_host_peak_bytes = host_growth_peak(
            self.scene_host_peak_bytes,
            self.world.reserved_bytes(),
            gpu.grown_bytes,
        );
        let over_budget = over_budget_cause(
            kind,
            upload_bytes,
            gpu.writes,
            self.world.scene().rebuild_reason(),
            took_edit,
            gpu.grown_bytes > 0,
        );
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
            gpu_primary_ms: gpu.passes.and_then(|p| p.primary_ms),
            gpu_upscale_ms: gpu.passes.and_then(|p| p.upscale_ms),
            gpu_total_ms: gpu.passes.and_then(|p| p.total_ms),
            wait_not_passes_ms: gpu
                .passes
                .and_then(|p| p.total_ms)
                .map(|t| ms(gpu.wait) - t),
            other_ms: ms(time.saturating_sub(followed + gpu.cpu() + gpu.wait)),
            after_ms: ms(after),
            after_lock_ms: ms(lock_wait),
            harness_ms: ms(harness_wait),
            before_ms: ms(before),
            between_ms: 0.0,
            sleep_asked_ms: 0.0,
            sleep_over_ms: 0.0,
            cycle_ms: 0.0,
            update: kind,
            upload_bytes,
            patch_bytes,
            lengthen_ms: ms(lengthened),
            grow_ms: ms(gpu.grow),
            grown_bytes: gpu.grown_bytes,
            over_budget,
            hole_look: looking,
        };
        let slept = Instant::now();
        let asked = FRAME.saturating_sub(slept - began);
        std::thread::sleep(asked);
        let ended = Instant::now();
        self.last_ended = Some(ended);
        let parts = SlowFrame {
            between_ms: ms((slept - began).saturating_sub(after_end)),
            sleep_asked_ms: ms(asked),
            sleep_over_ms: ms((ended - slept).saturating_sub(asked)),
            cycle_ms: ms(ended - began),
            ..parts
        };
        Ok(Frame {
            began,
            time_ms: parts.time_ms,
            cpu_ms: parts.cpu_ms,
            parts,
            soft_misses: self.world.stats().soft_misses,
            camera_soft_miss: self.score_camera
                && soft_miss_under(self.world.scene(), camera, self.world.bands()),
            release_rebuild: kind == "rebuild"
                && self.world.scene().rebuild_reason() == Some(Rebuild::Release),
            lag_over_floor: self.score_camera
                && self.world.bands().is_some_and(|bands| {
                    self.world.lag(camera.position)
                        > bands.floor(1) * (1.0 - render::lod::HYSTERESIS)
                }),
            at_camera: self.world.lag(camera.position) == 0.0,
            scene_used_bytes: self.world.used_bytes(),
            throttled: self.world.stats().throttled,
            loader_queue: self.world.stats().loader.pending,
            lag: self.world.lag(camera.position),
            edit_taken,
            taken_at,
        })
    }

    /// Looks at the memory, and returns how long the store's read lock took to get (zero in a frame
    /// that did not take it).
    fn look_at_memory(&mut self) -> Duration {
        let mut lock_wait = Duration::ZERO;
        let used = self.world.used_bytes();
        if used > self.scene_used_peak_bytes {
            self.scene_used_peak_bytes = used;
            self.scene_used_peak_in.clone_from(&self.step);
            self.scene_used_peak_levels = (0..render::lod::LEVELS)
                .map(|l| self.world.level_bricks(l))
                .collect();
        }
        self.scene_reserved_bytes = self.scene_reserved_bytes.max(self.world.reserved_bytes());
        self.frames += 1;
        if self.frames.is_multiple_of(STORE_EVERY) {
            let asked = Instant::now();
            let store = self.world.store();
            lock_wait = asked.elapsed();
            self.store_peak = self.store_peak.max(store.streamed_len());
        }
        if self.frames.is_multiple_of(RESIDENT_EVERY) {
            self.look_at_resident_memory();
        }
        lock_wait
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
        // Not a frame of a path: what it took to lengthen the copy is not any frame's.
        self.world.take_lengthened();
        Ok(())
    }

    /// Ends a path: a look at every split brick, so that no path has none, and its report.
    fn end_path(
        &mut self,
        mut capture: Capture,
        camera: &Camera,
        settle: Option<Settle>,
        still_cuts: Option<StillCutsReport>,
    ) -> Result<PathReport, Box<dyn Error>> {
        capture.frames_done(&self.world);
        self.look(camera, true)?;
        Ok(capture.finish(&self.world, settle, still_cuts))
    }

    /// Whether the resident set is complete: nothing waits, and the loader has nothing out.
    fn idle(&self) -> bool {
        self.world.waiting() == 0 && self.world.loader().is_some_and(|l| l.pending == 0)
    }

    /// Whether the resident set is complete and the drawn scene is at `camera`.
    fn at_rest_at(&self, camera: &Camera) -> bool {
        self.idle() && self.world.lag(camera.position) == 0.0
    }

    /// Frames at `camera`, not recorded, until the loader has caught up **and the scene is at the
    /// camera**, and then a look at every split brick: whatever the scene thread found between
    /// paths is in the run's holes. The camera's jump to it is not a flight: the scene thread is
    /// told it is not moving. `after` names what came before, for an error.
    ///
    /// The scene's camera is read as well as `idle`, which reads the stats of the last publish: the
    /// scene thread has not learnt that the camera is back at the Spawn until a frame has been run
    /// at it, so `idle` can be true from the stats of the path before (the 180 degree turn was
    /// left about 300 m up).
    fn settle(
        &mut self,
        camera: &Camera,
        cap: Duration,
        after: &str,
    ) -> Result<(), Box<dyn Error>> {
        self.step = format!("the loader's catch-up after {after}");
        let began = Instant::now();
        // At least one frame, at the target camera, before the stats are believed.
        loop {
            self.frame(camera, false)?;
            if self.at_rest_at(camera) {
                break;
            }
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
        // A path's first frame follows the end look of the path before (about 0.3 s at full size),
        // which is not a wait for a wake-up: its `before_ms` is 0.
        self.last_ended = None;
        let mut capture = Capture::start("cold start", &self.world, self.limit_ms);
        let mut control = None;
        let mut guard_complete = None;
        let mut edit_during_fill = None;
        let mut edit_pending = 0;
        while !self.idle() || control.is_none() {
            let frame = self.frame(&setup.spawn, true)?;
            capture.add(&frame);
            if control.is_none() && level_0_under(self.world.scene(), &setup.spawn) {
                control = Some(setup.load + began.elapsed());
            }
            if guard_complete.is_none() && self.world.guard_complete() {
                guard_complete = Some(setup.load + began.elapsed());
            }
            // One Edit, in the fill, once the loader has a queue (its workers are ahead, and the
            // scene thread takes in for a long slice): its time is not the next frame's wait.
            let pending = self.world.loader().map_or(0, |l| l.pending);
            if control.is_some() && edit_during_fill.is_none() && pending > 0 {
                if let Edit::Taken(latency) = self.edit(&setup.spawn)? {
                    edit_during_fill = Some(latency);
                    edit_pending = pending;
                }
                self.last_ended = None;
                self.step = "cold start".to_string();
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
            spawn_ring_s: setup.ring.as_secs_f64(),
            guard_s: setup.guard.as_secs_f64(),
            guard_cells: setup.guard_cells.0 as u32,
            guard_cells_at_control: setup.guard_cells.1 as u32,
            time_to_control_s: setup.load.as_secs_f64(),
            time_to_level_0_under_camera_s: control
                .expect("the loop ends with control")
                .as_secs_f64(),
            guard_complete_s: guard_complete
                .expect("the loop ends with the Guard complete")
                .as_secs_f64(),
            fill_s: (setup.load + began.elapsed()).as_secs_f64(),
            edit_during_fill,
            edit_during_fill_pending: edit_pending,
        };
        Ok((cold, self.end_path(capture, &setup.spawn, None, None)?))
    }

    /// The camera at `at(world, t)` for `seconds`, `t` the seconds since the path began.
    fn path(
        &mut self,
        name: &'static str,
        seconds: Duration,
        at: impl FnMut(&World, f64) -> Camera,
    ) -> Result<PathReport, Box<dyn Error>> {
        self.path_until(name, at, |_, t| t >= seconds.as_secs_f64())
    }

    /// The camera at `at(world, t)` until `done(world, t)` says so after a frame, `t` the seconds
    /// since the path began.
    fn path_until(
        &mut self,
        name: &'static str,
        mut at: impl FnMut(&World, f64) -> Camera,
        mut done: impl FnMut(&World, f64) -> bool,
    ) -> Result<PathReport, Box<dyn Error>> {
        self.step = name.to_string();
        // A path's first frame follows the end look of the path before (about 0.3 s at full size),
        // which is not a wait for a wake-up: its `before_ms` is 0.
        self.last_ended = None;
        let mut capture = Capture::start(name, &self.world, self.limit_ms);
        let began = Instant::now();
        self.score_camera = FLIGHTS.contains(&name);
        let camera = loop {
            let t = began.elapsed().as_secs_f64();
            let camera = at(&self.world, t);
            self.camera_wait = began.elapsed() - Duration::from_secs_f64(t);
            let frame = self.frame(&camera, true)?;
            capture.add(&frame);
            if done(&self.world, began.elapsed().as_secs_f64()) {
                break camera;
            }
        };
        self.score_camera = false;
        self.end_path(capture, &camera, None, None)
    }

    /// The sea-level crossing (#213): straight down from the Spawn to [`CROSSING_UNDER_METRES`] under
    /// sea level, held there until the scene has come to the camera (or [`CROSSING_HOLD_CAP`]),
    /// up to [`CROSSING_OVER_METRES`] over it, and held there the same way, along the Spawn's
    /// radial up at the flights' speed. The horizon cap goes off when the scene's raised camera is
    /// under the sea's sphere and comes back on 50 m over it (`select::CAP_ON`), so this path has
    /// both flips of the cap, the update that looks at every node again being the one it is for.
    /// The scene follows a camera that goes through the ground much slower than the camera moves
    /// (at full size, about a step of 0.8 m for each update of 100 to 500 ms), so a path that
    /// does not wait for it never brings the scene to the flip: the holds do.
    fn sea_level_crossing(
        &mut self,
        spawn: &Camera,
        sea: f64,
    ) -> Result<PathReport, Box<dyn Error>> {
        #[derive(Clone, Copy, PartialEq)]
        enum Leg {
            Down,
            HoldBottom,
            Up,
            HoldTop,
            Done,
        }
        let speed = FLIGHT_METRES_PER_SECOND * VOXELS_PER_METRE;
        let start = norm(spawn.position);
        let bottom = sea - CROSSING_UNDER_METRES * VOXELS_PER_METRE;
        let top = sea + CROSSING_OVER_METRES * VOXELS_PER_METRE;
        let spawn = *spawn;
        let at = |radius: f64| Camera {
            position: std::array::from_fn(|a| {
                spawn.position[a] + spawn.horizon_up[a] * (radius - start)
            }),
            ..spawn
        };
        // The leg, and the time it began.
        let state = std::cell::Cell::new((Leg::Down, 0.0));
        let done = std::cell::Cell::new(false);
        let cap = CROSSING_HOLD_CAP.as_secs_f64();
        self.path_until(
            "sea-level crossing",
            |world, t| {
                let (leg, since) = state.get();
                // Held until the scene is at the camera, and at the top until the flip is absorbed too
                // (`World::caught_up`): so the path ends with nothing left to absorb, which would
                // spill into the next path. At the bottom the flip is *not* waited for: the scene a
                // cap that is off builds when it is waited for is the wide one (544 to 557 MiB at
                // 30 m under the sea, 567 at the top, over the 512 MiB gate; see `measurements.md`),
                // and what the path is for is the flip's update.
                let held = |camera: Camera, next: Leg, absorbed: bool| {
                    let at_camera = world.lag(camera.position) < crate::world::CAUGHT_UP_LAG;
                    if (at_camera && (!absorbed || world.caught_up(camera.position)))
                        || t - since > cap
                    {
                        state.set((next, t));
                    }
                    camera
                };
                match leg {
                    Leg::Down => {
                        let radius = (start - t * speed).max(bottom);
                        if radius <= bottom {
                            state.set((Leg::HoldBottom, t));
                        }
                        at(radius)
                    }
                    Leg::HoldBottom => held(at(bottom), Leg::Up, false),
                    Leg::Up => {
                        let radius = (bottom + (t - since) * speed).min(top);
                        if radius >= top {
                            state.set((Leg::HoldTop, t));
                        }
                        at(radius)
                    }
                    Leg::HoldTop => held(at(top), Leg::Done, true),
                    Leg::Done => {
                        done.set(true);
                        at(top)
                    }
                }
            },
            |_, _| done.get(),
        )
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
        // A path's first frame follows the end look of the path before (about 0.3 s at full size),
        // which is not a wait for a wake-up: its `before_ms` is 0.
        self.last_ended = None;
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
        // The harness's own look at the stop: a wait for the scene thread to count, and a copy of
        // what that made to the GPU. It sits between two frames, and the next one says so
        // (`harness_ms`), so it is not taken for a slow frame cycle.
        self.look(&camera, false)?;
        self.harness_wait = stopped.elapsed();
        // The scene has to be at the camera too: a count made while it is still walking to where the
        // camera stopped, with the bands as they were for the flight, is a count for another place.
        let mut walked = None;
        let mut rebanded = None;
        // The view is Still from the frame the camera has stood for STILL_FRAMES frames. That is
        // only marked: nothing is held back here, the settle time is of the scene thread at work.
        let mut stillness = Stillness::default();
        let cuts_before = self.world.still_cuts();
        let settle = loop {
            let standing = stillness.observe(camera);
            self.world.mark_still(standing >= STILL_FRAMES);
            let frame = self.frame(&camera, true)?;
            capture.add(&frame);
            if frame.at_camera && walked.is_none() {
                walked = Some(stopped.elapsed().as_secs_f64());
            }
            // The last frame that still has nodes to look at for the bands (the release of the depth
            // comes half a second after the stop, so the first frames have none yet).
            if walked.is_some() && self.world.rebanding() > 0 {
                rebanded = Some(stopped.elapsed().as_secs_f64());
            }
            let seconds = stopped.elapsed().as_secs_f64();
            let settled = frame.at_camera && frame.soft_misses == 0 && self.world.waiting() == 0;
            if settled || stopped.elapsed() >= script.settle_cap {
                let walk_s = walked.unwrap_or(seconds);
                let reband_s = (rebanded.unwrap_or(walk_s) - walk_s)
                    .max(0.0)
                    .min(seconds - walk_s);
                break Settle {
                    seconds,
                    settled,
                    walk_s,
                    reband_s,
                    fill_s: seconds - walk_s - reband_s,
                };
            }
        };
        self.world.mark_still(false);
        let still_cuts = StillCutsReport::from(self.world.still_cuts().since(cuts_before));
        self.end_path(capture, &camera, Some(settle), Some(still_cuts))
    }

    /// A flight, and then the camera held where it stopped with the game's policy for a Still view
    /// in Reference mode (#217): once the camera has stood for [`STILL_FRAMES`] frames the scene
    /// catches up with the camera (loading goes on), and then loading is paused for
    /// [`Script::still_hold`], as `Main::stream` does through [`World::hold_still`]; and the scene
    /// thread holds back what it may. The updates the frame thread takes while it is held are the
    /// cuts of the Reference view, counted by their cause. Then the view changes: loading goes on,
    /// and the time to catch up (no soft misses, nothing waiting, the scene at the camera) and the
    /// holes the look finds after it are reported. The frames are not those of a path.
    fn still_after_a_flight(
        &mut self,
        course: &Course,
        script: &Script,
    ) -> Result<StillAfterFlight, Box<dyn Error>> {
        self.step = "still after a flight".to_string();
        self.last_ended = None;
        let began = Instant::now();
        let camera = loop {
            let t = began.elapsed().as_secs_f64();
            let camera = course.along(&self.world, course.ahead, FLIGHT_METRES_PER_SECOND * t, 0.0);
            self.frame(&camera, true)?;
            if began.elapsed() >= script.flight_before_stop {
                break camera;
            }
        };
        // The harness's own count at the stop, as the other stop's: not a frame of the hold.
        self.look(&camera, false)?;
        self.harness_wait = Duration::ZERO;
        let mut stillness = Stillness::default();
        // The wait for the scene to reach the camera is the world's (it ends at the cap at the
        // latest); the frames in it are Primary's, with loading going on.
        let hold_began = loop {
            let standing = stillness.observe(camera);
            let hold = self
                .world
                .hold_still(Mode::Reference, standing, camera.position);
            self.frame(&camera, true)?;
            if hold == Hold::Held {
                break self
                    .world
                    .still_hold()
                    .began()
                    .expect("a hold that has begun says how");
            }
        };
        let before = self.world.still_cuts();
        let held = Instant::now();
        while held.elapsed() < script.still_hold {
            let standing = stillness.observe(camera);
            self.world
                .hold_still(Mode::Reference, standing, camera.position);
            self.frame(&camera, true)?;
        }
        let hold_s = held.elapsed().as_secs_f64();
        let cuts = StillCutsReport::from(self.world.still_cuts().since(before));
        // The view changes (the player moves the mouse): loading goes on, and what was held back
        // is caught up.
        self.world.hold_still(Mode::Reference, 0, camera.position);
        let holes_before = self.world.stats().work.holes_found;
        let left = Instant::now();
        let catch_up = loop {
            let frame = self.frame(&camera, false)?;
            if frame.soft_misses == 0
                && self.world.waiting() == 0
                && self.world.caught_up(camera.position)
            {
                break CatchUp {
                    seconds: left.elapsed().as_secs_f64(),
                    settled: true,
                };
            }
            if left.elapsed() >= script.settle_cap {
                break CatchUp {
                    seconds: left.elapsed().as_secs_f64(),
                    settled: false,
                };
            }
        };
        self.look(&camera, true)?;
        Ok(StillAfterFlight {
            before_hold: BeforeHold {
                lag_when_still_voxels: hold_began.lag_when_still,
                lag_when_held_voxels: hold_began.lag_when_held,
                waited_s: hold_began.waited.as_secs_f64(),
                hit_cap: hold_began.capped,
            },
            hold_s,
            cuts,
            catch_up,
            holes_after_catch_up: self.world.stats().work.holes_found - holes_before,
        })
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
        let made_before = self.world.edits_made();
        let began = Instant::now();
        let update = self.world.edit(&aim, &tool)?;
        let call = began.elapsed();
        // With something aimed at, an Edit that is not made is one that the client knows the server
        // would turn down.
        if self.world.edits_made() == made_before {
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
        paths.push(self.sea_level_crossing(&spawn, setup.sea)?);
        self.settle(&spawn, cap, "the sea-level crossing")?;
        paths.push(self.stop_after_a_flight(&course, script)?);
        self.settle(&spawn, cap, "the stop after a flight")?;
        let still_after_flight = self.still_after_a_flight(&course, script)?;
        self.settle(&spawn, cap, "the still after a flight")?;
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
            scene_used_peak_in: self.scene_used_peak_in.clone(),
            scene_used_peak_level_bricks: self.scene_used_peak_levels.clone(),
            scene_reserved_mib: mib(self.scene_reserved_bytes),
            scene_gpu_peak_growth_mib: mib(self.gpu.peak_growth_bytes()),
            scene_gpu_peak_rebuild_mib: mib(self.gpu.peak_rebuild_bytes()),
            scene_host_peak_growth_mib: mib(self.scene_host_peak_bytes),
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
            still_after_flight,
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
    world.set_backed_up_slice(script.stream_slice);
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
pub(super) fn catching<T>(
    work: impl FnOnce() -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
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
            step: "building the world, making the GPU context, or ending the run".to_string(),
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
    // The atmosphere measures the camera's height from the Planet's sea level.
    gpu.set_ground_level(f64::from(if full {
        planet_gen::world_map::WORLD_RADIUS
    } else {
        planet_gen::world_map::TEST_RADIUS
    }));
    // A growth is cut to what a buffer of this GPU may hold (see `Scene::set_buffer_limit`).
    built.world.set_buffer_limit(gpu.buffer_limit_words());
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
    stream_slice: Option<Duration>,
) -> RealTimeReport {
    let mut script = if full {
        Script::full()
    } else {
        Script::short()
    };
    if let Some(slice) = stream_slice {
        script.stream_slice = slice;
    }
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

/// [`PathReport::camera_soft_miss_per_second`]: the misses by second, with an entry for every
/// second that the path's frames began in (`last` is the start of the last of them).
fn misses_by_second(miss_seconds: &[u32], last: Option<Instant>, began: Instant) -> Vec<u32> {
    let mut seconds = miss_seconds.to_vec();
    if let Some(last) = last {
        let covered = last.saturating_duration_since(began).as_secs() as usize + 1;
        if seconds.len() < covered {
            seconds.resize(covered, 0);
        }
    }
    seconds
}

#[cfg(test)]
mod tests {
    use super::super::stream::Measured;
    use super::*;

    #[test]
    fn the_misses_by_second_cover_every_second_the_path_had() {
        let began = Instant::now();
        let at = |s: f64| Some(began + Duration::from_secs_f64(s));
        // A miss in the sixth second of a path of ten: the array is ten long, not six.
        let mut recorded = vec![0; 6];
        recorded[5] = 17;
        let by_second = misses_by_second(&recorded, at(9.99), began);
        assert_eq!(by_second, [0, 0, 0, 0, 0, 17, 0, 0, 0, 0]);
        // No miss at all: a zero for each second.
        assert_eq!(misses_by_second(&[], at(2.5), began), [0, 0, 0]);
        // A path with no frames has no seconds.
        assert!(misses_by_second(&[], None, began).is_empty());
    }

    const BUDGETS: StreamingBudgets = StreamingBudgets {
        frame_time_p99_ms: 16.7,
        frame_cpu_p99_ms: 8.0,
        holes: 0,
        scene_mib: 512.0,
        camera_soft_miss_frames: 0,
        scene_lag_d0_max: 8.0,
        spawn_ring_s: 2.0,
        time_to_control_s: 2.0,
        settle_s: 30.0,
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
            frame_cycle: Spread::default(),
            sleep_overshoot: Spread::default(),
            frames_over_limit: 0,
            frames_over_limit_by_update: UpdateKinds::default(),
            slowest: vec![slow_frame(0, frame_p99 * 2.0)],
            slowest_cycles: Vec::new(),
            over_limit_at: Vec::new(),
            largest_updates: Vec::new(),
            scene_lag_voxels_max: 0.0,
            scene_lag_d0_max: 0.0,
            scene_lag_voxels_p99: 0.0,
            scene_lag_d0_p99: 0.0,
            scene_lag_d0_mean: 0.0,
            scene_used_peak_mib: 0.0,
            scene_follow_steps: 0,
            scene_budget_cuts: 0,
            scene_release_rebuilds: 0,
            release_frame_ms_max: 0.0,
            release_update_ms_max: 0.0,
            camera_soft_miss_per_second: Vec::new(),
            camera_soft_miss_lag_side: 0,
            camera_soft_miss_loader_side: 0,
            lag_over_floor_frames: 0,
            scene_used_end_mib: 0.0,
            scene_level_bricks_end: Vec::new(),
            upload: PathUpload::default(),
            soft_misses: Series::default(),
            soft_misses_per_frame: 0.0,
            camera_soft_miss_frames: 0,
            generated_bricks: 0,
            arrived_bricks: 0,
            loader_queue_max: 0,
            loader_queue_mean: 0.0,
            loader_generating_s: 0.0,
            loader_held_back_s: 0.0,
            stream_calls: 0,
            stream_slices_used: 0,
            hole_looks: 2,
            holes,
            holes_per_frame: 0.0,
            scene_thread: SceneThread::default(),
            scene_round_max_ms: 0.0,
            scene_update_max_ms: 0.0,
            cap_flips: 0,
            cap_flip_max_ms: 0.0,
            backlog_peak: 0,
            absorb_updates: 0,
            absorb_ms: 0.0,
            end_look_ms: 0.0,
            settle: None,
            still_cuts: None,
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
            gpu_primary_ms: Some(1.0),
            gpu_upscale_ms: Some(0.5),
            gpu_total_ms: Some(1.6),
            wait_not_passes_ms: Some(time_ms - 1.8 - 1.6),
            other_ms: 0.0,
            after_ms: 0.0,
            after_lock_ms: 0.0,
            harness_ms: 0.0,
            between_ms: 0.0,
            before_ms: 0.1,
            sleep_asked_ms: 0.0,
            sleep_over_ms: 0.0,
            cycle_ms: time_ms,
            update: "patch",
            upload_bytes: 4096,
            patch_bytes: 4000,
            lengthen_ms: 0.0,
            grow_ms: 0.0,
            grown_bytes: 0,
            over_budget: None,
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
                load_s: 6.0,
                spawn_ring_s: 1.0,
                guard_s: 0.2,
                guard_cells: 600,
                guard_cells_at_control: 30,
                time_to_control_s: 1.5,
                time_to_level_0_under_camera_s: 61.0,
                guard_complete_s: 20.0,
                fill_s: 200.0,
                edit_during_fill: None,
                edit_during_fill_pending: 0,
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
                scene_used_peak_in: String::new(),
                scene_used_peak_level_bricks: Vec::new(),
                scene_reserved_mib: 560.0,
                scene_gpu_peak_growth_mib: 600.0,
                scene_gpu_peak_rebuild_mib: 0.0,
                scene_host_peak_growth_mib: 600.0,
                store_peak_bricks: 20_000_000,
                store_limit_bricks: 23_000_000,
            },
            sizing: Sizing::check(measured(), &Constants::of_the_client()),
            still_after_flight: StillAfterFlight {
                before_hold: BeforeHold {
                    lag_when_still_voxels: 0.0,
                    lag_when_held_voxels: 0.0,
                    waited_s: 0.0,
                    hit_cap: false,
                },
                hold_s: 3.0,
                cuts: StillCutsReport::default(),
                catch_up: CatchUp {
                    seconds: 0.5,
                    settled: true,
                },
                holes_after_catch_up: 0,
            },
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
    }

    /// A path of `name` with a soft miss in `frames` of its frames: the scene thread's count, and
    /// the camera's.
    fn soft_misses_in(run: &mut RunReport, name: &str, frames: usize) {
        let path = run.paths.iter_mut().find(|p| p.name == name).unwrap();
        path.soft_misses = Series {
            frames: 100,
            total: frames as u64 * 10,
            max: 10,
            frames_with: frames,
        };
    }

    fn camera_soft_misses_in(run: &mut RunReport, name: &str, frames: usize) {
        let path = run.paths.iter_mut().find(|p| p.name == name).unwrap();
        path.camera_soft_miss_frames = frames;
    }

    #[test]
    fn the_soft_miss_gate_fires_alone_for_one_frame_with_one_under_the_camera_on_a_flight() {
        let one = run_with(|r| camera_soft_misses_in(r, "flight at 2 m", 1));
        assert_eq!(over(std::slice::from_ref(&one)), ["soft_misses"]);
        // Frames are summed over the flights, and the worst run is the one that counts.
        let report = RealTimeReport::new("test".into(), 64.0, vec![one], &BUDGETS);
        assert_eq!(report.gates.camera_soft_miss_frames.worst, 1.0);
        assert_eq!(exit_code(&report, false), 2);
        // The cold start and the stop are not flights: they fill and refine, and have soft misses.
        let cold = run_with(|r| camera_soft_misses_in(r, "cold start", 9000));
        assert!(over(&[cold]).is_empty());
    }

    #[test]
    fn the_scene_threads_own_soft_misses_are_reported_and_not_gated() {
        let run = run_with(|r| soft_misses_in(r, "flight at 2 m", 482));
        assert!(over(std::slice::from_ref(&run)).is_empty());
        let report = RealTimeReport::new("test".into(), 64.0, vec![run], &BUDGETS);
        assert_eq!(report.gates.scene_soft_miss_frames, 482);
        assert_eq!(report.gates.camera_soft_miss_frames.worst, 0.0);
        assert_eq!(exit_code(&report, false), 0);
    }

    #[test]
    fn the_scene_lag_gate_fires_alone_for_a_flight_that_lags_over_its_limit() {
        let lag = |name: &str, d0: f64| {
            run_with(|r| {
                let path = r.paths.iter_mut().find(|p| p.name == name).unwrap();
                path.scene_lag_d0_max = d0;
            })
        };
        assert_eq!(over(&[lag("flight at 2 m", 8.01)]), ["scene_lag"]);
        assert!(over(&[lag("flight at 2 m", 8.0)]).is_empty());
        // Not a flight: the cold start has no camera to lag.
        assert!(over(&[lag("cold start", 50.0)]).is_empty());
        let report = RealTimeReport::new(
            "test".into(),
            64.0,
            vec![lag("flight at 2 m", 9.2)],
            &BUDGETS,
        );
        assert_eq!(report.gates.scene_lag_d0_max.worst, 9.2);
        assert_eq!(exit_code(&report, false), 2);
    }

    #[test]
    fn the_spawn_ring_gate_fires_alone_for_a_ring_over_its_limit() {
        let slow = run_with(|r| r.cold_start.spawn_ring_s = 2.01);
        assert_eq!(over(std::slice::from_ref(&slow)), ["spawn_ring"]);
        assert!(over(&[run_with(|r| r.cold_start.spawn_ring_s = 2.0)]).is_empty());
        // The Guard's visible part after the ring is in the time to control, not in this.
        assert!(over(&[run_with(|r| r.cold_start.guard_s = 1.0)]).is_empty());
    }

    #[test]
    fn the_time_to_control_gate_fires_alone_for_a_load_to_the_window_over_its_limit() {
        let slow = run_with(|r| r.cold_start.time_to_control_s = 2.01);
        assert_eq!(over(std::slice::from_ref(&slow)), ["time_to_control"]);
        assert!(over(&[run_with(|r| r.cold_start.time_to_control_s = 2.0)]).is_empty());
        // The rest of the cold start is reported, not gated.
        assert!(
            over(&[run_with(|r| {
                r.cold_start.load_s = 600.0;
                r.cold_start.time_to_level_0_under_camera_s = 600.0;
            })])
            .is_empty()
        );
    }

    #[test]
    fn the_settle_gate_fires_alone_for_a_stop_that_takes_longer_than_its_limit_or_never_settles() {
        let settle = |seconds: f64, settled: bool| {
            run_with(|r| {
                let flight = r
                    .paths
                    .iter_mut()
                    .find(|p| p.name == "flight at 2 m")
                    .unwrap();
                flight.settle = Some(Settle {
                    seconds,
                    settled,
                    walk_s: seconds / 3.0,
                    reband_s: seconds / 3.0,
                    fill_s: seconds / 3.0,
                });
            })
        };
        assert_eq!(over(&[settle(30.1, true)]), ["settle"]);
        assert!(over(&[settle(30.0, true)]).is_empty());
        // At the cap: over whatever the limit is.
        assert_eq!(over(&[settle(20.0, false)]), ["settle"]);
        // Reported, and not gated, for any other path: those have no settle.
        let report = RealTimeReport::new("test".into(), 64.0, vec![settle(31.0, true)], &BUDGETS);
        assert_eq!(report.gates.settle_s.worst, 31.0);
        assert_eq!(exit_code(&report, false), 2);
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
                "sizing",
                "still_after_flight"
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
                "scene_gpu_peak_growth_mib",
                "scene_gpu_peak_rebuild_mib",
                "scene_host_peak_growth_mib",
                "scene_reserved_mib",
                "scene_used_peak_in",
                "scene_used_peak_level_bricks",
                "scene_used_peak_mib",
                "store_limit_bricks",
                "store_peak_bricks",
                "whole_process_peak_mib"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]),
            [
                "absorb_ms",
                "absorb_updates",
                "arrived_bricks",
                "backlog_peak",
                "camera_soft_miss_frames",
                "camera_soft_miss_lag_side",
                "camera_soft_miss_loader_side",
                "camera_soft_miss_per_second",
                "cap_flip_max_ms",
                "cap_flips",
                "end_look_ms",
                "frame_cpu",
                "frame_cycle",
                "frame_time",
                "frames",
                "frames_over_limit",
                "frames_over_limit_by_update",
                "generated_bricks",
                "hole_looks",
                "holes",
                "holes_per_frame",
                "lag_over_floor_frames",
                "largest_updates",
                "loader_generating_s",
                "loader_held_back_s",
                "loader_queue_max",
                "loader_queue_mean",
                "name",
                "over_limit_at",
                "release_frame_ms_max",
                "release_update_ms_max",
                "scene_budget_cuts",
                "scene_follow_steps",
                "scene_lag_d0_max",
                "scene_lag_d0_mean",
                "scene_lag_d0_p99",
                "scene_lag_voxels_max",
                "scene_lag_voxels_p99",
                "scene_level_bricks_end",
                "scene_release_rebuilds",
                "scene_round_max_ms",
                "scene_thread",
                "scene_update_max_ms",
                "scene_used_end_mib",
                "scene_used_peak_mib",
                "seconds",
                "settle",
                "sleep_overshoot",
                "slowest",
                "slowest_cycles",
                "soft_misses",
                "soft_misses_per_frame",
                "still_cuts",
                "stream_calls",
                "stream_slices_used",
                "upload"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]["upload"]),
            [
                "bytes_max",
                "bytes_p99",
                "cut_frames",
                "grow_ms_max",
                "grown_bytes",
                "growth_frames",
                "lengthen_ms_max",
                "longest_backlog_frames",
                "longest_backlog_ms",
                "over_budget",
                "time",
                "updates",
                "writes_max",
                "writes_p99"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]["slowest"][0]),
            [
                "after_lock_ms",
                "after_ms",
                "at_ms",
                "before_ms",
                "between_ms",
                "camera_ms",
                "cpu_ms",
                "cycle_ms",
                "encode_ms",
                "gpu_primary_ms",
                "gpu_total_ms",
                "gpu_upscale_ms",
                "grow_ms",
                "grown_bytes",
                "harness_ms",
                "hole_look",
                "index",
                "lengthen_ms",
                "other_ms",
                "over_budget",
                "patch_bytes",
                "recentre_ms",
                "sleep_asked_ms",
                "sleep_over_ms",
                "submit_ms",
                "time_ms",
                "update",
                "update_flush_ms",
                "update_ms",
                "update_write_ms",
                "upload_bytes",
                "wait_ms",
                "wait_not_passes_ms",
                "writes"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]["frames_over_limit_by_update"]),
            ["none", "patch", "rebuild"]
        );
        assert_eq!(
            keys(&json["runs"][0]["paths"][0]["upload"]["over_budget"]),
            ["bands", "edit", "frames", "growth", "jump", "operation"]
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
                "reband_ms",
                "release_ms",
                "upload_ms"
            ]
        );
        assert_eq!(
            keys(&json["runs"][0]["cold_start"]),
            [
                "edit_during_fill",
                "edit_during_fill_pending",
                "fill_s",
                "guard_cells",
                "guard_cells_at_control",
                "guard_complete_s",
                "guard_s",
                "load_s",
                "spawn_ring_s",
                "time_to_control_s",
                "time_to_level_0_under_camera_s",
                "world_map_s"
            ]
        );
        assert_eq!(
            keys(&json["gates"]),
            [
                "camera_soft_miss_frames",
                "frame_cpu_p99_ms",
                "frame_time_p99_ms",
                "hole_looks_fewest_in_a_path",
                "hole_looks_fewest_in_a_run",
                "holes",
                "over",
                "scene_lag_d0_max",
                "scene_mib",
                "scene_soft_miss_frames",
                "settle_s",
                "spawn_ring_s",
                "store_bricks",
                "time_to_control_s"
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
    fn the_list_of_largest_updates_keeps_the_five_with_the_most_bytes_largest_first() {
        let mut largest = Worst::by(|f| f.upload_bytes as f64);
        for (index, bytes) in [10u64, 900, 30, 7, 500, 20, 1200, 40]
            .into_iter()
            .enumerate()
        {
            largest.offer(SlowFrame {
                upload_bytes: bytes,
                ..slow_frame(index, 1.0)
            });
        }
        let kept: Vec<(usize, u64)> = largest
            .sorted()
            .iter()
            .map(|f| (f.index, f.upload_bytes))
            .collect();
        assert_eq!(kept, [(6, 1200), (1, 900), (4, 500), (7, 40), (2, 30)]);
    }

    #[test]
    fn the_host_peak_of_a_growth_is_set_by_a_frame_that_grew_and_is_0_without_one() {
        // No growth: the reserve is not a peak, whatever it is.
        assert_eq!(host_growth_peak(0, 900, 0), 0);
        assert_eq!(host_growth_peak(500, 900, 0), 500);
        // A growth: the new reserve plus the old words copied, and it only rises.
        assert_eq!(host_growth_peak(0, 900, 100), 1000);
        assert_eq!(host_growth_peak(1200, 900, 100), 1200);
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
            passes: None,
            grow: ms(50),
            grown_bytes: 1 << 20,
        };
        // The growth is inside the write: it is not another part of the CPU's time.
        assert_eq!(frame.cpu(), ms(15));
        assert_eq!(GpuFrame::default().cpu(), Duration::ZERO);
    }

    #[test]
    fn a_frame_is_over_the_budget_only_by_its_bytes_or_copies_and_under_one_cause() {
        let (bytes, writes) = (UPLOAD_BUDGET.bytes, UPLOAD_BUDGET.writes);
        let cause = |update, bytes, writes, why, edit, grown| {
            over_budget_cause(update, bytes, writes, why, edit, grown)
        };
        // Under the budget, whatever the update is: not counted (a small rebuild too).
        assert_eq!(cause("patch", bytes, writes, None, true, true), None);
        assert_eq!(
            cause("rebuild", bytes - 1, 0, Some(Rebuild::Jump), false, false),
            None
        );
        assert_eq!(cause("none", 0, 0, None, false, false), None);
        // Over by bytes or by copies, the first cause that applies.
        let over = (bytes + 1, writes);
        assert_eq!(
            cause("rebuild", over.0, 0, Some(Rebuild::Jump), true, true),
            Some("jump")
        );
        assert_eq!(
            cause("rebuild", over.0, 0, Some(Rebuild::Bands), true, true),
            Some("bands")
        );
        assert_eq!(
            cause("patch", over.0, over.1, None, true, true),
            Some("edit")
        );
        assert_eq!(
            cause("patch", over.0, over.1, None, false, true),
            Some("growth")
        );
        assert_eq!(
            cause("patch", 1, writes + 1, None, false, false),
            Some("operation")
        );
        // The causes are exclusive and add up to the frames.
        let mut counted = OverBudget::default();
        for c in [
            Some("jump"),
            None,
            Some("edit"),
            Some("operation"),
            Some("growth"),
            Some("bands"),
        ] {
            counted.add(c);
        }
        assert_eq!(counted.frames, 5);
        assert_eq!(
            counted.operation + counted.edit + counted.jump + counted.bands + counted.growth,
            counted.frames
        );
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
        assert_eq!(session.world.edits_made(), 0);
    }

    /// A dig that the client knows the server would turn down is `refused`: the allowance is spent
    /// by digging in the world itself, which has no scene thread (so no threads and no waits).
    #[test]
    fn a_dig_the_server_would_turn_down_is_no_edit_and_is_refused() {
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
        let aim = within_reach(&session.world, &spawn).expect("the Spawn looks at the ground");
        let tool = Tool::default();
        let mut made = 0;
        loop {
            let before = session.world.edits_made();
            session.world.edit(&aim, &tool).expect("the link is up");
            if session.world.edits_made() == before {
                break;
            }
            made += 1;
            assert!(made < 100_000, "the allowance never ran out");
        }
        assert!(made > 0, "the first Edit is within the allowance");
        match session.edit(&spawn).expect("a refused dig is no error") {
            Edit::NotMade(why) => assert_eq!(why, "refused"),
            Edit::Taken(_) => panic!("the allowance is spent"),
        }
        assert_eq!(session.world.edits_made(), made);
    }

    #[test]
    fn a_frame_with_no_gpu_pass_times_reports_them_null_and_never_zero() {
        // A device with no timers, or a frame whose own times did not come in.
        let mut frame = slow_frame(0, 30.0);
        frame.gpu_primary_ms = None;
        frame.gpu_upscale_ms = None;
        frame.gpu_total_ms = None;
        frame.wait_not_passes_ms = None;
        let json = serde_json::to_value(frame).unwrap();
        for key in [
            "gpu_primary_ms",
            "gpu_upscale_ms",
            "gpu_total_ms",
            "wait_not_passes_ms",
        ] {
            assert!(json[key].is_null(), "{key}: {}", json[key]);
        }
        // And in a run of them, the envelope has them null too.
        let none = || {
            run_with(|r| {
                r.paths[1].slowest = vec![frame];
            })
        };
        let report = RealTimeReport::new("test".into(), 64.0, vec![none(), none()], &BUDGETS);
        let json = serde_json::to_value(&report).unwrap();
        assert!(json["max"]["paths"][1]["slowest"][0]["gpu_total_ms"].is_null());
        // The GPU frame of a run with no GPU has none either.
        assert_eq!(GpuFrame::default().passes, None);
    }

    #[test]
    fn the_frames_over_the_limit_are_counted_by_what_their_update_was() {
        let mut kinds = UpdateKinds::default();
        for kind in ["none", "patch", "none", "rebuild", "none", "patch"] {
            kinds.add(kind);
        }
        assert_eq!(
            kinds,
            UpdateKinds {
                none: 3,
                patch: 2,
                rebuild: 1
            }
        );
    }

    #[test]
    fn the_places_of_the_frames_over_the_limit_are_in_each_run_and_not_in_the_min_and_max() {
        use serde_json::json;
        // Two runs with lists of the same length, which the envelope used to take place by place
        // (the least and most of the i-th frame's index, which means nothing), and one more.
        let report = RealTimeReport::new(
            "test".into(),
            64.0,
            vec![
                run_with(|r| r.paths[1].over_limit_at = vec![10, 50, 90]),
                run_with(|r| r.paths[1].over_limit_at = vec![300, 310, 700]),
            ],
            &BUDGETS,
        );
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["runs"][0]["paths"][1]["over_limit_at"],
            json!([10, 50, 90])
        );
        assert_eq!(
            json["runs"][1]["paths"][1]["over_limit_at"],
            json!([300, 310, 700])
        );
        for edge in ["min", "max"] {
            let path = json[edge]["paths"][1].as_object().unwrap();
            assert!(
                !path.contains_key("over_limit_at"),
                "{edge} has the places of one run's frames"
            );
            // The rest of the path is still in it.
            assert!(path.contains_key("largest_updates") && path.contains_key("upload"));
        }
    }

    #[test]
    fn the_lag_of_a_path_is_its_largest_and_its_99th_percentile_by_nearest_rank() {
        assert_eq!(lag_spread(&[]), (0.0, 0.0));
        assert_eq!(lag_spread(&[7.0]), (7.0, 7.0));
        // 200 frames: the 99th is the 198th smallest, and the largest is not it.
        let lags: Vec<f64> = (0..200).rev().map(f64::from).collect();
        assert_eq!(lag_spread(&lags), (199.0, 197.0));
    }

    #[test]
    fn the_scene_lag_of_a_path_is_in_the_report_and_its_min_and_max_are_the_runs() {
        let report = RealTimeReport::new(
            "test".into(),
            64.0,
            vec![
                run_with(|r| {
                    r.paths[1].scene_lag_voxels_max = 640.0;
                    r.paths[1].scene_lag_d0_max = 10.0;
                    r.paths[1].scene_lag_voxels_p99 = 600.0;
                }),
                run_with(|r| {
                    r.paths[1].scene_lag_voxels_max = 6400.0;
                    r.paths[1].scene_lag_d0_max = 100.0;
                    r.paths[1].scene_lag_voxels_p99 = 6000.0;
                }),
            ],
            &BUDGETS,
        );
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["min"]["paths"][1]["scene_lag_voxels_max"], 640.0);
        assert_eq!(json["max"]["paths"][1]["scene_lag_voxels_max"], 6400.0);
        assert_eq!(json["max"]["paths"][1]["scene_lag_d0_max"], 100.0);
        assert_eq!(json["min"]["paths"][1]["scene_lag_voxels_p99"], 600.0);
        assert_eq!(json["max"]["paths"][1]["scene_lag_voxels_p99"], 6000.0);
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

    /// The catch-up between paths ends with the scene at the camera: the scene thread has not
    /// learnt that the camera is back until a frame has run at it, so stats from the path before
    /// say "idle" while the scene is still where that path left it (the 180 degree turn was left
    /// about 300 m up).
    #[test]
    fn the_catch_up_between_paths_ends_with_the_scene_at_the_camera() {
        let built = build_test().expect("the test Planet builds");
        let Built { world, setup, .. } = built;
        let mut gpu = NoGpu;
        let mut session = Session::new(world, &mut gpu, BUDGETS.frame_time_p99_ms);
        let spawn = setup.spawn;
        let cap = Duration::from_secs(120);
        session.settle(&spawn, cap, "the start").expect("settles");
        // The camera goes up and the scene follows it there and goes idle.
        let mut away = spawn;
        for a in 0..3 {
            away.position[a] += spawn.horizon_up[a] * 300.0;
        }
        let began = Instant::now();
        while session.world.lag(away.position) != 0.0 || !session.idle() {
            session.frame(&away, false).expect("a frame");
            assert!(began.elapsed() < cap, "the scene did not follow the camera");
        }
        assert!(session.world.lag(spawn.position) > 0.0);
        // The catch-up at the Spawn does not return on the stats of the place it came from.
        session.settle(&spawn, cap, "the flight").expect("settles");
        assert_eq!(session.world.lag(spawn.position), 0.0);
        assert!(session.idle());
    }

    /// The real-time mode on the test Planet, with real workers and a real scene thread and no GPU:
    /// every path runs and the report has its fields. It asserts no time bound, only what was run and
    /// that the parts of a frame's cycle are inside it: how long anything took is not for a test
    /// to hold. It is in `.github/real-thread-tests.txt`, and does
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
                "sea-level crossing",
                "stop after a flight"
            ]
        );
        // The view held Still after a flight (#217): the hold was run and the catch-up looked at.
        // Nothing is asserted of how many cuts there were: that is what the report is for.
        let still = &run.still_after_flight;
        assert!(still.hold_s >= Script::short().still_hold.as_secs_f64());
        assert!(still.cuts.still_frames > 0, "{still:?}");
        assert!(still.catch_up.settled, "{still:?}");
        assert_eq!(still.holes_after_catch_up, 0, "{still:?}");
        // The hold began with the scene at the camera (the test Planet's scene keeps up), and the
        // wait for it is reported.
        assert!(!still.before_hold.hit_cap, "{still:?}");
        assert!(
            still.before_hold.lag_when_held_voxels < crate::world::CAUGHT_UP_LAG,
            "{still:?}"
        );
        assert!(still.before_hold.waited_s < crate::world::CATCH_UP_CAP.as_secs_f64());
        // The crossing took the horizon cap off and on again (#213).
        let crossing = run.paths.iter().find(|p| p.name == "sea-level crossing");
        assert!(crossing.is_some_and(|p| p.cap_flips >= 2), "{crossing:?}");
        // The flips were absorbed: the updates that did it are counted in the crossing or the
        // path after it.
        assert!(
            run.paths
                .iter()
                .any(|p| p.absorb_updates > 0 && p.absorb_ms > 0.0),
            "no path absorbed a flip"
        );
        // The scene thread's longest round (#213) was timed: in the run, and in the crossing,
        // where the thread works the whole time.
        assert!(
            run.paths.iter().any(|p| p.scene_round_max_ms > 0.0),
            "no path timed a round of the scene thread"
        );
        assert!(
            crossing.is_some_and(|p| p.scene_round_max_ms > 0.0),
            "the crossing timed no round: {crossing:?}"
        );
        let stop = run.paths.last().unwrap();
        assert!(stop.still_cuts.is_some());
        assert!(run.paths[..6].iter().all(|p| p.still_cuts.is_none()));
        for path in &run.paths {
            assert!(path.frames > 0, "{}", path.name);
            assert_eq!(path.frame_time.samples, path.frames);
            assert_eq!(path.frame_cpu.samples, path.frames);
            // (That the scene thread's longest round was timed is asserted below for the run and
            // for the crossing, not for each path: a thread on a runner that is slow to schedule
            // may have no round end in the frames of a path, as the CI's Ubuntu runner had none in
            // the turn and its Windows runner none in the 50 m flight.)
            // Not that the round is at least as long as the update: the counters are taken
            // between an update and the end of its round, so the longest update of the round that
            // straddles the end of a path is in this path and the round's time in the next.
            eprintln!(
                "{}: round {:.2} ms, update {:.2} ms, {} cap flips",
                path.name, path.scene_round_max_ms, path.scene_update_max_ms, path.cap_flips
            );
            // The cycles are from one frame's start to the next's (the last has none), and the
            // frames are paced to 16.667 ms: a cycle is never shorter, and the sleeps that ran
            // over by more than a frame would show in the worst figure.
            assert_eq!(path.frame_cycle.samples, path.frames - 1, "{}", path.name);
            assert!(path.frame_cycle.mean_ms >= 16.6, "{}", path.name);
            assert!(path.sleep_overshoot.samples <= path.frames);
            assert!(path.sleep_overshoot.worst_ms >= 0.0);
            for frame in path.slowest.iter().chain(&path.slowest_cycles) {
                // A frame's cycle is its parts, all of them: the time, the look at the memory
                // after it, the harness's work between that and the sleep, the sleep it asked for
                // and the sleep's overshoot (they are taken at the same instants, so they add up
                // to the microsecond; 0.05 ms is for the rounding of the f64 milliseconds). A part
                // that went missing from the report would show as a cycle over their sum.
                let parts = frame.time_ms
                    + frame.after_ms
                    + frame.between_ms
                    + frame.sleep_asked_ms
                    + frame.sleep_over_ms;
                assert!(
                    (frame.cycle_ms - parts).abs() < 0.05,
                    "{}: the parts of the cycle do not add up to it: {frame:?}",
                    path.name
                );
            }
            // The longest turns are kept, longest first, and the parts of a turn are inside it: the
            // lock is inside the look at the memory, the harness's look inside `before_ms`.
            assert!(!path.slowest_cycles.is_empty(), "{}", path.name);
            let turn = |f: &SlowFrame| f.before_ms + f.cycle_ms;
            assert!(
                path.slowest_cycles
                    .windows(2)
                    .all(|pair| turn(&pair[0]) >= turn(&pair[1])),
                "{}",
                path.name
            );
            for frame in path.slowest.iter().chain(&path.slowest_cycles) {
                assert!(frame.after_lock_ms <= frame.after_ms + 0.05, "{frame:?}");
                assert!(frame.harness_ms <= frame.before_ms + 0.05, "{frame:?}");
                // Only the stop of a flight has a look between its frames.
                if path.name != "stop after a flight" {
                    assert_eq!(frame.harness_ms, 0.0, "{}", path.name);
                }
            }
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

    /// One run of the paths at **full size with no GPU**: the real loader and scene thread on the
    /// machine's clock, the frame thread's work without the copy and the draw. What the scene
    /// thread does (soft misses, the drawn lag, the settle time, the time to level 0) is what a
    /// run with a GPU has to within the machine's speed; the frame times are not (#168 chose the
    /// speed-scaled curve from it, on a box with no GPU). `BENCH_RUNS` runs (default 1).
    /// `cargo test --release -p client --bin client -- --ignored --nocapture a_full_size_run_with_no_gpu`
    #[test]
    #[ignore = "generates the full-size world map and takes about 15 minutes a run"]
    fn a_full_size_run_with_no_gpu_reports_what_the_scene_thread_did() {
        let runs: u32 = std::env::var("BENCH_RUNS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        // `STREAM_SLICE_MS`: another slice for taking in while the loader is backed up.
        let mut script = Script::full();
        if let Some(ms) = std::env::var("STREAM_SLICE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            script.stream_slice = Duration::from_millis(ms);
        }
        for run in 1..=runs {
            let built = build_full().expect("the full-size Planet builds");
            let report = one_run(built, &mut NoGpu, &script, BUDGETS.frame_time_p99_ms)
                .unwrap_or_else(|stopped| {
                    panic!("the run stopped at {}: {}", stopped.step, stopped.error)
                });
            eprintln!(
                "RUN {run}: stream slice {:?}, load {:.1} s, level 0 under the camera {:.1} s, fill {:.0} s, holes {}, edit {:?}, edit in the fill {:?} with {} asked for, scene thread in the release rebuild {:.0} ms",
                script.stream_slice,
                report.cold_start.load_s,
                report.cold_start.time_to_level_0_under_camera_s,
                report.cold_start.fill_s,
                report.holes,
                report.edit_latency,
                report.cold_start.edit_during_fill,
                report.cold_start.edit_during_fill_pending,
                report.scene_thread.release_ms,
            );
            for path in &report.paths {
                eprintln!(
                    "  {:>20}: {:>5} frames, soft misses/frame {:>9.0} (frames with {:>5}, max {:>7}; under the camera {:>4}: lag side {:>4}, loader side {:>4}; frames over the floor {:>4}), lag max {:>5.2} d0 mean {:>5.2}, scene used {:>6.1} MiB, \
                     cpu p99 {:>5.2} ms, scene follow {:>7.0} ms pack {:>7.0} ms reband {:>7.0} ms, generated {:>9} arrived {:>9} \
                     loader queue max {:>8} mean {:>9.0}, loader generating {:>6.1} s held back {:>6.1} s, \
                     stream calls {:>6} slices used up {:>6}, settle (s, walk, reband, fill, settled) {:?}",
                    path.name,
                    path.frames,
                    path.soft_misses_per_frame,
                    path.soft_misses.frames_with,
                    path.soft_misses.max,
                    path.camera_soft_miss_frames,
                    path.camera_soft_miss_lag_side,
                    path.camera_soft_miss_loader_side,
                    path.lag_over_floor_frames,
                    path.scene_lag_d0_max,
                    path.scene_lag_d0_mean,
                    path.scene_used_peak_mib,
                    path.frame_cpu.p99_ms,
                    path.scene_thread.follow_ms,
                    path.scene_thread.pack_ms,
                    path.scene_thread.reband_ms,
                    path.generated_bricks,
                    path.arrived_bricks,
                    path.loader_queue_max,
                    path.loader_queue_mean,
                    path.loader_generating_s,
                    path.loader_held_back_s,
                    path.stream_calls,
                    path.stream_slices_used,
                    path.settle
                        .map(|s| (s.seconds, s.walk_s, s.reband_s, s.fill_s, s.settled)),
                );
            }
            for path in &report.paths {
                eprintln!(
                    "  {:>20}: scene used peak {:.1} MiB, at the end {:.1} MiB, bricks at each level at the end {:?}",
                    path.name,
                    path.scene_used_peak_mib,
                    path.scene_used_end_mib,
                    path.scene_level_bricks_end,
                );
                if path.camera_soft_miss_frames > 0 {
                    eprintln!(
                        "  {:>20}: camera misses by second {:?}, follow rounds {}, budget cuts {}",
                        path.name,
                        path.camera_soft_miss_per_second,
                        path.scene_follow_steps,
                        path.scene_budget_cuts,
                    );
                }
                if path.scene_release_rebuilds > 0 {
                    eprintln!(
                        "  {:>20}: {} release rebuilds, scene thread {:.0} ms, longest frame {:.1} ms with an update of {:.1} ms",
                        path.name,
                        path.scene_release_rebuilds,
                        path.scene_thread.release_ms,
                        path.release_frame_ms_max,
                        path.release_update_ms_max,
                    );
                }
            }
            {
                let still = &report.still_after_flight;
                eprintln!(
                    "  still after a flight: before the hold {:?}, held {:.1} s, cuts {:?}, catch up {:?}, holes {}",
                    still.before_hold,
                    still.hold_s,
                    still.cuts,
                    still.catch_up,
                    still.holes_after_catch_up
                );
            }
            eprintln!(
                "  scene used peak {:.1} MiB in {:?}, bricks at each level then {:?}",
                report.memory.scene_used_peak_mib,
                report.memory.scene_used_peak_in,
                report.memory.scene_used_peak_level_bricks
            );
        }
    }

    /// The sea-level crossing at full size, without a GPU (#213): the scene thread's longest round
    /// in each path, and the flips' updates held against the others'. The scene thread's rounds are
    /// the same work as on the 4090 (a frame's time is not, with no GPU). It runs the whole
    /// real-time run once, as the bench does: about 8 minutes.
    /// `cargo test --release -p client --bin client -- --ignored --nocapture sea_level_crossing`
    #[test]
    #[ignore = "generates the full-size world map and runs every path: about 8 minutes"]
    fn the_sea_level_crossing_round_at_full_size_is_no_longer_than_the_other_paths_worst() {
        let built = build_full().expect("the full-size Planet loads");
        let run = one_run(
            built,
            &mut NoGpu,
            &Script::full(),
            BUDGETS.frame_time_p99_ms,
        )
        .unwrap_or_else(|stopped| panic!("the run stopped at {}: {}", stopped.step, stopped.error));
        for p in &run.paths {
            eprintln!(
                "{:<20} longest scene round {:>8.2} ms, update {:>8.2} ms, cap flips {} (longest {:.2} ms), scene {:.1} MiB, backlog at most {} nodes, {} absorbing updates over {:.0} ms (follow {:.0} ms, pack {:.0} ms, publish {:.0} ms in {} frames); holes {}",
                p.name,
                p.scene_round_max_ms,
                p.scene_update_max_ms,
                p.cap_flips,
                p.cap_flip_max_ms,
                p.scene_used_peak_mib,
                p.backlog_peak,
                p.absorb_updates,
                p.absorb_ms,
                p.scene_thread.follow_ms,
                p.scene_thread.pack_ms,
                p.scene_thread.upload_ms,
                p.frames,
                p.holes,
            );
        }
        for p in &run.paths {
            eprintln!(
                "{}: {:.1} s, soft misses {:.0} a frame, settle {:?}",
                p.name, p.seconds, p.soft_misses_per_frame, p.settle
            );
        }
        let crossing = run
            .paths
            .iter()
            .find(|p| p.name == "sea-level crossing")
            .expect("the crossing is a path");
        // The paths in which no flip was absorbed: the update of an ordinary follow is what the
        // flip's updates are held against. A flip's absorbing goes on into the path after the
        // crossing, so the flips' longest update is the longest of any path's `cap_flip_max_ms`.
        let ordinary = |of: fn(&PathReport) -> f64| {
            run.paths
                .iter()
                .filter(|p| p.cap_flip_max_ms == 0.0)
                .map(of)
                .fold(0.0, f64::max)
        };
        let (ordinary_update, ordinary_round) = (
            ordinary(|p| p.scene_update_max_ms),
            ordinary(|p| p.scene_round_max_ms),
        );
        let flip_update = run
            .paths
            .iter()
            .map(|p| p.cap_flip_max_ms)
            .fold(0.0, f64::max);
        eprintln!(
            "sea-level crossing: the flips' longest update {flip_update:.2} ms ({} flips); the \
             crossing's longest update {:.2} ms and round {:.2} ms (any update of an ordinary \
             follow, which varies by a few percent from run to run); the other paths' longest \
             update {ordinary_update:.2} ms and round {ordinary_round:.2} ms",
            crossing.cap_flips, crossing.scene_update_max_ms, crossing.scene_round_max_ms,
        );
        assert_eq!(crossing.holes, 0);
        // The depth of the dive is kept under the sea (#168): a release that found the camera there
        // made the scene as the wide one, 552 to 572 MiB.
        assert!(
            crossing.scene_used_peak_mib <= BUDGETS.scene_mib,
            "the crossing's scene peaked at {:.1} MiB, over the {} MiB gate",
            crossing.scene_used_peak_mib,
            BUDGETS.scene_mib
        );
        assert!(
            crossing.cap_flips >= 2,
            "the crossing flipped the cap both ways"
        );
        // The bound is loose since #168: an "ordinary" update now includes the follow step right
        // after a step of the speed scale's depth (550 to 718 ms on the 4090 at `0f5a936`, 504 to 665
        // in the orchestrator's later runs, against 304 to 476 before it: millions of queue entries
        // come due at once), so a regression in #213's flip
        // update (214 to 284 ms there) of up to that much would not fail this.
        assert!(
            flip_update <= ordinary_update,
            "a flip's update is {flip_update:.2} ms, over the other paths' {ordinary_update:.2} ms"
        );
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
