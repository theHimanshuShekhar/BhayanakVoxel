//! The client's view of the Planet: the bricks it predicts on, the packed copy that gets drawn,
//! and the loopback to the server. All Edits come through here, and reach the Voxels only via
//! [`sim::Predictor`], which calls [`sim::apply`].

use std::sync::Arc;

use brick_store::{Material, TableSize, VoxelCoord};
use net::{ClientEnd, Disconnected, EditMode, EditOp, MoveInput, TryRecvError};
use render::reference::{Hit, Ray, trace};
use render::{Camera, Mode, RingsFull, Scene, SceneUpdate, camera};
use sim::{Predictor, dig_seconds, target_hardness};

use crate::ghost::Ghost;
use crate::loader::Loader;
use crate::scene_worker::{LoaderStats, SceneWorker, Stats};
use crate::tool::{Tool, rotation_for};

/// How many frames in a row the view must be unchanged before the reference image is taken to be
/// accumulating it, and loading pauses for it.
pub const STILL_FRAMES: u32 = 2;

/// Whether loading pauses, given the mode and how many frames in a row the view (camera, sun,
/// size and LOD bias) has been unchanged: only while the reference accumulates a still view.
/// Anything that changes it, moving included, lets loading go the same frame.
pub fn loading_paused_for(mode: Mode, still_frames: u32) -> bool {
    mode == Mode::Reference && still_frames >= STILL_FRAMES
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
    /// The Planet no longer fits the rings' buffers; the scene is not to be used again.
    RingsFull,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "the server stopped"),
            Self::RingsFull => write!(f, "{RingsFull}"),
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
    fn from(_: RingsFull) -> Self {
        Self::RingsFull
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
    /// How long [`World::edit_due`] waits: [`EDIT_WAIT`], or more in a test on a slow build.
    edit_wait: std::time::Duration,
    /// Whether [`World::recentre`] tells the scene thread how the camera is moving, for prefetch.
    lead: bool,
    motion: Motion,
}

/// The camera's velocity as the frames show it: a running mean over about a tenth of a second, so
/// that a jittery camera does not move where prefetch looks every frame.
#[derive(Clone, Copy, Debug, Default)]
struct Motion {
    last: Option<(std::time::Instant, [f64; 3])>,
    velocity: [f64; 3],
}

/// Faster than this, in Voxels a second (400 m/s, several times the flight speed), the camera
/// has jumped (a teleport, a Zoom's cut) and is not looked ahead along.
const MOST_LEAD_SPEED: f64 = 400.0 * brick_store::VOXELS_PER_METRE;

impl Motion {
    /// The velocity after the camera was seen at `camera` at `now`.
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
        if dt > 0.25 || speed > MOST_LEAD_SPEED {
            self.velocity = [0.0; 3];
            return self.velocity;
        }
        let alpha = (dt / 0.1).min(1.0);
        self.velocity =
            std::array::from_fn(|a| self.velocity[a] + (raw[a] - self.velocity[a]) * alpha);
        self.velocity
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
            ghost: None,
            ghost_next: None,
            resim: std::time::Duration::ZERO,
            unshown_edit: None,
            edit_wait: EDIT_WAIT,
            lead: true,
            motion: Motion::default(),
        }
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

    /// What the loader is doing, as of the scene the world has, if there is one.
    pub fn loader(&self) -> Option<LoaderStats> {
        self.worker.as_ref().map(|_| self.stats.loader)
    }

    /// What the scene thread said of the scene and the loader with the words the world has. Empty
    /// before [`World::with_loader`].
    #[cfg(test)]
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
    /// there is less, or more, to want than it had room for).
    #[cfg(test)]
    pub fn set_budget(&self, budget: brick_store::Budget) {
        self.predictor
            .shared_store()
            .write()
            .expect("the store's lock is not poisoned")
            .set_budget(budget);
    }

    /// Has the scene thread count soft misses in [`Stats::soft_misses`] (a diagnostic).
    #[cfg(test)]
    pub fn count_soft_misses(&self) {
        self.worker().count_soft_misses(true);
    }

    /// Holds back (or lets go of) what the loader generates: while paused, nothing it generated
    /// is taken in, so the scene stays as it is and the reference image can accumulate. The
    /// loader's threads stop at [`crate::loader::MAX_AHEAD`] jobs ahead, so what waits is bounded.
    pub fn pause_loading(&mut self, paused: bool) {
        self.loading_paused = paused;
        if let Some(worker) = &self.worker {
            worker.pause(paused);
        }
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
    #[cfg(test)]
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
        let velocity = self.lead_velocity(camera, std::time::Instant::now());
        self.recentre_moving(camera, velocity)
    }

    /// The velocity [`World::recentre`] hands to the scene thread for the camera seen at `camera`
    /// at `now`: the measured one, or none if the lead is off.
    fn lead_velocity(&mut self, camera: [f64; 3], now: std::time::Instant) -> [f64; 3] {
        if self.lead {
            self.motion.observe(camera, now)
        } else {
            [0.0; 3]
        }
    }

    /// [`World::recentre`] with the camera's `velocity` given, in Voxels a second, for the lead
    /// (prefetch) to look ahead along: for a path that is not at the speed of the clock.
    pub fn recentre_moving(
        &mut self,
        camera: [f64; 3],
        velocity: [f64; 3],
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        match &self.worker {
            Some(worker) => {
                worker.follow_moving(camera, velocity);
                let wait = self.edit_due()?;
                self.take_ready(wait)
            }
            None => self.scene.recentre(&self.predictor.store(), camera),
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

    /// How far the scene as the world has it is behind `camera`, in Voxels: where the scene thread
    /// had moved the rings to, when it made what the world has taken.
    #[cfg(test)]
    pub fn lag(&self, camera: [f64; 3]) -> f64 {
        let shown = self.stats.camera.unwrap_or(camera);
        (0..3)
            .map(|a| (camera[a] - shown[a]).powi(2))
            .sum::<f64>()
            .sqrt()
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

    /// Takes what the scene thread has made, and brings the copy of the scene that is drawn up to
    /// date with it. Returns how it changed, for copying to the GPU.
    fn take_ready(&mut self, wait: bool) -> Result<Option<SceneUpdate>, RingsFull> {
        let Some(worker) = &self.worker else {
            return Ok(None);
        };
        let ready = worker.take(wait)?;
        let update = if let Some(mirror) = ready.mirror {
            // The old copy is freed by the thread that made the new one.
            worker.discard_scene(std::mem::replace(&mut self.scene, *mirror));
            Some(SceneUpdate::Rebuilt)
        } else if let Some(copy) = ready.patch {
            self.scene.paste(&copy);
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
