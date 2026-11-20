//! The client's view of the Planet: the bricks it predicts on, the packed copy that gets drawn,
//! and the loopback to the server. All Edits come through here, and reach the Voxels only via
//! [`sim::Predictor`], which calls [`sim::apply`].

use std::sync::Arc;

use brick_store::{Material, TableSize, VoxelCoord};
use net::{ClientEnd, Disconnected, EditMode, EditOp, MoveInput, TryRecvError};
use render::reference::{Hit, Ray, trace};
use render::{Camera, Mode, RingsFull, Scene, SceneUpdate, camera};
use sim::{Predictor, dig_seconds, target_hardness};

use crate::clock::Clock;
use crate::ghost::Ghost;
use crate::loader::Loader;
use crate::scene_worker::{Cause, Causes, LoaderStats, Longest, SceneWorker, Spent, Stats};
use crate::tool::{Tool, rotation_for};

/// How many frames in a row the view must be unchanged before the reference image is taken to be
/// accumulating it, and loading pauses for it: the renderer's own count, which shows the Reference
/// view from the same frame (#189).
pub const STILL_FRAMES: u32 = render::still::STILL_FRAMES;

/// Whether loading pauses, given the mode and how many frames in a row the view (camera, sun,
/// size and LOD bias) has been unchanged: only while the reference accumulates a still view.
/// Anything that changes it, moving included, lets loading go the same frame.
pub fn loading_paused_for(mode: Mode, still_frames: u32) -> bool {
    mode == Mode::Reference && still_frames >= STILL_FRAMES
}

/// The longest a Still view waits for the scene to reach the camera before it holds the scene
/// where it is (#217): 12 s (it was 5 s; the owner raised it on 2026-11-17, #168). A Reference
/// view that starts later costs nothing but the wait, and the Primary view is drawn meanwhile;
/// one that starts on a scene that is behind the camera converges on ground coarser than
/// Primary's, for as long as the camera stands. So the cap is for a scene that cannot get there
/// (the loader far behind, say), not a time to be reached in the usual way: the bench reports a
/// hold that began at the cap ([`HoldBegan::capped`]). 12 s is what lets the wait cover the
/// release rebuild of a flight's end (#168: the scene made again at the finer depth, which
/// ends 10.9 to 11.2 s after the view goes Still on the 4090, measured on `a7c6b72`: 12 s leaves
/// little room); the dev box's is 24 to 37 s and still hits it.
pub const CATCH_UP_CAP: std::time::Duration = std::time::Duration::from_secs(12);

/// How far the scene may be behind the camera, in Voxels, for it to have caught up (#217): one
/// (5 cm). The scene thread follows the camera to the exact place, and a still camera is the
/// same place for every round, so a scene that has reached it is 0 behind; a Voxel is below
/// anything the selection's bands tell apart (they are `d0` wide at the narrowest, which is
/// hundreds of Voxels), so it only lets floating point and a lag that was last published a
/// round early not hold the Reference view back.
pub const CAUGHT_UP_LAG: f64 = 1.0;

/// What a Still view does about the scene, from frame to frame (#94, #217).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// The view is not Still (or the mode is Primary): the scene follows the camera and takes in
    /// what the loader makes, and Primary is shown.
    Open,
    /// The view is Still and the scene is behind the camera: loading goes on, and Primary is shown
    /// (the Reference view does not start) until the scene has reached the camera, or
    /// [`CATCH_UP_CAP`] has gone by.
    CatchingUp,
    /// The view is Still and the scene is where the camera is: loading is paused, and the Reference
    /// view accumulates the scene as it is.
    Held,
}

/// Decides the [`Hold`] of every frame: when a view goes Still the scene is first caught up with
/// the camera, and then held. A Reference view of a scene that is behind the camera is a view of
/// ground coarser than the Primary view would have drawn there (about 4 times coarser at 4 `d0`
/// behind), converged on, so what the owner chose is to catch up first (#217). The world still
/// freezes only while the Reference view accumulates (#94): it starts later after a fast flight.
#[derive(Clone, Copy, Debug, Default)]
pub struct StillHold {
    /// When the view went Still, and how far the scene was behind the camera then, while it is.
    since: Option<(std::time::Instant, f64)>,
    /// How the hold began, once it has.
    began: Option<HoldBegan>,
}

/// How a hold began: what the catch-up before it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HoldBegan {
    /// The scene's lag behind the camera, in Voxels, when the view went Still and when the hold
    /// began.
    pub lag_when_still: f64,
    pub lag_when_held: f64,
    /// From the frame that the view went Still to the one the hold began in.
    pub waited: std::time::Duration,
    /// The cap ran out before the scene reached the camera.
    pub capped: bool,
}

impl StillHold {
    /// The hold for a frame at `now`, `mode` being how the window is drawn, `still_frames` how many
    /// frames in a row the view has been unchanged, `caught_up` whether the scene has reached the
    /// camera and nothing else is going to move it ([`World::caught_up`]) and `lag` how far behind
    /// it is. A hold that has begun lasts as long as the view is Still: the scene does not move,
    /// so it stays caught up.
    pub fn step(
        &mut self,
        mode: Mode,
        still_frames: u32,
        caught_up: bool,
        lag: f64,
        now: std::time::Instant,
    ) -> Hold {
        if !loading_paused_for(mode, still_frames) {
            *self = Self::default();
            return Hold::Open;
        }
        let (since, lag_when_still) = *self.since.get_or_insert((now, lag));
        if self.began.is_none() {
            let waited = now.saturating_duration_since(since);
            if caught_up || waited >= CATCH_UP_CAP {
                self.began = Some(HoldBegan {
                    lag_when_still,
                    lag_when_held: lag,
                    waited,
                    capped: !caught_up,
                });
            }
        }
        if self.began.is_some() {
            Hold::Held
        } else {
            Hold::CatchingUp
        }
    }

    /// How the hold began, once it has: None while the view is not Still or is still catching up.
    pub fn began(&self) -> Option<HoldBegan> {
        self.began
    }
}

/// How long what is compared has been the same, frame by frame: the view the reference image is
/// of, held still (the scene is not part of it, as arrivals from the loader that this decides
/// about change it).
#[derive(Debug)]
pub struct Stillness<K> {
    last: Option<K>,
    frames: u32,
}

impl<K> Default for Stillness<K> {
    fn default() -> Self {
        Self {
            last: None,
            frames: 0,
        }
    }
}

impl<K: PartialEq> Stillness<K> {
    /// Takes in this frame's `key` and returns how many frames in a row it has equalled the
    /// frame before's: 0 for a key that is new, then 1, 2, ...
    pub fn observe(&mut self, key: K) -> u32 {
        if self.last.as_ref() == Some(&key) {
            self.frames = self.frames.saturating_add(1);
        } else {
            self.last = Some(key);
            self.frames = 0;
        }
        self.frames
    }
}

/// When an update is a cut, by the frames the renderer has had (`render::still::Still::plan`): the
/// Reference view is shown in a frame after [`STILL_FRAMES`] frames in a row with the same view and
/// no update (an update is part of the key, as the scene's epoch), so an update restarts something
/// only if the view was Still in the frame before it and the last update was [`STILL_FRAMES`]
/// frames or more before that. An update sooner than that came while the view was Primary still
/// (the renderer had not shown the Reference view), and is none of the Reference view's to lose.
#[derive(Clone, Copy, Debug, Default)]
struct StillClock {
    /// Whether the frame before was Still (loading paused, or marked).
    was_still: bool,
    /// Frames since an update was taken, as of the start of the frame that is being looked at: the
    /// frames in between, not counting that one.
    since_update: u32,
    /// Whether an update in this frame is a cut, from the two above.
    cut_ready: bool,
}

impl StillClock {
    /// A frame begins, in which the view is Still or is not.
    fn begin_frame(&mut self, still: bool) {
        self.cut_ready = still && self.was_still && self.since_update >= STILL_FRAMES;
        self.was_still = still;
        self.since_update = self.since_update.saturating_add(1);
    }

    /// An update made of `causes` is taken in: counted in `cuts` if the view is Still and it
    /// cut the Reference view.
    fn update(&mut self, still: bool, cuts: &mut StillCuts, causes: Causes) {
        if still && std::mem::take(&mut self.cut_ready) {
            cuts.count(causes);
        }
        self.since_update = 0;
    }
}

/// The cuts of a Still view that the scene thread's patches made (#217): updates that the frame
/// thread took in while the view was Still, each of which restarts the Reference view from sample
/// 0 (the renderer's epoch moves with every update). Counted here, and not in the renderer, because
/// only the world knows what an update was made of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StillCuts {
    /// Frames the view was Still for, when the world was asked to look at the scene.
    pub frames: u64,
    /// Updates taken in those frames (or by a look of the harness's between them).
    pub cuts: u64,
    /// Cuts that an [`Cause`] was part of, in the order of [`Cause::ALL`]. A cut that several
    /// kinds of work made counts under each, so these add up to at least `cuts`.
    pub by_cause: [u64; Cause::ALL.len()],
}

impl StillCuts {
    fn count(&mut self, causes: Causes) {
        self.cuts += 1;
        for cause in causes.iter() {
            self.by_cause[cause as usize] += 1;
        }
    }

    /// What was counted since `before`, an earlier reading.
    pub fn since(self, before: StillCuts) -> StillCuts {
        StillCuts {
            frames: self.frames - before.frames,
            cuts: self.cuts - before.cuts,
            by_cause: std::array::from_fn(|i| self.by_cause[i] - before.by_cause[i]),
        }
    }

    /// Cuts that `cause` was part of.
    pub fn of(self, cause: Cause) -> u64 {
        self.by_cause[cause as usize]
    }
}

/// A ghost that took some time to make is not made again for a new Edit for this many times as
/// long, and the last one is shown until then: so the frame thread spends at most a ninth of its
/// time on it, whatever the Brush, while the cursor moves.
const GHOST_BACKOFF: u32 = 8;

/// How far the player aims an Edit, in Voxels: 7.5 m, which leaves half a metre of the server's
/// reach ([`sim::EDIT_REACH`], measured from where the server has the character) for the character
/// to have moved on from where the client saw it.
pub(crate) const AIM_REACH: f64 = sim::EDIT_REACH - 0.5 * brick_store::VOXELS_PER_METRE;

/// Why the Planet cannot be kept going.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// The server is gone.
    Disconnected,
    /// The Planet no longer fits the rings' buffers (a scene that was given its room, or a buffer
    /// that would be longer than the device allows); the scene is not to be used again.
    RingsFull(RingsFull),
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "the server stopped"),
            Self::RingsFull(full) => write!(f, "{full}"),
        }
    }
}

