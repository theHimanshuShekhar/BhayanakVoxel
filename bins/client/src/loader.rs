//! The ring loader (#86): generates the Planet's bricks on worker threads, as the LOD bands ask
//! for them, so that the frame thread never generates one.
//!
//! The scene draws a brick whole, a coarser frame, until its children (the eight bricks of the
//! level below that it covers) are resident (see [`brick_store::BrickStore::stream_from`]), and
//! says which it is waiting for: [`render::Scene::take_wanted`]. Those are
//! [`Loader::request`]ed, nearest the camera first. The workers read the Planet's generator
//! ([`BrickSource`]) for each one's children and send them back, and the frame thread takes in a
//! bounded number per frame ([`Loader::receive`]) and tells the scene, which splits the brick.
//! That brings new bricks to be wanted, a level finer: the world fills in outward and downward
//! until the resident set is complete for the bands, and then nothing is wanted and the workers
//! sleep. Whether or not the camera moves: moving only changes which are nearest, and which are
//! wanted at all.
//!
//! **Nearest first, end to end.** The workers run only [`MAX_AHEAD`] jobs ahead of what the frame
//! thread has taken in. Past that they wait, and the jobs stay in the queue, which is ordered by
//! distance from the camera and put in order again as it moves. So a brick that is wanted newly
//! and is near is generated within a few frames, not after everything that was asked for before.
//! (Without the bound the workers finish far faster than a frame can take bricks in, and what is
//! finished piles up in the channel in the order it was asked for.)
//!
//! A job is dropped, not run, if the scene no longer wants it: the camera has gone from where the
//! bands would split its brick, or its brick was merged or buried ([`Loader::cancel`]). So
//! nothing is generated beyond the bands. The workers run at the lowest priority
//! ([`crate::priority`]), and there are two fewer of them than cores, for the simulation and the
//! frame thread. A worker that panics is reported on the frame thread, which panics too.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering as Atomic};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::thread::JoinHandle;

use brick_store::{Brick, BrickCoord, BrickSource, BrickStore, children_from};
use render::lod::{Bands, brick_distance};
use render::scene::select::Key;

/// The first level of the Guard: the levels from here to the top of the pyramid (8 to 12, since
/// [`render::lod::LEVELS`] is 13) are generated for the whole Planet right after the spawn ring
/// (ADR 0004).
pub const GUARD_FIRST: usize = 8;

/// Most jobs that the workers have taken from the queue and the frame thread has not yet taken
/// in: being generated, or finished and waiting in the channel. A brick wanted newly waits for at
/// most this many to be taken in before it is taken. The frame takes in a hundred or so a frame.
pub const MAX_AHEAD: usize = 512;

/// The Guard: the whole Planet at the levels from `first` to the top of `store`'s pyramid, held
/// in it; from then on the store streams, and a brick of level `first` is split when its children
/// are resident. Returns how many bricks it holds. The bricks, and whether each is solid all the
/// way down, are worked out on all the cores; holding them is serial.
pub fn hold_guard(
    store: &mut BrickStore,
    planet: &planet_gen::planet::Planet,
    first: usize,
) -> usize {
    let top = store.level_count() - 1;
    let guard = planet.guard(first..=top);
    let held = guard.len();
    for (level, coord, brick, solid) in guard {
        store.hold(level, coord, brick, solid);
    }
    store.stream_from(first);
    held
}

