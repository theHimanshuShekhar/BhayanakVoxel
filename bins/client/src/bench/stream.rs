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
//! hash of all of them, which the tests compare with a value that is committed.
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
use render::lod::{Bands, LEVELS, Margins};
use render::scene::Bound;
use render::tracer::upload_bytes;
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

/// What one frame did, in counts.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FrameCounts {
    phase: Phase,
    /// Bytes the copy to the GPU writes (all of the buffers for a rebuild), and of them the bytes
    /// of the patch (none for a rebuild).
    upload_bytes: u64,
    patch_bytes: u64,
    rebuilt: bool,
    updated: bool,
    soft_misses: [usize; LEVELS],
    holes: usize,
    /// The store's size as the budget counts it, the bricks drawn, those waiting, those generated.
    streamed: usize,
    drawn: usize,
    waiting: usize,
    generated: u64,
    diverged: usize,
}

/// FNV-1a over 64-bit words: the same on every machine, which `DefaultHasher` does not promise.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn word(&mut self, word: u64) {
        for byte in word.to_le_bytes() {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
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
    fn add(&mut self, count: u64) {
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
    /// Updates that made the buffers again.
    pub rebuilds: u64,
    /// Frames that had an update at all.
    pub updates: u64,
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
    /// Frames from making the Edit to the frame that took it in from the scene thread (the Edit
    /// goes out through the scene thread's inbox, and is looked at in the second frame after).
    pub edit_latency_frames: u32,
}

/// The measured values that the client's sizing constants are checked against (see
/// [`Sizing::check`]), taken when the resident set was complete.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Measured {
    /// Bricks drawn per `d0` squared (`BRICKS_PER_D0_SQUARED`).
    pub bricks_per_d0_squared: f64,
    /// Words of `brick_refs` and of `bricks` in use per brick drawn (`REFS_PER_BRICK`,
    /// `WORDS_PER_BRICK`).
    pub refs_per_brick: f64,
    pub words_per_brick: f64,
    /// The store's size as its budget counts it, per `d0` squared (`STREAMED_PER_D0_SQUARED`).
    pub streamed_per_d0_squared: f64,
}

/// One constant beside what was measured.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Check {
    pub constant: &'static str,
    /// The report field it is checked against.
    pub field: &'static str,
    pub value: f64,
    /// The most that may be measured: the constant times `HEADROOM`.
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
            headroom: crate::HEADROOM,
        }
    }
}

