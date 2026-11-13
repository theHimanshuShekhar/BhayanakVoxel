//! The scene's selection and packing on a thread of their own (#127, ADR 0006).
//!
//! Following the camera and taking in the bricks the loader makes cost the frame thread from a
//! millisecond to tens of them a frame (the selection decides which bricks change level, and each
//! region that gains or loses one is packed again), in proportion to how fast the camera moves
//! and how fast the Planet fills in. So the frame thread does none of it. This thread owns the
//! [`Scene`] of rings (the selection, the packed words, the free space) and the [`Loader`]. It
//! reads the store through the lock that the client's [`sim::Predictor`] shares, and writes to it
//! only to hold what the loader generated. What it makes it publishes as a [`Ready`]: the words
//! that changed, copied out, and counts to go with them. The frame thread takes the newest
//! ([`SceneWorker::take`]), writes the words into its own copy of the scene (see
//! [`Scene::mirror`]) and copies them to the GPU: a few memory copies, whatever the camera did.
//!
//! **What the frame thread sees is always a whole state.** Every [`Ready`] is the scene after
//! the thread finished a job: the words, the region records and the table that go together, and
//! nothing from a job half done. A [`Ready`] that the frame thread has not taken when the next is
//! made is merged with it, in the outbox and under its lock, never taken out: the words that
//! either wrote are copied from the scene as it is then, so that taking the one is the same as
//! taking both, and a copy of all of the scene that is waiting has the later changes pasted into it
//! instead of being made again. The counts in a [`Ready`] are of the same state as its words.
//!
//! **Eviction** (#141, ADR 0004) runs here too, so that the frame thread pays for none of it. The
//! store only grows as the loader holds bricks; when it holds more than its
//! [`brick_store::Budget`] allows, this thread takes the children of bricks that are no longer
//! wanted out of it, a few at a time under the write lock (see [`Engine::evict`]).
//!
//! **Edits** do not wait for the camera, and the frame thread does not wait for them (#156). It
//! changes level 0 of the store (the predictor does, under the lock's write side, waiting for a
//! read that is going on), which is a few milliseconds for the largest Brush, and then
//! [`SceneWorker::edit_later`]s with the bricks changed and goes on. This thread remakes the
//! coarse levels over them (the predictor leaves that to it: [`sim::Predictor::defer_lod`]) and
//! packs them ahead of anything else it has to do but the job it is in the middle of, and
//! publishes. The Edit is on the screen the first frame that takes what is published, so at most
//! one frame later than the frame that made it if the thread takes less than a frame over it, as it
//! does for the largest Brush (see ADR 0006). What the server says that takes an Edit back or
//! changes the Voxels (other players' Edits) is still waited for: [`SceneWorker::edit`].

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use brick_store::{BrickCoord, BrickStore, Candidate, LodWork, TableSize, VOXELS_PER_METRE};
use render::lod::{AHEAD, Bands, Class, LEVELS, brick_distance};
use render::{PatchCopy, RingPatch, RingsFull, Scene, SceneUpdate, UploadCost};
use sim::SharedStore;

use crate::clock::Clock;
use crate::loader::{Arrival, Loader};

/// The time this thread spends taking in bricks before it looks at what the frame thread has
/// asked of it (a new camera, an Edit) and publishes what it has done.
const SLICE: Duration = Duration::from_millis(2);

/// How much the scene thread lets wait for the frame thread to upload (#145): it stops following the
/// camera and taking in what the loader made while what it has published and not had taken, and
/// what it has packed since, comes to this much, and goes on when the frame thread has taken it.
/// So a frame uploads about this much at the most, in this many copies (see
/// [`render::RingPatch::cost`]), instead of whatever piled up: the baseline's worst frame
/// copied 37 MB in 12 841 writes. The work is not dropped: the loader's arrivals wait in its queue
/// and the camera is followed from where the scene is, so what is not packed yet is packed in
/// the frames after, in the order the camera and the loader asked, which no frame draws a brick
/// out of: every patch is whole, and the words of a brick are in the patch that lists it.
///
/// It is a limit on what is added, checked between operations, and it is **reached** when the
/// cost is at least the budget in bytes or in copies (`>=`): work is held back from then on. One
/// batch of arrivals ([`BATCH`] bricks' children at the most) or one follow step can take it
/// over, by what that costs. Two rules keep arrivals from starving behind the follow steps, which
/// are checked first in a round (#145): the first batch of arrivals of a round is taken even over
/// the budget, if the outbox is empty (so that a frame always carries some of the loader's work
/// while the camera moves), and a further batch is taken only if a batch like the last (its
/// cost, kept under the budget so that with nothing waiting a further batch is always tried) would
/// still be under the budget after it. The last, not the largest: one big batch, such as one of
/// the fill's, must not hold back every later one. An Edit is packed whatever the backlog is (it is never behind streamed
/// bricks: it goes into the same patch, ahead of what is not yet packed). A teleport (the camera
/// put further than a jump in one call) or other bands pack the whole scene and are not limited
/// either (`Rebuilt`), and nor is the copy of a growth. **A follow step is cut** ([`FOLLOW_SHARE`]):
/// a camera that is further than a quarter of the budget's worth of its move is walked to in
/// steps, so the one operation that takes what waits to the budget is a batch or a quarter-budget
/// follow step, not a whole walk (the 4090's first run followed a second of flight, 17 to 34 MB, in
/// one). So a frame takes up to **the budget, plus the one operation that took what waits to it,
/// plus the round's first batch of arrivals** (taken whenever the outbox is empty, so after a
/// follow step that crossed the budget a frame carries two operations beyond it), **plus the words
/// between joined ranges**; the project owner accepted that as "bounded by the budget" on 2026-11-13 (the last comment on #145)
/// (`docs/agents/benchmarking.md`).
///
/// **How the value was chosen:** from the 4090's real-time bench, not the stepped one. A frame's
/// upload costs about 0.2 ms a MB and under a microsecond a copy there (`upload.time` of the run of
/// `9539899`: 0.31 ms p99 for 146 copies and 1 MB, 7.5 ms for 1 890 copies and 34 MB), so 8 MiB is
/// under 2 ms and 2 048 copies about 1.5 ms: **3 or 4 ms is the most a frame is to spend on
/// copying**, under half the frame-thread CPU gate. The copies are over the 1 400 that a baseline
/// fill frame made (about 2 MB; frame time p99 4.9 ms, #162) so that the budget is not what bounds
/// a fill: the first value, 128 copies, was, and held the fill to 1 MB a frame (56 217 of its
/// 60 443 frames were cut, and it took 1 035 s against 254 to 265 s).
pub const UPLOAD_BUDGET: UploadCost = UploadCost {
    bytes: 8 << 20,
    writes: 2048,
};
/// How long it sleeps when there was nothing to do but the loader has bricks on their way, before
/// it looks again: the arrivals do not wake it. With nothing asked for it sleeps until it is told.
const IDLE: Duration = Duration::from_millis(1);
/// The longest a thread that holds work back for the budget sleeps before it looks again. It is
/// woken when the frame thread takes what waits, so this is only for what is not told: it was a
/// millisecond (`IDLE`), which a timed wait on Windows rounds up to a clock tick of about 15 ms, so
/// that on the 4090 about every second frame of a fill had nothing to upload.
const THROTTLED_WAIT: Duration = Duration::from_millis(20);
/// The share of the budget that one step of following the camera is to cost, as a fraction
/// (numerator, denominator): a camera that is further from the scene than that is walked to in
/// steps, each packed, published and uploaded in its turn, and not in one operation that is
/// several times the budget (the 4090's flights followed in one step of 17 to 34 MB, and a scene
/// that was behind by more than a jump after them was made again, 871 MB).
const FOLLOW_SHARE: (u64, u64) = (1, 4);

/// The least that a Voxel of the camera's move is taken to cost, a little under the 4090's flights
/// (34 KB and 1.9 copies a Voxel at 2 m): a step of following the camera is never longer than
/// [`FOLLOW_SHARE`] of the budget over this, 64 Voxels at the default budget, whatever the steps
/// before it cost. A step that moved the scene a Voxel and changed nothing costs nothing, and a
/// rebuild is counted as costing nothing over a long distance, so what the steps cost cannot be
/// all that sizes the next one (it was 1 028 Voxels at full size once, the 34 MB operation of the
/// first 4090 run).
const FOLLOW_MIN_COST: (f64, f64) = (32.0 * 1024.0, 2.0);

/// How much a follow step's length changes by after one step, at the most, up and down: a step
/// that cost half of what it may goes on twice as far, and one that cost over twice goes half.
const FOLLOW_STEP_CHANGE: (f64, f64) = (0.5, 2.0);

/// The shortest follow step, as a fraction of its limit (see [`Engine::follow_limit`]). A step's
/// cost has a part that does not depend on its length: the regions that it rewrites are written
/// whole, so on the 4090 a step of 64 Voxels cost 3.5 MB and one of 4 cost 2.2 MB. A length that
/// is made to fit a share of the budget by the steps' cost therefore fell to 4 Voxels and stayed
/// there (measured, 240 steps of 4 Voxels covered 1 200 Voxels in 7.4 s, 2 MB each: eight times
/// the bytes of steps of 64 for the same walk, and a step costs the scene thread as long a Voxel
/// at 4 as at 64).
const FOLLOW_STEP_LEAST: f64 = 0.25;

/// How many jobs of the loader are done in one step of a stepped scene thread (see
/// [`SceneWorker::step`]): about what the real workers make between two frames of a test, so that a
/// stepped fill takes about as many frames. Only the stepped mode reads it.
const STEP_JOBS: usize = 256;
/// The most steps a stepped wait for a job takes before it says that the job is never done.
const MOST_STEPS_FOR_A_JOB: usize = 100_000;
/// How often it works out the sizes of the tables, which costs a walk over the scene's free
/// ranges.
const TABLES_EVERY: Duration = Duration::from_millis(250);

/// Most bricks' children taken in at a time, between looks at the clock. How many a slice takes in
/// at a time is what its time has left over what one costs, so a slice with room takes in many at
/// once: the scene packs the regions they are in once, not once for each.
const BATCH: usize = 128;
/// The share of the time left in a slice that a batch is expected to take, as a fraction: an
/// arrival that costs more than was expected then takes the slice over by less, and the next batch
/// is sized by what this one cost.
const BATCH_SHARE: (u128, u128) = (3, 5);
/// Arrivals taken in at a time even when the slice has room for fewer, so that a slice with little
/// left still makes progress.
const LEAST_BATCH: u128 = 8;
/// The most that one arrival is expected to cost, however slow the last ones were, so that one
/// slow batch (a table that grew) does not stop taking bricks in for the slices after it.
const MOST_ARRIVAL_COST: Duration = Duration::from_micros(500);
/// What taking in one brick's children is expected to cost before it has been measured.
const ARRIVAL_COST: Duration = Duration::from_micros(40);
/// Most bricks asked of the loader in one slice (the rest wait for the next): the first call has
/// every brick the Guard leaves to split, and the table that holds them is not made in a slice.
const WANTED_PER_SLICE: usize = 20_000;

/// The longest eviction holds the store's write lock for at a time: the frame thread reads the
/// store, and waits for a writer, so eviction takes the lock for a few units at a time.
const EVICT_HOLD: Duration = Duration::from_micros(500);
/// How long it lets go of the store after a hold, for a reader that waits for the lock to take it
/// (as [`COARSE_PAUSE`] does).
const EVICT_PAUSE: Duration = Duration::from_micros(50);
/// Most holds of the write lock in one round of the thread's work, each with a pause after.
const EVICT_HOLDS: usize = 3;
/// The time one round of looking at the units spends, between looks at what the frame thread asked
/// of the thread. A part of the units is looked at under the read lock, which is let go of between
/// parts.
const EVICT_LOOK: Duration = Duration::from_millis(2);
/// When eviction has done all it can and the store is still over its budget (what is left is all
/// wanted), it is not begun again until the store has grown by this fraction of the overflow
/// (the room between the budget and its limit): looking at every unit is not free.
const EVICT_RETRY: (usize, usize) = (1, 4);

/// At the memory cap (ADR 0004): when the store is over its limit and everything left is wanted,
/// the farthest bands are drawn coarser a step at a time instead of a wanted brick being dropped,
/// as (first level, factor) of [`render::lod::Bands::coarsened`]; the first few step in the
/// bands nearest the Guard, the last ones reach nearly to the camera.
const COARSENING: [(usize, f64); 6] = [(8, 1.5), (7, 1.5), (6, 1.5), (5, 1.5), (4, 2.0), (2, 2.0)];
/// How long a step of coarsening stands before the next, or before one is taken back: the scene
/// is selected and packed again for each.
#[cfg(not(test))]
const COARSEN_HOLD: Duration = Duration::from_secs(10);
#[cfg(test)]
const COARSEN_HOLD: Duration = Duration::from_millis(300);
/// A step is taken back when what the scene would want with it taken back (worked out from the
/// nodes it wants now, see [`Engine::relax`]) is under this fraction of the store's budget, as
/// (numerator, denominator). A step is taken at the limit, 10% over the budget at the most (see
/// `BUDGET_OVERFLOW` in `main`), so a step taken back leaves room: it is not taken again at once.
const RELAX_BELOW: (usize, usize) = (19, 20);