impl std::error::Error for Stopped {}

impl From<Disconnected> for Stopped {
    fn from(_: Disconnected) -> Self {
        Self::Disconnected
    }
}

impl From<RingsFull> for Stopped {
    fn from(full: RingsFull) -> Self {
        Self::RingsFull(full)
    }
}

/// How long the frame thread spent on the scene each frame (following the camera, taking in
/// bricks, copying the changes to the GPU), for reporting the worst and the 99th percentile.
#[derive(Debug, Default)]
pub struct SceneWork {
    millis: Vec<f64>,
}

impl SceneWork {
    pub fn record(&mut self, took: std::time::Duration) {
        self.millis.push(took.as_secs_f64() * 1000.0);
    }

    /// Adds the frames of `other`, which came after these.
    #[cfg(test)]
    pub fn merge(&mut self, other: SceneWork) {
        self.millis.extend(other.millis);
    }

    /// The longest frame, in milliseconds.
    pub fn worst(&self) -> Option<f64> {
        self.millis.iter().copied().max_by(f64::total_cmp)
    }

    /// The 99th percentile (nearest rank): the time that 99% of the frames did not exceed.
    pub fn p99(&self) -> Option<f64> {
        let mut sorted = self.millis.clone();
        sorted.sort_by(f64::total_cmp);
        let rank = (sorted.len() * 99).div_ceil(100);
        rank.checked_sub(1).map(|i| sorted[i])
    }

    /// One line for a log, if any frame was recorded.
    pub fn summary(&self) -> Option<String> {
        Some(format!(
            "scene work on the frame thread: {} frames, p99 {:.2} ms, worst {:.2} ms",
            self.millis.len(),
            self.p99()?,
            self.worst()?
        ))
    }
}

pub struct World {
    /// What is drawn, and what the frame thread traces rays in to aim. Once the world streams
    /// ([`World::with_loader`]) it is a copy, which what the scene thread makes is written into
    /// (see [`SceneWorker`]); else it is the scene itself.
    scene: Scene,
    predictor: Predictor,
    link: ClientEnd,
    /// The thread that follows the camera and takes in what the loader generates, if the Planet
    /// is streamed in.
    worker: Option<SceneWorker>,
    /// What the scene thread said of the scene and the loader with the words that `scene` has.
    stats: Stats,
    /// Whether [`World::stream`] leaves what the loader has generated where it is: see
    /// [`World::pause_loading`].
    loading_paused: bool,
    /// The view is Still by the frame policy, said by a harness that does not pause loading
    /// ([`World::mark_still`]): cuts are counted in it as when loading is paused.
    still_view: bool,
    still_cuts: StillCuts,
    /// What a cut is counted by, as the renderer's own count of Still frames has it (see
    /// [`World::begin_still_frame`]).
    still_clock: StillClock,
    /// Where a Still view is in catching up and holding: see [`World::hold_still`].
    still_hold: StillHold,
    /// What the next click would change, while there is something under the cursor to apply the
    /// tool to. Made again when the Edit it is of changes, or a brick it reads does.
    ghost: Option<Ghost>,
    /// Not before this is the ghost made again for a new Edit: see [`GHOST_BACKOFF`].
    ghost_next: Option<std::time::Instant>,
    /// Time spent running inputs again after the server and the prediction differed, since
    /// [`World::take_resim`].
    resim: std::time::Duration,
    /// The Edits given to the scene thread that have not been waited for ([`World::edit`]).
    unshown_edit: Option<UnshownEdit>,
    /// The scene thread's job of the newest Edit made, and the last job done that a take brought in:
    /// the Edit is in the scene that is drawn once the second is at least the first (see
    /// [`World::edit_taken`]).
    edit_ticket: u64,
    taken_done: u64,
    /// Edits made so far: those that the client did not know the server would turn down (see
    /// [`World::edits_made`]).
    edits_made: u64,
    /// The longest a result that the scene thread published waited, since it was last asked for,
    /// when the counts are exact (see [`World::take_outbox_wait`]).
    outbox_wait: std::time::Duration,
    /// How long making the buffers of the copy of the scene longer took, for the buffers that grew
    /// in what was taken since [`World::take_lengthened`] was last called.
    lengthened: std::time::Duration,
    /// How long [`World::edit_due`] waits: [`EDIT_WAIT`], or more in a test on a slow build.
    edit_wait: std::time::Duration,
    /// Whether [`World::recentre`] tells the scene thread how the camera is moving, for prefetch.
    lead: bool,
    motion: Motion,
    /// The time that the camera's velocity is measured in: the machine's, or a stepped world's.
    clock: Clock,
    /// Where the Guard was made complete around, and how far from it a camera may be before the Guard
    /// is complete (see [`World::hold_to_guard`]).
    guard_hold: Option<([f64; 3], f64)>,
}

/// The camera's velocity as the frames show it: a running mean over about a tenth of a second, so
/// that a jittery camera does not move where prefetch looks every frame.
#[derive(Clone, Copy, Debug, Default)]
struct Motion {
    last: Option<(std::time::Instant, [f64; 3])>,
    velocity: [f64; 3],
    /// The speed the bands are drawn for (ADR 0009), in Voxels a second: a running mean as the
    /// velocity is, but not cut at [`MOST_LEAD_SPEED`], since a Zoom's flight that fast is a camera
    /// to draw coarse, though it is not one to look ahead along; a single frame's jump (a
    /// teleport) is not a speed: it is 0 for it, and the speed is what the camera does after.
    speed: f64,
    /// Frames in a row that were over [`MOST_LEAD_SPEED`].
    fast: u32,
}

/// Faster than this, in Voxels a second (400 m/s, several times the flight speed), the camera
/// has jumped (a teleport, a Zoom's cut) and is not looked ahead along.
const MOST_LEAD_SPEED: f64 = 400.0 * brick_store::VOXELS_PER_METRE;

impl Motion {
    /// The velocity after the camera was seen at `camera` at `now`. [`Motion::speed`] is the
    /// bands' own.
    fn observe(&mut self, camera: [f64; 3], now: std::time::Instant) -> [f64; 3] {
        let before = self.last.replace((now, camera));
        let Some((then, from)) = before else {
            return [0.0; 3];
        };
        let dt = now.duration_since(then).as_secs_f64();
        if dt <= 0.0 {
            return self.velocity;
        }
        let raw: [f64; 3] = std::array::from_fn(|a| (camera[a] - from[a]) / dt);
        let speed = raw.iter().map(|v| v * v).sum::<f64>().sqrt();
        let alpha = (dt / 0.1).min(1.0);
        if dt > 0.25 {
            self.speed = 0.0;
            self.fast = 0;
        } else if speed > MOST_LEAD_SPEED {
            self.fast += 1;
            // The second frame in a row is a flight, not a jump: at once the speed of the flight.
            // The first is a jump (a teleport) and no speed: the speed the camera had before it
            // would be given back a frame at a time after it, long after the scene has been made
            // again at the camera's new place.
            self.speed = if self.fast >= 2 { speed } else { 0.0 };
        } else {
            self.fast = 0;
            self.speed += (speed - self.speed) * alpha;
        }
        if dt > 0.25 || speed > MOST_LEAD_SPEED {
            self.velocity = [0.0; 3];
            return self.velocity;
        }
        self.velocity =
            std::array::from_fn(|a| self.velocity[a] + (raw[a] - self.velocity[a]) * alpha);
        self.velocity
    }

    /// The speed the bands are drawn for, as of the last sighting.
    fn speed(&self) -> f64 {
        self.speed
    }
}

/// How long the frame thread waits for the scene thread to have packed the bricks of level 0 of
/// an Edit, in the frame after the one that made it: a few milliseconds. The pack is done at once
/// and is a few milliseconds of the thread's, so it is there; if the thread was busy with
/// something else for longer, the Edit shows the frame after instead of the frame thread waiting.
const EDIT_WAIT: std::time::Duration = std::time::Duration::from_millis(4);

/// Edits that [`World::edit`] gave to the scene thread and that the frame thread is to see in the
/// scene by the second look at it ([`World::recentre`]) after the oldest of them.
#[derive(Clone, Copy, Debug)]
struct UnshownEdit {
    /// The job of the newest: the thread does them in order, so it is done when they all are.
    newest: u64,
    /// Whether the frame that the oldest was made in has looked at the scene since. The next look
    /// is the one that waits for them.
    looked_since_oldest: bool,
}

/// The first solid Voxel along the camera's view through the middle of the screen.
pub(crate) fn centre_hit(scene: &Scene, camera: &Camera) -> Option<Hit> {
    let (cell, origin) = camera::split(camera.position);
    trace(
        scene,
        &Ray {
            cell,
            origin,
            dir: camera.forward().map(|c| c as f32),
        },
    )
}

impl World {
    /// `scene` must be the packing of the predictor's bricks.
    pub fn new(scene: Scene, predictor: Predictor, link: ClientEnd) -> Self {
        Self {
            scene,
            predictor,
            link,
            worker: None,
            stats: Stats::default(),
            loading_paused: false,
            still_view: false,
            still_cuts: StillCuts::default(),
            still_clock: StillClock::default(),
            still_hold: StillHold::default(),
            ghost: None,
            ghost_next: None,
            resim: std::time::Duration::ZERO,
            unshown_edit: None,
            edit_ticket: 0,
            edits_made: 0,
            taken_done: 0,
            outbox_wait: std::time::Duration::ZERO,
            lengthened: std::time::Duration::ZERO,
            edit_wait: EDIT_WAIT,
            lead: true,
            motion: Motion::default(),
            clock: Clock::real(),
            guard_hold: None,
        }
    }

    /// Says that the part of the Guard that was made before the world started is every cell that a
    /// camera within `reach` Voxels of `spawn` could see (#220, [`crate::loader::plan_guard`]):
    /// until the rest is made, a camera that is not the character's is to be held to that ball
    /// ([`World::hold_to_guard`]), which is what a camera that flies fast or jumps far before the
    /// Guard is complete gets, in place of sky past the cells that are there.
    pub fn with_guard_hold(mut self, spawn: [f64; 3], reach: f64) -> Self {
        self.guard_hold = Some((spawn, reach));
        self
    }

    /// Whether every cell of the Guard has been taken in (as of the scene the world has).
    pub fn guard_complete(&self) -> bool {
        self.loader().is_none_or(|loader| loader.guard_pending == 0)
    }