/// One brick's children to generate.
#[derive(Clone)]
struct Job {
    /// How far `key` was from the camera when this was queued or last ordered.
    distance: f64,
    key: Key,
    /// Which request this is: a brick can be asked for again after it was given up on.
    ticket: u64,
    /// The children that are held already, which are not generated: bit `i` for the `i`th of
    /// [`BrickCoord::children`].
    held: u8,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Job {}

impl PartialOrd for Job {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Nearest is greatest, for the max-heap that [`BinaryHeap`] is.
impl Ord for Job {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .distance
            .total_cmp(&self.distance)
            .then_with(|| other.key.cmp(&self.key))
    }
}

struct State {
    queue: BinaryHeap<Job>,
    camera: [f64; 3],
    bands: Bands,
    /// The camera that the queue was last ordered for.
    ordered_for: [f64; 3],
    /// Set when the camera has gone far enough that the queue is no longer in order.
    stale: bool,
    /// Tickets of jobs that were given up on: skipped when they come to the front. A ticket of a
    /// job that had gone to a worker already stays until the queue is empty.
    cancelled: HashSet<u64>,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    source: Arc<dyn BrickSource>,
    /// Bricks the workers have asked the generator for.
    generated: AtomicU64,
    /// Jobs taken from the queue that the frame thread has not taken in (see [`MAX_AHEAD`]).
    ahead: AtomicUsize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("a ring loader thread panicked")
    }
}

/// What a worker sends back.
pub enum Arrival {
    /// The children of `key` that are not air and were not held, as
    /// [`brick_store::BrickStore::hold_children`] takes them.
    Children {
        key: Key,
        bricks: Vec<(BrickCoord, Brick)>,
    },
    /// The camera went before the children of `key` were generated: they were not.
    Dropped(Key),
}

/// The workers, and the work they have been asked for. Lives on the frame thread, which is the
/// only one that talks to it.
pub struct Loader {
    shared: Arc<Shared>,
    arrivals: Receiver<(u64, Arrival)>,
    workers: Vec<JoinHandle<()>>,
    /// Jobs not yet handed to the workers, for when they are busy with the lock.
    outbox: Vec<Job>,
    /// Tickets given up on that the workers have not been told of yet.
    cancels: Vec<u64>,
    /// Bricks asked for whose children have not been received, and the request.
    requested: HashMap<Key, u64>,
    next_ticket: u64,
}

/// Workers for `cores` cores: two fewer, so that the simulation and the frame thread always have
/// one each, and at least one.
pub fn workers_for(cores: usize) -> usize {
    cores.saturating_sub(2).max(1)
}