/// A step that was taken back and taken again stays until the camera has moved this fraction of
/// `d0`: the bands' model of what a step wants (`Bands::weight`) is for flat ground.
const BARRED_FOR: f64 = 4.0;

/// How often the soft misses are counted, when a diagnostic asks for them.
const SOFT_MISSES_EVERY: Duration = Duration::from_millis(100);

/// How often every split brick is looked at for a hole, when the real-time bench asks for it
/// (#162). The look is one pass over every node of the selection with the store's read lock held:
/// a third of a second at full size, so it is not done every frame, and the bench reports how much
/// of the thread's time it took (`Spent::hole_scan`).
const HOLES_EVERY: Duration = Duration::from_secs(5);

/// The places that prefetch looks at, in seconds along the camera's velocity (ADR 0004: a lead of
/// 1 to 2 s).
const LEAD: [f64; AHEAD] = [1.0, 2.0];
/// Slower than this, in Voxels a second (1 m/s), the camera is standing and there is no lead.
const LEAD_FROM_SPEED: f64 = VOXELS_PER_METRE;

/// `bands` at `step` of [`COARSENING`] (0 is as they are).
fn coarsened(bands: Bands, step: usize) -> Bands {
    match step.checked_sub(1).and_then(|i| COARSENING.get(i)) {
        Some(&(from, factor)) => bands.coarsened(from, factor),
        None => bands,
    }
}

/// How many arrivals the next batch takes in, given the `left` of the slice and what one is
/// expected to `cost`: [`BATCH_SHARE`] of the room, at least [`LEAST_BATCH`] if there is room for
/// that many, and at most [`BATCH`]. None if there is no room for one. The batch is expected to
/// take no more than `left`.
fn batch_size(left: Duration, cost: Duration) -> Option<usize> {
    let room = left.as_nanos() / cost.as_nanos().max(1);
    if room == 0 {
        return None;
    }
    let most = (room * BATCH_SHARE.0 / BATCH_SHARE.1)
        .max(room.min(LEAST_BATCH))
        .min(BATCH as u128);
    Some(most as usize)
}

/// What the loader is doing, as of a [`Ready`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoaderStats {
    /// Bricks asked for whose children have not been taken in (see [`Loader::pending`]).
    pub pending: usize,
    /// Bricks the workers have asked the generator for, since the start.
    pub generated: u64,
    /// Jobs the workers have taken that have not been taken in.
    #[cfg(test)]
    pub ahead: usize,
    /// Jobs a worker has taken and not finished.
    #[cfg(test)]
    pub in_flight: usize,
    /// Jobs in the workers' queue, and tickets marked as given up on there. Only tests look, as
    /// finding out takes the loader's lock.
    #[cfg(test)]
    pub queued: usize,
    #[cfg(test)]
    pub cancelled: usize,
}

impl LoaderStats {
    fn of(loader: &Loader) -> Self {
        #[cfg(test)]
        let (queued, cancelled) = loader.queue_sizes();
        Self {
            pending: loader.pending(),
            generated: loader.generated(),
            #[cfg(test)]
            ahead: loader.ahead(),
            #[cfg(test)]
            in_flight: loader.in_flight(),
            #[cfg(test)]
            queued,
            #[cfg(test)]
            cancelled,
        }
    }
}

/// What the scene and the loader are, as of a [`Ready`]: what the frame thread cannot ask the
/// scene (which is on this thread) or its own copy of it (which has the words and not the
/// selection). Nothing in it is allocated for each one: the frame thread does not free what this
/// thread allocated (see [`SceneWorker::discard`]).
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Bricks that wait for the bricks under them to be resident.
    pub waiting: usize,
    /// Bricks drawn at each level (see [`Scene::level_bricks`]).
    pub level_bricks: [usize; LEVELS],
    pub bands: Option<Bands>,
    /// Steps of coarsening at the memory cap (see [`COARSENING`]), 0 for none.
    pub coarsening: usize,
    /// Bricks drawn coarser than the bands ask because their children are not resident; counted
    /// only if [`SceneWorker::count_soft_misses`] says to.
    pub soft_misses: usize,
    /// [`Stats::soft_misses`] by the level of the brick drawn whole (the Ring that is behind), and
    /// the bricks that are split with the bricks under them not resident (holes, which are none);
    /// both counted with the soft misses, and only when counted exactly (see
    /// [`SceneWorker::count_exactly`]).
    pub soft_misses_by_level: [usize; LEVELS],
    pub holes: usize,
    /// What the thread has done, by category, since the start.
    pub work: Done,
    /// The thread is holding work back, to keep what the frame thread uploads to its budget (see
    /// [`UPLOAD_BUDGET`]): as of this publish.
    pub throttled: bool,
    pub loader: LoaderStats,
    /// What costs more to find out, as of a few times a second.
    pub slow: Arc<Slow>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub eviction: EvictionStats,
    /// Where the scene had been moved to: the centre of the scene that the frame thread draws once
    /// it has taken this (see [`crate::world::World::lag`]).
    pub camera: Option<[f64; 3]>,
    /// How far the camera and the places looked ahead to have gone as far as the selection is
    /// concerned (see [`render::Scene::travelled`]): what it is made to look at again for.
    #[cfg(test)]
    pub travelled: f64,
    /// What the operations of the publish added that were not inside the budget: those (a follow
    /// step, a batch of arrivals, an Edit) that began when what waits had reached the budget (the
    /// batch of arrivals that a round is always given), and the one that took it there. A publish's
    /// upload is at most the budget and this, and the words that joining copies puts between them.
    #[cfg(test)]
    pub excess_ops: UploadCost,
    /// The most that one operation added, in bytes and in copies, since the start.
    #[cfg(test)]
    pub largest_op: UploadCost,
    /// The most that one batch of arrivals added, and the most that one follow step added, in
    /// bytes and in copies, since the start: sizes of patches, whatever the budget decided.
    #[cfg(test)]
    pub largest_batch: UploadCost,
    #[cfg(test)]
    pub largest_follow: UploadCost,
}

/// What the scene thread has done since the start, in steps and bricks, not milliseconds (#181):
/// the same count on every machine and every run when it is stepped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// Rounds in which the thread moved the scene to a new camera (selecting again), and the
    /// bricks drawn that changed in them, which came or went.
    pub follow_steps: u64,
    pub follow_bricks: u64,
    /// Batches of the loader's arrivals that the thread took in and packed, and the bricks drawn
    /// that changed in them.
    pub pack_steps: u64,
    pub pack_bricks: u64,
    /// Bricks the loader's arrivals held in the store, which the packing is for.
    pub arrived_bricks: u64,
    /// Rounds in which eviction was going on, the units it took, and the bricks they held.
    pub evict_steps: u64,
    pub evicted_units: u64,
    pub evicted_bricks: u64,
    /// Looks at every split brick for a hole (see [`SceneWorker::count_holes`]), and the holes
    /// those looks found, summed.
    pub hole_scans: u64,
    pub holes_found: u64,
    /// Rounds in which the thread held work back for the upload budget.
    pub budget_cuts: u64,
}

/// The parts of a round that [`Spent`] times.
#[derive(Clone, Copy, Debug)]
enum Part {
    /// Moving the scene to a new camera: selecting again, and packing what changed.
    Follow,
    /// Asking the loader, taking in what it made, and packing it.
    Pack,
    /// Making the changes available to the frame thread: copying them out for it.
    Publish,
    /// Eviction.
    Evict,
    /// Remaking the coarse levels over an Edit.
    Coarse,
    /// Looking at every split brick for a hole (see [`HOLES_EVERY`]).
    HoleScan,
}

const PARTS: usize = 6;

/// How long the scene thread spent on each part of its rounds since timing was turned on
/// ([`SceneWorker::time_work`]): the real-time bench's scene-thread time by category (#162).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    pub follow: Duration,
    pub pack: Duration,
    pub publish: Duration,
    pub evict: Duration,
    pub coarse: Duration,
    pub hole_scan: Duration,
}

/// What eviction has done and what it cost the other threads, since the start.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EvictionStats {
    /// Passes begun, and how long the last took to list the units in (the parts looked at and
    /// put in order), and the longest.
    pub passes: u64,
    pub listing: Duration,
    pub longest_listing: Duration,
    /// The longest the store's read lock was held for at a time to look at units, and its write
    /// lock to take them away.
    pub longest_read: Duration,
    pub longest_write: Duration,
    /// Parts looked at in all, and the time the read lock was held for them: their mean hold.
    pub reads: u64,
    pub read_total: Duration,
}

/// The parts of [`Stats`] that cost a walk over the scene's free ranges, found a few times a
/// second.
#[derive(Debug, Default)]
pub struct Slow {
    /// The sizes of the tables of the scene and the loader (see [`Scene::table_sizes`]).
    pub tables: Vec<TableSize>,
    /// Bytes of the scene's buffers that hold something (see [`Scene::used_bytes`]).
    pub used_bytes: u64,
    /// The words of `brick_refs` and of `bricks` that the bricks drawn at each level take (see
    /// [`Scene::words_by_level`]), worked out only when the counts are asked for exactly (see
    /// [`SceneWorker::count_exactly`]), and then at every publish, so that they are of the scene
    /// that the bricks drawn are of: it looks at every brick drawn. Zero otherwise.
    pub words_by_level: [(usize, usize); LEVELS],
}

impl Stats {
    /// Whether `other` has the same counts, which are cheap to look at: the rest changes slowly.
    /// What the thread has done (see [`Done`]) and the soft misses by level and the holes are only
    /// compared when the counts are exact (the bench's): they change on rounds that change nothing
    /// else, and the game does not publish for them.
    fn same_counts(&self, other: &Stats, exactly: bool) -> bool {
        self.waiting == other.waiting
            && self.soft_misses == other.soft_misses
            && self.coarsening == other.coarsening
            && self.throttled == other.throttled
            && self.loader == other.loader
            && self.level_bricks == other.level_bricks
            && self.work.hole_scans == other.work.hole_scans
            && (!exactly
                || (self.holes == other.holes
                    && self.soft_misses_by_level == other.soft_misses_by_level
                    && self.work == other.work))
    }
}

/// The scene and the loader as they are now, with `slow` as it was last looked at.
fn stats_of(scene: &Scene, loader: &Loader, slow: &Arc<Slow>, eviction: EvictionStats) -> Stats {
    let mut level_bricks = [0; LEVELS];
    for (level, bricks) in level_bricks.iter_mut().enumerate().take(scene.levels.len()) {
        *bricks = scene.level_bricks(level);
    }
    Stats {
        waiting: scene.waiting(),
        level_bricks,
        bands: scene.bands(),
        coarsening: 0,
        soft_misses: 0,
        soft_misses_by_level: [0; LEVELS],
        holes: 0,
        work: Done::default(),
        throttled: false,
        loader: LoaderStats::of(loader),
        slow: Arc::clone(slow),
        eviction,
        camera: scene.camera(),
        #[cfg(test)]
        travelled: scene.travelled(),
        #[cfg(test)]
        excess_ops: UploadCost::default(),
        #[cfg(test)]
        largest_op: UploadCost::default(),
        #[cfg(test)]
        largest_batch: UploadCost::default(),
        #[cfg(test)]
        largest_follow: UploadCost::default(),
    }
}

/// The parts of [`Stats`] that cost more to find out.
fn slow_of(scene: &Scene, loader: &Loader, exactly: bool) -> Arc<Slow> {
    let mut tables = scene.table_sizes();
    tables.extend(loader.table_sizes());
    Arc::new(Slow {
        tables,
        used_bytes: scene.used_bytes(),
        words_by_level: if exactly {
            scene.words_by_level()
        } else {
            [(0, 0); LEVELS]
        },
    })
}

/// What this thread has made since the frame thread last took: the frame thread's copy of the
/// scene is made, from [`Ready::mirror`], if it is there, else brought up to date with
/// [`Ready::patch`].
#[derive(Default)]
pub struct Ready {
    /// The scene was packed again: a copy of all of it, which replaces the frame thread's. Any
    /// changes since are in it.
    pub mirror: Option<Box<Scene>>,
    /// What the scene changed, with the words (see [`Scene::copy_patch`]).
    pub patch: Option<PatchCopy>,
    /// The counts, of the same state as the words.
    pub stats: Option<Stats>,
    /// The number of the last job that was done and published when this was taken (see
    /// [`SceneWorker::wait_for`]): an Edit is in the scene that this makes once it is that.
    pub done: u64,
    /// When the oldest of what this holds was published, if the counts are exact (see
    /// [`SceneWorker::count_exactly`]); else not stamped, since reading the clock is not free.
    pub published_at: Option<Instant>,
}

/// A job for the thread that is not following the camera.
enum Job {
    /// Bricks of level 0 that an Edit changed (see [`Scene::update`]).
    Edit(Vec<BrickCoord>),
    /// Other LOD bands (see [`Scene::set_bands`]).
    Bands(Bands),
    /// A count of the soft misses, and a look at every split brick for a hole if `holes`, now and
    /// not when the clock says (see [`SceneWorker::look_now`]).
    Look { holes: bool },
    /// Nothing: for a test to know that what it sent before has been looked at.
    #[cfg(test)]
    Sync,
    /// A panic, for a test of what the frame thread does about one.
    #[cfg(test)]
    Panic,
    /// Busy for a while, for a test of what the frame thread does not wait for.
    #[cfg(test)]
    Stall(Duration),
    /// From now on the remaking of the coarse levels sleeps this long before each step, with the
    /// store not held, for a test that it does not hold up what is not coarse.
    #[cfg(test)]
    SlowCoarse(Duration),
}

