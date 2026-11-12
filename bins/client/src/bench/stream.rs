//! The streaming mode of `client bench` (#181): the loader, the rings' recentring, the scene's
//! packing and the copy to the GPU, on the test Planet, along a camera path, with the counts of
//! what they did that are the same on every machine and every run.
//!
//! The floating bench (`mod.rs`) has the whole Planet packed before frame 0, so none of this runs
//! in it. Here the world streams as the game's does ([`World`], the loader, the scene thread), but
//! stepped (#180, docs/agents/ci.md): no threads, no sleeps, and a clock that only this driver
//! moves, a frame at a time, so the work of each frame does not depend on how long anything took.
//! What the machine does differ in is the GPU's time, which is in [`Timing`] and is not
//! deterministic; every other field of [`StreamReport`] is, and [`StreamReport::counts_hash`] is a
//! hash of every integer, boolean and name in them (see [`hash_json`]: the report and each frame's
//! counts are hashed as they are serialized, so a field added to either is in the hash without
//! anyone listing it), which the tests compare with a value that is committed. Floats are left
//! out of the hash (a platform's maths library may differ in the last bit); each one in the report
//! is worked out from integers that are in it.
//!
//! The frames, by index: the camera at the spawn until the resident set is complete (the **fill**);
//! then a path that stands at the spawn for a quarter of its frames (**steady**) and walks the rest
//! (**moving**), with an Edit in the middle of the walk and a budget on the store that is the
//! complete set's, so that the walk evicts.

use std::error::Error;
use std::time::{Duration, Instant};

use brick_store::{BrickStore, Budget, VOXELS_PER_METRE, VoxelCoord};
use net::{ServerEnd, loopback};
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::WorldMap;
use render::UploadCost;
use render::lod::{Bands, LEVELS, Margins};
use render::scene::Bound;
use render::tracer::upload_cost;
use render::{Camera, Offscreen, Scene, SceneUpdate};
use serde::Serialize;
use sim::Predictor;

use super::path::{CameraPath, Keyframe};
use super::stats::Stats;
use crate::loader::{GUARD_FIRST, Loader, hold_guard};
use crate::world::World;

/// The test Planet: the world map's size, which the stepped tests use too.
const MAP_SIZE: u32 = 512;
/// Voxels that the loader's first ring reaches round the spawn.
const RING: i32 = 160;
/// The bands' `d0`, in Voxels. A small one makes a fill of a few hundred frames; the sizing
/// constants are per `d0` squared, so they carry over to the game's.
const D0: f64 = 64.0;
/// A frame at 60 a second.
const FRAME: Duration = Duration::from_micros(16_667);
/// The most frames the fill takes before it is said not to fill.
const FILL_LIMIT: u32 = 5_000;
/// Frames of the path: standing at the spawn, then walking at 0.4 m a frame (24 m a second), in
/// three segments of 24 m.
const STEADY_FRAMES: u32 = 60;
const WALK_FRAMES: u32 = 180;
const PATH_FRAMES: u32 = STEADY_FRAMES + WALK_FRAMES;
const SEGMENT_METRES: f64 = 24.0;
/// The frame of the path at which the Edit is made.
const EDIT_FRAME: u32 = 150;
/// The budget over the complete set that the walk is held to, in percent.
const BUDGET_PERCENT: usize = 102;

/// What the driver gives each frame to: the copy to the GPU and the drawing, or nothing.
pub trait Sink {
    /// Called after a frame's update was taken: `index` counts every frame from 0, `scene` is what
    /// is drawn, and `update` how it changed (copy it to the GPU).
    fn frame(
        &mut self,
        index: u32,
        camera: &Camera,
        scene: &Scene,
        update: Option<&SceneUpdate>,
    ) -> Result<(), Box<dyn Error>>;
}

/// A run with no GPU: only the counts.
#[cfg(test)]
pub struct NoGpu;

#[cfg(test)]
impl Sink for NoGpu {
    fn frame(
        &mut self,
        _: u32,
        _: &Camera,
        _: &Scene,
        _: Option<&SceneUpdate>,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}

/// The copy to a GPU and the drawing, timed on the CPU. Every update is copied (the GPU's buffers
/// must follow the scene), and a frame is drawn every `draw_every` frames.
pub struct GpuSink<'a> {
    gpu: &'a mut Offscreen,
    draw_every: u32,
    uploaded_before: u64,
    upload_ms: Vec<f64>,
    draw_ms: Vec<f64>,
}

impl<'a> GpuSink<'a> {
    pub fn new(gpu: &'a mut Offscreen, draw_every: u32) -> Self {
        Self {
            uploaded_before: gpu.uploaded_bytes(),
            gpu,
            draw_every: draw_every.max(1),
            upload_ms: Vec::new(),
            draw_ms: Vec::new(),
        }
    }

    /// What the sink timed and counted.
    pub fn timing(&self) -> Timing {
        Timing {
            deterministic: false,
            upload_ms: Stats::of(&self.upload_ms),
            draw_ms: Stats::of(&self.draw_ms),
            gpu_uploaded_bytes: self.gpu.uploaded_bytes() - self.uploaded_before,
            drawn_frames: self.draw_ms.len(),
        }
    }
}

