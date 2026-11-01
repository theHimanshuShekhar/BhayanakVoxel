//! The ring loader (#86): generates the Planet's bricks on worker threads, as the LOD bands ask
//! for them, so that the frame thread never generates one.
//!
//! The scene draws a brick whole, a coarser frame, when the bricks under it are not loaded (see
//! [`brick_store::BrickStore::stream_from`]), and says which: [`render::Scene::take_wanted`]. Those
//! are [`Loader::request`]ed, nearest the camera first. The workers read the Planet's generator
//! ([`BrickSource`]) for the eight bricks under each and send them back, and the frame thread
//! takes in a bounded number per frame ([`Loader::receive`]) and tells the scene, which splits the
//! brick. That brings new bricks to be wanted, a level finer: the world fills in outward and
//! downward until every band is complete, and then there is nothing wanted and the workers
//! sleep. Whether or not the camera moves: moving only changes which are nearest, and which are
//! wanted at all.
//!
//! A job is dropped, not run, if the camera has gone from where the bands would split its brick,
//! so nothing is generated beyond the bands. The workers run at the lowest priority
//! ([`crate::priority`]), and there are two fewer of them than cores, for the simulation and the
//! frame thread.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering as Atomic};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use brick_store::{Brick, BrickCoord, BrickSource, BrickStore, children_from};
use render::lod::{Bands, brick_distance};
use render::scene::select::Key;

/// The first level of the Guard: the levels from here to the top of the pyramid are generated for
/// the whole Planet right after the spawn ring (ADR 0004).
pub const GUARD_FIRST: usize = 8;

/// The Guard: the whole Planet at the levels from `first` to the top of `store`'s pyramid, held
/// in it; from then on the store streams, and a brick of level `first` is split when its children
/// have been loaded. Returns how many bricks it holds.
pub fn hold_guard(
    store: &mut BrickStore,
    planet: &planet_gen::planet::Planet,
    first: usize,
) -> usize {
    let top = store.level_count() - 1;
    let guard = planet.guard(first..=top);
    let held = guard.len();
    for (level, coord, brick) in guard {
        store.insert_at(level, coord, brick);
    }
    store.stream_from(first);
    held
}

/// One family to generate: the eight bricks under `key`.
#[derive(Clone)]
struct Job {
    /// How far `key` was from the camera when this was queued or last ordered.
    distance: f64,
    key: Key,
    /// The children that are held already, which are not generated.
    held: Vec<BrickCoord>,
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
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    source: Arc<dyn BrickSource>,
    /// Bricks the workers have asked the generator for.
    generated: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("a loader thread panicked")
    }
}

/// What a worker sends back.
pub enum Arrival {
    /// The children of `key` that are not air and were not held, as
    /// [`brick_store::BrickStore::load_children`] takes them.
    Family {
        key: Key,
        bricks: Vec<(BrickCoord, Brick)>,
    },
    /// The camera went before `key` was generated: it was not.
    Dropped(Key),
}

/// The workers, and the work they have been asked for. Lives on the frame thread, which is the
/// only one that talks to it.
pub struct Loader {
    shared: Arc<Shared>,
    arrivals: Receiver<Arrival>,
    workers: Vec<JoinHandle<()>>,
    /// Jobs not yet handed to the workers, for when they are busy with the lock.
    outbox: Vec<Job>,
    /// Families asked for and not yet received.
    requested: HashSet<Key>,
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
                shutdown: false,
            }),
            wake: Condvar::new(),
            source,
            generated: AtomicU64::new(0),
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
            requested: HashSet::new(),
        }
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
            if !self.requested.insert(key) {
                continue;
            }
            let (level, coord) = key;
            let held = coord
                .children()
                .into_iter()
                .filter(|&child| store.holds(level - 1, child))
                .collect();
            self.outbox.push(Job {
                distance: brick_distance(camera, level, coord),
                key,
                held,
            });
        }
    }

    /// Tells the workers where the camera is and what the bands are, and hands them the jobs
    /// asked for. If they are busy with the queue it waits for the next frame, rather than
    /// holding up this one.
    pub fn sync(&mut self, camera: [f64; 3], bands: Bands) {
        let Ok(mut state) = self.shared.state.try_lock() else {
            return;
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
        let pushed = !self.outbox.is_empty();
        state.queue.extend(self.outbox.drain(..));
        drop(state);
        if pushed {
            self.shared.wake.notify_all();
        }
    }

    /// Takes the families that have been generated, at most `most` of them (and the jobs that
    /// were dropped, which are not counted). The caller loads them
    /// ([`brick_store::BrickStore::load_children`]).
    pub fn receive(&mut self, most: usize) -> Vec<Arrival> {
        let mut out = Vec::new();
        let mut families = 0;
        while families < most {
            match self.arrivals.try_recv() {
                Ok(arrival) => {
                    match &arrival {
                        Arrival::Family { key, .. } => {
                            families += 1;
                            self.requested.remove(key);
                        }
                        Arrival::Dropped(key) => {
                            self.requested.remove(key);
                        }
                    }
                    out.push(arrival);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    /// Families asked for that have not been received: queued, being generated, or waiting to be
    /// taken. Nothing is queued once it is zero and the scene waits for nothing.
    pub fn pending(&self) -> usize {
        self.requested.len()
    }

    /// Bricks the workers have asked the generator for, since the start.
    pub fn generated(&self) -> u64 {
        self.shared.generated.load(Atomic::Relaxed)
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
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

/// A worker: takes the nearest job, drops it if the camera has gone from it, and generates it.
fn work(shared: &Shared, arrivals: &Sender<Arrival>) {
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
                if let Some(job) = state.queue.pop() {
                    break (job, state.camera, state.bands);
                }
                state = shared.wake.wait(state).expect("a loader thread panicked");
            }
        };
        let (level, coord) = job.key;
        // The bands split a brick nearer than its level is used from; with the margin that a
        // brick that is split keeps (see `Bands::split`), so that a job is only dropped for a
        // brick that the camera has really left.
        let arrival = if bands.split(level, brick_distance(camera, level, coord), Some(true)) {
            let bricks = children_from(&*shared.source, level, coord, |c| job.held.contains(&c));
            shared
                .generated
                .fetch_add(8 - job.held.len() as u64, Atomic::Relaxed);
            Arrival::Family {
                key: job.key,
                bricks,
            }
        } else {
            Arrival::Dropped(job.key)
        };
        if arrivals.send(arrival).is_err() {
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
            held: Vec::new(),
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
}