/// How long a step of remaking the coarse levels may take with the store's write lock held, so
/// that the frame thread's reads of the store wait about this long at the most.
const COARSE_STEP: Duration = Duration::from_micros(500);

/// How long the thread lets go of the store between two steps.
const COARSE_PAUSE: Duration = Duration::from_micros(50);

/// The coarse levels over the bricks of an Edit, being remade in steps.
struct CoarseWork {
    changed: Vec<BrickCoord>,
    work: Option<LodWork>,
}

struct Inbox {
    camera: [f64; 3],
    /// The camera's velocity, in Voxels a second, as the frame thread has seen it move: what the
    /// lead looks ahead along (zero for no lead).
    velocity: [f64; 3],
    /// Jobs that the frame thread waits for, with the number each is to be told done by.
    jobs: VecDeque<(u64, Job)>,
    issued: u64,
    shutdown: bool,
    /// The most that the camera moved in one of the frame thread's calls since the thread last
    /// looked, in Voxels: a camera that was put somewhere far (a teleport) is told from one that
    /// the scene thread has fallen behind (see [`Engine::follow`]).
    moved: f64,
}

struct Outbox {
    ready: Ready,
    /// Jobs finished, and published: the number of the last.
    done: u64,
    /// The scene ran out of room and is not to be used again.
    failed: Option<RingsFull>,
    /// The thread panicked: the frame thread panics too, rather than wait for it.
    died: bool,
}

struct Shared {
    inbox: Mutex<Inbox>,
    /// The thread waits here, for the camera and the jobs.
    wake: Condvar,
    outbox: Mutex<Outbox>,
    /// The frame thread waits here, for a job.
    finished: Condvar,
    /// Whether taking in what the loader generated is held back.
    paused: AtomicBool,
    /// Whether [`Stats::soft_misses`] is counted, which looks at every brick that waits (a
    /// diagnostic: see [`SceneWorker::count_soft_misses`]).
    count_soft_misses: AtomicBool,
    /// Whether the soft misses and the holes are counted at every publish, not a few times a second
    /// (see [`SceneWorker::count_exactly`]).
    count_exactly: AtomicBool,
    /// Whether every split brick is looked at for a hole every [`HOLES_EVERY`].
    count_holes: AtomicBool,
    /// Whether a look for holes is going on now (see [`SceneWorker::looking_for_holes`]).
    looking: AtomicBool,
    /// Whether the time of the parts of a round is added to `spent` (reading the clock is not free,
    /// so the game does not).
    time_work: AtomicBool,
    /// Nanoseconds spent on each [`Part`], while timing is on.
    spent: [AtomicU64; PARTS],
    /// Whether the frame thread has taken what the thread published since the thread last looked:
    /// a thread that holds work back for the budget goes on at once (see
    /// [`Engine::await_work`]), and does not sleep through the rest of the frame.
    taken: AtomicBool,
    /// How many throttled waits (see [`Engine::await_work`]) the thread began, and how many of
    /// them ended by the timeout and not by being woken: for the test that the frame thread's
    /// take wakes it, which cannot tell a wake from the timeout by counting frames when the
    /// machine is busy.
    #[cfg(test)]
    throttled_waits: AtomicU64,
    #[cfg(test)]
    throttled_timeouts: AtomicU64,
    /// The upload budget (see [`UPLOAD_BUDGET`]): bytes, and copies.
    budget_bytes: AtomicU64,
    budget_writes: AtomicU32,
    /// A test asked for the largest-operation figures to start again (see
    /// [`SceneWorker::forget_largest`]).
    #[cfg(test)]
    forget_largest: AtomicBool,
    /// The most words a buffer of the scene may have (see [`SceneWorker::set_buffer_limit`]).
    buffer_limit: AtomicUsize,
}

/// What the frame thread has no more use for, which it hands back for the scene thread to use
/// again or to free (see [`SceneWorker::recycle`]).
#[allow(dead_code)] // the buffers are held only to be freed by the thread that made them
enum Garbage {
    /// A copy that was written into the frame thread's scene.
    Copy(Box<PatchCopy>),
    Slow(Arc<Slow>),
    Scene(Box<Scene>),
}

/// Sets `died` and wakes the frame thread if the scene thread panics.
struct DeathWatch(Arc<Shared>);

impl Drop for DeathWatch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            match self.0.outbox.lock() {
                Ok(mut outbox) => outbox.died = true,
                Err(poisoned) => poisoned.into_inner().died = true,
            }
            self.0.finished.notify_all();
        }
    }
}

/// The frame thread's end of the scene thread.
pub struct SceneWorker {
    shared: Arc<Shared>,
    garbage: Sender<Garbage>,
    thread: Option<JoinHandle<()>>,
    /// The engine of a stepped scene thread, which has no thread (see
    /// [`SceneWorker::start_stepped`]). `None` in the game.
    stepped: Option<Box<Mutex<Engine>>>,
    /// Until when (on the stepped clock) a stepped thread is busy and does nothing: what a wait of
    /// the frame thread's that has a timeout runs out against (see [`SceneWorker::busy_for`]).
    busy_until: Mutex<Option<Instant>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("the scene thread panicked")
}

impl SceneWorker {
    /// Starts the thread with `scene`, a scene of rings, and the `loader` that makes the bricks it
    /// waits for, reading and holding bricks in `store`. Returns the frame thread's end of it, a
    /// copy of the scene to draw (see [`Scene::mirror`]), and its counts.
    pub fn start(scene: Scene, loader: Loader, store: SharedStore) -> (Self, Scene, Stats) {
        let (engine, mut worker, mirror, stats) = Self::build(scene, loader, store, Clock::real());
        let thread = std::thread::Builder::new()
            .name("scene".into())
            .spawn(move || engine.run())
            .expect("a thread for the scene");
        worker.thread = Some(thread);
        (worker, mirror, stats)
    }

    /// A scene thread with no thread, for a test: what the thread does in a round is done on the
    /// calling thread a step at a time ([`SceneWorker::step`], and the calls that wait for a job),
    /// by the code that the thread runs, with the loader (which should be [`Loader::stepped`]) done
    /// a step in each. It reads the time from `clock`, which should be stepped.
    pub fn start_stepped(
        scene: Scene,
        loader: Loader,
        store: SharedStore,
        clock: Clock,
    ) -> (Self, Scene, Stats) {
        let (engine, mut worker, mirror, stats) = Self::build(scene, loader, store, clock);
        worker.stepped = Some(Box::new(Mutex::new(engine)));
        (worker, mirror, stats)
    }

    /// The engine, its frame thread's end with no thread started, and what the frame thread starts
    /// with.
    fn build(
        scene: Scene,
        loader: Loader,
        store: SharedStore,
        clock: Clock,
    ) -> (Engine, Self, Scene, Stats) {
        assert!(scene.is_rings(), "only a scene of rings follows a camera");
        let mirror = scene.mirror();
        let slow = slow_of(&scene, &loader, false);
        let stats = stats_of(&scene, &loader, &slow, EvictionStats::default());
        let camera = scene.camera().expect("a scene of rings has a camera");
        let shared = Arc::new(Shared {
            inbox: Mutex::new(Inbox {
                camera,
                velocity: [0.0; 3],
                jobs: VecDeque::new(),
                issued: 0,
                shutdown: false,
                moved: 0.0,
            }),
            wake: Condvar::new(),
            outbox: Mutex::new(Outbox {
                ready: Ready::default(),
                done: 0,
                failed: None,
                died: false,
            }),
            finished: Condvar::new(),
            paused: AtomicBool::new(false),
            count_soft_misses: AtomicBool::new(false),
            count_exactly: AtomicBool::new(false),
            count_holes: AtomicBool::new(false),
            looking: AtomicBool::new(false),
            time_work: AtomicBool::new(false),
            spent: std::array::from_fn(|_| AtomicU64::new(0)),
            taken: AtomicBool::new(false),
            #[cfg(test)]
            throttled_waits: AtomicU64::new(0),
            #[cfg(test)]
            throttled_timeouts: AtomicU64::new(0),
            budget_bytes: AtomicU64::new(UPLOAD_BUDGET.bytes),
            budget_writes: AtomicU32::new(UPLOAD_BUDGET.writes),
            #[cfg(test)]
            forget_largest: AtomicBool::new(false),
            buffer_limit: AtomicUsize::new(usize::MAX),
        });
        let (garbage, graveyard) = mpsc::channel();
        let user_bands = scene.bands();
        let engine = Engine {
            scene,
            loader,
            store,
            shared: Arc::clone(&shared),
            camera: Some(camera),
            paused: false,
            arrival_cost: ARRIVAL_COST,
            changes: Changes::None,
            published: stats.clone(),
            graveyard,
            spare: Vec::new(),
            slow,
            slow_at: clock.now(),
            coarse: VecDeque::new(),
            sync_waiting: None,
            evicting: None,
            evict_from: 0,
            eviction: EvictionStats::default(),
            user_bands,
            coarsening: 0,
            coarsened_at: clock.now(),
            units_per_node: 0.0,
            relaxed: false,
            barred: None,
            over_the_cap: false,
            soft_misses: 0,
            soft_misses_by_level: [0; LEVELS],
            holes: 0,
            soft_misses_at: clock.now(),
            holes_at: clock.now(),
            recount_due: false,
            look_due: false,
            work: Done::default(),
            ahead: [None; AHEAD],
            velocity: [0.0; 3],
            idle: false,
            throttled: false,
            pending: UploadCost::default(),
            batch_cost: UploadCost::default(),
            follow_step: None,
            teleported: false,
            clock,
            #[cfg(test)]
            coarse_sleep: Duration::ZERO,
            #[cfg(test)]
            excess_ops: UploadCost::default(),
            #[cfg(test)]
            largest_op: UploadCost::default(),
            #[cfg(test)]
            largest_batch: UploadCost::default(),
            #[cfg(test)]
            largest_follow: UploadCost::default(),
        };
        let worker = Self {
            shared,
            garbage,
            thread: None,
            stepped: None,
            busy_until: Mutex::new(None),
        };
        (engine, worker, mirror, stats)
    }

    /// Tells the thread the most words that a buffer of the scene may have, which is what the
    /// device allows a storage buffer to be: a buffer that is full grows by no more than that
    /// leaves, and a scene that needs more stops with [`RingsFull::BufferLimit`]. The thread has
    /// it from its next round.
    pub fn set_buffer_limit(&self, words: usize) {
        self.shared.buffer_limit.store(words, Ordering::Relaxed);
    }