    /// `position`, a camera that flies, held to the ball that the Guard that is made so far covers
    /// ([`World::with_guard_hold`]) while the Guard is not complete, else as it is. The hold is the
    /// position, not the speed: a camera is free in a ball of 2 km and cannot see sky in it, and it
    /// is not told how fast it may go.
    pub fn hold_to_guard(&self, position: [f64; 3]) -> [f64; 3] {
        let Some((centre, reach)) = self.guard_hold else {
            return position;
        };
        if self.guard_complete() {
            return position;
        }
        let from: [f64; 3] = std::array::from_fn(|a| position[a] - centre[a]);
        let away = from.iter().map(|c| c * c).sum::<f64>().sqrt();
        if away <= reach {
            return position;
        }
        std::array::from_fn(|a| centre[a] + from[a] * reach / away)
    }

    /// Streams the Planet in with `loader`, on a thread of its own that follows the camera and takes
    /// in what the loader makes (see [`SceneWorker`]): [`World::recentre`] and [`World::stream`]
    /// only tell it where the camera is, and take what it has made. The scene, which must be of
    /// rings, moves to that thread, and the world keeps a copy to draw. It must have been made from
    /// the predictor's store, which must stream ([`brick_store::BrickStore::stream_from`]).
    pub fn with_loader(mut self, loader: Loader) -> Self {
        let scene = std::mem::take(&mut self.scene);
        // The scene thread remakes the coarse levels over what an Edit changed.
        self.predictor.defer_lod();
        let (worker, mirror, stats) =
            SceneWorker::start(scene, loader, self.predictor.shared_store());
        (self.worker, self.scene, self.stats) = (Some(worker), mirror, stats);
        self
    }

    /// [`World::with_loader`] with no threads, for a test: `loader` should be [`Loader::stepped`].
    /// The scene thread's work, the loader's too, is done on the calling thread by the code the
    /// thread runs, one step for each time the camera is told ([`World::recentre`]) and as many as a
    /// call that waits for the thread needs ([`World::sync`], [`World::set_bands`]). Nothing waits
    /// for time and nothing reads the machine's: the world has a clock that only the test moves
    /// ([`World::advance`]), which the scene thread and the camera's velocity read.
    pub fn with_stepped_loader(mut self, loader: Loader) -> Self {
        let scene = std::mem::take(&mut self.scene);
        self.predictor.defer_lod();
        self.clock = Clock::stepped();
        let (worker, mirror, stats) = SceneWorker::start_stepped(
            scene,
            loader,
            self.predictor.shared_store(),
            self.clock.clone(),
        );
        (self.worker, self.scene, self.stats) = (Some(worker), mirror, stats);
        self
    }

    /// Whether the world's scene thread is stepped (see [`World::with_stepped_loader`]).
    #[cfg(test)]
    pub fn is_stepped(&self) -> bool {
        self.clock.is_stepped()
    }

    /// Edits made in this world so far. [`World::edit`] makes none when nothing is aimed at or when
    /// the client knows the server would turn it down, so a caller that needs to know which of the
    /// two happened compares this before and after.
    pub fn edits_made(&self) -> u64 {
        self.edits_made
    }

    /// Whether the scene thread has packed the newest Edit made ([`World::edit`]) and this world has
    /// taken it in: the scene that is drawn shows it. Not the same as the wait for it being over,
    /// which a timeout also ends. False before any Edit.
    pub fn edit_taken(&self) -> bool {
        self.edit_ticket > 0 && self.taken_done >= self.edit_ticket
    }

    /// The longest that what the scene thread published waited in the outbox before this world took
    /// it, since this was last called (ADR 0006), if the counts are exact ([`World::count_exactly`]);
    /// zero when it was taken in the frame it was published in, and always zero otherwise.
    pub fn take_outbox_wait(&mut self) -> std::time::Duration {
        std::mem::take(&mut self.outbox_wait)
    }

    /// How long the frame thread spent, since this was last called, making the buffers of its copy
    /// of the scene longer for the buffers that grew (a `Vec` that is longer: a reallocation, and
    /// the new words written as zero), which is what a growth costs the frame thread besides
    /// pasting the words and copying them to the GPU. Zero if none grew.
    pub fn take_lengthened(&mut self) -> std::time::Duration {
        std::mem::take(&mut self.lengthened)
    }

    /// Moves a stepped world's clock on by `time`.
    pub fn advance(&self, time: std::time::Duration) {
        self.clock.advance(time);
    }

    /// The time on the world's clock.
    #[cfg(test)]
    pub fn time(&self) -> std::time::Instant {
        self.clock.now()
    }

    /// How long [`World::edit_due`] waits for an Edit at a look at the scene.
    #[cfg(test)]
    pub fn edit_wait_time(&self) -> std::time::Duration {
        self.edit_wait
    }

    /// What the loader is doing, as of the scene the world has, if there is one.
    pub fn loader(&self) -> Option<LoaderStats> {
        self.worker.as_ref().map(|_| self.stats.loader)
    }

    /// What the scene thread said of the scene and the loader with the words the world has. Empty
    /// before [`World::with_loader`].
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Whether [`World::recentre`] tells the scene thread how the camera moves, so that it
    /// prefetches along the velocity (on by default).
    #[cfg(test)]
    pub fn set_lead(&mut self, on: bool) {
        self.lead = on;
    }

    /// Gives the store another budget while the scene thread runs (a test of what it does when
    /// there is less, or more, to want than it had room for; the streaming bench's walk, which is held
    /// to the complete set).
    pub fn set_budget(&self, budget: brick_store::Budget) {
        self.predictor
            .shared_store()
            .write()
            .expect("the store's lock is not poisoned")
            .set_budget(budget);
    }

    /// Has the scene thread count soft misses in [`Stats::soft_misses`], a few times a second (a
    /// diagnostic: the real-time bench's, #162).
    pub fn count_soft_misses(&self) {
        if let Some(worker) = &self.worker {
            worker.count_soft_misses(true);
        }
    }

    /// Has the scene thread look at every split brick for a hole every few seconds, and the time of
    /// the parts of its rounds be added up (see [`World::scene_thread_time`]): what the real-time
    /// bench asks for (#162).
    pub fn measure_scene_thread(&self) {
        if let Some(worker) = &self.worker {
            worker.count_holes(true);
            worker.time_work(true);
        }
    }

    /// How long the scene thread has spent on the parts of its rounds since
    /// [`World::measure_scene_thread`].
    pub fn scene_thread_time(&self) -> Spent {
        self.worker
            .as_ref()
            .map_or_else(Spent::default, SceneWorker::spent)
    }

    /// The longest round and update of the scene thread since the last call (see
    /// [`SceneWorker::take_longest`]); zeros without a thread.
    pub fn take_longest(&self) -> Longest {
        self.worker
            .as_ref()
            .map_or_else(Longest::default, SceneWorker::take_longest)
    }

    /// Tells the scene thread the most words that a buffer of the scene may have, which is what
    /// the device allows a storage buffer to be (`Renderer::buffer_limit_words`).
    pub fn set_buffer_limit(&self, words: usize) {
        if let Some(worker) = &self.worker {
            worker.set_buffer_limit(words);
        }
    }

    /// Tells the scene thread how long to take in what the loader made for while the loader's
    /// workers are as far ahead as they may be (see [`SceneWorker::set_backed_up_slice`]).
    pub fn set_backed_up_slice(&self, slice: std::time::Duration) {
        if let Some(worker) = &self.worker {
            worker.set_backed_up_slice(slice);
        }
    }

    /// Has the scene thread count the soft misses (by level) and the holes for every publish, so
    /// that the stats the world has are exact for each frame (see [`SceneWorker::count_exactly`]).
    pub fn count_exactly(&self) {
        if let Some(worker) = &self.worker {
            worker.count_exactly();
        }
    }

    /// Has the scene thread count everything exactly once, for the scene as it is: the soft misses
    /// by level, the holes and the words by level (see [`World::count_exactly`]), and takes what it
    /// published, which the caller copies to the GPU. For the real-time bench, at full size, where
    /// counting at every publish is a pass over every brick each time.
    pub fn count_once_exactly(&mut self) -> Result<Option<SceneUpdate>, RingsFull> {
        let Some(worker) = &self.worker else {
            return Ok(None);
        };
        worker.count_exactly();
        // A job is published with the counts of its own round, which are exact now.
        let counted = worker.edit(Vec::new());
        worker.stop_counting_exactly();
        counted?;
        self.take_ready(true)
    }

    /// Has the scene thread count the soft misses now, and look at every split brick for a hole if
    /// `holes`, for the camera it was last told and what it has taken in, and takes what it published,
    /// which the caller copies to the GPU. For the real-time bench: a look at the end of each
    /// path, and a count of the soft misses that was made after the camera stopped (see
    /// [`SceneWorker::look_now`]).
    pub fn look_now(&mut self, holes: bool) -> Result<Option<SceneUpdate>, RingsFull> {
        let Some(worker) = &self.worker else {
            return Ok(None);
        };
        worker.look_now(holes)?;
        self.take_ready(true)
    }