impl Sink for GpuSink<'_> {
    fn frame(
        &mut self,
        index: u32,
        camera: &Camera,
        scene: &Scene,
        update: Option<&SceneUpdate>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(update) = update {
            let began = Instant::now();
            self.gpu.update_scene(scene, update)?;
            self.upload_ms.push(began.elapsed().as_secs_f64() * 1e3);
        }
        if index.is_multiple_of(self.draw_every) {
            let began = Instant::now();
            self.gpu.draw_upscaled(camera);
            self.draw_ms.push(began.elapsed().as_secs_f64() * 1e3);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Fill,
    Steady,
    Moving,
}

/// What one frame did, in counts. Every field is hashed, as it is serialized (see [`hash_json`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct FrameCounts {
    phase: Phase,
    /// Bytes the copy to the GPU writes (all of the buffers for a rebuild), and of them the bytes
    /// of the patch (none for a rebuild).
    upload_bytes: u64,
    /// Copies the GPU makes for it (each a barrier of its own: see `render::RingPatch::cost`).
    upload_writes: u64,
    patch_bytes: u64,
    rebuilt: bool,
    updated: bool,
    /// The scene thread held work back in this frame for the upload budget (see
    /// `scene_worker::UPLOAD_BUDGET`): the frame's upload was cut.
    throttled: bool,
    soft_misses: [usize; LEVELS],
    holes: usize,
    /// The store's size as the budget counts it, the bricks drawn, those waiting, those generated.
    streamed: usize,
    drawn: usize,
    waiting: usize,
    generated: u64,
    diverged: usize,
    /// Frames that the newest result taken waited in the outbox (see
    /// [`Residency::outbox_latency_frames`]).
    outbox_wait_frames: u64,
}

/// FNV-1a over 64-bit words: the same on every machine, which `DefaultHasher` does not promise.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn word(&mut self, word: u64) {
        self.bytes(&word.to_le_bytes());
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    /// A name, with its length so that two names in a row cannot run into each other.
    fn text(&mut self, text: &str) {
        self.word(text.len() as u64);
        self.bytes(text.as_bytes());
    }
}

/// Hashes everything deterministic in `value`: the integers, booleans and strings, with the names
/// of the fields they are in (sorted) and the lengths of the lists. Floats are left out.
fn hash_json(hash: &mut Fnv, value: &serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Null => hash.word(0),
        Value::Bool(b) => {
            hash.word(1);
            hash.word(u64::from(*b));
        }
        Value::Number(n) => {
            if let Some(n) = n.as_u64() {
                hash.word(2);
                hash.word(n);
            } else if let Some(n) = n.as_i64() {
                hash.word(3);
                hash.word(n as u64);
            }
        }
        Value::String(text) => {
            hash.word(4);
            hash.text(text);
        }
        Value::Array(items) => {
            hash.word(5);
            hash.word(items.len() as u64);
            for item in items {
                hash_json(hash, item);
            }
        }
        Value::Object(fields) => {
            hash.word(6);
            let mut names: Vec<_> = fields.keys().collect();
            names.sort();
            for name in names {
                hash.text(name);
                hash_json(hash, &fields[name]);
            }
        }
    }
}

/// The hash of the report (with `counts_hash` empty and no timing) after that of the frames.
fn counts_hash(frames: Fnv, report: &StreamReport) -> String {
    let mut hash = frames;
    let value = serde_json::to_value(report).expect("the report serializes");
    hash_json(&mut hash, &value);
    format!("{:016x}", hash.0)
}

/// What a series of counts came to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Series {
    pub frames: usize,
    pub total: u64,
    pub max: u64,
    /// Frames in which the count was not 0.
    pub frames_with: usize,
}