    /// Changes the upload budget (see [`UPLOAD_BUDGET`]), for a test or the bench to see what a
    /// smaller one does.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_upload_budget(&self, budget: UploadCost) {
        self.shared
            .budget_bytes
            .store(budget.bytes, Ordering::Relaxed);
        self.shared
            .budget_writes
            .store(budget.writes, Ordering::Relaxed);
    }

    /// Starts the largest-operation figures of [`Stats`] (`largest_op`, `largest_batch`,
    /// `largest_follow`) again from the thread's next round, so that a test can read the sizes of a
    /// stretch of the run and not the whole session's.
    #[cfg(test)]
    pub fn forget_largest(&self) {
        self.shared.forget_largest.store(true, Ordering::Release);
    }

    /// How many waits for the frame thread to take what was published the scene thread began, and
    /// how many of those ended by the timeout and not by a wake-up.
    #[cfg(test)]
    pub fn throttled_waits(&self) -> (u64, u64) {
        (
            self.shared.throttled_waits.load(Ordering::Relaxed),
            self.shared.throttled_timeouts.load(Ordering::Relaxed),
        )
    }

    /// One step of a stepped scene thread: the loader makes up to [`STEP_JOBS`] jobs, and the
    /// thread does a round of its work (following the camera it was last told, its jobs, taking in
    /// what was made, publishing). Does nothing for one with a thread, nor for one that is busy
    /// ([`SceneWorker::busy_for`]). The frame thread's calls that wait for the thread step it until
    /// it has done.
    pub fn step(&self) {
        if let Some(engine) = &self.stepped {
            let mut engine = engine.lock().expect("the engine is not poisoned");
            if let Some(until) = *lock(&self.busy_until)
                && engine.clock.now() < until
            {
                return;
            }
            engine.step();
        }
    }

    /// Has a stepped thread busy for `time` of its clock from now, as the camera or the arrivals
    /// keep a real one: it does nothing until then, the jobs it has included. A wait for a job with
    /// a timeout that is over before that gives up with the clock moved on to the timeout, and the
    /// job is done by the first step after the thread is free; a wait with no timeout moves the
    /// clock on to when it is free.
    #[cfg(test)]
    pub fn busy_for(&self, time: Duration) {
        let engine = self
            .stepped
            .as_ref()
            .expect("only a stepped thread is busy");
        let now = engine
            .lock()
            .expect("the engine is not poisoned")
            .clock
            .now();
        *lock(&self.busy_until) = Some(now + time);
    }

    /// Tells the thread where the camera is, a position in Voxels: it follows it when it has the
    /// time, and a camera that moved on again by then is not followed to. Does not wait: if the
    /// thread is at the camera at this moment, it is told the next time.
    /// A `velocity` for the camera, in Voxels a second, makes the thread look ahead along it for
    /// prefetch (zero for none).
    pub fn follow_moving(&self, camera: [f64; 3], velocity: [f64; 3]) {
        if let Ok(mut inbox) = self.shared.inbox.try_lock()
            && (inbox.camera != camera || inbox.velocity != velocity)
        {
            let moved = (0..3)
                .map(|a| (camera[a] - inbox.camera[a]).powi(2))
                .sum::<f64>()
                .sqrt();
            inbox.moved = inbox.moved.max(moved);
            inbox.camera = camera;
            inbox.velocity = velocity;
            self.shared.wake.notify_one();
        }
    }

    /// Has the thread count the soft misses in each [`Stats`] it publishes (or not): a diagnostic,
    /// since it looks at every brick that waits.
    pub fn count_soft_misses(&self, on: bool) {
        self.shared.count_soft_misses.store(on, Ordering::Relaxed);
    }

    /// Has the thread look at every split brick for a hole every [`HOLES_EVERY`] (or not), and add
    /// the holes found to [`Done::holes_found`]: for the real-time bench, which cannot count them at
    /// every publish as [`SceneWorker::count_exactly`] does (that is a pass over every node each
    /// time, and the thread would do nothing else).
    pub fn count_holes(&self, on: bool) {
        self.shared.count_holes.store(on, Ordering::Relaxed);
    }

    /// Whether the thread is in a look for holes at this moment: from before it takes the store's
    /// read lock (so waiting for the lock is in it) to after it has let go of it, and it publishes
    /// nothing meanwhile. For the real-time bench, which says if one overlapped a slow frame.
    pub fn looking_for_holes(&self) -> bool {
        self.shared.looking.load(Ordering::Relaxed)
    }

    /// Has the thread add the time of the parts of its rounds to what [`SceneWorker::spent`] says
    /// (or not).
    pub fn time_work(&self, on: bool) {
        self.shared.time_work.store(on, Ordering::Relaxed);
    }

    /// How long the thread has spent on each part of its rounds while timing was on.
    pub fn spent(&self) -> Spent {
        let part = |part: Part| {
            Duration::from_nanos(self.shared.spent[part as usize].load(Ordering::Relaxed))
        };
        Spent {
            follow: part(Part::Follow),
            pack: part(Part::Pack),
            publish: part(Part::Publish),
            evict: part(Part::Evict),
            coarse: part(Part::Coarse),
            hole_scan: part(Part::HoleScan),
        }
    }

    /// Has the thread count the soft misses (if they are counted) **now**, and look at every split
    /// brick for a hole if `holes` (and [`SceneWorker::count_holes`] is on), with the camera it
    /// has been told and the bricks it has taken in, publish, and returns when that is published.
    /// For the real-time bench, which wants a look at the end of each path and when the camera
    /// has stood still, and a count of the soft misses that was made after the camera stopped, not
    /// one from before it.
    pub fn look_now(&self, holes: bool) -> Result<(), RingsFull> {
        self.job(Job::Look { holes })
    }

    /// Has the thread count the soft misses (by level) and the holes at every publish, not a few
    /// times a second by the clock (which a stepped clock that a frame moves on by a sixtieth of a
    /// second would make one in six): for the bench, which wants a count for each frame. Counting
    /// them is turned on too. It looks at every brick that waits and every brick that is split.
    pub fn count_exactly(&self) {
        self.shared.count_soft_misses.store(true, Ordering::Relaxed);
        self.shared.count_exactly.store(true, Ordering::Relaxed);
    }

    /// Has the thread stop counting exactly (see [`SceneWorker::count_exactly`]); the soft misses
    /// are still counted, a few times a second. For the real-time bench, which needs the exact
    /// counts once, at full size, where they cost a pass over every brick each.
    pub fn stop_counting_exactly(&self) {
        self.shared.count_exactly.store(false, Ordering::Relaxed);
    }

    /// Holds back (or lets go of) taking in what the loader generated (see
    /// [`crate::world::World::pause_loading`]).
    pub fn pause(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Release);
        // Taking the lock the thread decides to wait under means it is waiting, and is woken, or
        // has not decided yet and will see the flag.
        drop(lock(&self.shared.inbox));
        self.shared.wake.notify_one();
    }

    /// Remakes the coarse levels over the bricks of level 0 that an Edit changed, which the store
    /// has been changed to show (the predictor leaves the coarse levels to this thread: see
    /// [`sim::Predictor::defer_lod`]), packs them, and returns when that is published. The Edit is
    /// in what [`SceneWorker::take`] gives next, with whatever else the thread had done.
    pub fn edit(&self, changed: Vec<BrickCoord>) -> Result<(), RingsFull> {
        let ticket = self.edit_later(changed);
        self.wait(ticket)
    }

    /// [`SceneWorker::edit`], without waiting: the thread does it ahead of anything it has to do
    /// but what it is in the middle of, and publishes it. Returns the job's number, for
    /// [`SceneWorker::wait`].
    pub fn edit_later(&self, changed: Vec<BrickCoord>) -> u64 {
        self.enqueue(Job::Edit(changed))
    }

    /// Selects the bricks again for other `bands`, and returns when that is published.
    pub fn set_bands(&self, bands: Bands) -> Result<(), RingsFull> {
        self.job(Job::Bands(bands))
    }

    /// Returns when the thread has looked at what it was told before (the camera, pausing), and
    /// published what it made by then. What it was doing at the time may have been published
    /// before.
    #[cfg(test)]
    pub fn sync(&self) -> Result<(), RingsFull> {
        self.job(Job::Sync)
    }

    /// Has the thread busy for `time` before it looks at what comes after, and returns at once.
    #[cfg(test)]
    pub fn stall(&self, time: Duration) -> u64 {
        self.enqueue(Job::Stall(time))
    }

    /// Has the thread sleep `time` before each step of remaking the coarse levels from now on.
    #[cfg(test)]
    pub fn slow_coarse(&self, time: Duration) {
        let ticket = self.enqueue(Job::SlowCoarse(time));
        self.wait(ticket).unwrap();
    }

    /// Has the thread panic, and waits for it to: the call panics too.
    #[cfg(test)]
    pub fn panic(&self) {
        let _ = self.job(Job::Panic);
    }

    fn job(&self, job: Job) -> Result<(), RingsFull> {
        let ticket = self.enqueue(job);
        self.wait(ticket)
    }

    /// Gives the thread `job` and returns its number.
    fn enqueue(&self, job: Job) -> u64 {
        let ticket = {
            let mut inbox = lock(&self.shared.inbox);
            inbox.issued += 1;
            let ticket = inbox.issued;
            inbox.jobs.push_back((ticket, job));
            ticket
        };
        self.shared.wake.notify_one();
        ticket
    }

    /// Returns when the job `ticket` (or one after it) is published. An error if the scene ran out
    /// of room.
    pub fn wait(&self, ticket: u64) -> Result<(), RingsFull> {
        self.wait_for(ticket, None).map(|_| ())
    }

    /// [`SceneWorker::wait`], for `timeout` at the most if there is one. Whether the job is
    /// published.
    pub fn wait_for(&self, ticket: u64, timeout: Option<Duration>) -> Result<bool, RingsFull> {
        // A stepped thread steps until the job is done. It is as quick as the frame thread needs it
        // to be unless the test has it busy ([`SceneWorker::busy_for`]): then a timeout runs out on
        // the stepped clock if the thread is still busy at the end of it, and the wait is over.
        if let Some(engine) = &self.stepped {
            let clock = engine
                .lock()
                .expect("the engine is not poisoned")
                .clock
                .clone();
            let until = timeout.map(|t| clock.now() + t);
            for _ in 0..MOST_STEPS_FOR_A_JOB {
                {
                    let outbox = lock(&self.shared.outbox);
                    if outbox.done >= ticket || outbox.failed.is_some() {
                        break;
                    }
                }
                let free = *lock(&self.busy_until);
                if let Some(free) = free
                    && free > clock.now()
                {
                    if let Some(until) = until
                        && free > until
                    {
                        clock.advance(until.saturating_duration_since(clock.now()));
                        return Ok(false);
                    }
                    clock.advance(free - clock.now());
                }
                self.step();
            }
            let outbox = lock(&self.shared.outbox);
            assert!(
                outbox.done >= ticket || outbox.failed.is_some(),
                "the job was never done"
            );
            return match outbox.failed {
                Some(full) => Err(full),
                None => Ok(true),
            };
        }
        let until = timeout.map(|t| Instant::now() + t);
        let mut outbox = lock(&self.shared.outbox);
        while outbox.done < ticket && outbox.failed.is_none() && !outbox.died {
            outbox = match until {
                None => self
                    .shared
                    .finished
                    .wait(outbox)
                    .expect("the scene thread panicked"),
                Some(until) => {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(false);
                    }
                    self.shared
                        .finished
                        .wait_timeout(outbox, left)
                        .expect("the scene thread panicked")
                        .0
                }
            };
        }
        assert!(!outbox.died, "the scene thread panicked");
        if let Some(full) = outbox.failed {
            return Err(full);
        }
        Ok(true)
    }

    /// What the thread has made since the last call. With `wait` false it does not wait for the
    /// thread, which has the lock for a moment each time it hands something over: then it is
    /// nothing if the thread is handing it over now, and it is there the next time. Waiting is for
    /// after a job, which the Edit that is to be on the screen this frame is. An error if the scene
    /// ran out of room: it is not to be used again.
    pub fn take(&self, wait: bool) -> Result<Ready, RingsFull> {
        let mut outbox = if wait {
            lock(&self.shared.outbox)
        } else {
            match self.shared.outbox.try_lock() {
                Ok(outbox) => outbox,
                Err(TryLockError::WouldBlock) => return Ok(Ready::default()),
                Err(TryLockError::Poisoned(_)) => panic!("the scene thread panicked"),
            }
        };
        assert!(!outbox.died, "the scene thread panicked");
        if let Some(full) = outbox.failed {
            return Err(full);
        }
        outbox.ready.done = outbox.done;
        let ready = std::mem::take(&mut outbox.ready);
        drop(outbox);
        if ready.patch.is_some() || ready.mirror.is_some() {
            self.wake_after_take();
        }
        Ok(ready)
    }

    /// Wakes a scene thread that holds work back for the budget: what it waited for is taken. The
    /// camera's own wake-up comes at the start of the frame, before the frame takes what the
    /// thread published, so the thread finds it still waiting and sleeps; without this it sleeps
    /// on until its next look (a millisecond, and on Windows a clock tick of about 15 ms), so
    /// that about every second frame had nothing to upload. The flag is set before the inbox is
    /// locked and the thread tests it with the inbox locked, so a wake-up is not lost between its
    /// test and its wait.
    fn wake_after_take(&self) {
        self.shared.taken.store(true, Ordering::Release);
        drop(lock(&self.shared.inbox));
        self.shared.wake.notify_one();
    }

    /// Hands back a copy that has been written into the frame thread's scene (see
    /// [`Scene::paste`]), for its buffers to be used again. The frame thread does not free what
    /// the scene thread allocated: the allocator makes a thread that frees memory wait for the
    /// threads that are allocating from the same arena, which is milliseconds when the scene
    /// thread is making a big table, and the frame thread has none. What is handed back waits in a
    /// channel for the scene thread, which never loses it.
    pub fn recycle(&self, copy: PatchCopy) {
        let _ = self.garbage.send(Garbage::Copy(Box::new(copy)));
    }

    /// Hands back the stats' slow part, which the scene thread replaced and the frame thread was
    /// the last to hold, to be freed there.
    pub fn discard(&self, slow: Arc<Slow>) {
        let _ = self.garbage.send(Garbage::Slow(slow));
    }

    /// Hands back a copy of the scene that a new one replaced.
    pub fn discard_scene(&self, scene: Scene) {
        let _ = self.garbage.send(Garbage::Scene(Box::new(scene)));
    }
}

impl Drop for SceneWorker {
    fn drop(&mut self) {
        match self.shared.inbox.lock() {
            Ok(mut inbox) => inbox.shutdown = true,
            Err(poisoned) => poisoned.into_inner().shutdown = true,
        }
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
            && !std::thread::panicking()
        {
            eprintln!("the scene thread panicked");
        }
    }
}

/// What the scene changed since the last publish.
enum Changes {
    None,
    Patch(RingPatch),
    /// Packed again, so what the frame thread has is of no use.
    Rebuilt,
}