impl Sizing {
    /// Each measured value against its constant times the headroom.
    pub fn check(measured: Measured, constants: &Constants) -> Self {
        let check = |constant, field, value: f64, measured: f64| {
            let limit = value * constants.headroom;
            Check {
                constant,
                field,
                value,
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
                measured.bricks_per_d0_squared,
            ),
            check(
                "REFS_PER_BRICK",
                "streaming.sizing.measured.refs_per_brick",
                constants.refs_per_brick,
                measured.refs_per_brick,
            ),
            check(
                "WORDS_PER_BRICK",
                "streaming.sizing.measured.words_per_brick",
                constants.words_per_brick,
                measured.words_per_brick,
            ),
            check(
                "STREAMED_PER_D0_SQUARED",
                "streaming.sizing.measured.streamed_per_d0_squared",
                constants.streamed_per_d0_squared,
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
    /// FNV-1a of every count of every frame and of the end state, in hex: equal on every run and
    /// on both operating systems, and compared with a committed value by the tests.
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
    soft: SoftMisses,
    holes: Series,
    store_max: usize,
    first_drawn: Option<[usize; LEVELS]>,
}

impl Tally {
    fn add(&mut self, counts: &FrameCounts) {
        let hash = self.hash.get_or_insert_with(Fnv::new);
        for word in [
            counts.phase as u64,
            counts.upload_bytes,
            counts.patch_bytes,
            u64::from(counts.rebuilt),
            u64::from(counts.updated),
            counts.holes as u64,
            counts.streamed as u64,
            counts.drawn as u64,
            counts.waiting as u64,
            counts.generated,
            counts.diverged as u64,
        ]
        .into_iter()
        .chain(counts.soft_misses.iter().map(|&n| n as u64))
        {
            hash.word(word);
        }
        self.upload.bytes.add(counts.upload_bytes);
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
        self.store_max = self.store_max.max(counts.streamed);
    }
}

/// What the world was at the spawn, before any frame: the Guard, which is the same whatever `d0` is,
/// and the spawn ring, which is held already and is not scaled either (see `room_for` in
/// `main.rs`). The sizing constants are per `d0` squared, so these are taken out of what is
/// measured.
#[derive(Clone, Copy)]
struct Start {
    /// The store's size as its budget counts it.
    streamed: usize,
    /// Words of `brick_refs` and `bricks` in use, and the bricks drawn.
    refs: usize,
    words: usize,
    bricks: usize,
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

/// `camera` pulled forward along its view to 40 Voxels from the ground it looks at: within reach
/// of a dig.
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

impl Streamer {
    /// The world right after the spawn, with the Guard held and the rest waiting for the loader.
    pub fn new() -> Self {
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
        let (refs, words) = scene.used_words().expect("a scene of rings");
        let start = Start {
            streamed: store.streamed_len(),
            refs,
            words,
            bricks: (0..LEVELS).map(|level| scene.level_bricks(level)).sum(),
        };
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
        self.world.advance(FRAME);
        let stats = self.world.stats();
        let store = self.world.store();
        let (upload, patch) = match &update {
            Some(update) => (
                upload_bytes(self.world.scene(), update),
                match update {
                    SceneUpdate::Rings(patch) => patch.words() as u64 * 4,
                    _ => 0,
                },
            ),
            None => (0, 0),
        };
        let counts = FrameCounts {
            phase,
            upload_bytes: upload,
            patch_bytes: patch,
            rebuilt: matches!(update, Some(SceneUpdate::Rebuilt)),
            updated: update.is_some(),
            soft_misses: stats.soft_misses_by_level,
            holes: stats.holes,
            streamed: store.streamed_len(),
            drawn: stats.level_bricks.iter().sum(),
            waiting: stats.waiting,
            generated: stats.loader.generated,
            diverged: store.diverged_len(),
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
        let measured = self.measure(store_when_complete);
        // The walk is held to the complete set, so that it evicts what it leaves behind.
        let budget = Budget {
            bricks: store_when_complete * BUDGET_PERCENT / 100,
            overflow: 0.01,
        };
        self.world.set_budget(budget);

        let mut edited = None;
        let mut latency = 0;
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
            if frame == EDIT_FRAME {
                let aim = within_reach(&self.world, &self.reach);
                self.world.edit(&aim, &crate::tool::Tool::default())?;
                edited = Some(self.frame);
            }
            self.step(phase, &camera, sink)?;
            if let Some(since) = edited
                && latency == 0
                && !self.world.edit_pending()
            {
                latency = self.frame - since;
            }
        }
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
        let mut hash = counts.hash.take().expect("there were frames");
        for n in end.iter().chain(&when_complete) {
            hash.word(*n as u64);
        }
        for word in [
            done.follow_steps,
            done.follow_bricks,
            done.pack_steps,
            done.pack_bricks,
            done.arrived_bricks,
            done.evict_steps,
            done.evicted_units,
            done.evicted_bricks,
            store_at_end as u64,
            diverged as u64,
            u64::from(latency),
        ] {
            hash.word(word);
        }
        let first_drawn = counts.first_drawn.unwrap_or([0; LEVELS]);
        Ok(StreamReport {
            deterministic: true,
            planet: format!("world-map-{MAP_SIZE}"),
            d0: D0,
            schedule: Schedule {
                fill_frames,
                steady_frames: counts.soft.steady.bricks.frames as u32,
                moving_frames: counts.soft.moving.bricks.frames as u32,
                edit_frame: EDIT_FRAME,
            },
            counts_hash: format!("{:016x}", hash.0),
            upload: counts.upload.clone(),
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
            },
            holes: counts.holes.clone(),
            sizing: Sizing::check(measured, &Constants::of_the_client()),
            timing: None,
        })
    }

    /// The sizes that the constants are for, as the resident set is complete. The constants are
    /// per `d0` squared, so what does not scale with it is left out (see [`Start`]): the Guard's
    /// levels (from `GUARD_FIRST`) and the store as it was at the spawn. The words of the Guard's
    /// bricks that are drawn are taken to be what the Guard's bricks weighed at the spawn, a brick
    /// each on average. The constants have the Guard in, which is 5% of the bricks drawn at full
    /// size and 30% here, so the check is a little easy on them, and not harder than the Planet
    /// the constants were measured on.
    fn measure(&self, streamed: usize) -> Measured {
        let d0_squared = D0 * D0;
        let stats = self.world.stats();
        let (ring, guard) = stats.level_bricks.split_at(GUARD_FIRST);
        let (ring, guard) = (
            ring.iter().sum::<usize>() as f64,
            guard.iter().sum::<usize>() as f64,
        );
        let used = |name: &str| {
            self.world
                .table_sizes()
                .iter()
                .find(|t| t.name == name)
                .map_or(0.0, |t| t.len as f64)
        };
        let per_guard_brick = |words: usize| words as f64 / self.start.bricks as f64;
        let of_ring = |words: f64, at_start: usize| words - per_guard_brick(at_start) * guard;
        Measured {
            bricks_per_d0_squared: ring / d0_squared,
            refs_per_brick: of_ring(used("scene brick_refs words"), self.start.refs) / ring,
            words_per_brick: of_ring(used("scene bricks words"), self.start.words) / ring,
            streamed_per_d0_squared: (streamed - self.start.streamed) as f64 / d0_squared,
        }
    }
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
            patch_bytes: 0,
            rebuilt: false,
            updated: false,
            soft_misses: [0; LEVELS],
            holes: 1,
            streamed: 0,
            drawn: 0,
            waiting: 0,
            generated: 0,
            diverged: 0,
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
}
