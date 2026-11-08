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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use brick_store::{BrickCoord, BrickStore, Candidate, LodWork, TableSize};
use render::lod::{Bands, LEVELS, brick_distance};
use render::{PatchCopy, RingPatch, RingsFull, Scene, SceneUpdate};
use sim::SharedStore;

use crate::loader::{Arrival, Loader};

/// The time this thread spends taking in bricks before it looks at what the frame thread has
/// asked of it (a new camera, an Edit) and publishes what it has done.
const SLICE: Duration = Duration::from_millis(2);
/// How long it sleeps when there was nothing to do but the loader has bricks on their way, before
/// it looks again: the arrivals do not wake it. With nothing asked for it sleeps until it is told.
const IDLE: Duration = Duration::from_millis(1);
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
/// wanted), it is not begun again until the store has grown by a quarter: looking at every unit
/// is not free. This is what happens at the cap for now: the LOD is not coarsened to fit it
/// (#143).
const EVICT_BACKOFF: (usize, usize) = (5, 4);

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
    pub loader: LoaderStats,
    /// What costs more to find out, as of a few times a second.
    pub slow: Arc<Slow>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub eviction: EvictionStats,
    /// Where the scene had been moved to.
    #[cfg(test)]
    pub camera: Option<[f64; 3]>,
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
}

impl Stats {
    /// Whether `other` has the same counts, which are cheap to look at: the rest changes slowly.
    fn same_counts(&self, other: &Stats) -> bool {
        self.waiting == other.waiting
            && self.loader == other.loader
            && self.level_bricks == other.level_bricks
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
        loader: LoaderStats::of(loader),
        slow: Arc::clone(slow),
        eviction,
        #[cfg(test)]
        camera: scene.camera(),
    }
}