/// The thread's state.
struct Engine {
    scene: Scene,
    loader: Loader,
    store: SharedStore,
    shared: Arc<Shared>,
    /// The camera the scene was last moved to.
    camera: Option<[f64; 3]>,
    paused: bool,
    /// What taking in one brick's children is expected to cost.
    arrival_cost: Duration,
    changes: Changes,
    /// What the frame thread handed back, and the copies of it to use again.
    graveyard: Receiver<Garbage>,
    spare: Vec<PatchCopy>,
    /// The counts last published.
    published: Stats,
    /// What is in the stats that is slow to find out, and when it was.
    slow: Arc<Slow>,
    slow_at: Instant,
    /// Edits whose coarse levels are still to be remade, oldest first.
    coarse: VecDeque<CoarseWork>,
    /// A sync that waits for the coarse levels.
    sync_waiting: Option<u64>,
    /// Eviction that is going on, if the store went over its budget.
    evicting: Option<Evicting>,
    /// Eviction does not begin before the store holds more than this (see [`EVICT_RETRY`]).
    evict_from: usize,
    eviction: EvictionStats,
    /// The bands the frame thread asked for, which the scene's are [`COARSENING`] of.
    user_bands: Option<Bands>,
    /// How many steps of [`COARSENING`] the scene's bands are at, and when that last changed.
    coarsening: usize,
    coarsened_at: Instant,
    /// How many units the store held for each node of the selection when the last step was taken
    /// (everything it held was wanted then), for working out what a step taken back would want.
    units_per_node: f64,
    /// The last change of step was one taken back: if the next is a step up again, what was taken
    /// back did not fit after all.
    relaxed: bool,
    /// A step that was taken back and had to be taken again, and where the camera was: it is not
    /// taken back until the camera has gone a way from there (the model that says it fits is
    /// for flat ground, and can be wrong).
    barred: Option<(usize, [f64; 3])>,
    /// Eviction has done what it can and the store is over its limit: the next step of coarsening
    /// is due when the last has stood long enough.
    over_the_cap: bool,
    /// The last count of soft misses, and when (see [`SOFT_MISSES_EVERY`]).
    soft_misses: usize,
    soft_misses_by_level: [usize; LEVELS],
    holes: usize,
    soft_misses_at: Instant,
    /// When every split brick was last looked at for a hole (see [`HOLES_EVERY`]).
    holes_at: Instant,
    /// A [`Job::Look`] is waiting to be done in the next publish: the count of the soft misses, and
    /// the look for holes if `look_due`, are due whatever the clock says.
    recount_due: bool,
    look_due: bool,
    /// What was done, by category (see [`Done`]).
    work: Done,
    /// The places the scene looks ahead to (see [`Engine::look_ahead`]), and the velocity of the
    /// camera that was looked at last.
    ahead: [Option<[f64; 3]>; AHEAD],
    velocity: [f64; 3],
    /// The last round did nothing, so a thread waits to be told (see [`Engine::await_work`]).
    idle: bool,
    /// The last round held work back for the upload budget (see [`Engine::over_budget`]), so the
    /// thread looks again soon, when the frame thread may have taken what it waits for.
    throttled: bool,
    /// What the patch that is in the outbox costs to upload, as of the last publish.
    pending: UploadCost,
    /// What the last batch of arrivals added to what is uploaded: the next batch is not begun if
    /// one like it could take what waits to the budget (see [`Engine::batch_estimate`]). It is the
    /// last, not the largest, so that one big batch is forgotten with the next.
    batch_cost: UploadCost,
    /// How long the next step of following the camera is (None before the first), from how the
    /// steps so far cost (see [`Engine::follow`]).
    follow_step: Option<FollowStep>,
    /// The camera was put somewhere far, further than the scene follows by patching (a
    /// teleport), since the last follow: the scene is made again there, not walked to.
    teleported: bool,
    /// The time that holds and slices are measured in: the machine's, or a test's.
    clock: Clock,
    #[cfg(test)]
    coarse_sleep: Duration,
    /// See [`Stats::excess_ops`].
    #[cfg(test)]
    excess_ops: UploadCost,
    /// See [`Stats::largest_op`].
    #[cfg(test)]
    largest_op: UploadCost,
    /// See [`Stats::largest_batch`].
    #[cfg(test)]
    largest_batch: UploadCost,
    /// See [`Stats::largest_follow`].
    #[cfg(test)]
    largest_follow: UploadCost,
}

/// The length of the next step of following the camera, in Voxels, from how the last one went
/// (see [`FollowStep::after`]).
#[derive(Clone, Copy, Debug)]
struct FollowStep(f64);

impl FollowStep {
    /// The first step of a walk, before any is measured: half of `limit`, and not under a Voxel
    /// (with a budget under 256 KiB the limit is a few Voxels, and half of it is under one).
    fn first(limit: f64) -> Self {
        let limit = limit.max(1.0);
        Self((limit / 2.0).max(1.0))
    }

    /// The length after a full step of `distance` Voxels that cost `cost`, to cost `target`: the
    /// step scaled by how far under or over `target` the cost was (at most
    /// [`FOLLOW_STEP_CHANGE`]), and within [`FOLLOW_STEP_LEAST`] of `limit` and `limit`. A cost
    /// over a Voxel is not a rate (see [`FOLLOW_STEP_LEAST`]).
    fn after(self, distance: f64, cost: UploadCost, target: UploadCost, limit: f64) -> Self {
        let room = |target: f64, cost: f64| {
            if cost <= 0.0 {
                FOLLOW_STEP_CHANGE.1
            } else {
                target / cost
            }
        };
        let scale = room(target.bytes as f64, cost.bytes as f64)
            .min(room(f64::from(target.writes), f64::from(cost.writes)))
            .clamp(FOLLOW_STEP_CHANGE.0, FOLLOW_STEP_CHANGE.1);
        let limit = limit.max(1.0);
        Self((distance * scale).clamp((limit * FOLLOW_STEP_LEAST).max(1.0), limit))
    }
}

/// What the thread was told: the camera, how it moves, and the jobs.
type Work = ([f64; 3], [f64; 3], VecDeque<(u64, Job)>, f64);

/// What the thread was told since it looked last, taken. None when it is to stop.
fn take_work(inbox: &mut Inbox) -> Option<Work> {
    if inbox.shutdown {
        return None;
    }
    Some((
        inbox.camera,
        inbox.velocity,
        std::mem::take(&mut inbox.jobs),
        std::mem::take(&mut inbox.moved),
    ))
}

/// Eviction that has begun.
enum Evicting {
    /// Looking at the units a part at a time, for those that may be taken.
    Listing {
        pass: u64,
        began: Instant,
        next: usize,
        found: Vec<Candidate>,
    },
    /// Taking them, in order.
    Taking {
        queue: VecDeque<(usize, BrickCoord)>,
        /// Units that could not be taken yet (units were under them), for another go once the
        /// rest are done, if any of the rest were taken.
        deferred: Vec<(usize, BrickCoord)>,
        took: bool,
    },
}

/// Whether the unit `(level, coord)`, whose children are resident, is wanted: the bands split it
/// or the margin about the camera wants its children ([`render::lod::Placed::class`]: the lead, prefetch, is
/// not kept, and goes first when the store is over its budget), the scene has split it (every
/// brick drawn is under one that is split, and so is every brick over it), or the loader is making
/// the children of one of its children, which would be held under it.
fn wanted<'a>(
    scene: &'a Scene,
    loader: &'a Loader,
    bands: Bands,
    camera: [f64; 3],
) -> impl Fn(usize, BrickCoord) -> bool + 'a {
    let reach = scene.reach().from(camera);
    move |level, coord| {
        matches!(
            reach.class(bands, level, coord),
            Some(Class::Drawn | Class::Margin)
        ) || scene.splits(level, coord)
            || (level > 1
                && coord
                    .children()
                    .iter()
                    .any(|&child| loader.is_requested((level - 1, child))))
    }
}

impl Engine {
    fn run(mut self) {
        let _watch = DeathWatch(Arc::clone(&self.shared));
        loop {
            let Some((camera, velocity, jobs, moved)) = self.await_work() else {
                return;
            };
            if !self.round(camera, velocity, jobs, moved) {
                return;
            }
        }
    }

    /// One step of a stepped scene thread (see [`SceneWorker::step`]): what it was told since the
    /// last (the camera, the jobs) is taken without waiting for more, and a round of the thread's
    /// work is done on the calling thread, the loader's included. Whether the scene is still good.
    fn step(&mut self) -> bool {
        let work = take_work(&mut lock(&self.shared.inbox));
        let Some((camera, velocity, jobs, moved)) = work else {
            return false;
        };
        self.round(camera, velocity, jobs, moved)
    }