impl Series {
    pub(super) fn add(&mut self, count: u64) {
        self.frames += 1;
        self.total += count;
        self.max = self.max.max(count);
        self.frames_with += usize::from(count > 0);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Upload {
    /// Bytes the copies to the GPU write, a frame's, over the run, and of them the bytes of patches.
    pub bytes: Series,
    pub patch_bytes: u64,
    /// The copies a frame made (a rebuild makes the buffers again and has none).
    pub writes: Series,
    /// The 99th percentile (nearest rank) of the bytes and of the copies of the frames that had an
    /// update. The maximum is in the series.
    pub bytes_p99: u64,
    pub writes_p99: u64,
    /// Updates that made the buffers again.
    pub rebuilds: u64,
    /// Frames that had an update at all.
    pub updates: u64,
    /// Frames in which the scene thread held work back for the upload budget, so that the frame's
    /// upload was cut, and the most frames in a row it did: how long the longest backlog lasted.
    pub cut_frames: u64,
    pub longest_backlog_frames: u64,
}

/// Soft misses (bricks drawn whole although the bands split them) in the frames of a phase.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Misses {
    pub bricks: Series,
    /// The same, summed over the phase, by the level of the brick drawn whole.
    pub by_level: [u64; LEVELS],
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SoftMisses {
    pub fill: Misses,
    pub steady: Misses,
    pub moving: Misses,
}

/// One Ring (the bricks of one level, drawn within its LOD band) against its band.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RingFill {
    pub level: usize,
    /// Where its band starts, in Voxels from the camera.
    pub band_start: f64,
    /// Bricks drawn in the first frame, when the Guard stands in for the rest, when the resident
    /// set was complete, and at the end of the walk.
    pub at_start: usize,
    pub when_complete: usize,
    pub at_end: usize,
}

/// What the scene thread did, by category, over the run (see [`Done`]): steps and bricks, not times.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SceneWork {
    /// Selecting again for a new camera: rounds, and the bricks drawn that came or went.
    pub follow_steps: u64,
    pub follow_bricks: u64,
    /// Packing what the loader made: rounds, bricks drawn that changed, and the bricks it made.
    pub pack_steps: u64,
    pub pack_bricks: u64,
    pub arrived_bricks: u64,
    /// Copying to the GPU: frames with an update, and the bytes (there are no bricks to count: a
    /// patch is words).
    pub upload_steps: u64,
    pub upload_bytes: u64,
    /// Eviction: rounds, the units it took and the bricks they held.
    pub evict_steps: u64,
    pub evicted_units: u64,
    pub evicted_bricks: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Residency {
    /// The bricks the Edit changed that the store holds (Diverged).
    pub diverged_bricks: usize,
    /// The store's size as its budget counts it: at the complete set, the most over the run, and
    /// at the end; and the budget the walk was held to.
    pub store_at_start: usize,
    pub store_when_complete: usize,
    pub store_max: usize,
    pub store_at_end: usize,
    pub budget: usize,
    /// **Edit latency**: frames from the frame in which the Edit was made to the frame that took it
    /// in from the scene thread (the one in which [`World::edit_taken`] first holds). The Edit is
    /// made before the frame's look at the scene, as the game makes it. **A stepped scene thread
    /// finishes its round inside that look**, so this is 0 here, and the field cannot show ADR
    /// 0006's bound (an Edit made in frame N shows in N+1 if the real thread has packed it by
    /// then): the stepped test `an_edit_a_busy_scene_thread_has_not_packed_is_waited_for_...` and
    /// the real-thread tests hold that, and #162's real-time run measures it. It is here to show
    /// when a change makes an Edit later than the look that made it.
    pub edit_latency_frames: u32,
    /// **Outbox latency**: frames that a result the scene thread published waited in its outbox
    /// before the frame thread took it (ADR 0006; the cap on uploads of #145 is what makes it more
    /// than 0), a count for each frame that took one. 0 in a stepped run, in which a frame takes
    /// what was published in it, until #145.
    pub outbox_latency_frames: Series,
}

/// The measured values that the client's sizing constants are checked against (see
/// [`Sizing::check`]), taken when the resident set was complete: the integers they are worked out
/// from, and then the values.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Measured {
    /// Bricks drawn at the levels below the Guard's (the Rings), and at the Guard's.
    pub ring_bricks: u64,
    pub guard_bricks: u64,
    /// Words of `brick_refs` and of `bricks` in use by the bricks drawn at the Rings' levels and at
    /// the Guard's, counted exactly (see `Scene::words_by_level`).
    pub ring_refs_words: u64,
    pub ring_bricks_words: u64,
    pub guard_refs_words: u64,
    pub guard_bricks_words: u64,
    /// The store's size as its budget counts it, when the resident set was complete and at the
    /// spawn.
    pub streamed: u64,
    pub streamed_at_start: u64,
    /// The bricks of level 0 held at the spawn and when the resident set was complete. At the
    /// spawn the store holds nothing else under the Guard, so the first is all of
    /// `streamed_at_start`: what is taken out is the spawn ring's level 0, as the 5.63 million
    /// taken out of the constant is. They are equal: level 0 beyond the ring is not held here (see
    /// `STREAMED_LEVEL_0_BEYOND_THE_RING_PER_D0_SQUARED`).
    pub level_0_bricks_at_start: u64,
    pub level_0_bricks_when_complete: u64,
    /// Ring bricks drawn per `d0` squared (`BRICKS_PER_D0_SQUARED`).
    pub bricks_per_d0_squared: f64,
    /// Words of `brick_refs` and of `bricks` in use per Ring brick (`REFS_PER_BRICK`,
    /// `WORDS_PER_BRICK`), and the words of `bricks` per Guard brick, for what the Guard's share of
    /// the constant is.
    pub refs_per_brick: f64,
    pub words_per_brick: f64,
    pub words_per_guard_brick: f64,
    /// What the store grew by from the spawn to the complete set, per `d0` squared, which is the
    /// part of `STREAMED_PER_D0_SQUARED` that scales with `d0` (the spawn ring's share is held
    /// at the start and does not).
    pub streamed_per_d0_squared: f64,
}

/// One constant beside what was measured.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Check {
    pub constant: &'static str,
    /// The report field it is checked against.
    pub field: &'static str,
    /// The constant.
    pub value: f64,
    /// The part of it that the measurement is held to: the whole of it, but for
    /// `STREAMED_PER_D0_SQUARED`, whose spawn ring's and level 0 beyond the ring's shares are not
    /// in what is measured.
    pub compared: f64,
    /// The most that may be measured: `compared` times `HEADROOM`.
    pub limit: f64,
    pub measured: f64,
    pub over: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Sizing {
    pub measured: Measured,
    pub headroom: f64,
    pub checks: Vec<Check>,
}

/// The constants of the client that size its tables, with the headroom they are made with.
#[derive(Clone, Copy, Debug)]
pub struct Constants {
    pub bricks_per_d0_squared: f64,
    pub refs_per_brick: f64,
    pub words_per_brick: f64,
    pub streamed_per_d0_squared: f64,
    /// The shares of `streamed_per_d0_squared` that the measurement does not have: the spawn ring's
    /// (not what grows with `d0`), and level 0 beyond the ring (the test Planet's level 0 is
    /// within the ring).
    pub streamed_spawn_ring_per_d0_squared: f64,
    pub streamed_level_0_beyond_the_ring_per_d0_squared: f64,
    pub headroom: f64,
}

impl Constants {
    /// The client's own (`bins/client/src/main.rs`).
    pub fn of_the_client() -> Self {
        Self {
            bricks_per_d0_squared: crate::BRICKS_PER_D0_SQUARED,
            refs_per_brick: crate::REFS_PER_BRICK,
            words_per_brick: crate::WORDS_PER_BRICK,
            streamed_per_d0_squared: crate::STREAMED_PER_D0_SQUARED,
            streamed_spawn_ring_per_d0_squared: crate::STREAMED_SPAWN_RING_PER_D0_SQUARED,
            streamed_level_0_beyond_the_ring_per_d0_squared:
                crate::STREAMED_LEVEL_0_BEYOND_THE_RING_PER_D0_SQUARED,
            headroom: crate::HEADROOM,
        }
    }
}