/// The parts of [`Stats`] that cost more to find out.
fn slow_of(scene: &Scene, loader: &Loader) -> Arc<Slow> {
    let mut tables = scene.table_sizes();
    tables.extend(loader.table_sizes());
    Arc::new(Slow {
        tables,
        used_bytes: scene.used_bytes(),
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
}

/// A job for the thread that is not following the camera.
enum Job {
    /// Bricks of level 0 that an Edit changed (see [`Scene::update`]).
    Edit(Vec<BrickCoord>),
    /// Other LOD bands (see [`Scene::set_bands`]).
    Bands(Bands),
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
    /// Jobs that the frame thread waits for, with the number each is to be told done by.
    jobs: VecDeque<(u64, Job)>,
    issued: u64,
    shutdown: bool,
}

struct Outbox {
    ready: Ready,
    /// Jobs finished, and published: the number of the last.
    done: u64,
    /// The scene ran out of room and is not to be used again.
    failed: bool,
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
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("the scene thread panicked")
}

impl SceneWorker {
    /// Starts the thread with `scene`, a scene of rings, and the `loader` that makes the bricks it
    /// waits for, reading and holding bricks in `store`. Returns the frame thread's end of it, a
    /// copy of the scene to draw (see [`Scene::mirror`]), and its counts.
    pub fn start(scene: Scene, loader: Loader, store: SharedStore) -> (Self, Scene, Stats) {
        assert!(scene.is_rings(), "only a scene of rings follows a camera");
        let mirror = scene.mirror();
        let slow = slow_of(&scene, &loader);
        let stats = stats_of(&scene, &loader, &slow, EvictionStats::default());
        let camera = scene.camera().expect("a scene of rings has a camera");
        let shared = Arc::new(Shared {
            inbox: Mutex::new(Inbox {
                camera,
                jobs: VecDeque::new(),
                issued: 0,
                shutdown: false,
            }),
            wake: Condvar::new(),
            outbox: Mutex::new(Outbox {
                ready: Ready::default(),
                done: 0,
                failed: false,
                died: false,
            }),
            finished: Condvar::new(),
            paused: AtomicBool::new(false),
        });
        let (garbage, graveyard) = mpsc::channel();
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
            slow_at: Instant::now(),
            coarse: VecDeque::new(),
            sync_waiting: None,
            evicting: None,
            evict_from: 0,
            eviction: EvictionStats::default(),
            #[cfg(test)]
            coarse_sleep: Duration::ZERO,
        };
        let thread = std::thread::Builder::new()
            .name("scene".into())
            .spawn(move || engine.run())
            .expect("a thread for the scene");
        (
            Self {
                shared,
                garbage,
                thread: Some(thread),
            },
            mirror,
            stats,
        )
    }

    /// Tells the thread where the camera is, a position in Voxels: it follows it when it has the
    /// time, and a camera that moved on again by then is not followed to. Does not wait: if the
    /// thread is at the camera at this moment, it is told the next time.
    pub fn follow(&self, camera: [f64; 3]) {
        if let Ok(mut inbox) = self.shared.inbox.try_lock()
            && inbox.camera != camera
        {
            inbox.camera = camera;
            self.shared.wake.notify_one();
        }
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
        let until = timeout.map(|t| Instant::now() + t);
        let mut outbox = lock(&self.shared.outbox);
        while outbox.done < ticket && !outbox.failed && !outbox.died {
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
        if outbox.failed {
            return Err(RingsFull);
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
        if outbox.failed {
            return Err(RingsFull);
        }
        Ok(std::mem::take(&mut outbox.ready))
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
    /// Eviction does not begin before the store holds more than this (see [`EVICT_BACKOFF`]).
    evict_from: usize,
    eviction: EvictionStats,
    #[cfg(test)]
    coarse_sleep: Duration,
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

/// Whether the unit `(level, coord)`, whose children are resident, is wanted: the bands split it,
/// the scene has split it (every brick drawn is under one that is split, and so is every brick
/// over it), or the loader is making the children of one of its children, which would be held
/// under it.
fn wanted<'a>(
    scene: &'a Scene,
    loader: &'a Loader,
    bands: Bands,
    camera: [f64; 3],
) -> impl Fn(usize, BrickCoord) -> bool + 'a {
    move |level, coord| {
        bands.split(level, brick_distance(camera, level, coord), Some(true))
            || scene.splits(level, coord)
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
        let mut idle = false;
        loop {
            let (camera, jobs) = {
                let mut inbox = lock(&self.shared.inbox);
                if idle && inbox.jobs.is_empty() && Some(inbox.camera) == self.camera {
                    let shared = Arc::clone(&self.shared);
                    let (camera, paused) = (self.camera, self.paused);
                    let quiet = |inbox: &mut Inbox| {
                        inbox.jobs.is_empty()
                            && Some(inbox.camera) == camera
                            && !inbox.shutdown
                            && shared.paused.load(Ordering::Acquire) == paused
                    };
                    inbox = if self.published.loader.pending == 0 {
                        // Nothing is on its way: only the frame thread can give it something to do.
                        self.shared
                            .wake
                            .wait_while(inbox, |inbox| quiet(inbox))
                            .expect("the frame thread panicked")
                    } else {
                        self.shared
                            .wake
                            .wait_timeout(inbox, IDLE)
                            .expect("the frame thread panicked")
                            .0
                    };
                }
                if inbox.shutdown {
                    return;
                }
                (inbox.camera, std::mem::take(&mut inbox.jobs))
            };
            self.paused = self.shared.paused.load(Ordering::Acquire);
            let mut worked = !jobs.is_empty();
            // A sync is done when the rest of what the thread does in this round is.
            #[cfg_attr(not(test), allow(unused_mut))]
            let mut synced: Option<u64> = None;
            for (ticket, job) in jobs {
                #[cfg(test)]
                match job {
                    Job::Sync => {
                        synced = Some(ticket);
                        continue;
                    }
                    Job::Panic => panic!("a panic that a test asked for"),
                    Job::Stall(time) => {
                        std::thread::sleep(time);
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
                if self.run_job(job).is_err() {
                    self.fail();
                    return;
                }
                self.publish(Some(ticket));
            }
            let outcome = self
                .remake_coarse()
                .and_then(|remade| {
                    worked |= remade;
                    self.follow(camera)
                })
                .and_then(|moved| {
                    worked |= moved;
                    self.stream(camera)
                });
            match outcome {
                Ok(took) => worked |= took,
                Err(RingsFull) => {
                    self.fail();
                    return;
                }
            }
            worked |= self.evict();
            // A sync is done when the coarse levels over the Edits before it are made, too.
            let busy = !self.coarse.is_empty();
            let synced = if busy {
                self.sync_waiting = synced.or(self.sync_waiting);
                None
            } else {
                synced.or(self.sync_waiting.take())
            };
            self.publish(synced);
            idle = !worked && !busy;
        }
    }

    /// The scene ran out of room: the frame thread is told, and waiting jobs end.
    fn fail(&mut self) {
        lock(&self.shared.outbox).failed = true;
        self.shared.finished.notify_all();
    }

    fn note(&mut self, update: Option<SceneUpdate>) {
        match update {
            None => {}
            Some(SceneUpdate::Rings(patch)) => match &mut self.changes {
                Changes::None => self.changes = Changes::Patch(patch),
                Changes::Patch(before) => before.merge(patch),
                Changes::Rebuilt => {}
            },
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
            Job::Bands(bands) => self.scene.set_bands(&store, bands)?,
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
        std::thread::sleep(self.coarse_sleep);
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
            store.step_lod(work, Some(Instant::now() + COARSE_STEP))
        };
        if done {
            let finished = self.coarse.pop_front().expect("there is a front");
            let store = self.store.read().expect("the store's lock is not poisoned");
            let update = self.scene.update(&store, &finished.changed)?;
            drop(store);
            self.note(Some(update));
        } else {
            // Long enough for a reader that waits for the lock to take it.
            std::thread::sleep(COARSE_PAUSE);
        }
        Ok(true)
    }

    /// Moves the scene to `camera`, if it is not there. Whether it was moved.
    fn follow(&mut self, camera: [f64; 3]) -> Result<bool, RingsFull> {
        if self.camera == Some(camera) {
            return Ok(false);
        }
        let store = self.store.read().expect("the store's lock is not poisoned");
        let update = self.scene.recentre(&store, camera)?;
        drop(store);
        self.camera = Some(camera);
        self.note(update);
        Ok(true)
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
        let started = Instant::now();
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
        if self.paused {
            return Ok(false);
        }
        let mut worst = Duration::ZERO;
        let mut took = false;
        loop {
            let left = SLICE.saturating_sub(started.elapsed());
            let Some(most) = batch_size(left, self.arrival_cost) else {
                break;
            };
            let batch_started = Instant::now();
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
                            store.hold_children_solid(key.0, key.1, bricks);
                            resident.push(key);
                        }
                        Arrival::Dropped(key) => self.scene.unwant(key),
                    }
                }
            }
            if resident.is_empty() {
                continue;
            }
            took = true;
            let store = self.store.read().expect("the store's lock is not poisoned");
            let update = self.scene.arrive(&store, &resident)?;
            drop(store);
            self.note(update);
            worst = worst.max(batch_started.elapsed() / resident.len() as u32);
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
                began: Instant::now(),
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
            self.evict_from = if store.over_budget() {
                store.streamed_len() / EVICT_BACKOFF.1 * EVICT_BACKOFF.0
            } else {
                0
            };
            self.evicting = None;
        }
        self.evicting.is_some()
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
        let started = Instant::now();
        while next < BrickStore::EVICTION_PARTS && started.elapsed() < EVICT_LOOK {
            let store = self.store.read().expect("the store's lock is not poisoned");
            let held = Instant::now();
            store.eviction_candidates(next, pass, &wanted, gap, &mut found);
            let took = held.elapsed();
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
        let listing = began.elapsed();
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
            let held = Instant::now();
            let until = held + EVICT_HOLD;
            // Edits are counted first, a brick at a time, so that what they keep is known.
            if store.sync_diverged(Some(until)) {
                while Instant::now() < until {
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
                        Some(_) => *took = true,
                        None => deferred.push((level, coord)),
                    }
                }
            }
            drop(store);
            self.eviction.longest_write = self.eviction.longest_write.max(held.elapsed());
            if done {
                break;
            }
            std::thread::sleep(EVICT_PAUSE);
        }
        done
    }

    /// Makes what changed since the last publish available to the frame thread, merged with what
    /// it has not taken yet, if there is anything to say; and tells a job's waiter it is done.
    /// What the frame thread has not taken stays in the outbox while it is merged, under the lock
    /// that it takes with, so that it never finds the outbox empty when a job is done; it takes
    /// with a try-lock, except after a job, so the copying costs it nothing.
    fn publish(&mut self, job: Option<u64>) {
        if self.slow_at.elapsed() >= TABLES_EVERY {
            self.slow_at = Instant::now();
            self.slow = slow_of(&self.scene, &self.loader);
        }
        while let Ok(garbage) = self.graveyard.try_recv() {
            if let Garbage::Copy(copy) = garbage
                && self.spare.len() < 4
            {
                self.spare.push(*copy);
            }
        }
        let stats = stats_of(&self.scene, &self.loader, &self.slow, self.eviction);
        let changes = std::mem::replace(&mut self.changes, Changes::None);
        if matches!(changes, Changes::None) && stats.same_counts(&self.published) && job.is_none() {
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
                    mirror.paste(&self.scene.copy_patch_into(patch, PatchCopy::default()));
                } else {
                    let (merged, buffers) = match outbox.ready.patch.take() {
                        Some(mut copy) => {
                            copy.patch.merge(patch);
                            (std::mem::take(&mut copy.patch), copy)
                        }
                        None => (patch, self.spare.pop().unwrap_or_default()),
                    };
                    outbox.ready.patch = Some(self.scene.copy_patch_into(merged, buffers));
                }
            }
            (Changes::None | Changes::Rebuilt, None) => {}
        }
        outbox.ready.stats = Some(stats.clone());
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