    /// Whether the scene thread is looking for holes at this moment (see
    /// [`SceneWorker::looking_for_holes`]); false with no scene thread.
    pub fn looking_for_holes(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| w.looking_for_holes())
    }

    /// Bytes the scene's buffers take, used or not: what the GPU copy reserves (see
    /// [`Scene::reserved_bytes`]).
    pub fn reserved_bytes(&self) -> u64 {
        self.scene.reserved_bytes()
    }

    /// Holds back (or lets go of) what changes the scene while the view is Still (#94, #217): while
    /// paused, nothing the loader generated is taken in, the scene is not moved to the camera and
    /// coarsening takes no step, so the scene stays as it is and the reference image can
    /// accumulate. An Edit still goes through. The loader's threads stop at
    /// [`crate::loader::MAX_AHEAD`] jobs ahead, so what waits is bounded, and what the scene thread
    /// held back is done in the rounds after the pause ends. The game and the bench pause through
    /// [`World::hold_still`], which has the scene catch up with the camera first.
    ///
    /// Called once a frame, before the frame takes in what the scene thread made: it is also where
    /// the frame's cuts begin to be counted ([`World::still_cuts`]).
    pub fn pause_loading(&mut self, paused: bool) {
        self.loading_paused = paused;
        if let Some(worker) = &self.worker {
            worker.pause(paused);
        }
        self.begin_still_frame();
    }

    /// What a Still view does about the scene in this frame (#94, #217): when the view goes Still
    /// the scene first catches up with the camera, with loading going on and Primary drawn, and
    /// then loading is paused and the Reference view accumulates the scene as it is. `mode` is how
    /// the window is drawn and `still_frames` how long the view has been unchanged
    /// ([`Stillness`]); `camera` is where the frame thread's camera is. Returns the [`Hold`] for
    /// the frame, which the caller shows the Reference view by (only in [`Hold::Held`]).
    pub fn hold_still(&mut self, mode: Mode, still_frames: u32, camera: [f64; 3]) -> Hold {
        let caught_up = self.caught_up(camera);
        let lag = self.lag(camera);
        let now = self.clock.now();
        let hold = self
            .still_hold
            .step(mode, still_frames, caught_up, lag, now);
        self.pause_loading(hold == Hold::Held);
        hold
    }

    /// Everything that must not change for the view to be Still (CONTEXT.md): `view` (the camera, the
    /// sun and the window, whatever the caller calls that) and `bias`, and that no Edit has been
    /// made. Put it to a [`Stillness`] once a frame, and what that says to [`World::hold_still`].
    pub fn still_key<V>(&self, view: V, bias: f32) -> (V, f32, u64) {
        (view, bias, self.edits_made)
    }

    /// Where the Still view is: see [`StillHold::began`].
    pub fn still_hold(&self) -> &StillHold {
        &self.still_hold
    }

    /// Says the view is Still (or not) without pausing anything, for the bench's baseline: updates
    /// that are taken meanwhile are counted as cuts ([`World::still_cuts`]), as they are while
    /// loading is paused, which is how the game says the same thing. Called once a frame, before
    /// the frame takes in what the scene thread made, as [`World::pause_loading`] is.
    pub fn mark_still(&mut self, still: bool) {
        self.still_view = still;
        self.begin_still_frame();
    }

    /// The cuts counted so far, while loading was paused or the view marked Still.
    pub fn still_cuts(&self) -> StillCuts {
        self.still_cuts
    }

    fn cutting_still(&self) -> bool {
        self.loading_paused || self.still_view
    }

    /// Whether there is a loader and it is paused: see [`World::pause_loading`].
    pub fn loading_paused(&self) -> bool {
        self.loading_paused && self.worker.is_some()
    }

    /// Bricks that wait for the bricks under them to be resident. Zero, and nothing pending with
    /// the loader, once the resident set is complete for the bands.
    pub fn waiting(&self) -> usize {
        self.of_scene(|s| s.waiting, Scene::waiting)
    }

    /// Nodes that the scene thread has yet to look at again for a change of the bands (ADR 0009):
    /// zero once the scene is drawn as the speed's bands ask. (Also in [`World::waiting`].)
    pub fn rebanding(&self) -> usize {
        self.of_scene(|s| s.rebanding, Scene::rebanding)
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// What the scene thread said of the scene (`stat`) if there is one, else what the scene itself
    /// says (`scene`): the selection is not in the copy that is drawn.
    fn of_scene<T>(&self, stat: impl Fn(&Stats) -> T, scene: impl Fn(&Scene) -> T) -> T {
        match self.worker {
            Some(_) => stat(&self.stats),
            None => scene(&self.scene),
        }
    }

    /// The LOD bands the scene is selected by, if it is rings.
    pub fn bands(&self) -> Option<render::lod::Bands> {
        self.of_scene(|s| s.bands, Scene::bands)
    }

    /// Bricks drawn at `level` (see [`Scene::level_bricks`]).
    pub fn level_bricks(&self, level: usize) -> usize {
        self.of_scene(
            |s| s.level_bricks.get(level).copied().unwrap_or(0),
            |scene| scene.level_bricks(level),
        )
    }

    /// Bytes of the scene's buffers that hold something (see [`Scene::used_bytes`]).
    pub fn used_bytes(&self) -> u64 {
        self.of_scene(|s| s.slow.used_bytes, Scene::used_bytes)
    }

    /// The size of every table that is made roomy up front: the store's, the selection's, the
    /// loader's and the scene's (see [`crate::growth`]). The last three are as the scene thread
    /// last looked, a few times a second.
    pub fn table_sizes(&self) -> Vec<TableSize> {
        let mut sizes = self.predictor.store().table_sizes();
        match self.worker {
            Some(_) => sizes.extend(self.stats.slow.tables.iter().cloned()),
            None => sizes.extend(self.scene.table_sizes()),
        }
        sizes
    }

    /// The Planet's bricks as the client holds them.
    pub fn store(&self) -> std::sync::RwLockReadGuard<'_, brick_store::BrickStore> {
        self.predictor.store()
    }

    /// Puts the character on the Planet at `position` (Voxels), and tells the server. It falls
    /// from there once the ground around it is ready here and the server has said how far it
    /// may go (see [`sim::Predictor::walk`]).
    pub fn spawn(&mut self, position: [f64; 3]) -> Result<(), Stopped> {
        self.link.send(self.predictor.spawn(position))?;
        Ok(())
    }

    /// Runs one tick of the character on `input` here and on the server. Does nothing before
    /// the character has spawned, and while it waits for its ground.
    pub fn walk(&mut self, input: MoveInput) -> Result<(), Stopped> {
        if let Some(request) = self.predictor.walk(input) {
            self.link.send(request)?;
        }
        Ok(())
    }

    /// Bricks the character's ground has generated so far.
    pub fn ground_generated(&self) -> u64 {
        self.predictor.ground().generated()
    }

    /// Times resimulating after a desync has made a whole ball of ground at once.
    pub fn full_fills(&self) -> u64 {
        self.predictor.full_fills()
    }

    /// The time spent running inputs again after a desync since the last call.
    pub fn take_resim(&mut self) -> std::time::Duration {
        std::mem::take(&mut self.resim)
    }

    /// The time the character's ticks have spent making ground since the last call.
    pub fn take_fill_time(&mut self) -> std::time::Duration {
        self.predictor.take_fill_time()
    }

    /// What bounds the character, once the server has said.
    pub fn leash(&self) -> Option<net::Leash> {
        self.predictor.character().and_then(sim::Character::leash)
    }

    /// Where the character's eyes are, if it has spawned.
    pub fn eye(&self) -> Option<[f64; 3]> {
        self.predictor.character().map(sim::Character::eye)
    }

    /// Moves the rings of the Planet to centre on `camera`, a position in Voxels. Returns how
    /// the scene changed, if it did. A world that streams only tells the scene thread where the
    /// camera is, and returns what it has made since the last call, which is of an earlier camera:
    /// the rings are never half moved.
    pub fn recentre(&mut self, camera: [f64; 3]) -> Result<Option<SceneUpdate>, RingsFull> {
        let velocity = self.lead_velocity(camera, self.clock.now());
        // The bands' speed is the camera's own, whether or not it is looked ahead along.
        let speed = self.motion.speed();
        self.recentre_at(camera, velocity, speed)
    }

    /// The velocity [`World::recentre`] hands to the scene thread for the camera seen at `camera`
    /// at `now`: the measured one, or none if the lead is off.
    fn lead_velocity(&mut self, camera: [f64; 3], now: std::time::Instant) -> [f64; 3] {
        let velocity = self.motion.observe(camera, now);
        if self.lead { velocity } else { [0.0; 3] }
    }

    /// [`World::recentre`] with the camera's `velocity` given, in Voxels a second, for the lead
    /// (prefetch) to look ahead along: for a path that is not at the speed of the clock.
    pub fn recentre_moving(
        &mut self,
        camera: [f64; 3],
        velocity: [f64; 3],
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        let speed = velocity.iter().map(|v| v * v).sum::<f64>().sqrt();
        self.recentre_at(camera, velocity, speed)
    }

    /// [`World::recentre_moving`] with the `speed` the bands are drawn for given too (Voxels a
    /// second), which is the camera's whether or not it has a lead: a Zoom (#58) is drawn for its
    /// speed, along which prefetch does not look.
    pub fn recentre_at(
        &mut self,
        camera: [f64; 3],
        velocity: [f64; 3],
        speed: f64,
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        match &self.worker {
            Some(worker) => {
                worker.follow_at(camera, velocity, speed);
                // A stepped scene thread does its round now, with the camera it was just told.
                worker.step();
                let wait = self.edit_due()?;
                self.take_ready(wait)
            }
            None => {
                let update = self.scene.recentre(&self.predictor.store(), camera)?;
                if update.is_some() {
                    self.count_cut(Causes::default().with(Cause::Follow));
                }
                Ok(update)
            }
        }
    }

    /// Whether the Edits that [`World::edit`] gave to the scene thread are to be taken with a wait
    /// now. That is at the second look at the scene after the oldest: the first is the frame's
    /// that made it, when the thread has only just begun, and the next is a frame later, when it
    /// has long packed the bricks of level 0 (the coarse levels are remade after, a step at a
    /// time, and are not waited for). The wait is for [`EDIT_WAIT`] at the most.
    ///
    /// So an Edit shows in the frame after the one that made it, if the scene thread has packed it
    /// by [`EDIT_WAIT`] into that frame, which is the bound of C1 in
    /// `docs/research/scheduling-and-upload.md`; and else in the first look after it is packed. The
    /// frame thread waits [`EDIT_WAIT`] at the most for it.
    fn edit_due(&mut self) -> Result<bool, RingsFull> {
        let Some(edit) = &mut self.unshown_edit else {
            return Ok(false);
        };
        if !edit.looked_since_oldest {
            edit.looked_since_oldest = true;
            return Ok(false);
        }
        let newest = edit.newest;
        self.unshown_edit = None;
        match &self.worker {
            Some(worker) => worker.wait_for(newest, Some(self.edit_wait)),
            None => Ok(false),
        }
    }

    /// Waits for the scene thread to have looked at the camera and at pausing, and takes what it
    /// has made. Nothing is taken in after a pause that this has waited for.
    #[cfg(test)]
    pub fn sync(&mut self) -> Result<Option<SceneUpdate>, RingsFull> {
        if let Some(worker) = &self.worker {
            worker.sync()?;
        }
        self.take_ready(true)
    }

    /// How far the scene as the world has it (the one that is drawn) is behind `camera`, in
    /// Voxels: the distance from `camera`, the frame thread's, to where the scene thread had moved
    /// the rings to when it made what the world has taken.
    ///
    /// The scene thread tells the frame thread where it has moved the scene with every publish. A
    /// round that moved it and changed nothing publishes nothing, so the lag can be more than it
    /// is by the Voxels of such steps (a follow step is at most 64 at the default budget), until
    /// the camera has stood for two rounds with the scene at it, when that is published (see
    /// `Engine::arrived_unsaid`): a flight's rounds nearly all change something, and publish.
    pub fn lag(&self, camera: [f64; 3]) -> f64 {
        let shown = self.stats.camera.unwrap_or(camera);
        (0..3)
            .map(|a| (camera[a] - shown[a]).powi(2))
            .sum::<f64>()
            .sqrt()
    }

    /// Whether the scene has reached `camera` and nothing else is going to move it, for a Still
    /// view to be held (see [`StillHold`]): the drawn scene's lag is under [`CAUGHT_UP_LAG`], and
    /// no reband is pending ([`World::reband_pending`]), and the selection is not still looking
    /// again at the nodes after a flip of the horizon cap (#213). A world with no scene thread has
    /// nothing to follow.
    pub fn caught_up(&self, camera: [f64; 3]) -> bool {
        self.lag(camera) < CAUGHT_UP_LAG && !self.reband_pending() && !self.stats.absorbing
    }

    /// Whether the bands have changed and nodes are yet to be looked at again for them, or the
    /// camera's speed has fallen and the scene thread is about to change them (a reband, ADR
    /// 0009): what every ring selects is about to change, so the scene is not one to hold, and
    /// a Still view waits for it as it waits for the camera. The scene thread says when this
    /// begins and ends with a publish (see `Stats::same_counts`); while the scene is held it does
    /// not look at them (`Engine::round`), so a reband that is listed after the hold began is
    /// pending until the view is not Still.
    pub fn reband_pending(&self) -> bool {
        self.rebanding() > 0 || (self.worker.is_some() && self.stats.speed_releasing)
    }

    /// An Edit that changes nothing: how long it takes the scene thread to come to it, and to
    /// publish. For measuring what an Edit waits for.
    #[cfg(test)]
    pub fn edit_wait(&mut self) -> Result<std::time::Duration, RingsFull> {
        let started = std::time::Instant::now();
        if let Some(worker) = &self.worker {
            worker.edit(Vec::new())?;
        }
        self.take_ready(true)?;
        Ok(started.elapsed())
    }

    /// Waits `time` for an Edit in the frame after it, instead of [`EDIT_WAIT`]: a test on a build
    /// that packs a large Edit in more than a few milliseconds.
    #[cfg(test)]
    pub fn set_edit_wait(&mut self, time: std::time::Duration) {
        self.edit_wait = time;
    }

    /// The scene thread's end, for a test to talk to it directly.
    #[cfg(test)]
    pub fn worker(&self) -> &SceneWorker {
        self.worker.as_ref().expect("the world streams")
    }

    /// A frame begins, as the one call a frame of [`World::pause_loading`] or [`World::mark_still`]
    /// says (not a look at the scene: the game looks twice in a frame): counts the frame if the view
    /// is Still, and says whether an update taken in it is a cut (see [`StillClock`]).
    fn begin_still_frame(&mut self) {
        let still = self.cutting_still();
        self.still_cuts.frames += u64::from(still);
        self.still_clock.begin_frame(still);
    }

    /// An update that was made of `causes` is about to be drawn: a cut if the Reference view was
    /// being shown (see [`StillClock`]).
    fn count_cut(&mut self, causes: Causes) {
        let cutting = self.cutting_still();
        self.still_clock
            .update(cutting, &mut self.still_cuts, causes);
    }

    /// Takes what the scene thread has made, and brings the copy of the scene that is drawn up to
    /// date with it. Returns how it changed, for copying to the GPU.
    fn take_ready(&mut self, wait: bool) -> Result<Option<SceneUpdate>, RingsFull> {
        let Some(worker) = &self.worker else {
            return Ok(None);
        };
        let ready = worker.take(wait)?;
        self.taken_done = self.taken_done.max(ready.done);
        if let Some(published) = ready.published_at {
            self.outbox_wait = self.outbox_wait.max(self.clock.since(published));
        }
        if ready.mirror.is_some() || ready.patch.is_some() {
            let cutting = self.loading_paused || self.still_view;
            self.still_clock
                .update(cutting, &mut self.still_cuts, ready.causes);
        }
        let update = if let Some(mirror) = ready.mirror {
            // The old copy is freed by the thread that made the new one.
            worker.discard_scene(std::mem::replace(&mut self.scene, *mirror));
            Some(SceneUpdate::Rebuilt)
        } else if let Some(copy) = ready.patch {
            self.lengthened += self.scene.paste(&copy);
            // The update is a copy of the patch: what the scene thread allocated goes back to it.
            let update = Some(SceneUpdate::Rings(copy.patch.clone()));
            worker.recycle(copy);
            update
        } else {
            None
        };
        if let Some(stats) = ready.stats {
            let before = std::mem::replace(&mut self.stats, stats);
            if !Arc::ptr_eq(&before.slow, &self.stats.slow) {
                worker.discard(before.slow);
            }
        }
        Ok(update)
    }

    /// Tells the scene thread where the camera is and takes what it has made, and `show`s how that
    /// changed the scene (to copy it to the GPU): bricks that the loader generated were taken in,
    /// and the bricks they are under split, by the scene thread, since the last call. Nothing
    /// happens in a world that does not stream.
    pub fn stream<E: From<RingsFull>>(
        &mut self,
        camera: [f64; 3],
        mut show: impl FnMut(&Scene, &SceneUpdate) -> Result<(), E>,
    ) -> Result<(), E> {
        if let Some(update) = self.recentre(camera)? {
            show(&self.scene, &update)?;
        }
        Ok(())
    }

    /// Selects the bricks again for other LOD `bands` (a new bias). Returns how the scene
    /// changed, if it did.
    pub fn set_bands(
        &mut self,
        bands: render::lod::Bands,
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        match &self.worker {
            Some(worker) => {
                worker.set_bands(bands)?;
                self.take_ready(true)
            }
            None => self.scene.set_bands(&self.predictor.store(), bands),
        }
    }

    /// The Edit that `tool` makes where the camera looks: at the first solid Voxel along the view
    /// or, to place, with the Brush sitting on the face that was hit (the Voxel in front of it,
    /// and then as far again out from the face as the Brush reaches). The Brush is turned to face
    /// the way the camera looks (see [`rotation_for`]). `None` if there is no Voxel, or the centre
    /// is further than [`AIM_REACH`] from the camera.
    pub fn aim(&self, camera: &Camera, tool: &Tool) -> Option<EditOp> {
        self.aim_hit(camera, tool).map(|(op, _)| op)
    }

    /// The Dig that holding the button would make, and how long it takes: by the hardness of the
    /// Material at the Voxel aimed at (see [`dig_seconds`]). `None` if the tool is not a Dig, or
    /// there is nothing to aim at.
    pub fn dig_aim(&self, camera: &Camera, tool: &Tool) -> Option<(EditOp, f64)> {
        if tool.mode != EditMode::Dig {
            return None;
        }
        let (op, hit) = self.aim_hit(camera, tool)?;
        Some((
            op,
            dig_seconds(target_hardness(Some(Material(hit.material)))),
        ))
    }

    /// [`World::aim`] with the Voxel that the view hit.
    fn aim_hit(&self, camera: &Camera, tool: &Tool) -> Option<(EditOp, Hit)> {
        let hit = centre_hit(&self.scene, camera)?;
        let out = if tool.mode == EditMode::Place {
            i32::from(tool.radius) + 1
        } else {
            0
        };
        let centre: [i32; 3] = std::array::from_fn(|a| hit.voxel[a] + hit.normal[a] * out);
        let away: f64 = (0..3)
            .map(|a| {
                let d = f64::from(centre[a]) + 0.5 - camera.position[a];
                d * d
            })
            .sum();
        if away > AIM_REACH * AIM_REACH {
            return None;
        }
        let centre = VoxelCoord(centre);
        let rotation = rotation_for(centre, tool.alignment, camera.forward());
        Some((tool.op(centre, rotation), hit))
    }

    /// Makes the ghost for where the camera looks, if there is a Voxel to apply `tool` to. It is
    /// kept while the Edit stays the same and the Planet where it reads stays as it is. When it
    /// has to be made again it is not at once if the last took long (see [`GHOST_BACKOFF`]).
    pub fn show_ghost(&mut self, camera: &Camera, tool: &Tool) {
        self.show_ghost_at(camera, tool, std::time::Instant::now());
    }

    /// [`World::show_ghost`] at the time `now`.
    fn show_ghost_at(&mut self, camera: &Camera, tool: &Tool, now: std::time::Instant) {
        let Some(op) = self.aim(camera, tool) else {
            self.ghost = None;
            return;
        };
        let foreseen = self.predictor.foresee(&op);
        let stale = self.ghost.as_ref().is_none_or(|g| g.op != op || g.dirty);
        let waiting = self.ghost.is_some() && self.ghost_next.is_some_and(|next| now < next);
        if stale && !waiting {
            let started = std::time::Instant::now();
            let plan = self.predictor.plan_bricks(&op);
            self.ghost = Some(Ghost::new(op, &plan));
            self.ghost_next = Some(now + started.elapsed() * GHOST_BACKOFF);
        }
        if let Some(ghost) = &mut self.ghost {
            // Grey if the server is known to turn it down: for what the op says, or for the
            // Voxels that the ghost shows it would change.
            ghost.blocked = foreseen.is_err() || !self.predictor.can_change(ghost.changed);
        }
    }

    /// Stops showing the ghost.
    pub fn hide_ghost(&mut self) {
        self.ghost = None;
    }

    /// The Voxels the next click would change, as [`World::show_ghost`] last found them.
    pub fn ghost(&self) -> Option<&Ghost> {
        self.ghost.as_ref()
    }

    /// Applies `tool` where the camera looks, and asks the server for it. The Voxels are changed
    /// here, which is the planning and the writing of the bricks (a few milliseconds for the
    /// largest Brush), and the rest is the scene thread's (the coarse levels, the packing): the
    /// Edit is on the screen within a frame, the first [`World::recentre`] that comes after the
    /// next, and not in this call, which returns how the scene changed only if something else the
    /// thread had made was ready. An Edit that the client knows the server will turn down (see
    /// [`sim::Predictor::foresee`]) is not made. A world with no scene thread (see
    /// [`World::with_loader`]) does all of it in the call, and returns how the scene changed.
    pub fn edit(&mut self, camera: &Camera, tool: &Tool) -> Result<Option<SceneUpdate>, Stopped> {
        let Some(op) = self.aim(camera, tool) else {
            return Ok(None);
        };
        let Ok((request, changed)) = self.predictor.edit_checked(op) else {
            return Ok(None);
        };
        self.edits_made += 1;
        self.link.send(request)?;
        let Some(worker) = &self.worker else {
            return Ok(self.refresh(&changed)?);
        };
        // The Voxels are changed. The scene thread remakes the coarse levels over them and packs
        // them, and the frame thread goes on: see [`World::recentre`], which takes it.
        if let Some(ghost) = &mut self.ghost {
            ghost.dirty |= ghost.touches(&changed);
        }
        // A second Edit before the first is waited for keeps the first one's frame.
        let newest = worker.edit_later(changed);
        self.edit_ticket = newest;
        self.unshown_edit = Some(UnshownEdit {
            newest,
            looked_since_oldest: self.unshown_edit.is_some_and(|e| e.looked_since_oldest),
        });
        Ok(self.take_ready(false)?)
    }

    /// Takes in what the server has sent. Returns how the scene changed, if it did.
    pub fn poll(&mut self) -> Result<Option<SceneUpdate>, Stopped> {
        let mut changed = Vec::new();
        loop {
            match self.link.try_recv() {
                Ok(message) => {
                    let desyncs = self.predictor.body_desyncs();
                    let started = std::time::Instant::now();
                    changed.extend(self.predictor.receive(message));
                    // A message that made the prediction differ ran the inputs again.
                    if self.predictor.body_desyncs() != desyncs {
                        self.resim += started.elapsed();
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(Disconnected.into()),
            }
        }
        Ok(self.refresh(&changed)?)
    }

    /// Packs the bricks that changed.
    fn refresh(
        &mut self,
        changed: &[brick_store::BrickCoord],
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        if changed.is_empty() {
            return Ok(None);
        }
        // What the ghost would change has changed with them, if they are where it reads.
        if let Some(ghost) = &mut self.ghost {
            ghost.dirty |= ghost.touches(changed);
        }
        match &self.worker {
            // The scene thread packs the bricks, and says so when it has: the Edit is in what it
            // made, with whatever else it had made since the last call.
            Some(worker) => {
                worker.edit(changed.to_vec())?;
                self.take_ready(true)
            }
            None => self
                .scene
                .update(&self.predictor.store(), changed)
                .map(Some),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::{Brick, BrickCoord, BrickStore, Material};
    use materials::MaterialTable;
    use net::{
        ClientMessage, EditMode, Leash, RejectReason, ServerEnd, ServerMessage, Shape, loopback,
    };

    const STONE: Material = Material(7);

    /// A wall of stone, well outside the Planet: Voxels x 4000..4032, y 0..32, z -32..0, in bricks of their own.
    fn wall() -> BrickStore {
        let mut store = BrickStore::new();
        for x in 500..504 {
            for y in 0..4 {
                for z in -4..0 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(STONE));
                }
            }
        }
        store
    }

    /// Looking at the wall's middle from in front of it.
    fn camera() -> Camera {
        Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 16.5, 0.0])
    }

    fn world() -> (World, ServerEnd) {
        let store = wall();
        let scene = Scene::new(&store, None);
        let (client, server) = loopback();
        (World::new(scene, Predictor::new(store), client), server)
    }

    /// A world whose camera has been going 150 Voxels a second for 200 frames.
    fn world_with_flight(base: std::time::Instant) -> (World, ServerEnd) {
        let (mut world, server) = world();
        for n in 0..=200 {
            world.lead_velocity([0.75 * f64::from(n), 0.0, 0.0], frames(base, n));
        }
        (world, server)
    }

    /// `n` frames of 5 ms after `base`.
    fn frames(base: std::time::Instant, n: u32) -> std::time::Instant {
        base + std::time::Duration::from_millis(5) * n
    }

    #[test]
    fn the_velocity_is_a_running_mean_of_the_frames_that_resets_on_a_gap_or_a_jump() {
        let base = std::time::Instant::now();
        let mut motion = Motion::default();
        // The first sighting has nothing to measure against.
        assert_eq!(motion.observe([0.0; 3], base), [0.0; 3]);
        // 0.75 Voxels every 5 ms along x is 150 Voxels a second, which the mean comes to.
        let mut velocity = [0.0; 3];
        for n in 1..=200 {
            velocity = motion.observe([0.75 * f64::from(n), 0.0, 0.0], frames(base, n));
        }
        assert!((velocity[0] - 150.0).abs() < 1.0, "{velocity:?}");
        assert!(velocity[1].abs() < 1e-9 && velocity[2].abs() < 1e-9);
        // The same moment twice gives the velocity there was.
        let now = frames(base, 200);
        assert_eq!(motion.observe([151.0, 0.0, 0.0], now), velocity);
        // A frame that came after more than a quarter of a second: no velocity.
        let late = now + std::time::Duration::from_millis(260);
        assert_eq!(motion.observe([190.0, 0.0, 0.0], late), [0.0; 3]);
        // Measuring starts again from there.
        let next = late + std::time::Duration::from_millis(5);
        let v = motion.observe([190.75, 0.0, 0.0], next);
        assert!(v[0] > 0.0 && v[0] < 150.0, "{v:?}");
        // A jump faster than MOST_LEAD_SPEED (a teleport): no velocity either.
        let jump = next + std::time::Duration::from_millis(5);
        let far = 190.75 + MOST_LEAD_SPEED * 0.005 * 2.0;
        assert_eq!(motion.observe([far, 0.0, 0.0], jump), [0.0; 3]);
    }

    #[test]
    fn recentre_hands_on_the_measured_velocity_only_while_the_lead_is_on() {
        let base = std::time::Instant::now();
        let (mut world, _server) = world();
        let mut on = [0.0; 3];
        for n in 0..=200 {
            on = world.lead_velocity([0.75 * f64::from(n), 0.0, 0.0], frames(base, n));
        }
        assert!((on[0] - 150.0).abs() < 1.0, "{on:?}");
        world.set_lead(false);
        let off = world.lead_velocity([151.5, 0.0, 0.0], frames(base, 202));
        assert_eq!(off, [0.0; 3]);
    }

    #[test]
    fn the_bands_get_the_cameras_speed_whether_or_not_there_is_a_lead_and_however_fast() {
        let base = std::time::Instant::now();
        let (mut world, _server) = world();
        world.set_lead(false);
        for n in 0..=200 {
            let lead = world.lead_velocity([0.75 * f64::from(n), 0.0, 0.0], frames(base, n));
            assert_eq!(lead, [0.0; 3]);
        }
        // No lead, and the bands still have the speed (150 Voxels a second).
        assert!(
            (world.motion.speed() - 150.0).abs() < 1.0,
            "{}",
            world.motion.speed()
        );
        // A Zoom faster than the lead is cut at: no lead (it is a jump), and the speed of a flight
        // once it is two frames in, so that it is not drawn at full detail.
        let step = MOST_LEAD_SPEED * 0.005 * 2.0;
        let mut at = 150.0;
        for n in 201..=205 {
            at += step;
            let lead = world.lead_velocity([at, 0.0, 0.0], frames(base, n));
            assert_eq!(lead, [0.0; 3]);
        }
        assert!(
            world.motion.speed() > MOST_LEAD_SPEED,
            "{}",
            world.motion.speed()
        );
        // A single jump in a flight is a teleport, not a speed: it is no speed (so that the depth
        // the flight had is not given back a frame at a time after the scene is made again at the
        // camera's new place; the bands hold a depth over a single sample of a lower speed), and
        // a flight that goes on after it is a flight at once.
        let (mut world, _server) = world_with_flight(base);
        assert!((world.motion.speed() - 150.0).abs() < 1.0);
        world.lead_velocity([1.0e9, 0.0, 0.0], frames(base, 203));
        assert_eq!(world.motion.speed(), 0.0);
    }

    #[test]
    fn a_click_digs_at_the_voxel_in_the_middle_of_the_view_and_asks_the_server() {
        let (mut world, server) = world();
        let update = world.edit(&camera(), &Tool::default()).unwrap();
        assert!(
            matches!(update, Some(SceneUpdate::Patched { .. })),
            "{update:?}"
        );
        // The wall's front face is z = -1; the view's middle is Voxel (4016, 16, -1).
        assert_eq!(
            server.try_recv(),
            Ok(ClientMessage::Edit {
                client_seq: 0,
                op: EditOp {
                    // Looking along the wall's face, level with the horizon, the Brush is turned to
                    // face that way: up is +x here, and the view is down -z.
                    rotation: rotation_for(
                        VoxelCoord([4016, 16, -1]),
                        net::Alignment::Local,
                        camera().forward()
                    ),
                    ..EditOp::dig_sphere(VoxelCoord([4016, 16, -1]), crate::tool::DEFAULT_RADIUS)
                }
            })
        );
        // The hole is drawn at once: the same ray now reaches deeper than the front face.
        let deeper = centre_hit(world.scene(), &camera()).unwrap();
        assert!(deeper.voxel[2] < -1, "{deeper:?}");
    }

    #[test]
    fn a_dig_in_a_scene_of_rings_is_drawn_at_once_and_the_rings_follow_the_camera() {
        let mut store = wall();
        store.build_lod(4);
        let bound = render::scene::Bound {
            centre: [0; 3],
            radius: 4200,
        };
        let scene = Scene::rings(
            &store,
            camera().position,
            render::lod::Bands::with_d0(64.0),
            bound,
        )
        .unwrap();
        let (client, _server) = loopback();
        let mut world = World::new(scene, Predictor::new(store), client);

        let update = world.edit(&camera(), &Tool::default()).unwrap();
        assert!(matches!(update, Some(SceneUpdate::Rings(_))), "{update:?}");
        let deeper = centre_hit(world.scene(), &camera()).unwrap();
        assert!(deeper.voxel[2] < -1, "{deeper:?}");

        // Where the camera is, the bands stay; a little way off, the bricks that change level are
        // patched; and a jump of more than a band's width makes the scene again.
        assert_eq!(world.recentre(camera().position), Ok(None));
        let [x, y, z] = camera().position;
        assert!(matches!(
            world.recentre([x + 60.0, y, z]),
            Ok(None | Some(SceneUpdate::Rings(_)))
        ));
        assert!(matches!(
            world.recentre([x + 900.0, y, z]),
            Ok(Some(SceneUpdate::Rebuilt))
        ));
    }

    #[test]
    fn the_character_waits_for_the_leash_then_lands_on_the_wall() {
        let (mut world, server) = world();
        assert_eq!(world.eye(), None);
        world.walk(MoveInput::default()).unwrap();
        assert_eq!(
            server.try_recv(),
            Err(TryRecvError::Empty),
            "not spawned yet"
        );

        // Above the middle of the wall's +x face (x = 4032).
        let spawn = [4036.0, 16.0, -16.0];
        world.spawn(spawn).unwrap();
        for _ in 0..10 {
            world.walk(MoveInput::default()).unwrap();
        }
        // Standing where it was put, eyes 1.6 m up: it waits.
        let eye = world.eye().unwrap();
        assert!((eye[0] - (4036.0 + 32.0)).abs() < 0.1, "{eye:?}");

        // The server's word: a ball round the spawn's brick, wide enough for the whole wall. The
        // wall's bricks are in the store, and the rest of the ground is air.
        server
            .send(ServerMessage::Leash {
                spawn: 1,
                from: 0,
                leash: Leash {
                    centre: [4036.0, 20.0, -12.0],
                    radius: 40.0,
                },
            })
            .unwrap();
        world.poll().unwrap();
        for _ in 0..200 {
            world.walk(MoveInput::default()).unwrap();
        }
        let eye = world.eye().unwrap();
        assert!((4032.0 + 31.0..4032.0 + 33.0).contains(&eye[0]), "{eye:?}");

        assert_eq!(
            server.try_recv(),
            Ok(ClientMessage::Spawn {
                spawn: 1,
                position: spawn
            })
        );
        let mut inputs = 0;
        while let Ok(ClientMessage::Input { seq, .. }) = server.try_recv() {
            assert_eq!(seq, inputs);
            inputs += 1;
        }
        // The ten ticks that waited for the server's word were not sent: all the others were.
        assert_eq!(inputs, 200);
    }

    fn place_cube() -> Tool {
        Tool {
            mode: EditMode::Place,
            shape: Shape::Cube,
            radius: 4,
            material: Material(35),
            ..Tool::default()
        }
    }

    #[test]
    fn placing_sits_the_brush_on_the_face_that_is_aimed_at() {
        let (mut world, server) = world();
        let update = world.edit(&camera(), &place_cube()).unwrap();
        assert!(update.is_some());
        // The wall's front face is z = -1, so the air Voxel in front of it is z = 0, and a cube of
        // radius 4 that sits on the face is centred 4 further out.
        let sent = server.try_recv();
        let Ok(ClientMessage::Edit { op, .. }) = sent else {
            panic!("{sent:?}");
        };
        assert_eq!(op.centre, VoxelCoord([4016, 16, 4]));
        assert_eq!((op.mode, op.material), (EditMode::Place, Material(35)));
        // It fills z = 0 to 8, all of it in the air, and none of it in the wall.
        assert_eq!(
            world.store().get(VoxelCoord([4016, 16, 0])),
            Some(Material(35))
        );
        assert_eq!(
            world.store().get(VoxelCoord([4016, 16, 8])),
            Some(Material(35))
        );
        assert_eq!(world.store().get(VoxelCoord([4016, 16, 9])), None);
        assert_eq!(
            world.store().get(VoxelCoord([4016, 16, -1])),
            Some(Material(7))
        );
        // And it is drawn: the same ray now stops where the cube's face is.
        let hit = centre_hit(world.scene(), &camera()).unwrap();
        assert_eq!(hit.voxel[2], 8);
    }

    #[test]
    fn the_brush_is_turned_to_face_the_way_the_camera_looks_and_not_on_the_grid() {
        let (world, _server) = world();
        let tool = place_cube();
        let op = world.aim(&camera(), &tool).unwrap();
        assert_eq!(
            op.rotation,
            rotation_for(op.centre, tool.alignment, camera().forward())
        );
        // Seen from the side, it is the Voxels on that side that it faces.
        let side = Camera::looking_at([4040.5, 16.5, 20.5], [4016.5, 16.5, 0.0]);
        let from_the_side = world.aim(&side, &tool).unwrap();
        assert_eq!(
            from_the_side.rotation,
            rotation_for(from_the_side.centre, tool.alignment, side.forward())
        );
        let grid = Tool {
            alignment: net::Alignment::World,
            ..tool
        };
        assert_eq!(world.aim(&side, &grid).unwrap().rotation, 0);
    }

    #[test]
    fn a_dig_takes_the_time_of_the_hardness_of_the_material_aimed_at() {
        let table = MaterialTable::built_in();
        let (world, _server) = world();
        let tool = Tool::default();
        let (op, seconds) = world.dig_aim(&camera(), &tool).unwrap();
        assert_eq!(Some(op), world.aim(&camera(), &tool));
        let hit = centre_hit(world.scene(), &camera()).unwrap();
        let hardness = table.get(Material(hit.material)).unwrap().hardness;
        assert_eq!(seconds, dig_seconds(hardness));
        // The other tools are instant: a hold does not dig for them.
        let place = Tool {
            mode: EditMode::Place,
            ..tool
        };
        assert_eq!(world.dig_aim(&camera(), &place), None);
    }

    #[test]
    fn nothing_is_aimed_at_beyond_the_reach() {
        let (mut world, server) = world();
        let far = Camera::looking_at([4016.5, 16.5, 220.5], [4016.5, 16.5, 0.0]);
        assert_eq!(world.aim(&far, &Tool::default()), None);
        assert!(world.edit(&far, &Tool::default()).unwrap().is_none());
        assert_eq!(server.try_recv(), Err(TryRecvError::Empty));
        // Just inside it, there is. To place, the Brush's centre is nearer than what is aimed at.
        let near = Camera::looking_at([4016.5, 16.5, 70.5], [4016.5, 16.5, 0.0]);
        assert!(world.aim(&near, &Tool::default()).is_some());
        assert!(world.aim(&near, &place_cube()).is_some());
    }

    #[test]
    fn the_ghost_is_exactly_the_voxels_the_click_changes_and_the_op_it_sends() {
        let mut non_empty = 0;
        for mode in [
            EditMode::Dig,
            EditMode::Place,
            EditMode::Smooth,
            EditMode::Flatten,
        ] {
            for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
                let (mut world, server) = world();
                let tool = Tool {
                    mode,
                    shape,
                    radius: 6,
                    material: Material(35),
                    ..Tool::default()
                };
                world.show_ghost(&camera(), &tool);
                let ghost = world.ghost().expect("something is aimed at").clone();
                let before = world.store().clone();

                world.edit(&camera(), &tool).unwrap();
                let Ok(ClientMessage::Edit { op, .. }) = server.try_recv() else {
                    panic!("no Edit sent");
                };
                assert_eq!(ghost.op, op, "the ghost is of the op that is sent");
                let after = world.store();
                // The Voxels that differ now are the ghost's, and are what it said they become.
                let mut differing = 0;
                let changes = ghost.changes();
                for x in 4016 - 20..=4016 + 20 {
                    for y in -4..=36 {
                        for z in -26..=26 {
                            let voxel = VoxelCoord([x, y, z]);
                            let now = after.get(voxel);
                            let change = changes.iter().find(|c| c.voxel == voxel);
                            assert_eq!(before.get(voxel) != now, change.is_some(), "{op:?}");
                            differing += usize::from(change.is_some());
                            if let Some(change) = change {
                                assert_eq!(change.to, now);
                            }
                        }
                    }
                }
                assert_eq!(differing, changes.len(), "{op:?}");
                non_empty += usize::from(differing > 0);
                // An Edit that changed the Planet where the ghost reads calls for a new ghost.
                assert_eq!(world.ghost().unwrap().dirty, differing > 0);
            }
        }
        // Rounding off a flat wall has nothing to do, but the others do.
        assert!(non_empty >= 9, "{non_empty}");
    }

    #[test]
    fn the_ghost_follows_the_aim_and_goes_when_nothing_is_aimed_at() {
        let (mut world, _server) = world();
        let tool = Tool::default();
        let start = std::time::Instant::now();
        let later = |millis| start + std::time::Duration::from_millis(millis);
        assert!(world.ghost().is_none());
        world.show_ghost_at(&camera(), &tool, start);
        let first = world.ghost().unwrap().op;
        // Aimed a little to the side, it is a different Edit.
        let aside = Camera::looking_at([4016.5, 16.5, 20.5], [4026.5, 16.5, 0.0]);
        world.show_ghost_at(&aside, &tool, later(10_000));
        assert_ne!(world.ghost().unwrap().op, first);
        let up = Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 100.0, 20.5]);
        world.show_ghost_at(&up, &tool, later(20_000));
        assert!(world.ghost().is_none());
        world.show_ghost_at(&camera(), &tool, later(30_000));
        world.hide_ghost();
        assert!(world.ghost().is_none());
    }

    #[test]
    fn a_ghost_that_took_long_to_make_is_not_made_again_for_a_new_aim_until_a_while_has_gone() {
        let (mut world, _server) = world();
        let tool = Tool::default();
        let start = std::time::Instant::now();
        let later = |millis| start + std::time::Duration::from_millis(millis);
        world.show_ghost_at(&camera(), &tool, start);
        let first = world.ghost().unwrap().op;
        // Say that it took 10 ms: the next is not before 8 times that.
        world.ghost_next = Some(later(80));
        let aside = Camera::looking_at([4016.5, 16.5, 20.5], [4026.5, 16.5, 0.0]);
        world.show_ghost_at(&aside, &tool, later(40));
        assert_eq!(world.ghost().unwrap().op, first, "the last one is shown");
        world.show_ghost_at(&aside, &tool, later(81));
        assert_ne!(world.ghost().unwrap().op, first);
        // Nothing aimed at takes the ghost away at once, whatever the time.
        let up = Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 100.0, 20.5]);
        world.show_ghost_at(&up, &tool, later(82));
        assert!(world.ghost().is_none());
    }

    #[test]
    fn a_ghost_is_made_again_for_a_change_where_it_reads_and_not_for_one_elsewhere() {
        let (mut world, _server) = world();
        let tool = Tool::default();
        let start = std::time::Instant::now();
        world.show_ghost_at(&camera(), &tool, start);
        let ghost = world.ghost().unwrap().op;
        // A brick far from the sphere of 1 m at (4016, 16, -1): the ghost is as it was, and not
        // asked to be made again.
        world.refresh(&[BrickCoord([500, 0, -4])]).unwrap();
        assert!(!world.ghost().unwrap().dirty);
        // One it reads: it is, when the time allows.
        world.refresh(&[BrickCoord([502, 1, -1])]).unwrap();
        assert!(world.ghost().unwrap().dirty);
        let waits = start + std::time::Duration::from_millis(1);
        world.ghost_next = Some(start + std::time::Duration::from_secs(1));
        world.show_ghost_at(&camera(), &tool, waits);
        assert!(world.ghost().unwrap().dirty, "not yet");
        world.show_ghost_at(&camera(), &tool, start + std::time::Duration::from_secs(2));
        assert!(!world.ghost().unwrap().dirty);
        assert_eq!(world.ghost().unwrap().op, ghost);
    }

    #[test]
    fn an_edit_that_is_over_the_allowance_is_not_made_and_its_ghost_is_grey() {
        let (mut world, server) = world();
        // The smallest Brush, so that it is the 20 Edits a second and not the Voxels that run out.
        let tool = Tool {
            radius: 0,
            ..Tool::default()
        };
        world.show_ghost(&camera(), &tool);
        assert!(!world.ghost().unwrap().blocked);
        // 20 Edits in a second are the most; the 21st is not made, nor sent.
        for _ in 0..20 {
            assert!(world.edit(&camera(), &tool).unwrap().is_some());
        }
        assert!(world.edit(&camera(), &tool).unwrap().is_none());
        let mut sent = 0;
        while let Ok(ClientMessage::Edit { .. }) = server.try_recv() {
            sent += 1;
        }
        assert_eq!(sent, 20);
        world.show_ghost_at(
            &camera(),
            &tool,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        );
        assert!(world.ghost().unwrap().blocked);
        // The character's ticks bring the allowance back, three for an Edit.
        for _ in 0..3 {
            world.walk(MoveInput::default()).unwrap();
        }
        assert!(world.edit(&camera(), &tool).unwrap().is_some());
    }

    #[test]
    fn an_edit_that_would_change_more_voxels_than_are_left_is_not_made_and_its_ghost_is_grey() {
        let (mut world, server) = world();
        // The biggest cube, 81 Voxels a side, on the wall's face, in the air: 531 441 Voxels of the 576 000.
        let tool = Tool {
            mode: EditMode::Place,
            shape: Shape::Cube,
            radius: sim::MAX_BRUSH_RADIUS,
            material: Material(35),
            alignment: net::Alignment::World,
        };
        let back = Camera::looking_at([4016.5, 16.5, 60.5], [4016.5, 16.5, 0.0]);
        world.show_ghost(&back, &tool);
        assert!(!world.ghost().unwrap().blocked);
        assert!(world.edit(&back, &tool).unwrap().is_some());
        // Another on the face of that one would change as many, and there are 44 559 left.
        world.show_ghost_at(
            &back,
            &tool,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        );
        let ghost = world.ghost().unwrap();
        assert!(ghost.changed > 44_559 && ghost.blocked, "{}", ghost.changed);
        assert!(world.edit(&back, &tool).unwrap().is_none());
        let mut sent = 0;
        while let Ok(ClientMessage::Edit { .. }) = server.try_recv() {
            sent += 1;
        }
        assert_eq!(sent, 1);
        // A small one fits, and is made.
        // (from further back: the camera is in the big cube).
        let small = Tool { radius: 4, ..tool };
        let further = Camera::looking_at([4016.5, 16.5, 140.5], [4016.5, 16.5, 0.0]);
        assert!(world.edit(&further, &small).unwrap().is_some());
    }

    #[test]
    fn placing_beyond_the_ball_the_scene_is_clipped_to_grows_it_and_the_new_voxels_are_drawn() {
        let mut store = wall();
        store.build_lod(4);
        // The wall reaches to x = 4031, and the ball a little past that.
        let bound = render::scene::Bound {
            centre: [0; 3],
            radius: 4040,
        };
        let eye = Camera::looking_at([4060.5, 16.5, -15.5], [4000.0, 16.5, -15.5]);
        let scene = Scene::rings(
            &store,
            eye.position,
            render::lod::Bands::with_d0(64.0),
            bound,
        )
        .unwrap();
        let (client, _server) = loopback();
        let mut world = World::new(scene, Predictor::new(store), client);
        let far = Camera::looking_at([4100.5, 16.5, -15.5], [4000.0, 16.5, -15.5]);
        let stops = |world: &World| centre_hit(world.scene(), &far).map(|hit| hit.voxel[0]);
        assert_eq!(stops(&world), Some(4031));

        // A cube of 41 Voxels a side sitting on the wall's +x face: its far side is at x = 4072.
        let tool = Tool {
            radius: 20,
            ..place_cube()
        };
        world.edit(&eye, &tool).unwrap();
        let grown = world.scene().bound.unwrap();
        assert!(grown.radius > 4072, "{grown:?}");
        assert_eq!(
            stops(&world),
            Some(4072),
            "the new Voxels are not clipped away"
        );
    }

    #[test]
    fn a_view_that_goes_still_catches_the_scene_up_and_then_holds_it_and_the_cap_ends_the_wait() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let at = |s: u64| now + Duration::from_secs(s);
        let mut hold = StillHold::default();
        // Not Still: open, whatever the scene is doing.
        assert_eq!(
            hold.step(Mode::Reference, 0, false, 900.0, at(0)),
            Hold::Open
        );
        assert_eq!(hold.step(Mode::Reference, 1, true, 0.0, at(0)), Hold::Open);
        // Primary mode is never held.
        assert_eq!(hold.step(Mode::Primary, 99, true, 0.0, at(0)), Hold::Open);
        // Still, with the scene behind the camera: loading goes on, and Primary is drawn.
        assert_eq!(
            hold.step(Mode::Reference, 2, false, 900.0, at(1)),
            Hold::CatchingUp
        );
        assert_eq!(
            hold.step(Mode::Reference, 3, false, 400.0, at(2)),
            Hold::CatchingUp
        );
        assert_eq!(hold.began(), None);
        // The scene reaches the camera: held, and how the wait went is kept.
        assert_eq!(hold.step(Mode::Reference, 4, true, 0.5, at(3)), Hold::Held);
        let began = hold.began().expect("the hold has begun");
        assert_eq!(began.lag_when_still, 900.0);
        assert_eq!(began.lag_when_held, 0.5);
        assert_eq!(began.waited, Duration::from_secs(2));
        assert!(!began.capped);
        // Held for as long as the view is Still, though what says "caught up" does not.
        assert_eq!(hold.step(Mode::Reference, 5, false, 3.0, at(4)), Hold::Held);
        assert_eq!(hold.began(), Some(began));
        // Anything that changes the view opens it, and the next Still view waits again.
        assert_eq!(hold.step(Mode::Reference, 0, true, 0.0, at(5)), Hold::Open);
        assert_eq!(hold.began(), None);
        assert_eq!(
            hold.step(Mode::Reference, 2, false, 70.0, at(6)),
            Hold::CatchingUp
        );

        // A scene that does not get there is held at the cap, and it is said that it was.
        let late = now + Duration::from_secs(6) + CATCH_UP_CAP;
        assert_eq!(
            hold.step(
                Mode::Reference,
                9,
                false,
                60.0,
                late - Duration::from_millis(1)
            ),
            Hold::CatchingUp
        );
        assert_eq!(
            hold.step(Mode::Reference, 10, false, 55.0, late),
            Hold::Held
        );
        let began = hold.began().expect("the hold has begun");
        assert!(began.capped);
        assert_eq!(began.waited, CATCH_UP_CAP);
        assert_eq!(began.lag_when_still, 70.0);
        assert_eq!(began.lag_when_held, 55.0);
    }

    #[test]
    fn an_update_is_a_cut_only_if_the_reference_view_was_being_shown_as_the_renderer_counts() {
        // Frames of a view that is Still from frame 0, as `Still::plan` sees them: the Reference
        // view is shown in a frame after STILL_FRAMES frames in a row with the same view and no
        // update, and an update in a frame is part of that frame's key.
        let run = |updates_in: &[usize], frames: usize| -> Vec<usize> {
            let mut clock = StillClock::default();
            let mut cuts = StillCuts::default();
            let mut cut_in = Vec::new();
            for frame in 0..frames {
                clock.begin_frame(true);
                if updates_in.contains(&frame) {
                    let before = cuts.cuts;
                    clock.update(true, &mut cuts, Causes::default().with(Cause::Follow));
                    if cuts.cuts > before {
                        cut_in.push(frame);
                    }
                }
            }
            cut_in
        };
        // Updates in every frame never show the Reference view: no cuts.
        assert_eq!(run(&[0, 1, 2, 3, 4, 5, 6], 7), Vec::<usize>::new());
        // Nothing in frames 0 and 1 shows the Reference view in frame 2, and an update in frame 3
        // cuts it; an update in frame 1 would have kept it from showing, and is no cut.
        assert_eq!(run(&[3], 8), vec![3]);
        assert_eq!(run(&[1], 8), Vec::<usize>::new());
        // After the cut at 3 the view shows again from frame 5: an update in 6 cuts it, one in 4
        // or 5 (the frames before) does not.
        assert_eq!(run(&[3, 6], 8), vec![3, 6]);
        assert_eq!(run(&[3, 5], 8), vec![3]);
        assert_eq!(run(&[3, 4], 8), vec![3]);
        // A cut restarts it: the next update has to follow STILL_FRAMES quiet frames too.
        assert_eq!(run(&[3, 6, 8, 9, 12], 14), vec![3, 6, 12]);
        // A view that was not Still the frame before has no Reference view to cut.
        let mut clock = StillClock::default();
        let mut cuts = StillCuts::default();
        for _ in 0..10 {
            clock.begin_frame(false);
        }
        clock.begin_frame(true);
        clock.update(true, &mut cuts, Causes::default());
        assert_eq!(cuts.cuts, 0);
    }

    #[test]
    fn a_click_on_the_sky_does_nothing() {
        let (mut world, server) = world();
        let up = Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 100.0, 20.5]);
        assert!(world.edit(&up, &Tool::default()).unwrap().is_none());
        assert_eq!(server.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn an_ack_keeps_the_hole_and_a_reject_fills_it_again() {
        let (mut world, server) = world();
        // Holes of half a metre in radius: the wall can take two of them one behind the other.
        let small = Tool {
            radius: 10,
            ..Tool::default()
        };
        let hit = |world: &World| centre_hit(world.scene(), &camera()).unwrap().voxel;
        let whole = hit(&world);
        world.edit(&camera(), &small).unwrap();
        let first = hit(&world);
        world.edit(&camera(), &small).unwrap();
        let second = hit(&world);
        assert!(whole != first && first != second);
        let Ok(ClientMessage::Edit { op, .. }) = server.try_recv() else {
            panic!("no Edit sent");
        };

        server
            .send(ServerMessage::Applied {
                global_seq: 1,
                op,
                ack: Some(0),
            })
            .unwrap();
        assert!(world.poll().unwrap().is_none(), "the prediction was right");
        assert_eq!(hit(&world), second);

        // The second dig was predicted on top of the first; rejecting it leaves the first.
        server
            .send(ServerMessage::Rejected {
                client_seq: 1,
                reason: RejectReason::BrushTooLarge,
            })
            .unwrap();
        assert!(world.poll().unwrap().is_some());
        assert_eq!(hit(&world), first);
    }

    #[test]
    fn a_server_that_is_gone_is_an_error() {
        let (mut world, server) = world();
        drop(server);
        assert_eq!(world.poll().err(), Some(Stopped::Disconnected));
        assert_eq!(
            world.edit(&camera(), &Tool::default()).err(),
            Some(Stopped::Disconnected)
        );
    }
}