impl Sizing {
    /// Each measured value against its constant times the headroom.
    pub fn check(measured: Measured, constants: &Constants) -> Self {
        let check = |constant, field, value: f64, taken_out: f64, measured: f64| {
            let compared = value - taken_out;
            let limit = compared * constants.headroom;
            Check {
                constant,
                field,
                value,
                compared,
                limit,
                measured,
                over: measured > limit,
            }
        };
        let checks = vec![
            check(
                "BRICKS_PER_D0_SQUARED",
                "streaming.sizing.measured.bricks_per_d0_squared",
                constants.bricks_per_d0_squared,
                0.0,
                measured.bricks_per_d0_squared,
            ),
            check(
                "REFS_PER_BRICK",
                "streaming.sizing.measured.refs_per_brick",
                constants.refs_per_brick,
                0.0,
                measured.refs_per_brick,
            ),
            check(
                "WORDS_PER_BRICK",
                "streaming.sizing.measured.words_per_brick",
                constants.words_per_brick,
                0.0,
                measured.words_per_brick,
            ),
            check(
                "STREAMED_PER_D0_SQUARED",
                "streaming.sizing.measured.streamed_per_d0_squared",
                constants.streamed_per_d0_squared,
                constants.streamed_spawn_ring_per_d0_squared
                    + constants.streamed_level_0_beyond_the_ring_per_d0_squared,
                measured.streamed_per_d0_squared,
            ),
        ];
        Self {
            measured,
            headroom: constants.headroom,
            checks,
        }
    }

    /// The constants that a measured value is over the limit of.
    #[cfg(test)]
    pub fn over(&self) -> Vec<&'static str> {
        self.checks
            .iter()
            .filter(|c| c.over)
            .map(|c| c.constant)
            .collect()
    }
}

/// What the GPU side of the run took, which is not deterministic (unlike everything else in the
/// report).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Timing {
    pub deterministic: bool,
    /// Per frame, in milliseconds on the CPU: the copy to the GPU, and the draw.
    pub upload_ms: Stats,
    pub draw_ms: Stats,
    /// Bytes the GPU's copies wrote (what `render::tracer::upload_bytes` works out on the CPU, the
    /// one in [`Upload`]), counted by the tracer.
    pub gpu_uploaded_bytes: u64,
    pub drawn_frames: usize,
}

/// How many frames of each phase there were. The steady frames are the 60 of the path and the two
/// that settle the sizes (one after the fill, one after the walk), each with a second of the
/// clock before it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Schedule {
    pub fill_frames: u32,
    pub steady_frames: u32,
    pub moving_frames: u32,
    pub edit_frame: u32,
}

/// The report of the streaming mode. Everything in it is made by the stepped mode and is the same
/// on every run, on every machine, except [`StreamReport::timing`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StreamReport {
    /// That everything outside `timing` is deterministic.
    pub deterministic: bool,
    pub planet: String,
    pub d0: f64,
    pub schedule: Schedule,
    /// FNV-1a of the counts of every frame and of every integer, boolean and name of this report
    /// (but this field and `timing`), in hex: equal on every run and on both operating systems,
    /// and compared with a committed value by the tests. Floats are not in it (see the module).
    pub counts_hash: String,
    pub upload: Upload,
    pub soft_misses: SoftMisses,
    pub rings: Vec<RingFill>,
    pub work: SceneWork,
    pub residency: Residency,
    /// Bricks split with the bricks under them not resident, a frame at a time. The gate: any is a
    /// failure of the run (exit status 2).
    pub holes: Series,
    pub sizing: Sizing,
    pub timing: Option<Timing>,
}

impl StreamReport {
    /// Whether the run holds: no frame had a hole.
    pub fn gate_holds(&self) -> bool {
        self.holes.total == 0
    }
}

/// The counts that are summed up over the run.
#[derive(Default)]
struct Tally {
    hash: Option<Fnv>,
    upload: Upload,
    /// The bytes and the copies of each frame that had an update, for their percentiles.
    update_bytes: Vec<u64>,
    update_writes: Vec<u64>,
    /// Frames in a row that work was held back.
    backlog: u64,
    soft: SoftMisses,
    holes: Series,
    outbox: Series,
    store_max: usize,
    first_drawn: Option<[usize; LEVELS]>,
}

impl Tally {
    fn add(&mut self, counts: &FrameCounts) {
        let hash = self.hash.get_or_insert_with(Fnv::new);
        hash_json(
            hash,
            &serde_json::to_value(counts).expect("the counts serialize"),
        );
        self.upload.bytes.add(counts.upload_bytes);
        self.upload.writes.add(counts.upload_writes);
        if counts.updated {
            self.update_bytes.push(counts.upload_bytes);
            self.update_writes.push(counts.upload_writes);
        }
        self.upload.cut_frames += u64::from(counts.throttled);
        self.backlog = if counts.throttled {
            self.backlog + 1
        } else {
            0
        };
        self.upload.longest_backlog_frames = self.upload.longest_backlog_frames.max(self.backlog);
        self.upload.patch_bytes += counts.patch_bytes;
        self.upload.rebuilds += u64::from(counts.rebuilt);
        self.upload.updates += u64::from(counts.updated);
        let misses = match counts.phase {
            Phase::Fill => &mut self.soft.fill,
            Phase::Steady => &mut self.soft.steady,
            Phase::Moving => &mut self.soft.moving,
        };
        misses
            .bricks
            .add(counts.soft_misses.iter().sum::<usize>() as u64);
        for (sum, &n) in misses.by_level.iter_mut().zip(&counts.soft_misses) {
            *sum += n as u64;
        }
        self.holes.add(counts.holes as u64);
        self.outbox.add(counts.outbox_wait_frames);
        self.store_max = self.store_max.max(counts.streamed);
    }
}

/// What the world was at the spawn, before any frame: the store's size as its budget counts it,
/// which has the spawn ring in already, held and not scaled with `d0` (see `room_for` in
/// `main.rs`). The sizing constants are per `d0` squared, so this is taken out of the store's.
#[derive(Clone, Copy)]
pub(super) struct Start {
    pub(super) streamed: usize,
    /// The bricks of level 0 held (the spawn ring's: level 0 within the ring's reach).
    pub(super) level_0: usize,
}

impl Start {
    /// What `store` holds right after the spawn.
    pub(super) fn of(store: &BrickStore) -> Self {
        Self {
            streamed: store.streamed_len(),
            level_0: store.level(0).map_or(0, BrickStore::len),
        }
    }
}