impl Loader {
    /// A loader with `workers` threads, generating from `source`, for a camera at `camera` and
    /// the LOD `bands`.
    pub fn new(
        source: Arc<dyn BrickSource>,
        camera: [f64; 3],
        bands: Bands,
        workers: usize,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: BinaryHeap::new(),
                camera,
                bands,
                ordered_for: camera,
                stale: false,
                cancelled: HashSet::new(),
                shutdown: false,
            }),
            wake: Condvar::new(),
            source,
            generated: AtomicU64::new(0),
            ahead: AtomicUsize::new(0),
        });
        let (sender, arrivals) = mpsc::channel();
        let workers = (0..workers.max(1))
            .map(|n| {
                let (shared, sender) = (Arc::clone(&shared), sender.clone());
                std::thread::Builder::new()
                    .name(format!("ring loader {n}"))
                    .spawn(move || work(&shared, &sender))
                    .expect("a thread for the loader")
            })
            .collect();
        Self {
            shared,
            arrivals,
            workers,
            outbox: Vec::new(),
            cancels: Vec::new(),
            requested: HashMap::new(),
            next_ticket: 0,
        }
    }

    /// Panics if a worker has stopped, which only a panic in the generator does: loading would
    /// otherwise stop without a word, with bricks asked for and never coming.
    fn assert_alive(&self) {
        assert!(
            !self.workers.iter().any(JoinHandle::is_finished),
            "a ring loader thread panicked"
        );
    }

    /// Makes room for `n` bricks asked for at once, in the tables and the queue, so that they do
    /// not double in the middle of a frame.
    pub fn reserve(&mut self, n: usize) {
        self.requested.reserve(n);
        self.outbox.reserve(n.min(1 << 16));
        self.shared.lock().queue.reserve(n);
    }

    /// Asks for the children of `keys`, the bricks that [`render::Scene::take_wanted`] says wait
    /// for them. The nearest to the camera are generated first. A brick already asked for is not
    /// asked for again.
    pub fn request(
        &mut self,
        store: &BrickStore,
        keys: impl IntoIterator<Item = Key>,
        camera: [f64; 3],
    ) {
        for key in keys {
            if self.requested.contains_key(&key) {
                continue;
            }
            let ticket = self.next_ticket;
            self.next_ticket += 1;
            self.requested.insert(key, ticket);
            let (level, coord) = key;
            let held = coord
                .children()
                .into_iter()
                .enumerate()
                .filter(|&(_, child)| store.holds(level - 1, child))
                .fold(0u8, |mask, (i, _)| mask | 1 << i);
            self.outbox.push(Job {
                distance: brick_distance(camera, level, coord),
                key,
                ticket,
                held,
            });
        }
    }

    /// Gives up on the children of `keys`, bricks of [`render::Scene::take_cancelled`]: they are
    /// not generated if they have not been started. They may be asked for again.
    pub fn cancel(&mut self, keys: impl IntoIterator<Item = Key>) {
        for key in keys {
            let Some(ticket) = self.requested.remove(&key) else {
                continue;
            };
            match self.outbox.iter().position(|job| job.ticket == ticket) {
                Some(at) => {
                    self.outbox.swap_remove(at);
                }
                None => self.cancels.push(ticket),
            }
        }
    }

    /// Tells the workers where the camera is and what the bands are, and hands them the jobs
    /// asked for and the ones given up on. If they are busy with the queue it waits for the next
    /// frame, rather than holding up this one.
    pub fn sync(&mut self, camera: [f64; 3], bands: Bands) {
        self.assert_alive();
        let mut state = match self.shared.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => return,
            Err(TryLockError::Poisoned(_)) => panic!("a ring loader thread panicked"),
        };
        state.camera = camera;
        state.bands = bands;
        let moved = (0..3)
            .map(|a| (camera[a] - state.ordered_for[a]).powi(2))
            .sum::<f64>()
            .sqrt();
        // Moving only puts the work in another order: the queue is sorted again by a worker once
        // the camera has gone a little way, not on every Voxel.
        if moved > bands.d0() / 16.0 && !state.queue.is_empty() {
            state.stale = true;
        }
        state.cancelled.extend(self.cancels.drain(..));
        let pushed = !self.outbox.is_empty();
        state.queue.extend(self.outbox.drain(..));
        drop(state);
        if pushed {
            self.shared.wake.notify_all();
        }
    }

    /// Takes the children that have been generated, at most `most` of them (and the jobs that
    /// were dropped, which are not counted). The caller holds them
    /// ([`brick_store::BrickStore::hold_children`]). Taking them in lets the workers go on.
    pub fn receive(&mut self, most: usize) -> Vec<Arrival> {
        self.assert_alive();
        let mut out = Vec::new();
        let mut counted = 0;
        let mut taken = 0;
        while counted < most {
            match self.arrivals.try_recv() {
                Ok((ticket, arrival)) => {
                    taken += 1;
                    match arrival {
                        Arrival::Children { key, bricks } => {
                            counted += 1;
                            if self.requested.get(&key) == Some(&ticket) {
                                self.requested.remove(&key);
                            }
                            // Held either way: the children are what the generator makes.
                            out.push(Arrival::Children { key, bricks });
                        }
                        Arrival::Dropped(key) => {
                            // A brick asked for again since is not given up on by the old
                            // request's end.
                            if self.requested.get(&key) == Some(&ticket) {
                                self.requested.remove(&key);
                                out.push(Arrival::Dropped(key));
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => panic!("the ring loader's threads are gone"),
            }
        }
        if taken > 0 {
            self.shared.ahead.fetch_sub(taken, Atomic::Relaxed);
            // A worker that checked `ahead` and is about to wait must not miss this.
            drop(self.shared.lock());
            self.shared.wake.notify_all();
        }
        out
    }

    /// Bricks asked for whose children have not been received: queued, being generated, or
    /// waiting to be taken in. Nothing is queued once it is zero and the scene waits for nothing.
    pub fn pending(&self) -> usize {
        self.requested.len()
    }

    /// Bricks the workers have asked the generator for, since the start.
    pub fn generated(&self) -> u64 {
        self.shared.generated.load(Atomic::Relaxed)
    }

    /// Jobs that the workers have taken and the frame thread has not taken in.
    #[cfg(test)]
    pub fn ahead(&self) -> usize {
        self.shared.ahead.load(Atomic::Relaxed)
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        // A worker that panicked poisoned nothing it holds (it generates outside the lock), but
        // be sure of getting the flag in.
        match self.shared.state.lock() {
            Ok(mut state) => state.shutdown = true,
            Err(poisoned) => poisoned.into_inner().shutdown = true,
        }
        self.shared.wake.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Puts the queue in order for the camera it has now, nearest first.
fn reorder(state: &mut State) {
    let camera = state.camera;
    let mut jobs = std::mem::take(&mut state.queue).into_vec();
    for job in &mut jobs {
        job.distance = brick_distance(camera, job.key.0, job.key.1);
    }
    state.queue = BinaryHeap::from(jobs);
    state.ordered_for = camera;
    state.stale = false;
}

/// A worker: takes the nearest job when the frame thread is not too far behind, drops it if the
/// camera has gone from it, and generates it.
fn work(shared: &Shared, arrivals: &Sender<(u64, Arrival)>) {
    crate::priority::lower_current_thread();
    loop {
        let (job, camera, bands) = {
            let mut state = shared.lock();
            loop {
                if state.shutdown {
                    return;
                }
                if state.stale {
                    reorder(&mut state);
                }
                if state.queue.is_empty() {
                    // Whatever is left was for jobs that are gone.
                    state.cancelled.clear();
                } else if shared.ahead.load(Atomic::Relaxed) < MAX_AHEAD {
                    let job = state.queue.pop().expect("it is not empty");
                    if state.cancelled.remove(&job.ticket) {
                        continue;
                    }
                    shared.ahead.fetch_add(1, Atomic::Relaxed);
                    break (job, state.camera, state.bands);
                }
                state = shared
                    .wake
                    .wait(state)
                    .expect("a ring loader thread panicked");
            }
        };
        let (level, coord) = job.key;
        // The bands split a brick nearer than its level is used from; with the margin that a
        // brick that is split keeps (see `Bands::split`), so that a job is only dropped for a
        // brick that the camera has really left.
        let arrival = if bands.split(level, brick_distance(camera, level, coord), Some(true)) {
            let kids = coord.children();
            let held = |c: BrickCoord| {
                kids.iter()
                    .position(|&k| k == c)
                    .is_some_and(|i| job.held >> i & 1 == 1)
            };
            let bricks = children_from(&*shared.source, level, coord, held);
            shared
                .generated
                .fetch_add(u64::from(8 - job.held.count_ones()), Atomic::Relaxed);
            Arrival::Children {
                key: job.key,
                bricks,
            }
        } else {
            Arrival::Dropped(job.key)
        };
        if arrivals.send((job.ticket, arrival)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(distance: f64, x: i32) -> Job {
        Job {
            distance,
            key: (3, BrickCoord([x, 0, 0])),
            ticket: 0,
            held: 0,
        }
    }

    #[test]
    fn the_nearest_job_is_taken_first_and_a_new_camera_puts_the_queue_in_order_again() {
        let mut state = State {
            queue: BinaryHeap::from([job(300.0, 30), job(10.0, 1), job(100.0, 10)]),
            camera: [0.0; 3],
            bands: Bands::with_d0(64.0),
            ordered_for: [0.0; 3],
            stale: false,
            cancelled: HashSet::new(),
            shutdown: false,
        };
        let order: Vec<i32> = state
            .queue
            .clone()
            .into_sorted_vec()
            .iter()
            .rev()
            .map(|j| j.key.1.0[0])
            .collect();
        assert_eq!(order, [1, 10, 30]);
        // The camera went to brick 30: it is now the nearest (a level 3 brick is 64 Voxels).
        state.camera = [30.0 * 64.0, 0.0, 0.0];
        reorder(&mut state);
        assert!(!state.stale);
        assert_eq!(state.ordered_for, state.camera);
        assert_eq!(state.queue.pop().unwrap().key.1.0[0], 30);
        assert_eq!(state.queue.pop().unwrap().key.1.0[0], 10);
    }

    #[test]
    fn a_loader_has_two_fewer_workers_than_cores_and_at_least_one() {
        assert_eq!(workers_for(8), 6);
        assert_eq!(workers_for(3), 1);
        assert_eq!(workers_for(2), 1);
        assert_eq!(workers_for(1), 1);
    }

    /// Solid bricks from any level, counting what it was asked for.
    #[derive(Debug, Default)]
    struct Counting {
        made: AtomicUsize,
    }

    impl BrickSource for Counting {
        fn brick(&self, _: usize, _: BrickCoord) -> Brick {
            self.made.fetch_add(1, Atomic::Relaxed);
            Brick::solid(brick_store::Material(1))
        }
    }

    fn bands() -> Bands {
        Bands::with_d0(64.0)
    }

    /// The bricks of level 3 within the band that splits them (512 Voxels) of the origin: 4096 of
    /// them, a brick being 64 Voxels across at level 3.
    fn near_the_origin() -> Vec<Key> {
        let mut keys = Vec::new();
        for x in -8..8 {
            for y in -8..8 {
                for z in -8..8 {
                    keys.push((3, BrickCoord([x, y, z])));
                }
            }
        }
        keys.sort_by(|a, b| {
            let d = |&(l, c): &Key| brick_distance([0.0; 3], l, c);
            d(a).total_cmp(&d(b))
        });
        // Those whose boxes are inside the band, with room to spare.
        keys.retain(|&(l, c)| bands().split(l, brick_distance([0.0; 3], l, c), Some(true)));
        keys
    }

    fn new_loader(source: Arc<dyn BrickSource>) -> Loader {
        Loader::new(source, [0.0; 3], bands(), 2)
    }

    /// Syncs until the workers have taken `n` jobs (they take no more than [`MAX_AHEAD`]) and
    /// have stopped taking them.
    fn wait_for_ahead(loader: &mut Loader, n: usize) {
        let started = std::time::Instant::now();
        let mut last = usize::MAX;
        loop {
            loader.sync([0.0; 3], bands());
            let now = loader.ahead();
            if now >= n && now == last {
                return;
            }
            last = now;
            assert!(started.elapsed().as_secs() < 30, "{now} of {n} taken");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn the_workers_go_no_further_ahead_than_what_the_frame_thread_has_taken_in() {
        let source = Arc::new(Counting::default());
        let mut loader = new_loader(source.clone());
        let keys = near_the_origin();
        assert!(keys.len() > 3 * MAX_AHEAD, "{}", keys.len());
        loader.request(&BrickStore::new(), keys.iter().copied(), [0.0; 3]);
        wait_for_ahead(&mut loader, MAX_AHEAD);
        assert_eq!(loader.ahead(), MAX_AHEAD);
        assert_eq!(source.made.load(Atomic::Relaxed), MAX_AHEAD * 8);
        // Taking some in lets the workers go on, by as many.
        let taken = loader.receive(100).len();
        assert_eq!(taken, 100);
        wait_for_ahead(&mut loader, MAX_AHEAD);
        assert_eq!(loader.ahead(), MAX_AHEAD);
        assert_eq!(source.made.load(Atomic::Relaxed), (MAX_AHEAD + 100) * 8);
    }

    #[test]
    fn a_brick_wanted_newly_and_near_is_not_queued_behind_the_far_ones_asked_for_before() {
        let source = Arc::new(Counting::default());
        let mut loader = new_loader(source.clone());
        let mut keys = near_the_origin();
        let newly = keys.remove(0);
        loader.request(&BrickStore::new(), keys.iter().copied(), [0.0; 3]);
        wait_for_ahead(&mut loader, MAX_AHEAD);
        // Now the nearest brick of all is wanted. It is behind what the workers took already, and
        // nothing else.
        loader.request(&BrickStore::new(), [newly], [0.0; 3]);
        let mut seen = 0;
        let started = std::time::Instant::now();
        let at = 'found: loop {
            loader.sync([0.0; 3], bands());
            for arrival in loader.receive(16) {
                if matches!(&arrival, Arrival::Children { key, .. } if *key == newly) {
                    break 'found seen;
                }
                seen += 1;
            }
            assert!(started.elapsed().as_secs() < 30, "never came");
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert!(at < MAX_AHEAD + 32, "it was number {at}");
        // The far ones are still queued: not everything was generated.
        assert!(
            source.made.load(Atomic::Relaxed) < keys.len() * 4,
            "{}",
            source.made.load(Atomic::Relaxed)
        );
    }

    #[test]
    fn a_job_that_was_given_up_on_is_not_generated() {
        let source = Arc::new(Counting::default());
        let mut loader = new_loader(source.clone());
        let keys = near_the_origin();
        loader.request(&BrickStore::new(), keys.iter().copied(), [0.0; 3]);
        wait_for_ahead(&mut loader, MAX_AHEAD);
        // The farthest hundred are given up on: some while they wait for the next sync, some
        // after.
        let gone: Vec<Key> = keys[keys.len() - 100..].to_vec();
        loader.cancel(gone[..50].iter().copied());
        loader.sync([0.0; 3], bands());
        loader.cancel(gone[50..].iter().copied());
        assert_eq!(loader.pending(), keys.len() - 100);
        let mut arrived = HashSet::new();
        let started = std::time::Instant::now();
        while loader.pending() > 0 || arrived.len() < keys.len() - 100 {
            loader.sync([0.0; 3], bands());
            for arrival in loader.receive(256) {
                if let Arrival::Children { key, .. } = arrival {
                    arrived.insert(key);
                }
            }
            assert!(started.elapsed().as_secs() < 60, "{} came", arrived.len());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(gone.iter().all(|key| !arrived.contains(key)));
        assert_eq!(
            source.made.load(Atomic::Relaxed),
            (keys.len() - 100) * 8,
            "nothing was generated for them"
        );
    }

    #[test]
    fn a_late_drop_does_not_undo_a_brick_that_was_asked_for_again() {
        let source = Arc::new(Counting::default());
        let mut loader = new_loader(source);
        let key = (5, BrickCoord([5000, 0, 0]));
        let far = [1.0e6, 0.0, 0.0];
        // The camera is far from it: the worker drops the job, and the drop waits to be taken.
        loader.request(&BrickStore::new(), [key], far);
        loader.sync(far, bands());
        let started = std::time::Instant::now();
        while loader.ahead() == 0 {
            loader.sync(far, bands());
            assert!(started.elapsed().as_secs() < 30);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        // The scene gave up on it, wanted it again, and the camera came back.
        loader.cancel([key]);
        let near = [5000.0 * 256.0, 0.0, 0.0];
        loader.request(&BrickStore::new(), [key], near);
        loader.sync(near, bands());
        let mut dropped = 0;
        let mut children = 0;
        while children == 0 {
            loader.sync(near, bands());
            for arrival in loader.receive(8) {
                match arrival {
                    Arrival::Dropped(_) => dropped += 1,
                    Arrival::Children { .. } => children += 1,
                }
            }
            assert!(started.elapsed().as_secs() < 30);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(dropped, 0, "the old request's drop was not reported");
        assert_eq!(loader.pending(), 0);
    }

    #[derive(Debug)]
    struct Panicking;

    impl BrickSource for Panicking {
        fn brick(&self, _: usize, _: BrickCoord) -> Brick {
            panic!("the generator failed");
        }
    }

    #[test]
    #[should_panic(expected = "ring loader")]
    fn a_worker_that_panics_panics_the_frame_thread_instead_of_loading_stopping_silently() {
        let mut loader = new_loader(Arc::new(Panicking));
        loader.request(&BrickStore::new(), [(3, BrickCoord([0, 0, 0]))], [0.0; 3]);
        let started = std::time::Instant::now();
        loop {
            loader.sync([0.0; 3], bands());
            loader.receive(8);
            assert!(started.elapsed().as_secs() < 30, "nothing was said");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