    /// What the thread was told since its last round, once there is something to do: it sleeps
    /// until the camera moves or a job comes if its last round did nothing, and wakes for a
    /// coarsening hold that is over. None when it is to stop.
    fn await_work(&mut self) -> Option<Work> {
        let mut inbox = lock(&self.shared.inbox);
        // Holding work back for the frame thread to take what is waiting: go on when it has
        // taken it ([`SceneWorker::take`] wakes the thread), or when told (an Edit or a new
        // camera wake it), or look again after [`THROTTLED_WAIT`] whatever happens.
        if self.throttled
            && inbox.jobs.is_empty()
            && !inbox.shutdown
            && !self.shared.taken.load(Ordering::Acquire)
        {
            let (woken, timeout) = self
                .shared
                .wake
                .wait_timeout(inbox, THROTTLED_WAIT)
                .expect("the frame thread panicked");
            inbox = woken;
            #[cfg(test)]
            {
                self.shared.throttled_waits.fetch_add(1, Ordering::Relaxed);
                if timeout.timed_out() {
                    self.shared
                        .throttled_timeouts
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            #[cfg(not(test))]
            let _ = timeout;
        }
        if self.idle
            && inbox.jobs.is_empty()
            && Some(inbox.camera) == self.camera
            && inbox.velocity == self.velocity
        {
            let shared = Arc::clone(&self.shared);
            let (camera, paused, moving) = (self.camera, self.paused, self.velocity);
            let quiet = |inbox: &mut Inbox| {
                inbox.jobs.is_empty()
                    && Some(inbox.camera) == camera
                    && inbox.velocity == moving
                    && !inbox.shutdown
                    && shared.paused.load(Ordering::Acquire) == paused
            };
            // While the bands are coarsened the thread looks at the clock, to take a step
            // back when what is wanted fits: when the hold is over, and then once in
            // each hold. A camera that is exactly still wakes nothing else.
            let held = if self.coarsening > 0 {
                match COARSEN_HOLD.saturating_sub(self.clock.since(self.coarsened_at)) {
                    Duration::ZERO => COARSEN_HOLD,
                    left => left,
                }
            } else {
                Duration::ZERO
            };
            inbox = if self.published.loader.pending == 0 && !self.over_the_cap {
                // Nothing is on its way: only the frame thread can give it something to do.
                if held.is_zero() {
                    self.shared
                        .wake
                        .wait_while(inbox, |inbox| quiet(inbox))
                        .expect("the frame thread panicked")
                } else {
                    self.shared
                        .wake
                        .wait_timeout_while(inbox, held, |inbox| quiet(inbox))
                        .expect("the frame thread panicked")
                        .0
                }
            } else {
                self.shared
                    .wake
                    .wait_timeout(inbox, IDLE)
                    .expect("the frame thread panicked")
                    .0
            };
        }
        take_work(&mut inbox)
    }

    /// One round of the thread's work, for the `camera` it was told, and the `jobs` it was given:
    /// the jobs, the coarse levels of Edits, following the camera, taking in what the loader made,
    /// eviction, coarsening, and publishing. A thread does this over and over; a stepped one does
    /// it once a step. False if the scene ran out of room, and is not to be used again.
    fn round(
        &mut self,
        camera: [f64; 3],
        velocity: [f64; 3],
        jobs: VecDeque<(u64, Job)>,
        moved: f64,
    ) -> bool {
        if let Some(bands) = self.scene.bands()
            && moved > render::scene::JUMP * bands.d0()
        {
            self.teleported = true;
        }
        self.shared.taken.store(false, Ordering::Release);
        // The places looked ahead to are from where the scene is to be after this round's first
        // step, which is the camera if it keeps up: not from the camera, which is as far as the
        // scene is behind it ahead of where the scene's step goes, and a selection that counts
        // the places' travel looks at what that travel made due (every round, for the camera's
        // whole move since the last, however far the round walked the scene).
        //
        // They are along the camera's velocity from there, though a scene that lags walks along
        // the line to the camera, which on a turn is not the velocity's way. Taking the way from
        // the line was tried and made things worse: the places are a second and two seconds of
        // speed on (2 400 and 4 800 Voxels in the test), so a camera whose line to the scene
        // turns a little moves them by hundreds of Voxels every round, and the selection's travel
        // was 6.7 times the scene's walk against 2.0 (the test below). The velocity is steady.
        // The effect of the other is on prefetch only (the places make the children of bricks
        // near them wanted, lowest priority; the scene's own selection is of the scene's place),
        // and ends when the scene is at the camera.
        let place = if self.teleported {
            camera
        } else {
            self.next_place(camera)
        };
        self.look_ahead(place, velocity);
        self.paused = self.shared.paused.load(Ordering::Acquire);
        self.scene
            .set_buffer_limit(self.shared.buffer_limit.load(Ordering::Relaxed));
        self.throttled = false;
        #[cfg(test)]
        if self.shared.forget_largest.swap(false, Ordering::AcqRel) {
            self.largest_op = UploadCost::default();
            self.largest_batch = UploadCost::default();
            self.largest_follow = UploadCost::default();
        }
        let mut worked = !jobs.is_empty();
        // A sync is done when the rest of what the thread does in this round is.
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut synced: Option<u64> = None;
        for (ticket, job) in jobs {
            if let Job::Look { holes } = job {
                self.recount_due = true;
                self.look_due = holes;
                self.publish(Some(ticket));
                continue;
            }
            #[cfg(test)]
            match job {
                Job::Sync => {
                    synced = Some(ticket);
                    continue;
                }
                Job::Panic => panic!("a panic that a test asked for"),
                Job::Stall(time) => {
                    self.clock.pause(time);
                    self.publish(Some(ticket));
                    continue;
                }
                Job::SlowCoarse(time) => {
                    self.coarse_sleep = time;
                    self.publish(Some(ticket));
                    continue;
                }
                _ => {}
            }
            if let Err(full) = self.run_job(job) {
                self.fail(full);
                return false;
            }
            self.publish(Some(ticket));
        }
        let began = self.began();
        let remade = self.remake_coarse();
        self.spend(Part::Coarse, began);
        let outcome = remade
            .and_then(|remade| {
                worked |= remade;
                let began = self.began();
                let moved = self.follow(camera);
                self.spend(Part::Follow, began);
                moved
            })
            .and_then(|moved| {
                worked |= moved;
                let began = self.began();
                let took = self.stream(camera);
                self.spend(Part::Pack, began);
                took
            });
        match outcome {
            Ok(took) => worked |= took,
            Err(full) => {
                self.fail(full);
                return false;
            }
        }
        let began = self.began();
        let evicting = self.evict();
        self.spend(Part::Evict, began);
        self.work.evict_steps += u64::from(evicting);
        worked |= evicting;
        self.coarsen_more();
        self.relax();
        self.work.budget_cuts += u64::from(self.throttled);
        // A sync is done when the coarse levels over the Edits before it are made, too.
        let busy = !self.coarse.is_empty();
        let synced = if busy {
            self.sync_waiting = synced.or(self.sync_waiting);
            None
        } else {
            synced.or(self.sync_waiting.take())
        };
        self.publish(synced);
        self.idle = !worked && !busy && !self.throttled;
        true
    }

    /// Whether what waits for the frame thread to upload, and what has been packed since it was
    /// published, comes to the budget (see [`UPLOAD_BUDGET`]), so that no more is to be added
    /// until the frame thread has taken it. A copy of all of the scene that waits is over any
    /// budget.
    fn over_budget(&mut self) -> bool {
        self.over_budget_with(UploadCost::default())
    }

    /// [`Engine::over_budget`] with `more` added to what waits: whether one more thing that costs
    /// that would take it to the budget.
    fn over_budget_with(&mut self, more: UploadCost) -> bool {
        let waiting = {
            let outbox = lock(&self.shared.outbox);
            if outbox.ready.mirror.is_some() {
                return true;
            }
            outbox.ready.patch.is_some()
        };
        if !waiting {
            self.pending = UploadCost::default();
        }
        if matches!(self.changes, Changes::Rebuilt) {
            return true;
        }
        self.reaches_budget(self.waiting_cost().plus(more))
    }

    /// What waits for the frame thread to upload: what the outbox has, and what has been packed
    /// since it was published.
    fn waiting_cost(&self) -> UploadCost {
        match &self.changes {
            Changes::None | Changes::Rebuilt => self.pending,
            Changes::Patch(patch) => self.pending.plus(patch.cost()),
        }
    }

    /// Whether `cost` is the budget, or more, in bytes or in copies.
    fn reaches_budget(&self, cost: UploadCost) -> bool {
        cost.bytes >= self.shared.budget_bytes.load(Ordering::Relaxed)
            || cost.writes >= self.shared.budget_writes.load(Ordering::Relaxed)
    }

    /// What a further batch of arrivals is expected to add: the last one's, less than the budget
    /// in each of bytes and copies. So that with nothing waiting a further batch is always tried,
    /// whatever one batch once cost: a batch that was itself over the budget does not hold every
    /// later one back for the rest of the session.
    fn batch_estimate(&self) -> UploadCost {
        let budget_bytes = self.shared.budget_bytes.load(Ordering::Relaxed);
        let budget_writes = self.shared.budget_writes.load(Ordering::Relaxed);
        UploadCost {
            bytes: self.batch_cost.bytes.min(budget_bytes.saturating_sub(1)),
            writes: self.batch_cost.writes.min(budget_writes.saturating_sub(1)),
        }
    }

    /// Whether the frame thread has taken what the thread published: nothing waits in the outbox.
    fn outbox_is_empty(&mut self) -> bool {
        let outbox = lock(&self.shared.outbox);
        let waits = outbox.ready.mirror.is_some() || outbox.ready.patch.is_some();
        drop(outbox);
        if !waits {
            self.pending = UploadCost::default();
        }
        !waits
    }

    /// The time a part of a round begins, if the parts are being timed.
    fn began(&self) -> Option<Instant> {
        self.shared
            .time_work
            .load(Ordering::Relaxed)
            .then(|| self.clock.now())
    }

    /// Adds the time since `began` to `part`'s.
    fn spend(&self, part: Part, began: Option<Instant>) {
        if let Some(began) = began {
            let nanos = u64::try_from(self.clock.since(began).as_nanos()).unwrap_or(u64::MAX);
            self.shared.spent[part as usize].fetch_add(nanos, Ordering::Relaxed);
        }
    }

    /// The scene ran out of room: the frame thread is told, and waiting jobs end.
    fn fail(&mut self, full: RingsFull) {
        lock(&self.shared.outbox).failed = Some(full);
        self.shared.finished.notify_all();
    }

    fn note(&mut self, update: Option<SceneUpdate>) {
        match update {
            None => {}
            Some(SceneUpdate::Rings(patch)) => {
                #[cfg(test)]
                {
                    // The operation is inside the budget unless what waited already reached it, or
                    // it takes what waits there.
                    let op = patch.cost();
                    self.largest_op.bytes = self.largest_op.bytes.max(op.bytes);
                    self.largest_op.writes = self.largest_op.writes.max(op.writes);
                    let before = self.waiting_cost();
                    if self.reaches_budget(before) || self.reaches_budget(before.plus(op)) {
                        self.excess_ops = self.excess_ops.plus(op);
                    }
                }
                match &mut self.changes {
                    Changes::None => self.changes = Changes::Patch(patch),
                    Changes::Patch(before) => before.merge(patch),
                    Changes::Rebuilt => {}
                }
            }
            Some(SceneUpdate::Rebuilt) => self.changes = Changes::Rebuilt,
            Some(SceneUpdate::Patched { .. }) => unreachable!("a scene of rings makes rings"),
        }
    }

    fn run_job(&mut self, job: Job) -> Result<(), RingsFull> {
        let store = self.store.read().expect("the store's lock is not poisoned");
        let update = match job {
            // The bricks of level 0 are packed at once, with the coarse levels as they were, which
            // is what is drawn near the camera and is published for the frame thread to wait for.
            // The coarse levels are remade after, a step at a time ([`Engine::remake_coarse`]).
            Job::Edit(changed) => {
                let update = self.scene.update(&store, &changed)?;
                self.coarse.push_back(CoarseWork {
                    changed,
                    work: None,
                });
                Some(update)
            }
            Job::Bands(bands) => {
                self.user_bands = Some(bands);
                self.scene
                    .set_bands(&store, coarsened(bands, self.coarsening))?
            }
            Job::Look { .. } => unreachable!("not run here"),
            #[cfg(test)]
            Job::Sync | Job::Panic | Job::Stall(_) | Job::SlowCoarse(_) => {
                unreachable!("not run here")
            }
        };
        drop(store);
        self.note(update);
        Ok(())
    }

    /// Does a step of remaking the coarse levels of the oldest Edit that has them to do, holding
    /// the store's write lock for [`COARSE_STEP`] at the most: the frame thread reads the store
    /// for the character and the ghost, and does not wait longer than that for it. Between steps
    /// the thread looks at what the frame thread asked of it, so that another Edit is packed first.
    /// When the coarse levels over an Edit are made, the bricks over it are packed again. Whether
    /// there was a step.
    ///
    /// An Edit made between steps changes level 0, which the steps that follow read: what is made
    /// is what a refresh made after it would, and that Edit's own refresh finds what it changed,
    /// so the levels end as a refresh of all of them at once would make them.
    fn remake_coarse(&mut self) -> Result<bool, RingsFull> {
        let Some(front) = self.coarse.front_mut() else {
            return Ok(false);
        };
        #[cfg(test)]
        self.clock.pause(self.coarse_sleep);
        let work = match &mut front.work {
            Some(work) => work,
            None => {
                let store = self.store.read().expect("the store's lock is not poisoned");
                front
                    .work
                    .insert(store.begin_lod(front.changed.iter().copied()))
            }
        };
        let done = {
            let mut store = self
                .store
                .write()
                .expect("the store's lock is not poisoned");
            store.step_lod(work, self.clock.deadline(self.clock.now() + COARSE_STEP))
        };
        if done {
            let finished = self.coarse.pop_front().expect("there is a front");
            let store = self.store.read().expect("the store's lock is not poisoned");
            let update = self.scene.update(&store, &finished.changed)?;
            drop(store);
            self.note(Some(update));
        } else {
            // Long enough for a reader that waits for the lock to take it.
            self.clock.pause(COARSE_PAUSE);
        }
        Ok(true)
    }

    /// Moves the scene to `camera`, if it is not there, in steps that each cost a share of the
    /// budget ([`FOLLOW_SHARE`]) when it is further than one: each step is packed and noted in
    /// its turn, and the steps go on while what waits is under the budget and the slice (the
    /// first step is taken whatever the time) has not run out. A camera that is not reached is
    /// walked to in the next rounds, so a thread that fell behind catches up, a step at a time,
    /// instead of making the scene again when it is more than a jump behind: only a camera that
    /// was *put* further than a jump ([`Engine::teleported`]) does that. Whether it was moved.
    fn follow(&mut self, camera: [f64; 3]) -> Result<bool, RingsFull> {
        // A camera that has not moved is looked at again for a brick that the loader gave up on.
        if self.camera == Some(camera) && !self.scene.rewanted() {
            return Ok(false);
        }
        // The camera is followed when the frame thread has taken what waits, from where the scene is:
        // a move that is waited for is a longer one, which is the same work.
        if self.over_budget() {
            self.throttled = true;
            return Ok(false);
        }
        // A teleport is jumped to (the scene is made again) only if the camera is past what
        // `Scene::recentre` makes again for: one that ended within a jump of the scene would be
        // patched in one operation of up to two `d0`, outside the budget, so it is walked to.
        let teleported = std::mem::take(&mut self.teleported)
            && match (self.scene.camera(), self.scene.bands()) {
                (Some(from), Some(bands)) => {
                    (0..3)
                        .map(|a| (camera[a] - from[a]).powi(2))
                        .sum::<f64>()
                        .sqrt()
                        > render::scene::JUMP * bands.d0()
                }
                _ => true,
            };
        let started = self.clock.now();
        loop {
            let target = if teleported {
                camera
            } else {
                self.next_place(camera)
            };
            let store = self.store.read().expect("the store's lock is not poisoned");
            let before = self.scene.bricks_changed();
            let from = self.scene.camera().unwrap_or(camera);
            let update = self.scene.recentre(&store, target)?;
            drop(store);
            self.work.follow_bricks += self.scene.bricks_changed() - before;
            self.camera = Some(target);
            let mut cost = UploadCost::default();
            if let Some(SceneUpdate::Rings(patch)) = &update {
                cost = patch.cost();
                #[cfg(test)]
                {
                    self.largest_follow.bytes = self.largest_follow.bytes.max(cost.bytes);
                    self.largest_follow.writes = self.largest_follow.writes.max(cost.writes);
                }
            }
            let distance = (0..3)
                .map(|a| (target[a] - from[a]).powi(2))
                .sum::<f64>()
                .sqrt();
            if target != camera && !teleported {
                self.resize_follow_step(distance, cost);
            }
            self.note(update);
            if self.camera == Some(camera) {
                break;
            }
            if self.over_budget() {
                self.throttled = true;
                break;
            }
            if self.clock.since(started) >= SLICE {
                break;
            }
        }
        self.work.follow_steps += 1;
        Ok(true)
    }

    /// What one step of following the camera is to cost: [`FOLLOW_SHARE`] of the budget.
    fn follow_target(&self) -> UploadCost {
        let share = |n: u64| n * FOLLOW_SHARE.0 / FOLLOW_SHARE.1;
        UploadCost {
            bytes: share(self.shared.budget_bytes.load(Ordering::Relaxed)),
            writes: share(u64::from(self.shared.budget_writes.load(Ordering::Relaxed))) as u32,
        }
    }

    /// The longest that a step of following the camera is: a step that costs the least that a
    /// Voxel does ([`FOLLOW_MIN_COST`]) at [`Engine::follow_target`], and no more than half a
    /// jump.
    fn follow_limit(&self, d0: f64) -> f64 {
        let target = self.follow_target();
        (target.bytes as f64 / FOLLOW_MIN_COST.0)
            .min(f64::from(target.writes) / FOLLOW_MIN_COST.1)
            .min(render::scene::JUMP * d0 / 2.0)
            .max(1.0)
    }

    /// Where the next step of following the camera to `camera` goes: the camera, or the point
    /// along the way that is the length of the step (half of its limit for the first one).
    fn next_place(&self, camera: [f64; 3]) -> [f64; 3] {
        let (Some(from), Some(bands)) = (self.scene.camera(), self.scene.bands()) else {
            return camera;
        };
        let distance = (0..3)
            .map(|a| (camera[a] - from[a]).powi(2))
            .sum::<f64>()
            .sqrt();
        let limit = self.follow_limit(bands.d0());
        let step = self
            .follow_step
            .unwrap_or(FollowStep::first(limit))
            .0
            .min(limit);
        if distance <= step {
            return camera;
        }
        std::array::from_fn(|a| from[a] + (camera[a] - from[a]) * step / distance)
    }

    /// Sizes the next step by a full step of `distance` Voxels that cost `cost`.
    fn resize_follow_step(&mut self, distance: f64, cost: UploadCost) {
        let Some(bands) = self.scene.bands() else {
            return;
        };
        let limit = self.follow_limit(bands.d0());
        let step = self.follow_step.unwrap_or(FollowStep::first(limit));
        self.follow_step = Some(step.after(distance, cost, self.follow_target(), limit));
    }

    /// Asks the loader for what the bands wait for, nearest `camera` first, and takes in what it
    /// has generated, for [`SLICE`] at the most. Some bricks' children at a time are held, the
    /// scene splits the bricks they are under. While loading is paused it still asks for bricks
    /// and keeps the loader's queue in order for the camera, but takes none in. Whether any were
    /// taken in.
    fn stream(&mut self, camera: [f64; 3]) -> Result<bool, RingsFull> {
        let Some(bands) = self.scene.bands() else {
            return Ok(false);
        };
        let started = self.clock.now();
        // The reach first: what is asked for is classed by it.
        self.loader.set_reach(self.scene.reach());
        {
            let store = self.store.read().expect("the store's lock is not poisoned");
            self.loader.cancel(self.scene.take_cancelled());
            self.loader.request(
                &store,
                self.scene.take_wanted_up_to(WANTED_PER_SLICE),
                camera,
            );
        }
        self.loader.sync(camera, bands);
        // A stepped loader has no workers: they do their jobs here, as far ahead as they may go.
        self.loader.step(STEP_JOBS);
        if self.paused {
            return Ok(false);
        }
        let mut worst = Duration::ZERO;
        let mut took = false;
        // The first batch of a round is taken whatever following the camera has added, if the frame
        // thread has taken all that was published: it uploads that and the batch. Else a camera that
        // moves so that following alone comes to the budget every round would be followed and no
        // arrival would ever be taken in, with the bricks that are asked for waiting behind it.
        let mut guaranteed = self.outbox_is_empty();
        loop {
            // What the loader made waits in its queue, in the order it made it, while the frame
            // thread has more to upload than the budget.
            let estimate = self.batch_estimate();
            if !std::mem::take(&mut guaranteed) && self.over_budget_with(estimate) {
                self.throttled = true;
                break;
            }
            let left = SLICE.saturating_sub(self.clock.since(started));
            let Some(most) = batch_size(left, self.arrival_cost) else {
                break;
            };
            let batch_started = self.clock.now();
            let mut resident = Vec::new();
            let arrivals = self.loader.receive(most);
            if arrivals.is_empty() {
                break;
            }
            {
                let mut store = self
                    .store
                    .write()
                    .expect("the store's lock is not poisoned");
                for arrival in arrivals {
                    match arrival {
                        Arrival::Children { key, bricks } => {
                            self.work.arrived_bricks += bricks.len() as u64;
                            store.hold_children_solid(key.0, key.1, bricks);
                            resident.push(key);
                        }
                        Arrival::Dropped(key) => {
                            self.scene.unwant(key);
                            // The scene looks at it again: that is work for the next round.
                            took = true;
                        }
                    }
                }
            }
            if resident.is_empty() {
                continue;
            }
            took = true;
            let store = self.store.read().expect("the store's lock is not poisoned");
            let before = self.scene.bricks_changed();
            let update = self.scene.arrive(&store, &resident)?;
            drop(store);
            if let Some(SceneUpdate::Rings(patch)) = &update {
                let cost = patch.cost();
                self.batch_cost = cost;
                #[cfg(test)]
                {
                    self.largest_batch.bytes = self.largest_batch.bytes.max(cost.bytes);
                    self.largest_batch.writes = self.largest_batch.writes.max(cost.writes);
                }
            }
            self.work.pack_steps += 1;
            self.work.pack_bricks += self.scene.bricks_changed() - before;
            self.note(update);
            worst = worst.max(self.clock.since(batch_started) / resident.len() as u32);
        }
        // What the next slice expects one to cost: a running mean of the worst batch of each
        // slice, and never more than [`MOST_ARRIVAL_COST`].
        if !worst.is_zero() {
            self.arrival_cost =
                (self.arrival_cost.mul_f64(0.8) + worst.mul_f64(0.2)).min(MOST_ARRIVAL_COST);
        }
        Ok(took)
    }

    /// Takes the children of bricks out of the store while it holds more than its budget. Whether it
    /// is going on. It does a little each time it is called, so that the thread's other work, and
    /// the other threads that use the store, are never held up by it for long.
    ///
    /// It begins when the store holds more than half way between [`brick_store::Budget::bricks`]
    /// and its [`brick_store::Budget::limit`]. It looks at the
    /// units (see [`brick_store::BrickStore::eviction_candidates`]) a part at a time, under the
    /// read lock, for [`EVICT_LOOK`] at a time, and lists those that are not wanted: those the
    /// bands still split, those the scene has split (so every brick drawn, and the ones above it),
    /// and those with a job of the loader among their children. It puts the first of them in
    /// order, with no lock held: furthest out of their band first and then the longest unwanted
    /// (wanted at the earliest pass). Then it takes them, for [`EVICT_HOLD`] under the write lock
    /// at a time with [`EVICT_PAUSE`] between, until the store holds no more than
    /// [`brick_store::Budget::bricks`]. What the store keeps itself is the Guard and the units
    /// over and beside the diverged bricks. Whether a unit is wanted is asked again as each is
    /// taken, since the scene and the loader move on in between. A brick that is needed after all
    /// is asked for by the scene like any other that is not resident, and generated again.
    ///
    /// A job that was given up on and arrives anyway leaves the unit under it resident with none
    /// over it: nothing wants that, so it is listed and taken like any other.
    fn evict(&mut self) -> bool {
        let (Some(camera), Some(bands)) = (self.scene.camera(), self.scene.bands()) else {
            return false;
        };
        if self.evicting.is_none() {
            let store = self.store.read().expect("the store's lock is not poisoned");
            let Some(budget) = store.budget() else {
                return false;
            };
            // It begins half way into the overflow, so that the store that grows while the units are
            // looked at and taken stays within the limit.
            let begins = budget.bricks + (budget.limit() - budget.bricks) / 2;
            if store.streamed_len() <= begins.max(self.evict_from) {
                return false;
            }
            self.evicting = Some(Evicting::Listing {
                pass: store.begin_eviction_pass(),
                began: self.clock.now(),
                next: 0,
                found: Vec::new(),
            });
            self.eviction.passes += 1;
        }
        let done = match self.evicting.take() {
            Some(Evicting::Listing {
                pass,
                began,
                next,
                found,
            }) => {
                let (evicting, done) = self.list(bands, camera, pass, began, next, found);
                self.evicting = evicting;
                done
            }
            Some(Evicting::Taking {
                mut queue,
                mut deferred,
                mut took,
            }) => {
                let done = self.take(bands, camera, &mut queue, &mut deferred, &mut took);
                self.evicting = Some(Evicting::Taking {
                    queue,
                    deferred,
                    took,
                });
                done
            }
            None => true,
        };
        if done {
            let store = self.store.read().expect("the store's lock is not poisoned");
            let (over_limit, over_budget) = (store.over_limit(), store.over_budget());
            self.evict_from = match store.budget() {
                Some(budget) if over_budget => {
                    store.streamed_len()
                        + (budget.limit() - budget.bricks) / EVICT_RETRY.1 * EVICT_RETRY.0
                }
                _ => 0,
            };
            self.evicting = None;
            drop(store);
            // Everything that is left is wanted and it is still over the limit: draw the farthest
            // bands coarser rather than drop a brick that is wanted (ADR 0004).
            self.over_the_cap = over_limit && self.coarsening < COARSENING.len();
        }
        self.evicting.is_some()
    }

    /// At the memory cap, takes the next step of [`COARSENING`] once the last has stood for
    /// [`COARSEN_HOLD`], and has eviction look again.
    fn coarsen_more(&mut self) {
        if !self.over_the_cap || self.clock.since(self.coarsened_at) < COARSEN_HOLD {
            return;
        }
        // What the store holds for each node, while all it holds is wanted: taken before the
        // scene is selected again.
        let per_node = {
            let store = self.store.read().expect("the store's lock is not poisoned");
            store.streamed_len() as f64 / self.scene.nodes().max(1) as f64
        };
        let step = self.coarsening + 1;
        let was_relaxed = self.relaxed;
        if self.coarsen(step) {
            self.units_per_node = per_node;
            self.relaxed = false;
            if was_relaxed {
                self.barred = self.scene.camera().map(|camera| (step, camera));
            }
            self.over_the_cap = false;
            self.evict_from = 0;
        }
    }

    /// Takes the scene to `step` of [`COARSENING`], if it has stood at its step for
    /// [`COARSEN_HOLD`]: the scene is selected and packed again, and the units that the coarser
    /// bands do not want are for eviction to take. Whether it did.
    fn coarsen(&mut self, step: usize) -> bool {
        let Some(bands) = self.user_bands else {
            return false;
        };
        if self.clock.since(self.coarsened_at) < COARSEN_HOLD {
            return false;
        }
        let store = self.store.read().expect("the store's lock is not poisoned");
        let update = self.scene.set_bands(&store, coarsened(bands, step));
        drop(store);
        match update {
            Ok(update) => {
                self.coarsening = step;
                self.coarsened_at = self.clock.now();
                self.note(update);
                true
            }
            // The scene is not to be used again, which the next job finds. It is not selected
            // again every round meanwhile.
            Err(_) => {
                self.coarsened_at = self.clock.now();
                false
            }
        }
    }

    /// Takes a step of coarsening back when what the scene would want with it taken back fits the
    /// store's budget with room to spare ([`RELAX_BELOW`]): the camera has slowed or settled, so
    /// the lead and the trail are gone, or it is where less is wanted. What the store holds says
    /// nothing of it, since eviction stops once the store is under its budget: this is worked out
    /// from the nodes the scene wants, as the bands' [`Bands::weight`] of the step before is to that
    /// of now, and the units the store held for a node when the step was taken.
    fn relax(&mut self) {
        let Some(bands) = self.user_bands else {
            return;
        };
        if self.coarsening == 0
            || self.loader.pending() != 0
            || self.evicting.is_some()
            || self.clock.since(self.coarsened_at) < COARSEN_HOLD
        {
            return;
        }
        if let Some((step, from)) = self.barred {
            let moved = self.scene.camera().map_or(f64::INFINITY, |camera| {
                (0..3)
                    .map(|a| (camera[a] - from[a]).powi(2))
                    .sum::<f64>()
                    .sqrt()
            });
            if step == self.coarsening && moved <= bands.d0() / BARRED_FOR {
                return;
            }
            self.barred = None;
        }
        let before = coarsened(bands, self.coarsening - 1).weight();
        let now = coarsened(bands, self.coarsening).weight();
        let would_want = self.scene.nodes() as f64 * before / now * self.units_per_node;
        let fits = {
            let store = self.store.read().expect("the store's lock is not poisoned");
            store.budget().is_some_and(|budget| {
                would_want * (RELAX_BELOW.1 as f64) < (budget.bricks * RELAX_BELOW.0) as f64
            })
        };
        if fits && self.coarsen(self.coarsening - 1) {
            self.relaxed = true;
        }
    }

    /// Looks ahead along `velocity` from `camera`: the places [`LEAD`] seconds on, which the scene
    /// wants the children of the bricks near at the lowest priority. A lead that has moved by less
    /// than a sixteenth of `d0` is kept, so that a camera whose velocity is noise does not make the
    /// selection look again.
    fn look_ahead(&mut self, camera: [f64; 3], velocity: [f64; 3]) {
        self.velocity = velocity;
        let Some(bands) = self.scene.bands() else {
            return;
        };
        let speed = velocity.iter().map(|v| v * v).sum::<f64>().sqrt();
        let ahead: [Option<[f64; 3]>; AHEAD] = if speed < LEAD_FROM_SPEED {
            [None; AHEAD]
        } else {
            std::array::from_fn(|i| {
                Some(std::array::from_fn(|a| camera[a] + velocity[a] * LEAD[i]))
            })
        };
        let kept = ahead.iter().zip(&self.ahead).all(|pair| match pair {
            (None, None) => true,
            (Some(new), Some(old)) => {
                let moved = (0..3)
                    .map(|a| (new[a] - old[a]).powi(2))
                    .sum::<f64>()
                    .sqrt();
                moved <= bands.d0() / 16.0
            }
            _ => false,
        });
        if !kept {
            self.ahead = ahead;
            self.scene.set_ahead(ahead);
        }
    }

    /// Looks at parts of the units for [`EVICT_LOOK`], from part `next`. Returns the state it is
    /// in after, and whether eviction is done (there is nothing to take).
    fn list(
        &mut self,
        bands: Bands,
        camera: [f64; 3],
        pass: u64,
        began: Instant,
        mut next: usize,
        mut found: Vec<Candidate>,
    ) -> (Option<Evicting>, bool) {
        let wanted = wanted(&self.scene, &self.loader, bands, camera);
        // How many levels a unit's children are from the level the bands use at its distance.
        let gap = |level: usize, coord| {
            let owner = bands.level_at(brick_distance(camera, level, coord), LEVELS - 1);
            (level as i64 - 1 - owner as i64).unsigned_abs() as u32
        };
        let started = self.clock.now();
        while next < BrickStore::EVICTION_PARTS && self.clock.since(started) < EVICT_LOOK {
            let store = self.store.read().expect("the store's lock is not poisoned");
            let held = self.clock.now();
            store.eviction_candidates(next, pass, &wanted, gap, &mut found);
            let took = self.clock.since(held);
            self.eviction.longest_read = self.eviction.longest_read.max(took);
            self.eviction.reads += 1;
            self.eviction.read_total += took;
            drop(store);
            next += 1;
        }
        if next < BrickStore::EVICTION_PARTS {
            let state = Evicting::Listing {
                pass,
                began,
                next,
                found,
            };
            return (Some(state), false);
        }
        // The first that are to be taken, in order: as many as the store is over by (each takes
        // a unit's entry at least).
        let over = {
            let store = self.store.read().expect("the store's lock is not poisoned");
            let budget = store.budget().expect("eviction began with a budget");
            store.streamed_len().saturating_sub(budget.bricks)
        };
        if over < found.len() {
            found.select_nth_unstable_by_key(over, Candidate::order);
            found.truncate(over);
        }
        found.sort_unstable_by_key(Candidate::order);
        let listing = self.clock.since(began);
        self.eviction.listing = listing;
        self.eviction.longest_listing = self.eviction.longest_listing.max(listing);
        let done = found.is_empty();
        let queue = found.iter().map(Candidate::unit).collect();
        let state = Evicting::Taking {
            queue,
            deferred: Vec::new(),
            took: false,
        };
        (Some(state), done)
    }

    /// Takes units of `queue` away for [`EVICT_HOLD`], then lets go of the store for
    /// [`EVICT_PAUSE`]. Whether it is done: the store is within its budget, or there is nothing
    /// more that can be taken.
    fn take(
        &mut self,
        bands: Bands,
        camera: [f64; 3],
        queue: &mut VecDeque<(usize, BrickCoord)>,
        deferred: &mut Vec<(usize, BrickCoord)>,
        took: &mut bool,
    ) -> bool {
        let wanted = wanted(&self.scene, &self.loader, bands, camera);
        let mut done = false;
        for _ in 0..EVICT_HOLDS {
            let mut store = self
                .store
                .write()
                .expect("the store's lock is not poisoned");
            let held = self.clock.now();
            let until = held + EVICT_HOLD;
            // Edits are counted first, a brick at a time, so that what they keep is known.
            if store.sync_diverged(self.clock.deadline(until)) {
                while self.clock.now() < until {
                    if !store.over_budget() {
                        done = true;
                        break;
                    }
                    let Some((level, coord)) = queue.pop_front() else {
                        if *took && !deferred.is_empty() {
                            *queue = std::mem::take(deferred).into();
                            *took = false;
                            continue;
                        }
                        done = true;
                        break;
                    };
                    match store.evict_children(level, coord, &wanted) {
                        Some(bricks) => {
                            *took = true;
                            self.work.evicted_units += 1;
                            self.work.evicted_bricks += bricks as u64;
                        }
                        None => deferred.push((level, coord)),
                    }
                }
            }
            drop(store);
            self.eviction.longest_write = self.eviction.longest_write.max(self.clock.since(held));
            if done {
                break;
            }
            self.clock.pause(EVICT_PAUSE);
        }
        done
    }

    /// Makes what changed since the last publish available to the frame thread, merged with what
    /// it has not taken yet, if there is anything to say; and tells a job's waiter it is done.
    /// What the frame thread has not taken stays in the outbox while it is merged, under the lock
    /// that it takes with, so that it never finds the outbox empty when a job is done; it takes
    /// with a try-lock, except after a job, so the copying costs it nothing.
    fn publish(&mut self, job: Option<u64>) {
        self.scan_for_holes();
        let began = self.began();
        self.publish_changes(job);
        self.spend(Part::Publish, began);
    }

    /// Looks at every split brick for a hole, if the bench asked for that and [`HOLES_EVERY`] has
    /// gone by since the last look, or a [`Job::Look`] says to now (`Scene::holes`).
    fn scan_for_holes(&mut self) {
        let forced = std::mem::take(&mut self.look_due);
        let due = forced || self.clock.since(self.holes_at) >= HOLES_EVERY;
        if !self.shared.count_holes.load(Ordering::Relaxed) || !due {
            return;
        }
        let began = self.began();
        self.shared.looking.store(true, Ordering::Relaxed);
        let store = self.store.read().expect("the store's lock is not poisoned");
        let found = self.scene.holes(&store);
        drop(store);
        self.shared.looking.store(false, Ordering::Relaxed);
        self.holes_at = self.clock.now();
        self.work.hole_scans += 1;
        self.work.holes_found += found as u64;
        self.spend(Part::HoleScan, began);
    }

    fn publish_changes(&mut self, job: Option<u64>) {
        let exactly = self.shared.count_exactly.load(Ordering::Relaxed);
        if self.clock.since(self.slow_at) >= TABLES_EVERY {
            self.slow_at = self.clock.now();
            self.slow = slow_of(&self.scene, &self.loader, exactly);
        } else if exactly {
            // The words by level are of the scene as it is in this publish, with the bricks drawn
            // in it: a round publishes several times, and the others would keep the words of an
            // earlier one. The game does not count exactly and does not pay for this.
            self.slow = Arc::new(Slow {
                tables: self.slow.tables.clone(),
                used_bytes: self.slow.used_bytes,
                words_by_level: self.scene.words_by_level(),
            });
        }
        while let Ok(garbage) = self.graveyard.try_recv() {
            if let Garbage::Copy(copy) = garbage
                && self.spare.len() < 4
            {
                self.spare.push(*copy);
            }
        }
        let mut stats = stats_of(&self.scene, &self.loader, &self.slow, self.eviction);
        stats.coarsening = self.coarsening;
        stats.work = self.work;
        stats.throttled = self.throttled;
        #[cfg(test)]
        {
            stats.excess_ops = std::mem::take(&mut self.excess_ops);
            stats.largest_op = self.largest_op;
            stats.largest_batch = self.largest_batch;
            stats.largest_follow = self.largest_follow;
        }
        // The look that a job asked for is done by the scan before this (the holes) and here (the
        // count), and is not due again.
        let recount_due = std::mem::take(&mut self.recount_due);
        if self.shared.count_soft_misses.load(Ordering::Relaxed) {
            // Looking at every brick that waits takes milliseconds in a fill, so it is done a
            // few times a second, and the count is the last one in between, unless the counts are
            // asked for exactly (the bench's: one for each frame). When nothing waits any more
            // the count is made at once, so that a last count of misses is not left standing
            // while the thread sleeps (it looks at no brick then).
            if exactly
                || recount_due
                || self.clock.since(self.soft_misses_at) >= SOFT_MISSES_EVERY
                || (self.soft_misses > 0 && stats.waiting == 0)
            {
                self.soft_misses_at = self.clock.now();
                self.soft_misses_by_level = self.scene.soft_misses_by_level();
                self.soft_misses = self.soft_misses_by_level.iter().sum();
                if exactly {
                    let store = self.store.read().expect("the store's lock is not poisoned");
                    self.holes = self.scene.holes(&store);
                }
            }
            stats.soft_misses = self.soft_misses;
            stats.soft_misses_by_level = self.soft_misses_by_level;
            stats.holes = self.holes;
        }
        let changes = std::mem::replace(&mut self.changes, Changes::None);
        if matches!(changes, Changes::None)
            && stats.same_counts(&self.published, exactly)
            && job.is_none()
        {
            return;
        }
        // A copy of all of the scene is tens of milliseconds: it is made before the lock is taken.
        let rebuilt = matches!(changes, Changes::Rebuilt).then(|| Box::new(self.scene.mirror()));
        let mut outbox = lock(&self.shared.outbox);
        match (changes, rebuilt) {
            (_, Some(mirror)) => {
                outbox.ready.mirror = Some(mirror);
                outbox.ready.patch = None;
            }
            (Changes::Patch(patch), None) => {
                if let Some(mirror) = outbox.ready.mirror.as_mut() {
                    // A copy of all of the scene is waiting: the changes go into it.
                    let _ = mirror.paste(&self.scene.copy_patch_into(patch, PatchCopy::default()));
                } else {
                    let (merged, buffers) = match outbox.ready.patch.take() {
                        Some(mut copy) => {
                            copy.patch.merge(patch);
                            (std::mem::take(&mut copy.patch), copy)
                        }
                        None => (patch, self.spare.pop().unwrap_or_default()),
                    };
                    let copy = self.scene.copy_patch_into(merged, buffers);
                    self.pending = copy.patch.cost();
                    outbox.ready.patch = Some(copy);
                }
            }
            (Changes::None | Changes::Rebuilt, None) => {}
        }
        outbox.ready.stats = Some(stats.clone());
        if exactly {
            outbox.ready.published_at.get_or_insert(self.clock.now());
        }
        if let Some(ticket) = job {
            outbox.done = outbox.done.max(ticket);
        }
        drop(outbox);
        if job.is_some() {
            self.shared.finished.notify_all();
        }
        self.published = stats;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_follow_step_is_sized_by_how_its_cost_compared_to_its_share_and_is_never_over_the_limit() {
        let target = UploadCost {
            bytes: 2 << 20,
            writes: 512,
        };
        let cost = |bytes, writes| UploadCost { bytes, writes };
        let step = FollowStep(32.0);
        // Half of what it may cost: twice as far. A tenth: still twice, and not over the limit.
        assert_eq!(step.after(32.0, cost(1 << 20, 100), target, 64.0).0, 64.0);
        assert_eq!(step.after(32.0, cost(200_000, 10), target, 100.0).0, 64.0);
        assert_eq!(step.after(32.0, cost(200_000, 10), target, 40.0).0, 40.0);
        // Over it by half: two thirds of the step; far over: half.
        let over = step.after(32.0, cost(3 << 20, 100), target, 64.0).0;
        assert!((over - 32.0 * 2.0 / 3.0).abs() < 1e-9, "{over}");
        assert_eq!(step.after(32.0, cost(40 << 20, 100), target, 64.0).0, 16.0);
        // The copies bound it as well as the bytes.
        assert_eq!(step.after(32.0, cost(1 << 10, 2048), target, 64.0).0, 16.0);
        // A step that cost nothing (the scene did not change) is twice as far, but not over the
        // limit; and a run of costly ones halves it to a quarter of the limit, and no less.
        assert_eq!(
            step.after(32.0, UploadCost::default(), target, 64.0).0,
            64.0
        );
        let mut step = FollowStep(64.0);
        for _ in 0..40 {
            step = step.after(step.0, cost(1 << 30, 100_000), target, 64.0);
        }
        assert_eq!(step.0, 16.0);
        // The least is a Voxel for a limit of a few.
        assert_eq!(step.after(1.0, cost(1 << 30, 9), target, 2.0).0, 1.0);
    }

    #[test]
    fn the_first_follow_step_is_half_the_limit_and_at_least_a_voxel() {
        assert_eq!(FollowStep::first(64.0).0, 32.0);
        assert_eq!(FollowStep::first(3.0).0, 1.5);
        // A budget of 192 KiB gives a limit of 1.5 Voxels, and half of it was 0.75.
        let limit = (192.0 * 1024.0 / 4.0) / FOLLOW_MIN_COST.0;
        assert_eq!(limit, 1.5);
        assert_eq!(FollowStep::first(limit).0, 1.0);
        assert_eq!(FollowStep::first(1.0).0, 1.0);
        assert_eq!(FollowStep::first(0.2).0, 1.0);
    }

    #[test]
    fn the_least_a_voxel_costs_makes_a_step_at_most_64_voxels_at_the_default_budget() {
        let share = UPLOAD_BUDGET.bytes * FOLLOW_SHARE.0 / FOLLOW_SHARE.1;
        assert_eq!((share as f64 / FOLLOW_MIN_COST.0) as u64, 64);
        let writes = u64::from(UPLOAD_BUDGET.writes) * FOLLOW_SHARE.0 / FOLLOW_SHARE.1;
        assert_eq!((writes as f64 / FOLLOW_MIN_COST.1) as u64, 256);
    }

    #[test]
    fn a_batch_is_expected_to_fit_in_what_is_left_of_the_slice() {
        let cost = Duration::from_micros(50);
        assert_eq!(batch_size(Duration::ZERO, cost), None);
        assert_eq!(batch_size(Duration::from_micros(49), cost), None);
        // Room for 3: all three, since fewer than the least batch fit.
        assert_eq!(batch_size(Duration::from_micros(160), cost), Some(3));
        for left_us in [100u64, 500, 2000, 6000, 100_000] {
            let left = Duration::from_micros(left_us);
            let most = batch_size(left, cost).unwrap();
            assert!(most <= BATCH);
            assert!(cost * most as u32 <= left, "{left_us}: {most}");
            // Unless the room is small, a batch leaves some of it for one that cost more.
            if left_us >= 1000 {
                assert!(cost * most as u32 <= left * 4 / 5, "{left_us}: {most}");
            }
        }
    }
}