/// The test Planet, streamed in stepped.
pub struct Streamer {
    world: World,
    start: Start,
    /// The server's end of the world's link: held so that it stays connected, and never read.
    _server: ServerEnd,
    path: CameraPath,
    reach: Camera,
    frame: u32,
    counts: Tally,
    bands: Bands,
}

/// The point 4 m above the ground, along the radial through `point`, as `store` reads the Planet.
pub(super) fn over_the_ground(store: &BrickStore, point: [f64; 3]) -> [f64; 3] {
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

/// `camera` pulled forward along its view to 40 Voxels from the ground it looks at: within reach
/// of a dig; None if it looks at nothing (the sky, or ground the scene has not got).
pub(super) fn within_reach(world: &World, camera: &Camera) -> Option<Camera> {
    let forward = camera.forward();
    let distance = crate::world::centre_hit(world.scene(), camera)?.t;
    Some(Camera {
        position: std::array::from_fn(|a| {
            camera.position[a] + forward[a] * f64::from(distance - 40.0)
        }),
        ..*camera
    })
}

/// The test Planet right after the spawn: the ring and the Guard held and the rest waiting for the
/// loader, the scene drawn from them, and where the camera is.
pub(super) struct TestPlanet {
    pub(super) store: BrickStore,
    pub(super) scene: Scene,
    pub(super) spawn: Camera,
    pub(super) bands: Bands,
    pub(super) start: Start,
}

/// The test Planet (see [`TestPlanet`]), which the stepped and the real-time modes both start from.
pub(super) fn test_planet() -> TestPlanet {
    let planet = Planet::new(WorldMap::generate(crate::SEED, MAP_SIZE));
    let ground = planet.spawn(100);
    let mut store = planet.crust_around(ground, RING, LEVELS - 1);
    hold_guard(&mut store, &planet, GUARD_FIRST);
    let spawn = crate::spawn_camera(ground);
    let bands = Bands::with_d0(D0);
    let mut scene = Scene::rings(
        &store,
        spawn.position,
        bands,
        Bound {
            centre: [0; 3],
            radius: planet_gen::planet::MAX_RADIUS,
        },
    )
    .expect("the rings of the spawn fit");
    scene.set_margins(&store, Margins::standard());
    let start = Start::of(&store);
    TestPlanet {
        store,
        scene,
        spawn,
        bands,
        start,
    }
}

impl Streamer {
    /// The world right after the spawn, with the Guard held and the rest waiting for the loader.
    pub fn new() -> Self {
        let TestPlanet {
            store,
            scene,
            spawn,
            bands,
            start,
        } = test_planet();
        let (ahead, _) = tangents(spawn.horizon_up);
        let point = |metres: f64| -> [f64; 3] {
            over_the_ground(
                &store,
                std::array::from_fn(|a| spawn.position[a] + ahead[a] * metres * VOXELS_PER_METRE),
            )
        };
        let key = |position: [f64; 3]| Keyframe {
            position,
            target: std::array::from_fn(|a| position[a] + ahead[a]),
        };
        // The walk: three segments from the spawn.
        let path = CameraPath::new(vec![
            key(spawn.position),
            key(point(SEGMENT_METRES)),
            key(point(2.0 * SEGMENT_METRES)),
            key(point(3.0 * SEGMENT_METRES)),
        ])
        .radial();
        let source = store.source().expect("the store has a source");
        let (client, server) = loopback();
        let world = World::new(scene, Predictor::new(store), client)
            .with_stepped_loader(Loader::stepped(source, spawn.position, bands));
        world.count_exactly();
        Self {
            world,
            start,
            _server: server,
            path,
            reach: spawn,
            frame: 0,
            counts: Tally::default(),
            bands,
        }
    }

    /// The scene as the world has it, to make a GPU copy of before the run.
    pub fn scene(&self) -> &Scene {
        self.world.scene()
    }

    /// One frame: the camera is told, the scene thread and the loader do a step, what was made is
    /// taken, and the clock moves on by a frame.
    fn step(
        &mut self,
        phase: Phase,
        camera: &Camera,
        sink: &mut dyn Sink,
    ) -> Result<FrameCounts, Box<dyn Error>> {
        let update = self.world.recentre(camera.position)?;
        let waited = self.world.take_outbox_wait();
        self.world.advance(FRAME);
        let stats = self.world.stats();
        let store = self.world.store();
        let (upload, patch) = match &update {
            Some(update) => (
                upload_cost(self.world.scene(), update),
                match update {
                    SceneUpdate::Rings(patch) => patch.words() as u64 * 4,
                    _ => 0,
                },
            ),
            None => (UploadCost::default(), 0),
        };
        let counts = FrameCounts {
            phase,
            upload_bytes: upload.bytes,
            upload_writes: u64::from(upload.writes),
            patch_bytes: patch,
            rebuilt: matches!(update, Some(SceneUpdate::Rebuilt)),
            updated: update.is_some(),
            throttled: stats.throttled,
            soft_misses: stats.soft_misses_by_level,
            holes: stats.holes,
            streamed: store.streamed_len(),
            drawn: stats.level_bricks.iter().sum(),
            waiting: stats.waiting,
            generated: stats.loader.generated,
            diverged: store.diverged_len(),
            outbox_wait_frames: (waited.as_nanos() / FRAME.as_nanos()) as u64,
        };
        drop(store);
        self.counts.add(&counts);
        if self.counts.first_drawn.is_none() {
            self.counts.first_drawn = Some(self.world.stats().level_bricks);
        }
        sink.frame(self.frame, camera, self.world.scene(), update.as_ref())?;
        self.frame += 1;
        Ok(counts)
    }

    /// Runs the whole schedule, and reports.
    pub fn run(mut self, sink: &mut dyn Sink) -> Result<StreamReport, Box<dyn Error>> {
        let first = self.path.camera_at(0, WALK_FRAMES);
        // The fill: still at the spawn, until nothing waits and the loader has nothing out.
        let mut fill_frames = 0;
        loop {
            self.step(Phase::Fill, &first, sink)?;
            fill_frames += 1;
            let stats = self.world.stats();
            if stats.waiting == 0 && stats.loader.pending == 0 {
                break;
            }
            if fill_frames >= FILL_LIMIT {
                return Err(
                    format!("the resident set is not complete after {FILL_LIMIT} frames").into(),
                );
            }
        }
        let when_complete = self.world.stats().level_bricks;
        let store_when_complete = self.world.store().streamed_len();
        // The scene thread works out the sizes of its tables every so often by the clock: a second
        // goes by, and a frame later they are the ones of the complete set.
        self.world.advance(Duration::from_secs(1));
        self.step(Phase::Steady, &first, sink)?;
        let measured = measure_sizes(&self.world, self.start, D0, store_when_complete)?;
        // The walk is held to the complete set, so that it evicts what it leaves behind.
        let budget = Budget {
            bricks: store_when_complete * BUDGET_PERCENT / 100,
            overflow: 0.01,
        };
        self.world.set_budget(budget);

        let mut edited = None;
        let mut latency = None;
        for frame in 0..PATH_FRAMES {
            // The camera stands for the first frames of the path, exactly where it was, and walks
            // for the rest.
            let (camera, phase) = if frame < STEADY_FRAMES {
                (first, Phase::Steady)
            } else {
                (
                    self.path.camera_at(frame - STEADY_FRAMES, WALK_FRAMES),
                    Phase::Moving,
                )
            };
            // The Edit is made before the frame's look at the scene, where the game makes it (its
            // input is handled before `recentre`).
            if frame == EDIT_FRAME {
                let aim =
                    within_reach(&self.world, &self.reach).expect("the camera looks at the ground");
                let update = self.world.edit(&aim, &crate::tool::Tool::default())?;
                // The call takes what the scene thread has published, which a stepped one has not
                // done since the last frame took it. An update here would reach no GPU: say so.
                if update.is_some() {
                    return Err("the Edit's own look at the scene took an update".into());
                }
                edited = Some(self.frame);
            }
            self.step(phase, &camera, sink)?;
            // The frame this one was (`step` counted it): the one that took the Edit in, if it did.
            if let Some(since) = edited
                && latency.is_none()
                && self.world.edit_taken()
            {
                latency = Some(self.frame - 1 - since);
            }
        }
        let latency = latency.ok_or("the Edit was not taken in by the end of the path")?;
        // The scene thread's last word on what it did.
        self.world.advance(Duration::from_secs(1));
        let last = self.path.camera_at(WALK_FRAMES - 1, WALK_FRAMES);
        self.step(Phase::Steady, &last, sink)?;

        let stats = self.world.stats();
        let done = stats.work;
        let end = stats.level_bricks;
        let store = self.world.store();
        let (store_at_end, diverged) = (store.streamed_len(), store.diverged_len());
        drop(store);
        let mut counts = self.counts;
        let frames_hash = counts.hash.take().expect("there were frames");
        let first_drawn = counts.first_drawn.unwrap_or([0; LEVELS]);
        let mut report = StreamReport {
            deterministic: true,
            planet: format!("world-map-{MAP_SIZE}"),
            d0: D0,
            schedule: Schedule {
                fill_frames,
                steady_frames: counts.soft.steady.bricks.frames as u32,
                moving_frames: counts.soft.moving.bricks.frames as u32,
                edit_frame: EDIT_FRAME,
            },
            counts_hash: String::new(),
            upload: Upload {
                bytes_p99: percentile_99(&counts.update_bytes),
                writes_p99: percentile_99(&counts.update_writes),
                ..counts.upload.clone()
            },
            soft_misses: counts.soft.clone(),
            rings: (0..LEVELS)
                .map(|level| RingFill {
                    level,
                    band_start: self.bands.floor(level),
                    at_start: first_drawn[level],
                    when_complete: when_complete[level],
                    at_end: end[level],
                })
                .collect(),
            work: SceneWork {
                follow_steps: done.follow_steps,
                follow_bricks: done.follow_bricks,
                pack_steps: done.pack_steps,
                pack_bricks: done.pack_bricks,
                arrived_bricks: done.arrived_bricks,
                upload_steps: counts.upload.updates,
                upload_bytes: counts.upload.bytes.total,
                evict_steps: done.evict_steps,
                evicted_units: done.evicted_units,
                evicted_bricks: done.evicted_bricks,
            },
            residency: Residency {
                diverged_bricks: diverged,
                store_at_start: self.start.streamed,
                store_when_complete,
                store_max: counts.store_max,
                store_at_end,
                budget: budget.bricks,
                edit_latency_frames: latency,
                outbox_latency_frames: counts.outbox.clone(),
            },
            holes: counts.holes.clone(),
            sizing: Sizing::check(measured, &Constants::of_the_client()),
            timing: None,
        };
        report.counts_hash = counts_hash(frames_hash, &report);
        Ok(report)
    }
}

/// The 99th percentile, nearest rank, of `values`: the smallest that at least 99% of them do not
/// exceed; 0 for none.
pub(super) fn percentile_99(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    match (sorted.len() * 99).div_ceil(100) {
        0 => 0,
        rank => sorted[rank - 1],
    }
}

/// The sizes that the constants are for, as the resident set of `world` is complete, which `start`
/// was the spawn of and `streamed` the store's size then. The constants are per `d0` squared, so
/// what does not scale with it is left out: the Guard's levels (from `GUARD_FIRST`, which hold what
/// they hold whatever `d0` is), whose bricks and words are counted exactly and apart; and the store
/// as it was at the spawn (see [`Start`]). The world's stats must be exact (words by level, see
/// `World::count_exactly` and `World::count_once_exactly`).
///
/// The constants were measured with the Guard's bricks in (178 000 of the 3.44 million at full
/// size), so each of the Ring values is held to a constant that has the Guard in: the check is
/// lenient by what the Guard changes the constant by, which `words_per_guard_brick` and the
/// docs of the constants put in numbers.
pub(super) fn measure_sizes(
    world: &World,
    start: Start,
    d0: f64,
    streamed: usize,
) -> Result<Measured, Box<dyn Error>> {
    let d0_squared = d0 * d0;
    let stats = world.stats();
    let (ring, guard) = stats.level_bricks.split_at(GUARD_FIRST);
    let (ring, guard) = (ring.iter().sum::<usize>(), guard.iter().sum::<usize>());
    let (ring_words, guard_words) = stats.slow.words_by_level.split_at(GUARD_FIRST);
    let sum = |levels: &[(usize, usize)]| {
        levels
            .iter()
            .fold((0, 0), |(a, b), &(refs, words)| (a + refs, b + words))
    };
    let ((ring_refs, ring_bricks_words), (guard_refs, guard_bricks_words)) =
        (sum(ring_words), sum(guard_words));
    // A brick drawn has one reference, so the references are the bricks: if they are not, the
    // words are of another state of the scene than the bricks (a stale snapshot).
    if (ring_refs, guard_refs) != (ring, guard) {
        return Err(format!(
            "the words by level are not of the scene the bricks are of: {ring_refs} references \
             for {ring} Ring bricks, {guard_refs} for {guard} of the Guard's"
        )
        .into());
    }
    let per = |words: usize, bricks: usize| words as f64 / bricks.max(1) as f64;
    Ok(Measured {
        ring_bricks: ring as u64,
        guard_bricks: guard as u64,
        ring_refs_words: ring_refs as u64,
        ring_bricks_words: ring_bricks_words as u64,
        guard_refs_words: guard_refs as u64,
        guard_bricks_words: guard_bricks_words as u64,
        streamed: streamed as u64,
        streamed_at_start: start.streamed as u64,
        level_0_bricks_at_start: start.level_0 as u64,
        level_0_bricks_when_complete: world.store().level(0).map_or(0, BrickStore::len) as u64,
        bricks_per_d0_squared: ring as f64 / d0_squared,
        refs_per_brick: per(ring_refs, ring),
        words_per_brick: per(ring_bricks_words, ring),
        words_per_guard_brick: per(guard_bricks_words, guard),
        streamed_per_d0_squared: (streamed - start.streamed) as f64 / d0_squared,
    })
}

/// The exit status of the streaming mode: [`super::budget::EXIT_OVER_BUDGET`] if a frame had a
/// hole, as for a pass over its budget, unless `no_fail` says to ignore it.
pub fn exit_code(report: &StreamReport, no_fail: bool) -> u8 {
    if report.gate_holds() || no_fail {
        0
    } else {
        super::budget::EXIT_OVER_BUDGET
    }
}

/// What the stepped run counted, with no GPU: what the tests compare.
#[cfg(test)]
pub fn counts() -> StreamReport {
    Streamer::new()
        .run(&mut NoGpu)
        .expect("the stepped run completes")
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    /// The hash of the counts of the run, as committed. Both operating systems run these tests in
    /// CI (the stepped run needs no GPU), so a count that differs between them fails one of the
    /// two jobs. When a change to the streaming code moves the counts on purpose, the failing
    /// test prints the new hash: put it in the file.
    pub const GOLDEN: &str = include_str!("streaming-counts.golden");

    /// One run, which the tests that read it share: a run takes seconds.
    fn report() -> &'static StreamReport {
        static REPORT: OnceLock<StreamReport> = OnceLock::new();
        REPORT.get_or_init(counts)
    }

    #[test]
    fn two_stepped_runs_give_identical_residency_soft_miss_hole_and_upload_counts() {
        let first = report();
        let second = counts();
        assert_eq!(first, &second);
        // The run is not trivially the same: it fills, walks, evicts and has an Edit.
        assert!(
            first.soft_misses.fill.bricks.total > 0,
            "the fill has soft misses"
        );
        assert!(first.upload.bytes.total > 0 && first.upload.updates > 0);
        assert!(first.work.evicted_units > 0, "the walk evicts");
        assert!(
            first.residency.diverged_bricks > 0,
            "the Edit diverges bricks"
        );
        assert_eq!(first.upload.bytes.total, first.work.upload_bytes);
        assert!(first.timing.is_none(), "a run with no GPU has no timing");
    }

    #[test]
    fn the_counts_are_the_committed_ones() {
        assert_eq!(
            report().counts_hash,
            GOLDEN.trim(),
            "the counts of the streaming run changed: if that is meant, commit this hash in \
             bins/client/src/bench/streaming-counts.golden"
        );
    }

    #[test]
    fn a_resident_set_with_no_holes_passes_and_a_frame_with_one_fails_with_status_2() {
        let passing = report().clone();
        assert!(passing.gate_holds());
        assert_eq!(exit_code(&passing, false), 0);
        // The count of a frame that has a hole is what the gate reads: one frame is enough.
        let mut tally = Tally::default();
        let frame = FrameCounts {
            phase: Phase::Moving,
            upload_bytes: 0,
            upload_writes: 0,
            patch_bytes: 0,
            rebuilt: false,
            updated: false,
            throttled: false,
            soft_misses: [0; LEVELS],
            holes: 1,
            streamed: 0,
            drawn: 0,
            waiting: 0,
            generated: 0,
            diverged: 0,
            outbox_wait_frames: 0,
        };
        tally.add(&FrameCounts {
            holes: 0,
            ..frame.clone()
        });
        tally.add(&frame);
        let failing = StreamReport {
            holes: tally.holes,
            ..passing
        };
        assert_eq!((failing.holes.total, failing.holes.frames_with), (1, 1));
        assert!(!failing.gate_holds());
        assert_eq!(exit_code(&failing, false), 2);
        assert_eq!(super::super::budget::EXIT_OVER_BUDGET, 2);
        // `--no-fail` ignores it, as it does a pass over its budget.
        assert_eq!(exit_code(&failing, true), 0);
    }

    #[test]
    fn the_sizes_measured_on_the_test_planet_are_within_the_constants_with_headroom() {
        let sizing = &report().sizing;
        for c in &sizing.checks {
            eprintln!(
                "{}: measured {:.3}, constant {} (limit {:.3})",
                c.constant, c.measured, c.value, c.limit
            );
        }
        assert_eq!(
            sizing.over(),
            Vec::<&str>::new(),
            "a measured size is over its constant times HEADROOM: the constant in main.rs is too \
             small for the Planet (or the measurement moved): see the report's sizing.checks"
        );
    }

    #[test]
    fn a_constant_lowered_below_what_is_measured_fails_its_check_and_no_other() {
        let measured = report().sizing.measured.clone();
        let real = Constants::of_the_client();
        type Lower = fn(&mut Constants, f64);
        let lowered: [(&str, Lower); 4] = [
            ("BRICKS_PER_D0_SQUARED", |c, v| c.bricks_per_d0_squared = v),
            ("REFS_PER_BRICK", |c, v| c.refs_per_brick = v),
            ("WORDS_PER_BRICK", |c, v| c.words_per_brick = v),
            ("STREAMED_PER_D0_SQUARED", |c, v| {
                c.streamed_per_d0_squared = v
                    + c.streamed_spawn_ring_per_d0_squared
                    + c.streamed_level_0_beyond_the_ring_per_d0_squared
            }),
        ];
        for (name, lower) in lowered {
            let mut constants = real;
            let value = report()
                .sizing
                .checks
                .iter()
                .find(|c| c.constant == name)
                .unwrap()
                .measured;
            // Below the measured value, headroom and all.
            lower(&mut constants, 0.5 * value / real.headroom);
            let sizing = Sizing::check(measured.clone(), &constants);
            assert_eq!(sizing.over(), vec![name]);
        }
        assert!(Sizing::check(measured, &real).over().is_empty());
    }

    /// The JSON pointers of every integer in `value`.
    fn integers(value: &serde_json::Value, at: String, into: &mut Vec<String>) {
        use serde_json::Value;
        match value {
            Value::Number(n) if n.is_u64() || n.is_i64() => into.push(at),
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    integers(item, format!("{at}/{i}"), into);
                }
            }
            Value::Object(fields) => {
                for (name, field) in fields {
                    integers(field, format!("{at}/{name}"), into);
                }
            }
            _ => {}
        }
    }

    /// Whether changing any one integer of `value` changes its hash.
    fn every_integer_is_hashed(value: &serde_json::Value) -> usize {
        let hash = |value: &serde_json::Value| {
            let mut hash = Fnv::new();
            hash_json(&mut hash, value);
            hash.0
        };
        let base = hash(value);
        let mut pointers = Vec::new();
        integers(value, String::new(), &mut pointers);
        for pointer in &pointers {
            let mut changed = value.clone();
            let n = changed.pointer_mut(pointer).unwrap();
            *n = serde_json::json!(n.as_u64().unwrap() + 1);
            assert_ne!(hash(&changed), base, "{pointer} is not in the hash");
        }
        pointers.len()
    }

    #[test]
    fn every_integer_of_the_report_and_of_a_frame_is_in_the_hash() {
        // The report is hashed as it is serialized: a field added to it is in the hash with no list
        // to keep. This is the test that would show the hash leaving something out.
        let value = serde_json::to_value(report()).unwrap();
        assert!(every_integer_is_hashed(&value) > 100);
        let frame = FrameCounts {
            phase: Phase::Steady,
            upload_bytes: 1,
            upload_writes: 11,
            patch_bytes: 2,
            rebuilt: false,
            updated: true,
            throttled: true,
            soft_misses: [3; LEVELS],
            holes: 4,
            streamed: 5,
            drawn: 6,
            waiting: 7,
            generated: 8,
            diverged: 9,
            outbox_wait_frames: 10,
        };
        assert!(every_integer_is_hashed(&serde_json::to_value(frame).unwrap()) > LEVELS);
    }

    #[test]
    fn the_hash_leaves_out_floats_and_the_timing_and_nothing_else() {
        let mut changed = report().clone();
        changed.d0 += 1.0;
        changed.sizing.measured.words_per_brick += 1.0;
        assert_eq!(
            counts_hash(Fnv::new(), &changed),
            counts_hash(Fnv::new(), report())
        );
        changed.upload.rebuilds += 1;
        assert_ne!(
            counts_hash(Fnv::new(), &changed),
            counts_hash(Fnv::new(), report())
        );
    }

    #[test]
    fn the_words_are_of_the_scene_the_bricks_are_of_and_the_latencies_are_what_a_stepped_run_gives()
    {
        let report = report();
        // The Edit is made before the frame's look, as the game makes it, and a stepped scene thread
        // finishes its round in that look: the frame that made the Edit takes it. This is not ADR
        // 0006's bound, which a stepped run cannot show (see `Residency::edit_latency_frames`).
        assert_eq!(report.residency.edit_latency_frames, 0);
        // Results published in a frame are taken in it, until #145's cap on uploads.
        assert_eq!(report.residency.outbox_latency_frames.total, 0);
        // Words are counted by level, not guessed: the Rings' and the Guard's add up to those in use.
        let m = &report.sizing.measured;
        assert!(m.ring_bricks > 0 && m.guard_bricks > 0);
        assert!(m.guard_bricks_words > 0 && m.ring_bricks_words > 0);
        // A brick drawn has one reference: words of an earlier state of the scene than the bricks
        // (a snapshot taken before a later publish of the round) would not add up to that.
        assert_eq!(m.ring_refs_words, m.ring_bricks);
        assert_eq!(m.guard_refs_words, m.guard_bricks);
        // The premises of the streamed check: the store holds only the spawn ring's level 0 at the
        // start, and no level 0 beyond it when the set is complete.
        assert_eq!(m.level_0_bricks_at_start, m.streamed_at_start);
        assert_eq!(m.level_0_bricks_when_complete, m.level_0_bricks_at_start);
    }
}
