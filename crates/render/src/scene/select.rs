//! Which bricks are drawn, at which level: the LOD bands (see [`crate::lod`]) applied to the
//! brick pyramid, and kept up to date as the camera moves.
//!
//! The pyramid is a tree: a brick of level `L` covers the eight bricks of level `L - 1` under
//! it. The selection is a *cut* through that tree: a brick is either drawn whole, or split into
//! its eight children, and every point of space is under exactly one drawn brick, so no level
//! boundary can open a gap or draw twice. A brick is split when it is nearer than its level is
//! used from ([`Bands::split`]). Nearer bricks have nearer parents, and a parent's boundary is
//! twice as far, so a split brick always has a split parent: the bricks drawn are the band rule,
//! apart from a sliver beside each boundary where a brick's parent is nearer than the boundary
//! and it is not, and is drawn at the finer level, never a hole.
//!
//! **Streaming.** In a store whose bricks are streamed in ([`BrickStore::stream_from`]) a brick is
//! split only when the bricks under it are resident. Until they are it is drawn whole, a coarser
//! frame than the rule asks for and never a hole, and it is *wanted* ([`Selection::take_wanted`]):
//! a loader reads those bricks, and [`Selection::arrived`] splits it when they are there. What a
//! store that streams has not made resident is never read, so the selection never waits for a brick to
//! be generated.
//!
//! Space that cannot be seen is left out. A brick that is air is not drawn, and a brick that is
//! solid, with solid bricks on all six faces, is *buried*: no ray reaches it, so it and
//! everything under it is out of the tree. That is what keeps the bricks drawn to a skin
//! around the surface, however deep the Planet is, instead of its volume.
//!
//! **The horizon cap** ([`Horizon`], ADR 0004). A brick that the bands split is split only if the
//! cap leaves it in: a brick past the horizon of the camera raised by the vertical margin cannot
//! be seen, so it stays whole at the coarse level that holds it (never a hole), and its children
//! are not wanted. The cap is another reason for a node to change, tracked like the bands' by
//! how far the camera has to go for it to (a unit of path moves the margin by up to a hundred near
//! the ground, for a climb: see `Cap::slack`). It is off for a camera under sea level, and is
//! turned on again [`CAP_ON`] above it, so that a swimmer does not flip it. A flip changes the cap of
//! every node at once, so the nodes are looked at again [`SWEEP`] at a time, one batch in each
//! [`Selection::update`], and not all in the update that found it (#213): a node that is not
//! reached yet is as it was, which is a cut through the tree like any other.
//!
//! **Incremental.** The distance of a brick's box changes no faster than the camera moves, so a
//! brick that is `s` from where its decision would change cannot change it until the camera has
//! travelled `s` (the path length, not the displacement). Each node is queued for the total
//! travel at which that is first possible, and [`Selection::update`] looks only at those that
//! are due: the work in a frame is the bricks that may change level, not all of them.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use brick_store::{Brick, BrickCoord, BrickStore, TableSize, VOXELS_PER_METRE};

use crate::lod::{Bands, HYSTERESIS, Horizon, LEVELS, Margins, Placed, Reach, brick_distance};

/// A fast hash for the small integer keys of the maps here, which SipHash is slow for: the
/// selection looks nodes up thousands of times a frame. It is not a defence against anything; the
/// keys are brick coordinates.
#[derive(Default)]
pub(super) struct FastHasher(u64);

impl Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u64(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn write_u32(&mut self, word: u32) {
        self.write_u64(u64::from(word));
    }

    fn write_i32(&mut self, word: i32) {
        self.write_u64(word as u32 as u64);
    }

    fn write_usize(&mut self, word: usize) {
        self.write_u64(word as u64);
    }
}

type HashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FastHasher>>;
type HashSet<K> = std::collections::HashSet<K, BuildHasherDefault<FastHasher>>;

/// A brick of the pyramid: its level, and its coordinate in bricks of that level.
pub type Key = (usize, BrickCoord);

/// What a node of the tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Drawn whole (or, if it is air, drawn as nothing), so it is a leaf.
    Whole,
    /// Split into the bricks of the level below, which are nodes too if they are in the tree.
    Split,
}

#[derive(Clone, Copy, Debug)]
struct Node {
    state: State,
    /// Whether the node is a brick that is drawn: whole, and not air.
    draws: bool,
    /// For a whole node: whether its children are wanted resident although the bands do not split
    /// it (the margin or the lead, see [`Reach`]), until it is [`crate::lod::HYSTERESIS`] out of
    /// reach again. Always false with no reach.
    margin: bool,
    /// The stamp of the entry that watches it for a change of state (see
    /// [`Selection::queue_node`]), or 0 for none: an entry with another stamp is stale.
    stamp: u64,
}

/// Set on the stamp of a node that [`Selection::set_bands`] parked: the rest is the stamp its
/// entries in the hot list and the queue have, which are kept for it (and are live again if the
/// bands go back to those they were made for before an update drops them, see
/// [`Selection::set_bands`]), though a parked node is not watched: no entry that has the stamp
/// without the bit is live. Stamps are far under it. A parked node whose entries an update has
/// dropped has the bit alone ([`LOST`]).
const PARKED: u64 = 1 << 63;

/// The stamp of a parked node whose entries were dropped: there is nothing to restore.
const LOST: u64 = PARKED;

/// Whether a node with `stamp` is watched by an entry: not unwatched (0), and not parked.
fn watched(stamp: u64) -> bool {
    stamp != 0 && stamp & PARKED == 0
}

/// Whether a node with `stamp` is parked and its entries are still there.
fn kept(stamp: u64) -> bool {
    stamp & PARKED != 0 && stamp != LOST
}

/// What looking at an entry that was due in the queue came to.
#[cfg_attr(test, derive(Debug, PartialEq))]
enum Due {
    /// The node is parked with the entry's stamp: the entry is dropped, and the node's stamp is
    /// [`LOST`].
    Parked,
    /// The node is gone, or has been queued again since: the entry is stale.
    Stale,
    /// The node changes now.
    Changes,
    /// It does not, and is due again when the camera has gone `slack` more.
    Later {
        split: bool,
        margin: bool,
        slack: f64,
    },
}

/// Entries due at once from which looking at them is shared among threads: below this the threads
/// cost more than they save (a thread is about 50 microseconds, and an entry about 0.2).
#[cfg(not(test))]
const PARALLEL_FROM: usize = 32_768;
/// Under test, in the render library's own unit tests (`cargo test -p render --lib`), it is low
/// enough for them to reach the threads, so that the thread-comparison test shows they change
/// nothing. `cfg(test)` is not set for the client's stepped runs or for the tests in `render/tests`,
/// which are built against the library without it and use the 32 768 above.
#[cfg(test)]
const PARALLEL_FROM: usize = 64;

/// The most threads that look at due entries, and at most half of the machine's.
const MOST_THREADS: usize = 4;

/// Looks at each of the `due` entries (a stamp and a node): whether the node changes, or when it
/// may. Each is looked at alone (the nodes are only read), so a long list is shared among threads,
/// and the answers are in the order of the entries: what is done with them is the same whatever
/// the threads, and the same on every run. At full size a call after the camera has moved some
/// tens of Voxels has millions due, and looking at them was 80% of it (700 ms of 900 ms).
fn look_at_due(
    nodes: &HashMap<Key, Node>,
    due: &[(u64, Key)],
    camera: [f64; 3],
    changes: &(impl Fn(Key, f64, bool, bool) -> bool + Sync),
    slack: &(impl Fn(Key, f64, bool, bool) -> f64 + Sync),
) -> Vec<Due> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get() / 2)
        .clamp(1, MOST_THREADS);
    look_at_due_by(
        nodes,
        due,
        camera,
        changes,
        slack,
        if due.len() < PARALLEL_FROM {
            1
        } else {
            threads
        },
    )
}

/// [`look_at_due`] shared among `threads` (one: here and now).
fn look_at_due_by(
    nodes: &HashMap<Key, Node>,
    due: &[(u64, Key)],
    camera: [f64; 3],
    changes: &(impl Fn(Key, f64, bool, bool) -> bool + Sync),
    slack: &(impl Fn(Key, f64, bool, bool) -> f64 + Sync),
    threads: usize,
) -> Vec<Due> {
    let look = |&(stamp, key): &(u64, Key)| {
        // An entry for a node that has gone, or has been queued again since, is stale.
        let Some(node) = nodes.get(&key) else {
            return Due::Stale;
        };
        if node.stamp == stamp | PARKED {
            return Due::Parked;
        }
        if node.stamp != stamp {
            return Due::Stale;
        }
        let (split, margin) = (node.state == State::Split, node.margin);
        let distance = brick_distance(camera, key.0, key.1);
        if changes(key, distance, split, margin) {
            Due::Changes
        } else {
            Due::Later {
                split,
                margin,
                slack: slack(key, distance, split, margin),
            }
        }
    };
    map_shared(due, threads, &look)
}

/// `look` of each of `items`, in their order, shared among `threads` (one: here and now).
fn map_shared<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    look: &(impl Fn(&T) -> R + Sync),
) -> Vec<R> {
    if threads <= 1 {
        return items.iter().map(look).collect();
    }
    let share = items.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let parts: Vec<_> = items
            .chunks(share)
            .map(|part| scope.spawn(|| part.iter().map(look).collect::<Vec<_>>()))
            .collect();
        parts
            .into_iter()
            .flat_map(|part| part.join().expect("a thread that looks at due entries"))
            .collect()
    })
}

/// A node that is close enough to the edge of its hysteresis to be looked at every frame.
#[derive(Clone, Copy, Debug)]
struct Hot {
    key: Key,
    stamp: u64,
    /// Whether the node is split (else it is whole).
    split: bool,
    /// Whether the node is whole and in reach (see [`Node::margin`]).
    margin: bool,
    /// Whether the sweep of a flip of the horizon cap put it here, to be looked at for the first
    /// time under the cap as it is now (#213): if it changes then, the revisit is the sweep's to
    /// spread (see [`ABSORB_REVISITS`]), and a change that comes later is not.
    swept: bool,
}

/// A look at all the stuck bricks, in steps.
#[derive(Clone, Debug)]
struct Scan {
    /// The bricks to look at, and how many have been.
    keys: Vec<Key>,
    done: usize,
    /// The nearest of those looked at, from `from`.
    nearest: [Option<f64>; LEVELS],
    from: [f64; 3],
    /// Bricks of `stuck` that were not stuck when it began.
    added: Vec<Key>,
    /// Bricks that were looked at and are not stuck any more.
    dead: Vec<Key>,
}

/// Stuck bricks looked at in one call of [`Selection::nearest_stuck`] when there are many.
#[cfg(not(test))]
const SCAN_STEP: usize = 16_384;
#[cfg(test)]
const SCAN_STEP: usize = 64;

/// Bricks that are stuck above which [`Selection::nearest_stuck`] keeps a bound instead of looking
/// at them all each time.
const MANY_STUCK: usize = 4096;

/// Camera travel, in Voxels, that nodes due within it of each other are queued together for. A
/// node is looked at no later than it is due, and at most this early, which costs a look and is
/// never wrong.
const BUCKET: f64 = 4.0;

/// A node that could change within this much camera travel is *hot*: its distance is worked out
/// every frame, with no table lookup, and it is looked at properly only when it changes. A node that
/// is further waits in the queue until it is this near, and a hot one goes back to the queue when
/// it is twice as far.
const HOT: f64 = 8.0;

/// The most nodes that one [`Selection::relook_some`] looks at again after a change of bands
/// (ADR 0009, #168): a few tens of milliseconds of the scene thread, which goes on following the
/// camera between the calls.
pub const RELOOK: usize = 16 * 1024;

/// What changed in what is drawn: the bricks that are drawn now and were not, and the bricks
/// that were drawn and are not. A brick that came and went in the same change is in neither.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub(super) added: HashSet<Key>,
    pub(super) removed: HashSet<Key>,
}

impl Diff {
    fn add(&mut self, key: Key) {
        if !self.removed.remove(&key) {
            self.added.insert(key);
        }
    }

    fn remove(&mut self, key: Key) {
        if !self.added.remove(&key) {
            self.removed.insert(key);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// What a brick of the store is to the tree.
#[derive(Clone, Copy)]
struct Probe {
    /// In the tree: not buried, and not air that nothing is under. Air is in it if the store holds
    /// the brick, or has a source that does not say that all the Voxels under it are air.
    present: bool,
    /// Drawn if it is whole: not air, and not buried.
    draws: bool,
    /// Not air: it has solid Voxels, which may be buried, and so not drawn.
    solid: bool,
}

/// The bricks that share a face with `coord`.
fn faces(coord: BrickCoord) -> impl Iterator<Item = BrickCoord> {
    (0..6).map(move |face| {
        let mut next = coord.0;
        next[face / 2] += if face % 2 == 0 { -1 } else { 1 };
        BrickCoord(next)
    })
}

/// What the brick at `coord` of `level` is to the tree, given the brick (`brick`, whether the
/// store `held` it) and which of the bricks of its level are known to be full (`known`: the others
/// are looked up).
///
/// Air is out of it, unless the store holds the brick, which means it holds bricks under it that
/// may not be air (a coarse brick is air if under half its Voxels are solid), or the source of the
/// store does not say that all the Voxels of level 0 under it are air
/// ([`BrickStore::may_have_ground_below`]): a coarse brick is sampled at the middle of its Voxels, so
/// ground that pokes up into a brick above a slope leaves it air, with ground under it that must
/// be drawn. Such a brick is split when its band asks, and drawn as nothing when it is whole. A
/// brick of level 0 reads as it is.
///
/// A brick that is full, with full bricks on all six faces, is *buried*: no ray from outside
/// reaches it, and it is not drawn. It is out of the tree for good only if it and all six of those
/// bricks are solid all the way down ([`BrickStore::solid_below`]). A full coarse brick can hide a
/// hollow up to half a coarse Voxel wide, and then either the hollow is in the brick, and a camera
/// in it splits the brick down to the bricks that have its walls (so it stays, drawing nothing), or
/// it is in a neighbour, against a face, and the wall across the face is Voxels of this brick (so it
/// stays too). Only the bricks round a hollow are such nodes, not the volume of rock.
///
/// An Edit that opens a brick is one that changes class, which [`Selection::refresh`] sees: it
/// looks again at the bricks changed and those beside them.
fn classify(
    store: &BrickStore,
    level: usize,
    coord: BrickCoord,
    brick: &Brick,
    held: bool,
    known: &[(BrickCoord, bool)],
) -> Probe {
    if brick.is_air() {
        return Probe {
            present: held || (level > 0 && store.may_have_ground_below(level, coord)),
            draws: false,
            solid: false,
        };
    }
    let full_at = |n: BrickCoord| match known.iter().find(|(c, _)| *c == n) {
        Some(&(_, full)) => full,
        None => store.is_full_known(level, n),
    };
    let buried = brick.is_full() && faces(coord).all(full_at);
    if buried {
        let sealed =
            store.solid_below(level, coord) && faces(coord).all(|n| store.solid_below(level, n));
        return Probe {
            present: !sealed,
            draws: false,
            solid: true,
        };
    }
    Probe {
        present: true,
        draws: true,
        solid: true,
    }
}

/// The brick at `coord` of `level` as the store reads it, and whether the store holds it. A brick
/// that is not held and that the store knows is full without generating it, as the buried bricks
/// that the loader leaves out are ([`BrickStore::reads_full_from_source`]), is read as a full brick
/// of some Material, which is all that the tree asks of a brick: reading it as it is would
/// generate it, on this thread. A brick that an Edit exposes is drawn from the store, and so is
/// read as it is.
fn read(store: &BrickStore, level: usize, coord: BrickCoord) -> (Cow<'_, Brick>, bool) {
    match store.level(level).and_then(|l| l.brick(coord)) {
        Some(brick) => (Cow::Borrowed(brick), true),
        None if store.reads_full_from_source(level, coord) => {
            (Cow::Owned(Brick::solid(brick_store::Material(1))), false)
        }
        None => (Cow::Owned(store.unheld_brick(level, coord)), false),
    }
}

fn probe(store: &BrickStore, level: usize, coord: BrickCoord) -> Probe {
    let (brick, held) = read(store, level, coord);
    classify(store, level, coord, &brick, held, &[])
}

/// The eight bricks under `coord` of `level`, and what each is to the tree. They are read once,
/// and a brick beside a sibling does not read it again.
fn probe_children(store: &BrickStore, level: usize, coord: BrickCoord) -> Vec<(BrickCoord, Probe)> {
    let below = level - 1;
    let children = coord.children();
    let reads: Vec<_> = children.iter().map(|&c| read(store, below, c)).collect();
    let known: Vec<(BrickCoord, bool)> = children
        .iter()
        .zip(&reads)
        .map(|(&c, (b, _))| (c, b.is_full()))
        .collect();
    children
        .iter()
        .zip(&reads)
        .map(|(&c, (b, held))| (c, classify(store, below, c, b, *held, &known)))
        .collect()
}

/// What a brick of `level` that is to be split is split into, given that it is `p`.
///
/// The bricks under it that are in the tree. If there are none because they are all buried, it is
/// buried too: nothing under it can be seen, so nothing is drawn. If there are none because they
/// are air, or the store has nothing under it, and it is not air, it stays whole, which is a
/// coarser frame than the rule asks for, rather than a hole.
fn decide(store: &BrickStore, level: usize, coord: BrickCoord, p: impl FnOnce() -> Probe) -> Plan {
    if !store.children_resident(level, coord) {
        return Plan::Wait;
    }
    let all = probe_children(store, level, coord);
    let present: Vec<_> = all.iter().filter(|(_, c)| c.present).copied().collect();
    // Nothing under it is in the tree, but a brick under it is something that is not air: it
    // stays whole, which is a coarser frame than the rule asks for, rather than a hole.
    let hidden_only = present.is_empty() && !all.iter().any(|(_, c)| c.solid) && p().solid;
    if hidden_only {
        Plan::Whole
    } else {
        Plan::Split(present)
    }
}

/// How many nodes of a level [`Selection::new_reached`] plans at a time while it puts the chunk
/// before into the tree.
const PLAN_CHUNK: usize = 1 << 16;

/// What each thread of [`par_map`] runs before it starts: the client lowers their priority with it
/// (`priority.rs` there). Making the scene again from nothing on every core at the frame thread's
/// priority kept the frame thread from waking for 30 ms at a time (#168): on the 4090, 26 to 31
/// frames over the limit while the scene thread made the scene again.
static WORKER_START: OnceLock<fn()> = OnceLock::new();

/// Sets what each thread of [`par_map`] runs before it starts. Once: a second call changes nothing.
pub fn set_worker_start(start: fn()) {
    let _ = WORKER_START.set(start);
}

/// `f` of each of `items`, in order, on as many threads as the machine has. Generating the
/// bricks of a Planet is what takes the time, and it does not depend on the order.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    const BLOCK: usize = 16;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    if threads == 1 || items.len() <= BLOCK {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let mut parts: Vec<(usize, Vec<R>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    if let Some(start) = WORKER_START.get() {
                        start();
                    }
                    let mut mine = Vec::new();
                    loop {
                        let from = next.fetch_add(BLOCK, AtomicOrdering::Relaxed);
                        if from >= items.len() {
                            break mine;
                        }
                        let to = (from + BLOCK).min(items.len());
                        mine.push((from, items[from..to].iter().map(&f).collect()));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().expect("a worker panicked"))
            .collect()
    });
    parts.sort_unstable_by_key(|&(from, _)| from);
    parts.into_iter().flat_map(|(_, part)| part).collect()
}

/// Whether a node `distance` from the camera changes: what is drawn, or (for a whole node) whether
/// it is in reach of `reach` (`margin` says whether it is). Both are the bands' and the cap's.
fn node_changes(
    bands: Bands,
    reach: Placed,
    key: Key,
    distance: f64,
    split: bool,
    margin: bool,
) -> bool {
    let (level, coord) = key;
    if split {
        !(bands.split(level, distance, Some(true)) && reach.in_cap_at(level, coord, distance, true))
    } else {
        (bands.split(level, distance, Some(false))
            && reach.in_cap_at(level, coord, distance, false))
            || (!reach.is_none()
                && (bands.split(level, reach.distance(level, coord), Some(margin))
                    && reach.in_cap_wanted(level, coord, margin))
                    != margin)
    }
}

/// How far the camera (and the places it looks ahead to) must go before the node `key`, `distance`
/// from the camera, could change: what is drawn, or for a whole node whether it is in reach of
/// `reach` (`margin` says whether it is). Both are the bands' and the horizon cap's.
fn node_slack(
    bands: Bands,
    reach: Placed,
    key: Key,
    distance: f64,
    split: bool,
    margin: bool,
) -> f64 {
    let (level, coord) = key;
    if split {
        return bands
            .slack(level, distance, true)
            .min(reach.cap_slack(level, coord, distance, true));
    }
    // A whole brick that the bands split is as it is for the cap: it is split when the cap lets
    // it, and that is all that can change it. One that they do not is split when they do.
    let eligible = bands.split(level, distance, Some(false));
    let drawn = if eligible {
        reach.cap_slack(level, coord, distance, false)
    } else {
        bands.slack(level, distance, false)
    };
    if level == 0 || reach.is_none() {
        return drawn;
    }
    let edge = if margin {
        bands.floor(level) * (1.0 + HYSTERESIS)
    } else {
        bands.split_edge(level)
    };
    let near = reach.distance(level, coord);
    let mut slack = drawn.min((near - edge).abs());
    if bands.split(level, near, Some(margin)) {
        slack = slack.min(reach.cap_slack_wanted(level, coord, distance, margin));
    }
    slack
}

/// How high over the horizon cap's sphere the raised camera must be for the cap to come back on
/// after it was off (it goes off when the raised camera is under the sphere): a swimmer bobbing at
/// sea level does not turn it on and off, which looks at every node.
const CAP_ON: f64 = 50.0 * VOXELS_PER_METRE;

/// How many nodes a flip of the horizon cap looks at again in one [`Selection::update`] (#213): the
/// most that the update can add to its own work for the flip. Looking at all of them (3 million at
/// full size) in the update that found it stood the scene thread still for seconds.
const SWEEP: usize = 8192;

/// How many nodes an update revisits (splits or merges) while it is absorbing a flip of the horizon
/// cap, and how many bricks of what is drawn those may change (ten for each), whichever is reached
/// first (#213); the nodes it does not get to are looked at in the next update, nearest first. At
/// full size a flip made 89 thousand nodes change at once, 620 thousand bricks, in one update of
/// 8.5 s.
const ABSORB_REVISITS: usize = 800;
/// How many entries of the queue a step of tidying it looks at (#213): the sweep stamps every node
/// again, and the entry that a node had in the queue is stale from then, so the queue is gone
/// through, a few at a time, and what is stale is dropped. A step is about a millisecond (at full
/// size an entry costs 150 ns to look at: 65 536 took 10 ms, which the stop after a flight felt
/// in its settle): the tidying is never to hold up what is drawn, and it is not what the flip's
/// absorbing is measured by.
const TIDY: usize = 8_192;
const ABSORB_BRICKS_PER_REVISIT: usize = 10;

/// What to do with a brick that is in the tree.
enum Plan {
    /// Draw it whole.
    Whole,
    /// Draw it whole, since the bricks it is to be split into are not resident yet.
    Wait,
    /// Split it into these bricks of the level below, which are in the tree.
    Split(Vec<(BrickCoord, Probe)>),
}

/// Whether the brick `key`, which is `p` and is new, is drawn whole or split.
fn plan(store: &BrickStore, bands: Bands, at: Placed, key: Key, p: Probe) -> Plan {
    let (level, coord) = key;
    let distance = brick_distance(at.camera(), level, coord);
    if bands.split(level, distance, None) && at.in_cap_at(level, coord, distance, false) {
        decide(store, level, coord, || p)
    } else {
        Plan::Whole
    }
}

/// The coarsest level that is looked at in `store`.
pub fn top_level(store: &BrickStore) -> usize {
    (LEVELS - 1).min(store.level_count() - 1)
}

/// Most bricks of the top level that a ball is searched for. A store whose pyramid stops short of
/// orbit has a top level of fine bricks, and a ball the size of the Planet has more of them than
/// can be looked at: it has the bricks that it holds, and nothing is looked for in the rest.
const MOST_ROOTS: u64 = 4_000_000;

/// Bricks of level `top` in the cube that holds a ball of `radius` Voxels.
fn ball_bricks(top: usize, radius: i32) -> u64 {
    let across = (2 * i64::from(radius)) / (8i64 << top) + 2;
    (across as u64).saturating_pow(3)
}

/// The bricks of level `top` to look from: those the store holds, and, if the Planet is known to
/// be inside a ball (its centre in Voxels and its radius), every one that the ball touches, since
/// a brick that is not held can still be something (see [`brick_store::BrickSource`]).
pub fn top_bricks(
    store: &BrickStore,
    top: usize,
    ball: Option<([i32; 3], i32)>,
) -> Vec<BrickCoord> {
    let mut roots: BTreeSet<BrickCoord> = store
        .level(top)
        .map(|l| l.sorted_coords())
        .unwrap_or_default()
        .into_iter()
        .collect();
    if let Some((_, radius)) = ball
        && ball_bricks(top, radius) > MOST_ROOTS
    {
        // Content that is only in the store's source, which is all of a Planet that the store
        // holds no bricks of, is not found: say so, once.
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            eprintln!(
                "warning: the ball of radius {radius} Voxels has {} bricks of level {top}, over the {MOST_ROOTS} \
                 that are looked at: only the bricks the store holds at level {top} are drawn from it",
                ball_bricks(top, radius)
            );
        });
    } else if let Some((centre, radius)) = ball {
        let size = 8i64 << top;
        let span = |a: usize| {
            let (c, r) = (i64::from(centre[a]), i64::from(radius));
            (
                (c - r).div_euclid(size) as i32,
                (c + r).div_euclid(size) as i32,
            )
        };
        let middle = centre.map(f64::from);
        for x in span(0).0..=span(0).1 {
            for y in span(1).0..=span(1).1 {
                for z in span(2).0..=span(2).1 {
                    let coord = BrickCoord([x, y, z]);
                    if brick_distance(middle, top, coord) <= f64::from(radius) {
                        roots.insert(coord);
                    }
                }
            }
        }
    }
    // The cells of a Guard that is not held yet are not there to walk (see
    // [`BrickStore::defer_guard`]): they are added when they are ([`Selection::add_roots`]).
    if top + 1 == store.level_count() {
        roots.retain(|&coord| !store.is_guard_cell_pending(coord));
    }
    roots.into_iter().collect()
}

/// The bricks drawn, as a cut through the pyramid that follows a camera. See the module docs.
#[derive(Clone, Debug)]
pub struct Selection {
    bands: Bands,
    /// The coarsest level, which is drawn from where it starts to infinity.
    top: usize,
    camera: [f64; 3],
    /// What residency looks at besides the bands (see [`Selection::set_margins`]), with the
    /// horizon cap if it is on.
    reach: Reach,
    /// The horizon cap that was asked for, which is in `reach` while the camera is over it.
    horizon: Option<Horizon>,
    /// `reach` seen from the camera.
    placed: Placed,
    /// Total length of the camera's path since the selection began, and of the places it was
    /// looking ahead to (a node's distance changes no faster than that sum).
    travelled: f64,
    /// A margin brick was given up on and is on the hot list for the next [`Selection::update`],
    /// which is to run whether the camera has moved or not, or a flip of the horizon cap has nodes
    /// still to be looked at again (`sweep`).
    rewanted: bool,
    /// The bricks of the coarsest level that the tree grows from.
    roots: Vec<BrickCoord>,
    /// The nodes that a flip of the horizon cap is yet to look at again (#213): the tree walked
    /// from its roots, depth first, [`SWEEP`] of them in each update.
    sweep: Vec<Key>,
    /// How many nodes a batch of the sweep is: [`SWEEP`], or fewer for a test.
    #[cfg(test)]
    sweep_by: usize,
    /// A flip of the horizon cap is being absorbed: the sweep has nodes to look at, or the backlog
    /// has nodes that change.
    absorbing: bool,
    /// The nodes that the sweep found to change and have not been revisited yet (see
    /// [`ABSORB_REVISITS`]), with the stamp they had. Each update revisits the nearest to the
    /// camera as it is then. Not on the hot list, which every update looks at in full.
    backlog: Vec<(u64, Key)>,
    /// The last stamp given out before the latest flip of the horizon cap began to be absorbed:
    /// a node that still has a stamp up to it was queued under the cap as it was, and what it does
    /// when it is looked at is the flip's to spread (the sweep, or the queue's turn for it,
    /// whichever comes first), not the camera's.
    flip_stamp: u64,
    /// Whether the nodes that a revisit puts on the hot list are the sweep's to spread too: they are
    /// while the nodes the sweep found are revisited, so that the children of a node that splits are
    /// not all split in the next update, and theirs after, without the budget (#213).
    marking: bool,
    /// The most that the backlog has held since [`Selection::take_backlog_peak`].
    backlog_peak: usize,
    /// Where the tidying of the queue is: the next bucket and the entry in it (the sweep stamps
    /// every node again and leaves the entries of the old stamps in the queue).
    tidy: Option<(u64, usize)>,
    /// How many entries of the queue a tidying step looks at: [`TIDY`], or fewer for a test.
    #[cfg(test)]
    tidy_by: usize,
    /// How many nodes an update revisits while absorbing: [`ABSORB_REVISITS`], or fewer for a test.
    #[cfg(test)]
    absorb_by: usize,
    nodes: HashMap<Key, Node>,
    /// The nodes that are not hot, by the bucket of the camera's travel in which they are to be
    /// looked at to see if they have become hot, with the stamp they were queued with (an entry for
    /// a node that was queued again since is stale).
    queue: BTreeMap<u64, Vec<(u64, Key)>>,
    /// The entries in `queue`, kept as they come and go (it is asked for on every relook).
    queued_entries: usize,
    /// The nodes that could change within [`HOT`] travel of the camera.
    hot: Vec<Hot>,
    /// The nodes still to be looked at again for other bands ([`Selection::set_bands`]), the next
    /// at the end: [`Selection::relook_some`] takes them a few at a time.
    relook: Vec<Key>,
    /// The bands that the entries of the parked nodes in `relook` were made for, if they are all of
    /// the same: a change back to them restores those nodes' stamps instead of looking at them.
    parked_for: Option<Bands>,
    /// The stamps given out when those nodes were parked: an entry with a stamp over it was made
    /// for other bands, since.
    park_mark: u64,
    /// The stamps given out so far.
    stamps: u64,
    /// Bricks drawn at each level.
    drawn: [usize; LEVELS],
    /// Bricks that are drawn whole although they are nearer than their level is used from, since
    /// there is nothing under them to be split into. Those that are not any more are dropped
    /// from it when it is looked at.
    stuck: HashSet<Key>,
    /// For a `stuck` that is large (see [`Selection::nearest_stuck`]): a lower bound on how near
    /// the nearest brick of it at each level was from `bound_at`, when it was last looked at in
    /// full; the bricks added to `stuck` since, which are not in the bound yet; and how many.
    bound: [Option<f64>; LEVELS],
    bound_at: Option<[f64; 3]>,
    since: Vec<Key>,
    /// The bound being found again, a few bricks of `stuck` at a time.
    scan: Option<Scan>,
    /// How many stuck bricks make `stuck` large: [`MANY_STUCK`], or fewer for a test.
    #[cfg(test)]
    many: usize,
    /// Bricks that are whole because the bricks under them are not resident (see
    /// [`Selection::take_wanted`]).
    waiting: HashSet<Key>,
    /// Those of `waiting` that nobody has been told of yet.
    fresh: Vec<Key>,
    /// Bricks that stopped waiting without their children arriving (they were removed, merged or
    /// buried), which nobody has been told of yet.
    cancelled: Vec<Key>,
}

impl Selection {
    /// The bricks of `store` that are drawn from `camera` (a position in Voxels), looking from the
    /// bricks `roots` of the coarsest level, `top`, down. The store must have its pyramid
    /// ([`BrickStore::build_lod`]) as far as `top`. See [`top_bricks`] for the roots.
    pub fn new(
        store: &BrickStore,
        camera: [f64; 3],
        bands: Bands,
        top: usize,
        roots: impl IntoIterator<Item = BrickCoord>,
    ) -> (Self, Diff) {
        Selection::new_capped(store, camera, bands, top, roots, None)
    }

    /// [`Selection::new`] under the horizon cap `horizon` (see [`Selection::set_horizon`]), which
    /// is on if the camera is over it.
    pub fn new_capped(
        store: &BrickStore,
        camera: [f64; 3],
        bands: Bands,
        top: usize,
        roots: impl IntoIterator<Item = BrickCoord>,
        horizon: Option<Horizon>,
    ) -> (Self, Diff) {
        Selection::new_reached(store, camera, bands, top, roots, horizon, Reach::NONE)
    }

    /// [`Selection::new_capped`] with the margins and the lead of `reach` (see
    /// [`Selection::set_margins`] and [`Selection::set_ahead`]) in force from the first look at
    /// the nodes, so that the cap is the raised camera's and the margin's children are wanted at
    /// once, and not by a second look at every node (and a patch of what is drawn) after. Its
    /// horizon is `horizon`, not the reach's.
    pub fn new_reached(
        store: &BrickStore,
        camera: [f64; 3],
        bands: Bands,
        top: usize,
        roots: impl IntoIterator<Item = BrickCoord>,
        horizon: Option<Horizon>,
        reach: Reach,
    ) -> (Self, Diff) {
        Selection::new_reached_in_chunks(
            store, camera, bands, top, roots, horizon, reach, PLAN_CHUNK,
        )
    }

    /// [`Selection::new_reached`] planning `chunk` nodes of a level at a time.
    #[allow(clippy::too_many_arguments)]
    fn new_reached_in_chunks(
        store: &BrickStore,
        camera: [f64; 3],
        bands: Bands,
        top: usize,
        roots: impl IntoIterator<Item = BrickCoord>,
        horizon: Option<Horizon>,
        reach: Reach,
        chunk: usize,
    ) -> (Self, Diff) {
        assert!(top < LEVELS && top < store.level_count());
        let mut selection = Selection {
            bands,
            top,
            camera,
            reach: Reach { horizon, ..reach },
            horizon,
            placed: Reach { horizon, ..reach }.from(camera),
            travelled: 0.0,
            rewanted: false,
            roots: Vec::new(),
            sweep: Vec::new(),
            #[cfg(test)]
            sweep_by: SWEEP,
            absorbing: false,
            backlog: Vec::new(),
            backlog_peak: 0,
            marking: false,
            flip_stamp: 0,
            tidy: None,
            #[cfg(test)]
            tidy_by: TIDY,
            #[cfg(test)]
            absorb_by: ABSORB_REVISITS,
            nodes: HashMap::default(),
            queue: BTreeMap::new(),
            queued_entries: 0,
            hot: Vec::new(),
            relook: Vec::new(),
            parked_for: None,
            park_mark: 0,
            stamps: 0,
            drawn: [0; LEVELS],
            stuck: HashSet::default(),
            bound: [None; LEVELS],
            bound_at: None,
            since: Vec::new(),
            scan: None,
            #[cfg(test)]
            many: MANY_STUCK,
            waiting: HashSet::default(),
            fresh: Vec::new(),
            cancelled: Vec::new(),
        };
        // Asked for, so that it is on above the sphere itself.
        selection.reach.horizon = horizon;
        selection.place();
        let mut diff = Diff::default();
        // Level by level, from the top, so that the bricks of a level are looked at on all the
        // threads at once.
        let roots: Vec<BrickCoord> = roots.into_iter().collect();
        selection.roots.clone_from(&roots);
        let probes = par_map(&roots, |&coord| probe(store, top, coord));
        let mut frontier: Vec<(Key, Probe)> = roots
            .iter()
            .zip(probes)
            .filter(|(_, p)| p.present)
            .map(|(&coord, p)| ((top, coord), p))
            .collect();
        while !frontier.is_empty() {
            let (bands, at, camera) = (selection.bands, selection.placed, selection.camera);
            // The plans of a chunk are made on all the threads while this thread puts the chunk
            // before it into the tree (planning reads the store, the bands and the camera, and
            // none of the tree), so the serial part is hidden behind the parallel one: on the 4090
            // the selection of a release rebuild was the sum of the two, 6 s of an 8.5 s rebuild.
            // The chunks are put in in order: the tree and the diff are what they were.
            let chunks: Vec<&[(Key, Probe)]> = frontier.chunks(chunk).collect();
            let plan_chunk =
                |chunk: &[(Key, Probe)]| par_map(chunk, |&(key, p)| plan(store, bands, at, key, p));
            let mut next = Vec::new();
            std::thread::scope(|scope| {
                let mut coming = Some(scope.spawn(|| plan_chunk(chunks[0])));
                for (n, chunk) in chunks.iter().enumerate() {
                    let plans = coming
                        .take()
                        .expect("a chunk is coming")
                        .join()
                        .expect("a planner panicked");
                    if let Some(&after) = chunks.get(n + 1) {
                        let plan_chunk = &plan_chunk;
                        coming = Some(scope.spawn(move || plan_chunk(after)));
                    }
                    for (&(key, p), plan) in chunk.iter().zip(plans) {
                        let distance = brick_distance(camera, key.0, key.1);
                        match plan {
                            Plan::Whole => selection.make_whole(store, key, distance, p, &mut diff),
                            Plan::Wait => {
                                selection.make_whole(store, key, distance, p, &mut diff);
                                selection.want(key);
                            }
                            Plan::Split(children) => {
                                selection.mark_split(key, distance);
                                next.extend(children.into_iter().map(|(c, p)| ((key.0 - 1, c), p)));
                            }
                        }
                    }
                }
            });
            frontier = next;
        }
        (selection, diff)
    }

    pub fn bands(&self) -> Bands {
        self.bands
    }

    pub fn camera(&self) -> [f64; 3] {
        self.camera
    }

    /// The camera's path since the selection began, and that of the places it looks ahead to, in
    /// Voxels: what the selection is made to look at again for (a node is looked at when the
    /// camera has gone as far as it was away from changing), so the cost of following a camera is
    /// in proportion to it.
    pub fn travelled(&self) -> f64 {
        self.travelled
    }

    /// The coarsest level that is looked at.
    pub fn top(&self) -> usize {
        self.top
    }

    /// Bricks drawn at `level`.
    pub fn count(&self, level: usize) -> usize {
        self.drawn.get(level).copied().unwrap_or(0)
    }

    /// The coarsest level with a brick drawn, if any is.
    pub fn coarsest(&self) -> Option<usize> {
        (0..LEVELS).rev().find(|&level| self.drawn[level] > 0)
    }

    /// Whether the brick at `coord` of `level` is drawn.
    pub fn draws(&self, level: usize, coord: BrickCoord) -> bool {
        self.nodes
            .get(&(level, coord))
            .is_some_and(|n| n.state == State::Whole && n.draws)
    }

    /// Whether the brick at `coord` of `level` is split into the bricks under it, which are then
    /// in use: they, and so what is resident under it, are not to be evicted.
    pub fn splits(&self, level: usize, coord: BrickCoord) -> bool {
        self.nodes
            .get(&(level, coord))
            .is_some_and(|n| n.state == State::Split)
    }

    /// Every brick drawn, in no order.
    pub fn bricks(&self) -> impl Iterator<Item = Key> + '_ {
        self.nodes
            .iter()
            .filter(|(_, n)| n.state == State::Whole && n.draws)
            .map(|(&key, _)| key)
    }

    /// For each level, how near the camera the nearest brick drawn at it is, if nearer than the
    /// level is used from: a brick that is drawn whole because there is nothing under it to be
    /// split into (the store holds no Voxels of level 0 there, say). `None` for a level with no
    /// such brick.
    ///
    /// A Planet that is streaming in has hundreds of thousands of such bricks while they wait for
    /// the bricks under them, and looking at all of them in one frame would cost more than the
    /// frame. Then the answer is a lower bound, which is exact when nothing but additions have
    /// happened with the camera where it was: it is never further than the nearest, so a hole made
    /// from it is never too big. When the camera has gone some way from where the bound is from
    /// (it loses what the camera went), or as many bricks have been added as there were, the
    /// bound is found again, [`SCAN_STEP`] bricks at a time over the frames that follow, and the
    /// old one stands until then. A brick that stopped being stuck only makes the bound smaller
    /// than it could be.
    pub fn nearest_stuck(&mut self) -> [Option<f64>; LEVELS] {
        if self.stuck.len() <= self.many() {
            self.bound_at = None;
            self.scan = None;
            return self.nearest_stuck_in_full();
        }
        let Some(at) = self.bound_at else {
            // The first time there are many: it began with few, so this was not many bricks.
            let nearest = self.nearest_stuck_in_full();
            self.bound = nearest;
            self.bound_at = Some(self.camera);
            self.since.clear();
            return nearest;
        };
        let drift = |camera: [f64; 3], from: [f64; 3]| {
            (0..3)
                .map(|a| (camera[a] - from[a]).powi(2))
                .sum::<f64>()
                .sqrt()
        };
        let moved = drift(self.camera, at);
        if self.scan.is_none()
            && (moved > self.bands.d0() || self.since.len() > self.stuck.len() / 2)
        {
            self.scan = Some(Scan {
                keys: self.stuck.iter().copied().collect(),
                done: 0,
                nearest: [None; LEVELS],
                from: self.camera,
                added: Vec::new(),
                dead: Vec::new(),
            });
        }
        if self.scan.is_some() {
            self.step_scan();
        }
        let moved = self.bound_at.map_or(0.0, |at| drift(self.camera, at));
        for (level, coord) in std::mem::take(&mut self.since) {
            // Its distance now is at least this less than it was from where the bound is from.
            let d = brick_distance(self.camera, level, coord) - moved;
            self.bound[level] = Some(self.bound[level].map_or(d, |b| b.min(d)));
        }
        self.bound.map(|b| b.map(|b| (b - moved).max(0.0)))
    }

    /// Looks at the next [`SCAN_STEP`] bricks of the scan, and ends it if that was the last.
    fn step_scan(&mut self) {
        let Some(mut scan) = self.scan.take() else {
            return;
        };
        let (bands, nodes) = (self.bands, &self.nodes);
        let end = (scan.done + SCAN_STEP).min(scan.keys.len());
        for &key in &scan.keys[scan.done..end] {
            if !nodes
                .get(&key)
                .is_some_and(|n| n.state == State::Whole && n.draws)
            {
                scan.dead.push(key);
                continue;
            }
            let (level, coord) = key;
            let d = brick_distance(scan.from, level, coord);
            if d < bands.clear_radius(level) && scan.nearest[level].is_none_or(|n| d < n) {
                scan.nearest[level] = Some(d);
            }
        }
        scan.done = end;
        if end < scan.keys.len() {
            self.scan = Some(scan);
            return;
        }
        for key in scan.dead {
            // Unless it was made stuck again since.
            let stuck_now = self
                .nodes
                .get(&key)
                .is_some_and(|n| n.state == State::Whole && n.draws);
            if !stuck_now {
                self.stuck.remove(&key);
            }
        }
        self.bound = scan.nearest;
        self.bound_at = Some(scan.from);
        self.since = scan.added;
    }

    fn nearest_stuck_in_full(&mut self) -> [Option<f64>; LEVELS] {
        let (bands, camera) = (self.bands, self.camera);
        let nodes = &self.nodes;
        self.stuck.retain(|key| {
            nodes
                .get(key)
                .is_some_and(|n| n.state == State::Whole && n.draws)
        });
        let mut nearest = [None::<f64>; LEVELS];
        for &(level, coord) in &self.stuck {
            let d = brick_distance(camera, level, coord);
            if d < bands.clear_radius(level) && nearest[level].is_none_or(|n| d < n) {
                nearest[level] = Some(d);
            }
        }
        nearest
    }

    fn many(&self) -> usize {
        #[cfg(test)]
        return self.many;
        #[cfg(not(test))]
        MANY_STUCK
    }

    fn add_stuck(&mut self, key: Key) {
        if self.stuck.insert(key) && self.bound_at.is_some() {
            self.since.push(key);
            if let Some(scan) = &mut self.scan {
                scan.added.push(key);
            }
        }
    }

    /// Whether a brick was given up on and the next [`Selection::update`] is to look at it, though
    /// the camera may not move (see [`Selection::unwant`]).
    pub fn rewanted(&self) -> bool {
        self.rewanted
    }

    /// Nodes that other bands ([`Selection::set_bands`]) have yet to look at again.
    pub fn relooking(&self) -> usize {
        self.relook.len()
    }

    /// Whether a flip of the horizon cap is still being absorbed (#213): nodes are left to look at
    /// again or to change, so what is drawn is not yet what the rule gives. (The tidying of the
    /// queue is [`Selection::tidying`].)
    pub fn absorbing(&self) -> bool {
        self.absorbing
    }

    /// Whether the queue is yet to be tidied of the entries that a flip made stale (#213). It
    /// changes nothing that is drawn, but the next [`Selection::update`] is to run for it.
    pub fn tidying(&self) -> bool {
        self.tidy.is_some()
    }

    /// The most nodes that waited to be revisited at once since the last call (#213).
    pub fn take_backlog_peak(&mut self) -> usize {
        std::mem::take(&mut self.backlog_peak)
    }

    /// Nodes of the tree: the bricks drawn, those split, and air that may have something in it.
    pub fn nodes(&self) -> usize {
        self.nodes.len()
    }

    /// Entries in the queue and among the hot nodes: the stale ones are counted too.
    pub fn queued(&self) -> usize {
        self.queued_entries + self.hot.len()
    }

    /// Adds `key`, which is in the tree and whose parent is split (or is the top), and what is
    /// under it if it is to be split.
    fn build(&mut self, store: &BrickStore, key: Key, p: Probe, diff: &mut Diff) {
        let distance = brick_distance(self.camera, key.0, key.1);
        match plan(store, self.bands, self.placed, key, p) {
            Plan::Whole => self.make_whole(store, key, distance, p, diff),
            Plan::Wait => {
                self.make_whole(store, key, distance, p, diff);
                self.want(key);
            }
            Plan::Split(children) => self.split(store, key, distance, children, diff),
        }
    }

    /// Notes that the whole brick `key` waits for the bricks under it to be resident.
    fn want(&mut self, key: Key) {
        if self.waiting.insert(key) {
            self.fresh.push(key);
        }
    }

    /// The bricks that are whole although the bands split them, because the bricks under them
    /// are not resident: the ones that became so since this was last called. Each has its eight
    /// children to be generated and held (and marked, with [`BrickStore::set_children_resident`]), after which
    /// [`Selection::arrived`] splits it. A brick is only listed again if it is
    /// [`unwanted`](Selection::unwant) first.
    pub fn take_wanted(&mut self) -> Vec<Key> {
        self.take_wanted_up_to(usize::MAX)
    }

    /// [`Selection::take_wanted`], but at most `limit` of them; the rest are for the next call.
    pub fn take_wanted_up_to(&mut self, limit: usize) -> Vec<Key> {
        let rest = self.fresh.split_off(self.fresh.len().min(limit));
        std::mem::replace(&mut self.fresh, rest)
            .into_iter()
            .filter(|key| self.waiting.contains(key))
            .collect()
    }

    /// Makes room for `nodes` nodes in the tree, `waiting` bricks waiting and `stuck` bricks
    /// stuck (in all, with those it holds already), so that the tables do not double in the middle of a frame.
    pub fn reserve(&mut self, nodes: usize, waiting: usize, stuck: usize) {
        // Room for that many **in all**: `HashMap::reserve` is for that many *more*, which on a
        // table that holds entries already (the selection a rebuild has just made) asks for room
        // for the entries and the reservation both, and doubles the table.
        self.nodes.reserve(nodes.saturating_sub(self.nodes.len()));
        self.waiting
            .reserve(waiting.saturating_sub(self.waiting.len()));
        self.stuck.reserve(stuck.saturating_sub(self.stuck.len()));
    }

    /// The size of every table that [`Selection::reserve`] makes room in.
    pub fn table_sizes(&self) -> Vec<TableSize> {
        vec![
            TableSize::hash::<(Key, Node)>(
                "selection nodes",
                self.nodes.len(),
                self.nodes.capacity(),
            ),
            TableSize::hash::<Key>(
                "selection waiting",
                self.waiting.len(),
                self.waiting.capacity(),
            ),
            TableSize::hash::<Key>("selection stuck", self.stuck.len(), self.stuck.capacity()),
        ]
    }

    /// The bricks of [`Selection::take_wanted`] that are not wanted any more since this was last
    /// called, because the node was removed (merged into its parent, or buried) before its
    /// children arrived. Whatever is generating them can stop. A brick that is waiting again
    /// (its node came back) is not listed: its children are wanted after all.
    pub fn take_cancelled(&mut self) -> Vec<Key> {
        let waiting = &self.waiting;
        let mut cancelled = std::mem::take(&mut self.cancelled);
        cancelled.retain(|key| !waiting.contains(key));
        cancelled
    }

    /// Takes `key` off the bricks that wait because its node is gone.
    fn forget(&mut self, key: Key) {
        if self.waiting.remove(&key) {
            self.cancelled.push(key);
        }
    }

    /// Bricks waiting for the bricks under them to be resident.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// The soft misses of this moment (CONTEXT.md): bricks that are drawn whole although the bands
    /// would split them if the bricks under them were resident. Those that wait only for the margin
    /// or the lead are not drawn coarser than the bands ask, and are not counted.
    pub fn soft_misses(&self) -> usize {
        self.soft_misses_by_level().iter().sum()
    }

    /// [`Selection::soft_misses`] by the level of the brick that is drawn whole.
    pub fn soft_misses_by_level(&self) -> [usize; LEVELS] {
        let mut by_level = [0; LEVELS];
        for &(level, coord) in &self.waiting {
            if self
                .nodes
                .get(&(level, coord))
                .is_some_and(|node| node.draws)
                && self.drawn_split((level, coord), None)
                && let Some(count) = by_level.get_mut(level)
            {
                *count += 1;
            }
        }
        by_level
    }

    /// Bricks that are split although the bricks under them are not resident in `store`: what a
    /// ray would find nothing in. A split node's children are resident by construction (it is split
    /// when they arrive, and eviction leaves what the scene has split), so this is the check of
    /// that, and is zero.
    ///
    /// The look is shared among threads that run at the lowest priority (see
    /// [`set_worker_start`]): a pass over every node was 90 ms of the scene thread on the 4090,
    /// with the store's read lock held, and a flight at 2 m had 17 to 30 frames of misses under the
    /// camera in the second that it came in (#168). The count is the same whatever the threads.
    pub fn holes(&self, store: &BrickStore) -> usize {
        let threads = if self.nodes.len() < PARALLEL_FROM {
            1
        } else {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get() / 2)
                .clamp(1, MOST_THREADS)
        };
        self.holes_by(store, threads)
    }

    /// [`Selection::holes`] shared among `threads` (one: here and now). Each looks at every
    /// `threads`th node of the table, whose order is the same for all of them.
    fn holes_by(&self, store: &BrickStore, threads: usize) -> usize {
        let hole = |(&(level, coord), node): (&Key, &Node)| {
            node.state == State::Split && !store.children_resident(level, coord)
        };
        if threads <= 1 {
            return self.nodes.iter().filter(|&entry| hole(entry)).count();
        }
        std::thread::scope(|scope| {
            let parts: Vec<_> = (0..threads)
                .map(|first| {
                    scope.spawn(move || {
                        if let Some(start) = WORKER_START.get() {
                            start();
                        }
                        self.nodes
                            .iter()
                            .skip(first)
                            .step_by(threads)
                            .filter(|&entry| hole(entry))
                            .count()
                    })
                })
                .collect();
            parts
                .into_iter()
                .map(|part| part.join().expect("a thread that looks for holes"))
                .sum()
        })
    }

    /// Takes `key` off the bricks that wait, because its children will not be generated after all (the
    /// camera has gone from it): it is listed again if the bands want it split later.
    pub fn unwant(&mut self, key: Key) {
        if self.waiting.remove(&key)
            && let Some(node) = self.nodes.get_mut(&key)
            && node.state == State::Whole
            && (!watched(node.stamp) || node.margin)
        {
            // It is watched again, so that the bands (or the margin) ask for its children again if
            // they want it split. One that was in the margin is looked at by the next update, not
            // when the camera has gone as far as it is inside: the loader may have dropped it
            // for a camera that was behind, and it is in the margin still.
            let was_margin = node.margin;
            node.margin = false;
            if was_margin {
                self.rewanted = true;
                self.stamps += 1;
                node.stamp = self.stamps;
                self.hot.push(Hot {
                    key,
                    stamp: self.stamps,
                    split: false,
                    margin: false,
                    swept: false,
                });
            } else {
                let distance = brick_distance(self.camera, key.0, key.1);
                self.queue_node(key, distance, false);
            }
        }
    }

    /// What residency looks at besides the bands: see [`Selection::set_margins`] and
    /// [`Selection::set_ahead`].
    pub fn reach(&self) -> Reach {
        self.reach
    }

    /// Wants the children of the whole bricks that are within `margins` of the camera resident,
    /// besides those the bands split (ADR 0004): [`Selection::take_wanted`] lists them as it does
    /// those, and they are not drawn. A brick is in until the camera is [`HYSTERESIS`] out of it. It
    /// looks at every whole brick once, so it is for a selection that is new (or a change of
    /// setting), not for every frame.
    pub fn set_margins(&mut self, store: &BrickStore, margins: Margins) {
        if self.reach.margins == margins {
            return;
        }
        self.reach.margins = margins;
        let flipped = self.place();
        self.look_again(store);
        // The cap seen from a camera raised by another margin has other nodes in it, whether or not
        // it came on or went off.
        if flipped || self.reach.horizon.is_some() {
            self.reconsider_all();
        }
    }

    /// Draws by other `bands`, **in place**: the tree is kept, not made again (that is
    /// [`crate::Scene::set_bands`]'s, seconds at full size). It is the speed-scaled `d0` of ADR 0009
    /// (#168): the bands differ only in `d0`'s scale, a few times in a flight. This only lists the
    /// nodes whose slack the new bands change (nothing is drawn differently yet);
    /// [`Selection::relook_some`] looks at them, a few at a time, and says what changed.
    ///
    /// **Coarser** bands (a smaller `d0`) bring a split node's merge nearer, and a whole node's
    /// split further, so the nodes to look at are the split ones: coarsest first, as a merge takes
    /// what is under it with it. A whole node that holds a margin request is not one: its entry is
    /// for a split nearer than the new bands' (it comes due early, and the node is watched again),
    /// and the margin that the node may now let go is let go then. Listed, it was parked, and a
    /// parked node is watched by nothing: a margin node that the camera came to before the relook
    /// (590 thousand nodes behind, in a flight) was not split though its children were resident,
    /// and was the soft miss under the camera for 45 frames at a time. **Finer** bands bring a whole node's split nearer, and a split node's merge further: the
    /// whole nodes of level 1 and up, nearest the camera first (level 0 cannot split).
    pub fn set_bands(&mut self, bands: Bands) {
        if self.bands == bands {
            return;
        }
        let coarser = bands.scaled_d0() < self.bands.scaled_d0();
        let old = self.bands;
        self.bands = bands;
        // The bound on how near a stuck brick is was made for the old bands' clear radii.
        self.bound = [None; LEVELS];
        self.bound_at = None;
        self.scan = None;
        self.since.clear();
        // What the last change of bands has not looked at yet is parked (it is in no list but
        // this), so it is looked at for these bands too, whatever they are: a flight's first
        // second changes the bands three times, and a node dropped from the list would be watched
        // by nothing.
        let pending = std::mem::take(&mut self.relook);
        // A change back to the bands that the parked nodes' entries were made for (a depth given
        // back before the nodes it listed were looked at) is a change of nothing for them: their
        // stamps are restored, and the entries are live again, instead of each being looked at and
        // given a new entry beside the old. What has entries made since (the nodes looked at for
        // the other bands, and what the camera's move built or looked at again) is the list.
        let back = self.parked_for == Some(bands);
        let mark = self.park_mark;
        let mut restored: HashSet<Key> = HashSet::default();
        for &key in &pending {
            if let Some(node) = self.nodes.get_mut(&key)
                && kept(node.stamp)
            {
                if back {
                    node.stamp &= !PARKED;
                    restored.insert(key);
                } else {
                    // Its entries are for older bands than the ones that are left now: lost.
                    node.stamp = LOST;
                }
            }
        }
        let pending: HashSet<Key> = pending
            .into_iter()
            .filter(|key| !restored.contains(key))
            .collect();
        // Whether a node's entry is for the bands that these are a return to (restored, or not
        // looked at since): it is not on the list.
        let valid = |node: &Node| back && watched(node.stamp) && node.stamp <= mark;
        self.parked_for = Some(old);
        self.park_mark = self.stamps;
        if coarser {
            let mut by_level: [Vec<Key>; LEVELS] = std::array::from_fn(|_| Vec::new());
            for (&key, node) in &self.nodes {
                if (node.state == State::Split || pending.contains(&key)) && !valid(node) {
                    by_level[key.0].push(key);
                }
            }
            self.relook = by_level.into_iter().flatten().collect();
        } else {
            // About 64 buckets of distance, a doubling in two: nearest at the end, with no sort.
            let mut by_distance: Vec<Vec<Key>> = vec![Vec::new(); 64];
            for (&key, node) in &self.nodes {
                if ((node.state == State::Whole && key.0 >= 1) || pending.contains(&key))
                    && !valid(node)
                {
                    let d = brick_distance(self.camera, key.0, key.1).max(1.0);
                    by_distance[((2.0 * d.log2()) as usize).min(63)].push(key);
                }
            }
            self.relook = by_distance.into_iter().rev().flatten().collect();
        }
        self.park_relook();
    }

    /// Takes the nodes that [`Selection::relook_some`] is to look at out of [`Selection::update`]'s
    /// way: their stamp is voided, so that what the lists hold for them (the hot list and the
    /// queue, made for the old bands' slack) is stale, and an update does not take a change of
    /// bands for the camera's move and change them all at once, outside the budget (3.3 million
    /// bricks in one follow step, 7 s at full size). `relook_some` gives each a new stamp and its
    /// place.
    ///
    /// The stamp is not lost: it is kept under [`PARKED`], with its entries (which `update` leaves
    /// where they are), for a change back to the bands they were made for.
    ///
    /// Every listed node is parked, for coarser bands too. Parking only the nodes on the hot list
    /// was tried, so that a split node that is queued came due by the camera's travel and merged
    /// late, with no relook (the splits from before a flight would not stay for it: 450 thousand
    /// bricks of level 0 at full size): at a flight's speed the camera goes further than the old
    /// slack of nearly all of them in a round or two, and they merge **in one update**, outside the
    /// budget, which is what parking is for (the 3.3 million bricks of a 7 s follow step). The dev
    /// box's flights went to a lag of 9 `d0` and 467 frames of camera soft misses (from about 30)
    /// with it, and the test of an update's size below holds it.
    fn park_relook(&mut self) {
        for key in &self.relook {
            if let Some(node) = self.nodes.get_mut(key)
                && node.stamp != 0
            {
                node.stamp |= PARKED;
            }
        }
    }

    /// Looks again at up to `nodes` of the nodes that [`Selection::set_bands`] listed, stopping
    /// early once the bricks that changed come to `bricks`, and puts each back to be looked at when
    /// the camera has gone as far as the new bands ask. Returns what changed in what is drawn; what
    /// is left is [`Selection::relooking`].
    pub fn relook_some(&mut self, store: &BrickStore, nodes: usize, bricks: usize) -> Diff {
        let mut diff = Diff::default();
        let (bands, reach, camera) = (self.bands, self.placed, self.camera);
        for _ in 0..nodes {
            let Some(key) = self.relook.pop() else { break };
            let Some(node) = self.nodes.get_mut(&key) else {
                continue;
            };
            self.stamps += 1;
            node.stamp = self.stamps;
            let (stamp, split, margin) = (self.stamps, node.state == State::Split, node.margin);
            let distance = brick_distance(camera, key.0, key.1);
            if node_changes(bands, reach, key, distance, split, margin) {
                self.revisit(store, key, &mut diff);
                if diff.added.len() + diff.removed.len() >= bricks {
                    break;
                }
            } else {
                let slack = node_slack(bands, reach, key, distance, split, margin);
                self.watch(key, stamp, split, margin, slack);
            }
        }
        // Each node looked at left its old entries stale where they were, and they are only
        // dropped when the camera has gone as far as they are due, which a camera that stays does
        // not: they are dropped when the last node is looked at, or if they come to more than twice
        // the nodes, so that bands that go to and fro do not grow the lists.
        if self.relook.is_empty() || self.queued() > 2 * self.nodes.len().max(1) {
            self.compact();
        }
        diff
    }

    /// Drops the entries of the hot list and the queue that are stale: for a node that has gone or
    /// was looked at again since. What is kept for a parked node is not stale.
    fn compact(&mut self) {
        let nodes = &self.nodes;
        let live = |stamp: u64, key: &Key| {
            nodes
                .get(key)
                .is_some_and(|n| n.stamp == stamp || n.stamp == stamp | PARKED)
        };
        self.hot.retain(|entry| live(entry.stamp, &entry.key));
        let mut kept = 0;
        self.queue.retain(|_, entries| {
            entries.retain(|(stamp, key)| live(*stamp, key));
            kept += entries.len();
            !entries.is_empty()
        });
        self.queued_entries = kept;
    }

    /// The horizon cap that was asked for, which [`Selection::reach`] has while it is on.
    pub fn horizon(&self) -> Option<Horizon> {
        self.horizon
    }

    /// Culls by `horizon` (see [`Horizon`]), or by none: what is past the horizon of the camera
    /// raised by the vertical margin is not split or wanted. Looks at every node once, so it is
    /// for a selection that is new, not for every frame. Returns what changed in what is drawn.
    pub fn set_horizon(&mut self, store: &BrickStore, horizon: Option<Horizon>) -> Diff {
        if self.horizon == horizon {
            return Diff::default();
        }
        self.horizon = horizon;
        // As when it was new: on above the sphere itself.
        self.reach.horizon = horizon;
        self.place();
        self.reconsider_all();
        self.update(store, self.camera)
    }

    /// Works out `placed`, and whether the cap is on: it is while the raised camera is over its
    /// sphere, and comes back on [`CAP_ON`] over. Returns whether the cap changed.
    fn place(&mut self) -> bool {
        let was = self.reach.horizon;
        let height = Reach {
            horizon: self.horizon,
            ..self.reach
        }
        .height(self.camera);
        let on = height.is_some_and(|h| h > if was.is_some() { 0.0 } else { CAP_ON });
        self.reach.horizon = if on { self.horizon } else { None };
        self.placed = self.reach.from(self.camera);
        self.reach.horizon != was
    }

    /// Whether the cap was asked for and the raised camera at `camera` is under the sea's sphere
    /// itself: no hysteresis, as [`Selection::place`] has for a cap that is on. A selection made
    /// again at such a camera has the cap off; at any camera over the sphere it has it on.
    pub fn camera_under_horizon(&self, camera: [f64; 3]) -> bool {
        let Some(horizon) = self.horizon else {
            return false;
        };
        let height = Reach {
            horizon: Some(horizon),
            ..self.reach
        }
        .height(camera);
        height.is_some_and(|h| h <= 0.0)
    }

    /// Looks at every node at the next [`Selection::update`], for a change of the cap.
    fn reconsider_all(&mut self) {
        self.rewanted = true;
        // Every node is looked at now: none is left for the relook of other bands, a sweep or the
        // backlog.
        self.relook.clear();
        self.parked_for = None;
        self.sweep.clear();
        self.backlog.clear();
        self.absorbing = false;
        self.tidy = None;
        self.queue.clear();
        self.queued_entries = 0;
        self.hot.clear();
        for (&key, node) in &mut self.nodes {
            self.stamps += 1;
            node.stamp = self.stamps;
            self.hot.push(Hot {
                key,
                stamp: self.stamps,
                split: node.state == State::Split,
                margin: node.margin,
                swept: false,
            });
        }
    }

    /// Has every node looked at again for a flip of the horizon cap, [`SWEEP`] of them in each
    /// [`Selection::update`] from now on (#213): the nodes that are not looked at yet are as they
    /// were, a cut through the tree that does not have a hole in it, and what a node was queued
    /// for under the old cap is looked at again when the sweep gets to it.
    fn begin_sweep(&mut self) {
        let top = self.top;
        self.sweep = self.roots.iter().map(|&coord| (top, coord)).collect();
        self.flip_stamp = self.stamps;
        // The tidying goes on through a flip that comes before the sweep of the last has ended
        // (and starts, for the one that has none): an entry that is stale is stale whenever it is
        // looked at, and a sweep that is cut short leaves its own.
        self.tidy.get_or_insert((0, 0));
    }

    /// Drops the entries of the queue whose node was stamped again since, [`TIDY`] of them in each
    /// call, in one pass from a flip and another from the end of its sweep: without it each flip
    /// leaves about one stale entry for each node, until the camera has travelled what the entry
    /// was queued for.
    fn tidy_some(&mut self) {
        let Some((from, mut at)) = self.tidy else {
            return;
        };
        #[cfg(test)]
        let mut left = self.tidy_by;
        #[cfg(not(test))]
        let mut left = TIDY;
        let nodes = &self.nodes;
        let mut next = None;
        let mut empty = Vec::new();
        let mut dropped = 0;
        // A bucket can hold a million entries, so the step may end inside one.
        for (&bucket, entries) in self.queue.range_mut(from..) {
            if bucket != from {
                at = 0;
            }
            while at < entries.len() && left > 0 {
                left -= 1;
                let (stamp, key) = entries[at];
                // What is kept for a parked node (for a change back to the bands it was made for)
                // is not stale.
                if nodes
                    .get(&key)
                    .is_some_and(|n| n.stamp == stamp || n.stamp == stamp | PARKED)
                {
                    at += 1;
                } else {
                    entries.swap_remove(at);
                    dropped += 1;
                }
            }
            if at < entries.len() {
                next = Some((bucket, at));
                break;
            }
            if entries.is_empty() {
                empty.push(bucket);
            }
            if left == 0 {
                next = Some((bucket + 1, 0));
                break;
            }
        }
        for bucket in empty {
            self.queue.remove(&bucket);
        }
        self.queued_entries -= dropped;
        // A pass that ends while the sweep goes on is not started again (at full size that was
        // seven passes over five million entries): the entries the sweep makes stale after it are
        // for the pass that starts when the sweep ends, or the next flip's.
        self.tidy = next;
    }

    /// Looks at the next batch of the sweep again: each node is stamped and put on the hot list,
    /// so that the update looks at it under the cap as it is now, and a split node's children are
    /// next.
    fn sweep_some(&mut self) {
        #[cfg(test)]
        let batch = self.sweep_by;
        #[cfg(not(test))]
        let batch = SWEEP;
        let had = !self.sweep.is_empty();
        for _ in 0..batch {
            let Some(key) = self.sweep.pop() else {
                break;
            };
            let Some(node) = self.nodes.get_mut(&key) else {
                continue;
            };
            self.stamps += 1;
            node.stamp = self.stamps;
            let (split, margin) = (node.state == State::Split, node.margin);
            self.hot.push(Hot {
                key,
                stamp: self.stamps,
                split,
                margin,
                swept: true,
            });
            if split {
                self.sweep
                    .extend(key.1.children().map(|child| (key.0 - 1, child)));
            }
        }
        if had && self.sweep.is_empty() {
            self.tidy = Some((0, 0));
        }
    }

    /// Whether the bands split the brick `key`, as `was` says it is (see [`Bands::split`]), and the
    /// horizon cap leaves it in: what a node is split by.
    fn drawn_split(&self, key: Key, was: Option<bool>) -> bool {
        let (level, coord) = key;
        let distance = brick_distance(self.camera, level, coord);
        self.bands.split(level, distance, was)
            && self
                .placed
                .in_cap_at(level, coord, distance, was.unwrap_or(false))
    }

    /// Wants the children of the whole bricks that are near the places `ahead` (where the camera
    /// will be, which [`Selection::set_margins`] does not cover), as the bands would split them for a
    /// camera there. Cheap: it looks at no brick, and the next [`Selection::update`] finds those
    /// that came into reach.
    pub fn set_ahead(&mut self, ahead: [Option<[f64; 3]>; crate::lod::AHEAD]) {
        if self.reach.ahead == ahead {
            return;
        }
        // A distance cannot fall by more than a place moved (from where the camera was, if it was
        // not looked at before), so a brick that is that far from coming into reach is due.
        let moved = ahead
            .iter()
            .zip(self.reach.ahead)
            .filter_map(|(new, old)| {
                new.map(|new| {
                    let from = old.unwrap_or(self.camera);
                    (0..3)
                        .map(|a| (new[a] - from[a]).powi(2))
                        .sum::<f64>()
                        .sqrt()
                })
            })
            .fold(0.0, f64::max);
        self.travelled += moved;
        self.reach.ahead = ahead;
        self.place();
    }

    /// Looks at every whole brick again, for a change of [`Reach`].
    fn look_again(&mut self, store: &BrickStore) {
        let keys: Vec<Key> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.state == State::Whole)
            .map(|(&key, _)| key)
            .collect();
        let at = self.placed;
        for key in keys {
            self.settle_margin(store, at, key);
            if self.nodes.get(&key).is_some_and(|node| watched(node.stamp)) {
                let distance = brick_distance(self.camera, key.0, key.1);
                self.queue_node(key, distance, false);
            }
        }
    }

    /// Sets whether the whole node `key` is in reach (see [`Node::margin`]), and wants its children
    /// if it is in reach and they are not resident, or gives up on them if it is not any more.
    fn settle_margin(&mut self, store: &BrickStore, at: Placed, key: Key) {
        if self.reach.is_none() {
            return;
        }
        let Some(node) = self.nodes.get(&key).copied() else {
            return;
        };
        let (level, coord) = key;
        let reach = at.distance(level, coord);
        let inside = self.bands.split(level, reach, Some(node.margin))
            && at.in_cap_wanted(level, coord, node.margin);
        if inside && !store.children_resident(level, coord) {
            self.want(key);
        } else if inside {
            self.want_below(store, at, key);
        } else if node.margin {
            self.forget(key);
        }
        if let Some(node) = self.nodes.get_mut(&key) {
            node.margin = inside;
        }
    }

    /// Wants the children of the bricks under `key` that the reach asks for, `key` being a brick
    /// that is not split and whose children are resident: they are not nodes, so the reach does
    /// not look at them as it moves, and this walks down them as far as it wants (a place the
    /// camera is going to, where the bands will want the finest level, is several levels under
    /// the brick that is drawn there now). Each is listed by [`Selection::take_wanted`] like any
    /// other, and walked below when it arrives.
    fn want_below(&mut self, store: &BrickStore, at: Placed, key: Key) {
        let (level, coord) = key;
        if level <= 1 || self.reach.is_none() {
            return;
        }
        for child in coord.children() {
            let below = (level - 1, child);
            if self.nodes.contains_key(&below) {
                continue;
            }
            let reach = at.distance(level - 1, child);
            if !self.bands.split(level - 1, reach, None)
                || !at.in_cap_wanted(level - 1, child, false)
                || !probe(store, level - 1, child).present
            {
                continue;
            }
            if store.children_resident(level - 1, child) {
                self.want_below(store, at, below);
            } else {
                self.want(below);
            }
        }
    }

    /// How far the camera and the places it looks ahead to must go before the whole node `key`, at
    /// `distance` from the camera, could change what is drawn or whether it is in reach.
    fn whole_slack(&self, key: Key, distance: f64, margin: bool) -> f64 {
        node_slack(self.bands, self.placed, key, distance, false, margin)
    }

    /// Takes in the bricks `cells` of the coarsest level, which are held now (the cells of a Guard
    /// that was not, [`BrickStore::hold_guard_cell`]): each is a root of the tree, and what is
    /// under it is built as the bands say, as it is for a root that was there at the start. No
    /// root and no node that was there is changed, except the bricks beside the new ones that a
    /// brick of theirs buries, as the arrival of a brick's children looks at them ([`Selection::arrived`]).
    /// A cell that is not air is a brick of the Planet, and a cell that is is one only if ground
    /// pokes up into it (see [`classify`]). So the selection is what [`Selection::new`] makes once
    /// every cell is held, whatever the order they came in.
    pub fn add_roots(
        &mut self,
        store: &BrickStore,
        cells: impl IntoIterator<Item = BrickCoord>,
    ) -> Diff {
        let mut diff = Diff::default();
        let top = self.top;
        let mut added = Vec::new();
        for coord in cells {
            if self.roots.contains(&coord) {
                continue;
            }
            self.roots.push(coord);
            added.push(coord);
            let p = probe(store, top, coord);
            if p.present {
                self.build(store, (top, coord), p, &mut diff);
            }
        }
        // The bricks beside the new cells, at the Guard's levels (there is nothing of them below), were told that a brick across
        // the border was not known to be full, which it may be now.
        let mut beside = BTreeSet::new();
        for &cell in &added {
            for level in store.stream_floor().unwrap_or(top).min(top)..=top {
                let span = 1i32 << (top - level);
                let low = cell.0.map(|c| c * span);
                for face in 0..6 {
                    let axis = face / 2;
                    let outside = if face % 2 == 0 {
                        low[axis] - 1
                    } else {
                        low[axis] + span
                    };
                    let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
                    for u in low[a]..low[a] + span {
                        for v in low[b]..low[b] + span {
                            let mut at = [0; 3];
                            (at[axis], at[a], at[b]) = (outside, u, v);
                            let key = (level, BrickCoord(at));
                            if self.nodes.contains_key(&key) {
                                beside.insert(key);
                            }
                        }
                    }
                }
            }
        }
        for key in beside {
            if store.is_full_known(key.0, key.1) {
                self.recheck(store, key, &mut diff);
            }
        }
        diff
    }

    /// Splits the bricks `parents` that were waiting, now that the bricks under them are resident.
    /// Returns what changed in what is drawn.
    pub fn arrived(&mut self, store: &BrickStore, parents: impl IntoIterator<Item = Key>) -> Diff {
        let mut diff = Diff::default();
        let mut beside = Vec::new();
        for key in parents {
            if self.waiting.remove(&key) {
                // It was whole as a new brick that the rule splits is, so it is split as one is.
                self.revisit_whole_as(store, key, None, &mut diff);
                // Not split: its children are for the reach to walk down.
                if self
                    .nodes
                    .get(&key)
                    .is_none_or(|node| node.state == State::Whole)
                {
                    self.want_below(store, self.placed, key);
                }
            }
            // The bricks that touch the children just made resident were told, when they were looked
            // at, that these were not known to be full (see [`BrickStore::is_full_known`]), so
            // they may be buried after all.
            let (level, coord) = key;
            for child in coord.children() {
                beside.extend(
                    faces(child)
                        .filter(|n| n.parent() != coord)
                        .map(|n| (level - 1, n)),
                );
            }
        }
        beside.sort_unstable();
        beside.dedup();
        for key in beside {
            // Only a brick that is full can be buried, which is all that the children have
            // changed for it.
            if store.is_full_known(key.0, key.1) && self.nodes.contains_key(&key) {
                self.recheck(store, key, &mut diff);
            }
        }
        diff
    }

    /// Notes that the brick `key` of `level` is not drawn any more.
    fn undraw(&mut self, level: usize, key: Key, diff: &mut Diff) {
        self.drawn[level] -= 1;
        self.stuck.remove(&key);
        diff.remove(key);
    }

    /// Makes `key` a node that is split.
    fn mark_split(&mut self, key: Key, distance: f64) {
        self.waiting.remove(&key);
        self.nodes.insert(
            key,
            Node {
                state: State::Split,
                draws: false,
                margin: false,
                stamp: 0,
            },
        );
        self.queue_node(key, distance, true);
    }

    /// Makes `key` a node that is split into `children`, and builds those.
    fn split(
        &mut self,
        store: &BrickStore,
        key: Key,
        distance: f64,
        children: Vec<(BrickCoord, Probe)>,
        diff: &mut Diff,
    ) {
        self.mark_split(key, distance);
        for (child, p) in children {
            self.build(store, (key.0 - 1, child), p, diff);
        }
    }

    /// Makes `key` a leaf, drawn if it is not air.
    fn make_whole(
        &mut self,
        store: &BrickStore,
        key: Key,
        distance: f64,
        p: Probe,
        diff: &mut Diff,
    ) {
        self.nodes.insert(
            key,
            Node {
                state: State::Whole,
                draws: p.draws,
                margin: false,
                stamp: 0,
            },
        );
        self.settle_margin(store, self.placed, key);
        let stuck = self.drawn_split(key, Some(false));
        if p.draws {
            self.drawn[key.0] += 1;
            diff.add(key);
            if stuck {
                self.add_stuck(key);
            }
        }
        if !stuck {
            self.queue_node(key, distance, false);
        }
    }

    /// Has `key` looked at when it may change, given its `distance` now: every frame if that
    /// could be soon, else when the camera has gone far enough that it could be.
    fn queue_node(&mut self, key: Key, distance: f64, split: bool) {
        let margin = self.nodes.get(&key).is_some_and(|node| node.margin);
        let slack = if split {
            node_slack(self.bands, self.placed, key, distance, true, margin)
        } else {
            self.whole_slack(key, distance, margin)
        };
        let stamp = if slack.is_finite() {
            self.stamps += 1;
            self.stamps
        } else {
            0
        };
        if let Some(node) = self.nodes.get_mut(&key) {
            node.stamp = stamp;
        }
        if stamp != 0 {
            self.watch(key, stamp, split, margin, slack);
        }
    }

    /// Puts the entry `stamp` for `key` with the hot nodes if it is `slack` from changing and
    /// that is near, or in the queue for when it will be.
    fn watch(&mut self, key: Key, stamp: u64, split: bool, margin: bool, slack: f64) {
        if slack <= HOT {
            self.hot.push(Hot {
                key,
                stamp,
                split,
                margin,
                swept: self.marking,
            });
        } else {
            // At least half of `HOT` on, so that a node that is just out of reach is not looked at
            // again and again.
            let at = self.travelled + (slack - HOT).max(HOT / 2.0);
            self.queue
                .entry((at / BUCKET) as u64)
                .or_default()
                .push((stamp, key));
            self.queued_entries += 1;
        }
    }

    /// Leaves `key`, a whole brick that the bands split but that cannot be (there is nothing to
    /// split it into, or it waits for that), unwatched: the camera does not change that, only the
    /// store does, and what changes the store looks at the brick again.
    fn unwatch(&mut self, key: Key) {
        if let Some(node) = self.nodes.get_mut(&key) {
            node.stamp = 0;
        }
    }

    /// Removes the nodes under `key`: the bricks of the level below it, and all they are split
    /// into.
    fn remove_under(&mut self, key: Key, diff: &mut Diff) {
        let (level, coord) = key;
        if level == 0 {
            return;
        }
        for child in coord.children() {
            let key = (level - 1, child);
            if let Some(node) = self.nodes.remove(&key) {
                self.forget(key);
                if node.draws {
                    self.undraw(level - 1, key, diff);
                }
                if node.state == State::Split {
                    self.remove_under(key, diff);
                }
            }
        }
    }

    /// Looks again at `key`, which is due.
    fn revisit(&mut self, store: &BrickStore, key: Key, diff: &mut Diff) {
        self.revisit_whole_as(store, key, Some(false), diff);
    }

    /// [`Selection::revisit`], where a whole brick is split by the rule as `whole_was` says (see
    /// [`Bands::split`]): `None` for a brick that is looked at as if it were new.
    fn revisit_whole_as(
        &mut self,
        store: &BrickStore,
        key: Key,
        whole_was: Option<bool>,
        diff: &mut Diff,
    ) {
        let Some(node) = self.nodes.get(&key).copied() else {
            return;
        };
        let (level, coord) = key;
        let distance = brick_distance(self.camera, level, coord);
        match node.state {
            State::Whole => {
                if self.drawn_split(key, whole_was) {
                    match decide(store, level, coord, || probe(store, level, coord)) {
                        Plan::Split(children) => {
                            if node.draws {
                                self.undraw(level, key, diff);
                            }
                            self.split(store, key, distance, children, diff);
                            return;
                        }
                        Plan::Wait => self.want(key),
                        Plan::Whole => {}
                    }
                    if node.draws {
                        self.add_stuck(key);
                    }
                    if self.drawn_split(key, Some(false)) {
                        self.unwatch(key);
                        return;
                    }
                }
                self.settle_margin(store, self.placed, key);
                self.queue_node(key, distance, false);
            }
            State::Split => {
                if self.drawn_split(key, Some(true)) {
                    self.queue_node(key, distance, true);
                    return;
                }
                self.remove_under(key, diff);
                let p = probe(store, level, coord);
                if p.present {
                    self.make_whole(store, key, distance, p, diff);
                } else {
                    self.nodes.remove(&key);
                    self.forget(key);
                }
            }
        }
    }

    /// Moves the camera to `camera` and changes what is drawn to suit, looking only at the
    /// bricks that the move can have changed. Returns what changed.
    pub fn update(&mut self, store: &BrickStore, camera: [f64; 3]) -> Diff {
        let moved = (0..3)
            .map(|a| (camera[a] - self.camera[a]).powi(2))
            .sum::<f64>()
            .sqrt();
        self.camera = camera;
        self.travelled += moved;
        // A change of the cap looks at every node, a batch of them in each update (#213).
        if self.place() {
            self.begin_sweep();
            self.absorbing = true;
        }
        self.sweep_some();
        self.tidy_some();
        let mut diff = Diff::default();
        // The nodes that may have become hot are taken out first, and each is either hot now,
        // changes, or is queued again for when it may be: what is queued again is looked at after
        // the camera has moved some more, not in this call.
        let now = (self.travelled / BUCKET) as u64;
        let mut popped = Vec::new();
        while let Some(entry) = self.queue.first_entry() {
            if *entry.key() > now {
                break;
            }
            let due = entry.remove();
            self.queued_entries -= due.len();
            popped.extend(due);
        }
        let (bands, camera) = (self.bands, self.camera);
        let reach = self.placed;
        // Whether a node `distance` from the camera changes: what is drawn, or (for a whole node)
        // whether it is in reach.
        let changes = |key: Key, distance: f64, split: bool, margin: bool| {
            node_changes(bands, reach, key, distance, split, margin)
        };
        let slack = |key: Key, distance: f64, split: bool, margin: bool| {
            node_slack(bands, reach, key, distance, split, margin)
        };
        let mut changing: Vec<(u64, Key)> = Vec::new();
        // The nodes the sweep has just looked at that change: those are spread over the updates.
        let mut owed: Vec<(u64, Key)> = Vec::new();
        let mut lost: Vec<Key> = Vec::new();
        let hot = std::mem::take(&mut self.hot);
        let mut stay = Vec::with_capacity(hot.len());
        let mut cooled = Vec::new();
        for mut entry in hot {
            // An entry for a node that was looked at again since is stale: it has a newer one. A
            // parked node's is kept, for the bands it was made for.
            match self.nodes.get(&entry.key) {
                Some(n) if n.stamp == entry.stamp => {}
                Some(n) if n.stamp == entry.stamp | PARKED => {
                    lost.push(entry.key);
                    continue;
                }
                _ => continue,
            }
            let swept = std::mem::take(&mut entry.swept);
            let distance = brick_distance(camera, entry.key.0, entry.key.1);
            if changes(entry.key, distance, entry.split, entry.margin) {
                if swept {
                    owed.push((entry.stamp, entry.key));
                } else {
                    changing.push((entry.stamp, entry.key));
                }
            } else {
                let slack = slack(entry.key, distance, entry.split, entry.margin);
                if slack > 2.0 * HOT {
                    cooled.push((entry, slack));
                } else {
                    stay.push(entry);
                }
            }
        }
        self.hot = stay;
        for (entry, slack) in cooled {
            self.watch(entry.key, entry.stamp, entry.split, entry.margin, slack);
        }
        let looked = look_at_due(&self.nodes, &popped, camera, &changes, &slack);
        for ((stamp, key), looked) in popped.into_iter().zip(looked) {
            match looked {
                Due::Stale => {}
                Due::Parked => lost.push(key),
                Due::Changes => changing.push((stamp, key)),
                Due::Later {
                    split,
                    margin,
                    slack,
                } => self.watch(key, stamp, split, margin, slack),
            }
        }
        for key in lost {
            if let Some(node) = self.nodes.get_mut(&key) {
                node.stamp = LOST;
            }
        }
        // A node that changes under the cap as it is now, and was queued under the one before, is
        // the flip's, and is spread with what the sweep found; the rest is the camera's.
        let flip_stamp = self.flip_stamp;
        let (flips, changing): (Vec<_>, Vec<_>) = changing
            .into_iter()
            .partition(|&(stamp, _)| stamp <= flip_stamp);
        owed.extend(flips);
        // What changes, but not because of the flip, is revisited now, as it is when
        // there is no flip: the camera's move is not held back by a flip (#213).
        for (stamp, key) in changing {
            if self.nodes.get(&key).is_some_and(|n| n.stamp == stamp) {
                self.revisit(store, key, &mut diff);
            }
        }
        // What the sweep found changing is revisited a few at a time, nearest to the camera as it
        // is now first, and the rest wait in the backlog for the next updates.
        owed.append(&mut self.backlog);
        let mut owed: Vec<(f64, u64, Key)> = owed
            .into_iter()
            .filter(|&(stamp, key)| self.nodes.get(&key).is_some_and(|n| n.stamp == stamp))
            .map(|(stamp, key)| (brick_distance(camera, key.0, key.1), stamp, key))
            .collect();
        #[cfg(test)]
        let most = self.absorb_by;
        #[cfg(not(test))]
        let most = ABSORB_REVISITS;
        let fewest_bricks = most.saturating_mul(ABSORB_BRICKS_PER_REVISIT);
        let nearest =
            |a: &(f64, u64, Key), b: &(f64, u64, Key)| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2));
        let rest = if owed.len() > most {
            owed.select_nth_unstable_by(most, nearest);
            owed.split_off(most)
        } else {
            Vec::new()
        };
        owed.sort_by(nearest);
        let mut waiting: Vec<(u64, Key)> = rest.into_iter().map(|(_, s, k)| (s, k)).collect();
        // The bricks the revisits of the backlog change are what the budget counts: not those that
        // the camera's own changes above did.
        let changed_before = diff.added.len() + diff.removed.len();
        self.marking = true;
        for (_, stamp, key) in owed {
            if diff.added.len() + diff.removed.len() - changed_before >= fewest_bricks {
                waiting.push((stamp, key));
            } else if self.nodes.get(&key).is_some_and(|n| n.stamp == stamp) {
                self.revisit(store, key, &mut diff);
            }
        }
        self.marking = false;
        self.backlog = waiting;
        self.backlog_peak = self.backlog_peak.max(self.backlog.len());
        self.absorbing = !self.sweep.is_empty()
            || !self.backlog.is_empty()
            || self.hot.iter().any(|entry| entry.swept);
        self.rewanted = self.absorbing() || self.tidying();
        diff
    }

    /// Brings the tree up to date after the bricks of level 0 in `changed` were edited, and the
    /// pyramid over them remade: a brick may have become air, or solid with no air beside it, or
    /// the reverse, and so may those beside it. Returns what is drawn now that was not, and the
    /// reverse. The bricks that stay drawn but read differently are the caller's to repack.
    pub fn refresh(&mut self, store: &BrickStore, changed: &[BrickCoord]) -> Diff {
        // The bricks that may be in a different class: those changed and the ones beside them,
        // and the same over them at every level.
        let mut levels: Vec<BTreeSet<BrickCoord>> = vec![BTreeSet::new(); self.top + 1];
        let mut here: BTreeSet<BrickCoord> = changed.iter().copied().collect();
        for set in &mut levels {
            let beside: Vec<_> = here.iter().flat_map(|&c| faces(c)).collect();
            set.extend(here.iter().copied().chain(beside));
            here = set.iter().map(|c| c.parent()).collect();
        }
        let mut diff = Diff::default();
        // Coarsest first, so that a brick that is gone takes what is under it, and one that
        // appears finds its parent known.
        for (level, set) in levels.iter().enumerate().rev() {
            for &coord in set {
                self.recheck(store, (level, coord), &mut diff);
            }
        }
        diff
    }

    fn recheck(&mut self, store: &BrickStore, key: Key, diff: &mut Diff) {
        let (level, coord) = key;
        let p = probe(store, level, coord);
        match self.nodes.get(&key).copied() {
            Some(_) if !p.present => {
                let node = self.nodes.remove(&key).expect("it was just found");
                self.forget(key);
                if node.draws {
                    self.undraw(level, key, diff);
                }
                self.remove_under(key, diff);
            }
            Some(node) if node.state == State::Whole => {
                if node.draws != p.draws {
                    if p.draws {
                        self.drawn[level] += 1;
                        diff.add(key);
                    } else {
                        self.undraw(level, key, diff);
                    }
                    if let Some(node) = self.nodes.get_mut(&key) {
                        node.draws = p.draws;
                    }
                }
                // It may have bricks to be split into now, which it did not have.
                let distance = brick_distance(self.camera, level, coord);
                if self.bands.split(level, distance, Some(false)) {
                    self.revisit(store, key, diff);
                } else if self.nodes.get(&key).is_some_and(|n| !watched(n.stamp)) {
                    // Left unwatched while it could not be split, and outside the band now, so
                    // nothing else will look at it when the camera comes back.
                    self.queue_node(key, distance, false);
                }
            }
            None if p.present => {
                if level == self.top && !self.roots.contains(&coord) {
                    self.roots.push(coord);
                }
                let parent_splits = level == self.top
                    || self
                        .nodes
                        .get(&(level + 1, coord.parent()))
                        .is_some_and(|n| n.state == State::Split);
                if parent_splits {
                    self.build(store, key, p, diff);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn looking_at_more_entries_than_it_shares_among_threads_comes_to_what_one_thread_finds() {
        use super::*;
        // 5 000 nodes, whole and split, some stale, some missing; the camera moves them from
        // changing to not, by distance.
        let mut nodes = HashMap::default();
        let mut due = Vec::new();
        for n in 0..5000i32 {
            let key: Key = (n as usize % 3, BrickCoord([n % 37, n % 11, n / 50]));
            let node = Node {
                state: if n % 4 == 0 {
                    State::Split
                } else {
                    State::Whole
                },
                draws: true,
                margin: n % 7 == 0,
                stamp: u64::from(n as u32 % 5 + 1),
            };
            nodes.insert(key, node);
            // A stamp that is the node's, or another (stale), and a key that is gone, in turn.
            match n % 9 {
                0 => due.push((node.stamp + 1, key)),
                1 => due.push((node.stamp, (7, key.1))),
                _ => due.push((node.stamp, key)),
            }
        }
        assert!(due.len() > PARALLEL_FROM);
        let camera = [100.0, 40.0, 3.0];
        let changes = |key: Key, d: f64, split: bool, margin: bool| {
            (d + key.0 as f64 + f64::from(u8::from(split)) * 3.0 + f64::from(u8::from(margin)))
                % 5.0
                < 1.0
        };
        let slack = |_: Key, d: f64, split: bool, _: bool| d * 0.25 + f64::from(u8::from(split));
        let one = look_at_due_by(&nodes, &due, camera, &changes, &slack, 1);
        assert!(one.iter().any(|d| matches!(d, Due::Stale)));
        assert!(one.iter().any(|d| matches!(d, Due::Changes)));
        assert!(one.iter().any(|d| matches!(d, Due::Later { .. })));
        for threads in [2, 3, 4, 8] {
            assert_eq!(
                look_at_due_by(&nodes, &due, camera, &changes, &slack, threads),
                one
            );
        }
        assert_eq!(look_at_due(&nodes, &due, camera, &changes, &slack), one);
    }

    #[test]
    fn what_threads_look_at_comes_back_in_the_order_of_the_entries_whatever_their_number() {
        let items: Vec<u64> = (0..1000).collect();
        let one = super::map_shared(&items, 1, &|n| n * n + 1);
        for threads in [2, 3, 4, 7, 64, 5000] {
            assert_eq!(super::map_shared(&items, threads, &|n| n * n + 1), one);
        }
        assert!(super::map_shared(&[] as &[u64], 4, &|n| *n).is_empty());
    }

    use brick_store::{Interior, Material};

    use super::*;

    const ROCK: Material = Material(1);
    const SOIL: Material = Material(2);

    /// A flat world: a floor of SOIL bricks one brick thick over `-n..n` bricks in x and z, on
    /// rock three bricks deep, with a pyramid of `levels` levels above level 0. Its surface is
    /// at y = 0.
    fn flat(n: i32, levels: usize) -> BrickStore {
        let mut store = BrickStore::new();
        for x in -n..n {
            for z in -n..n {
                store.insert(BrickCoord([x, -1, z]), Brick::solid(SOIL));
                for y in -4..-1 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(ROCK));
                }
            }
        }
        store.build_lod(levels);
        store
    }

    const D0: f64 = 32.0;

    /// `reserve` is room for that many in all, not that many more (#167): reserving what the
    /// table's room already is, on a selection that holds nodes, makes no new room (`HashMap::reserve`
    /// would have asked for the nodes it holds and that many more, and doubled the table).
    #[test]
    fn reserving_room_on_a_selection_that_holds_nodes_is_for_that_many_in_all() {
        let store = flat(8, 4);
        let mut selection = select(&store, [0.0, 4.0, 0.0], D0);
        let held = selection.nodes.len();
        assert!(held > 100, "{held} nodes");
        selection.reserve(held * 3, held, held);
        let room = selection.nodes.capacity();
        assert!(room >= held * 3);
        // The same reservation again, and the room the table has: no growth.
        selection.reserve(held * 3, held, held);
        assert_eq!(selection.nodes.capacity(), room);
        selection.reserve(room, 0, 0);
        assert_eq!(selection.nodes.capacity(), room);
        // More than it has is more room.
        selection.reserve(room + 1, 0, 0);
        assert!(selection.nodes.capacity() > room);
    }

    fn select(store: &BrickStore, camera: [f64; 3], d0: f64) -> Selection {
        let top = top_level(store);
        let roots = top_bricks(store, top, None);
        Selection::new(store, camera, Bands::with_d0(d0), top, roots).0
    }

    #[test]
    fn a_selection_planned_in_chunks_is_the_one_planned_in_one() {
        let store = flat(64, 5);
        let top = top_level(&store);
        let roots: Vec<_> = top_bricks(&store, top, None);
        let made = |chunk: usize| {
            let (selection, diff) = Selection::new_reached_in_chunks(
                &store,
                [0.5, 20.0, 0.5],
                Bands::with_d0(D0),
                top,
                roots.clone(),
                None,
                Reach::NONE,
                chunk,
            );
            let mut bricks: Vec<_> = selection.bricks().collect();
            bricks.sort_unstable();
            (
                bricks,
                selection.nodes(),
                selection.queued(),
                format!("{diff:?}"),
            )
        };
        let whole = made(usize::MAX);
        assert!(whole.1 > 1000, "{}", whole.1);
        for chunk in [1, 7, 100] {
            assert_eq!(made(chunk), whole, "chunks of {chunk}");
        }
    }

    /// Levels of the drawn bricks that contain the Voxel (x, -1, z): the top of the floor.
    fn covering(selection: &Selection, x: i32, z: i32) -> Vec<usize> {
        (0..=selection.top())
            .filter(|&level| {
                let size = 8i32 << level;
                let coord = BrickCoord([
                    x.div_euclid(size),
                    (-1i32).div_euclid(size),
                    z.div_euclid(size),
                ]);
                selection.draws(level, coord)
            })
            .collect()
    }

    #[test]
    fn near_the_camera_it_is_level_0_and_each_doubling_further_is_a_level() {
        let store = flat(64, 4);
        // Level 1 from 64 Voxels, level 2 from 128, level 3 from 256, level 4 from 512.
        let selection = select(&store, [0.5, 20.0, 0.5], D0);
        let at = |x| covering(&selection, x, 3);
        assert_eq!(at(3), [0]);
        assert_eq!(at(30), [0]);
        assert_eq!(at(85), [1]);
        assert_eq!(at(170), [2]);
        assert_eq!(at(200), [2]);
        assert_eq!(at(330), [3]);
        assert_eq!(at(500), [3]);
        assert_eq!(at(-505), [3]);
    }

    #[test]
    fn the_bricks_drawn_cover_the_surface_exactly_once_wherever_the_camera_is() {
        // The bands partition space: every column of the surface is under one drawn brick, and
        // no more.
        let store = flat(64, 4);
        for camera in [
            [0.5, 20.0, 0.5],
            [300.0, 5.0, -200.0],
            [-100.5, 400.0, 10.0],
            [127.9, 3.0, 128.1],
            [0.5, 3000.0, 0.5],
        ] {
            let selection = select(&store, camera, D0);
            for x in (-500..500).step_by(7) {
                for z in (-500..500).step_by(11) {
                    assert_eq!(covering(&selection, x, z).len(), 1, "{camera:?} {x} {z}");
                }
            }
        }
    }

    #[test]
    fn a_brick_is_in_the_level_its_distance_says_or_a_finer_one() {
        let store = flat(64, 4);
        let bands = Bands::with_d0(D0);
        for camera in [[0.5, 20.0, 0.5], [-200.0, 50.0, 300.0], [0.5, 450.0, 0.5]] {
            let selection = select(&store, camera, D0);
            let mut finer = 0;
            for (level, coord) in selection.bricks() {
                let d = brick_distance(camera, level, coord);
                // Never nearer than its level is used from...
                assert!(
                    level == 0 || d >= bands.floor(level),
                    "{level} {coord:?} {d}"
                );
                // ...and not beyond where the next level takes over, except that the parent
                // of a brick in the sliver beside the boundary is nearer than it, and it is
                // drawn finer than the rule says: by the diagonal of the parent at most.
                if level < selection.top() {
                    let beyond = bands.floor(level + 1);
                    let sliver = 3f64.sqrt() * (8u64 << (level + 1)) as f64;
                    assert!(d < beyond + sliver, "{level} {coord:?} {d}");
                    finer += usize::from(d >= beyond);
                }
            }
            // Those are a minority: the sliver beside each boundary.
            assert!(finer * 3 < selection.bricks().count(), "{finer}");
        }
    }

    #[test]
    fn bricks_the_store_does_not_hold_are_left_out_when_nothing_can_see_them() {
        // The core of a ball that no brick of is held: its skin is drawn and its volume is not.
        let mut store = BrickStore::new();
        store.set_source(Interior {
            radius: 400,
            material: ROCK,
        });
        store.build_lod(3);
        let top = top_level(&store);
        let roots = top_bricks(&store, top, Some(([0; 3], 450)));
        let (selection, _) =
            Selection::new(&store, [0.0, 700.0, 0.0], Bands::with_d0(64.0), top, roots);
        // 400 Voxels is 50 level-0 bricks: the ball is 65 thousand bricks, and its skin is a
        // few thousand at most, in the coarser levels where it is far.
        let drawn = (0..4).map(|l| selection.count(l)).sum::<usize>();
        assert!(drawn > 50 && drawn < 6000, "{drawn}");
        for (level, c) in selection.bricks() {
            let size = (8 << level) as f64;
            let middle = c.0.map(|v| f64::from(v) * size + size / 2.0);
            let radius = middle.iter().map(|v| v * v).sum::<f64>().sqrt();
            // Within a brick of the ball's skin.
            assert!(radius > 400.0 - 2.0 * size, "{level} {c:?} {radius}");
        }
    }

    fn first_batch(cells: &[BrickCoord]) -> &[BrickCoord] {
        &cells[..cells.len() / 3]
    }

    fn second_batch(cells: &[BrickCoord]) -> &[BrickCoord] {
        &cells[cells.len() / 3..]
    }

    #[test]
    fn cells_that_come_late_are_taken_in_without_a_rebuild_and_leave_what_a_selection_of_all_of_them_is()
     {
        let (mut store, top, roots) = ball();
        let (camera, bands) = ([0.0, 420.0, 0.0], Bands::with_d0(256.0));
        let (whole, _) = Selection::new(&store, camera, bands, top, roots.clone());
        assert!(roots.len() > 40, "{}", roots.len());
        // The cells with x over 0 are not held yet: not roots, and not drawn.
        let later: Vec<BrickCoord> = roots.iter().copied().filter(|c| c.0[0] > 0).collect();
        assert!(later.len() > 10 && later.len() < roots.len() - 10);
        store.defer_guard(later.iter().copied());
        let now = top_bricks(&store, top, Some(([0; 3], 450)));
        assert!(now.iter().all(|c| c.0[0] <= 0) && now.len() + later.len() == roots.len());
        let (mut partial, _) = Selection::new(&store, camera, bands, top, now);
        let drawn_in = |selection: &Selection, x: std::ops::RangeInclusive<i32>| {
            selection
                .bricks()
                .filter(|&(level, coord)| x.contains(&(coord.0[0] >> (top - level))))
                .count()
        };
        assert_eq!(
            drawn_in(&partial, 1..=i32::MAX),
            0,
            "nothing under a late cell"
        );
        assert!(drawn_in(&partial, i32::MIN..=0) > 100);
        assert!(partial.nodes() < whole.nodes());
        is_a_cut(&partial);
        // They come in two batches, and a cell that is told of again is not made twice.
        for batch in [first_batch(&later), second_batch(&later)] {
            for &cell in batch {
                store.hold_guard_cell(cell, []);
            }
            assert!(!partial.add_roots(&store, batch.iter().copied()).is_empty());
            is_a_cut(&partial);
        }
        let again = partial.add_roots(&store, first_batch(&later).iter().copied());
        assert!(again.is_empty(), "told twice");
        assert_eq!(store.guard_pending(), 0);
        assert_eq!(
            partial.bricks().collect::<BTreeSet<_>>(),
            whole.bricks().collect::<BTreeSet<_>>()
        );
        assert_eq!(partial.nodes(), whole.nodes());
        assert_eq!(partial.roots.len(), whole.roots.len());
        // What it follows the camera as is what a whole one does.
        let mut elsewhere = whole.clone();
        let there = [300.0, -100.0, 250.0];
        partial.update(&store, there);
        elsewhere.update(&store, there);
        assert_eq!(
            partial.bricks().collect::<BTreeSet<_>>(),
            elsewhere.bricks().collect::<BTreeSet<_>>()
        );
    }

    /// A ball of rock of 400 Voxels, whose skin is what is seen (like the Planet, with the sea at
    /// its surface), with a pyramid of four levels, and its roots.
    fn ball() -> (BrickStore, usize, Vec<BrickCoord>) {
        let mut store = BrickStore::new();
        store.set_source(Interior {
            radius: 400,
            material: ROCK,
        });
        store.build_lod(3);
        let top = top_level(&store);
        let roots = top_bricks(&store, top, Some(([0; 3], 450)));
        (store, top, roots)
    }

    const SEA: f64 = 400.0;

    fn capped_ball(camera: [f64; 3], capped: bool) -> (BrickStore, Selection) {
        let (store, top, roots) = ball();
        let horizon = capped.then(|| Horizon::new(SEA));
        let (selection, _) =
            Selection::new_capped(&store, camera, Bands::with_d0(256.0), top, roots, horizon);
        (store, selection)
    }

    /// Nobody is drawn under a brick that is drawn: the bricks drawn are a cut through the tree.
    fn is_a_cut(selection: &Selection) {
        let drawn: BTreeSet<Key> = selection.bricks().collect();
        for &(level, coord) in &drawn {
            let mut c = coord;
            for l in level + 1..=selection.top() {
                c = c.parent();
                assert!(
                    !drawn.contains(&(l, c)),
                    "{level} {coord:?} under {l} {c:?}"
                );
            }
        }
    }

    /// The running count of the queue's entries is the entries (#168: `queued` is not summed).
    fn queue_is_counted(selection: &Selection) {
        assert_eq!(
            selection.queued_entries,
            selection.queue.values().map(Vec::len).sum::<usize>()
        );
    }

    /// What the selection keeps true of every node: a split node is split by the rule and the cap,
    /// and a whole one that they split is unwatched (it is stuck or waits).
    fn is_as_the_rule_says(selection: &Selection, at: &str) {
        for (&key, node) in &selection.nodes {
            match node.state {
                State::Split => assert!(selection.drawn_split(key, Some(true)), "{at}: {key:?}"),
                State::Whole => assert!(
                    node.stamp == 0 || !selection.drawn_split(key, Some(false)),
                    "{at}: {key:?} is whole and split by the rule"
                ),
            }
        }
    }

    #[test]
    fn the_horizon_cap_leaves_what_is_past_the_horizon_coarse_and_draws_far_fewer_bricks() {
        let camera = [0.0, SEA + 8.0, 0.0];
        let (_, free) = capped_ball(camera, false);
        let (_, capped) = capped_ball(camera, true);
        assert!(capped.reach().horizon.is_some());
        is_a_cut(&capped);
        let (free_count, capped_count) = (free.bricks().count(), capped.bricks().count());
        assert!(
            capped_count * 3 < free_count,
            "{capped_count} drawn capped, {free_count} not"
        );
        // Each brick that was split into finer ones is one that the cap leaves out; the finer
        // bricks that are drawn are all in the cap.
        let at = capped.reach().from(camera);
        for (level, coord) in capped.bricks() {
            if level < capped.top() {
                let distance = brick_distance(camera, level, coord);
                let parent_in = at.in_cap(level + 1, coord.parent(), true);
                assert!(
                    parent_in,
                    "{level} {coord:?} {distance} under a parent out of the cap"
                );
            }
        }
        // The nearest of it is what it was.
        for key @ (level, coord) in free.bricks() {
            if brick_distance(camera, level, coord) < 60.0 {
                assert!(capped.draws(key.0, key.1), "{key:?}");
            }
        }
    }

    #[test]
    fn the_horizon_cap_is_off_for_a_camera_under_the_sea_and_back_on_well_over_it() {
        // A margin of 100 Voxels: the raised camera is 100 over the camera.
        let margins = Margins {
            radial: 1.25,
            vertical: 100.0,
        };
        let (store, mut selection) = capped_ball([0.0, 150.0, 0.0], true);
        selection.set_margins(&store, margins);
        assert!(selection.reach().horizon.is_none());
        assert_eq!(selection.horizon(), Some(Horizon::new(SEA)));
        // A camera is under the sphere when its raised height is: no hysteresis (a scene made
        // again over it has the cap on), though the cap here is off until 50 m over it.
        assert!(selection.camera_under_horizon([0.0, SEA - 101.0, 0.0]));
        assert!(!selection.camera_under_horizon([0.0, SEA - 99.0, 0.0]));
        assert!(!selection.camera_under_horizon([0.0, SEA + 500.0, 0.0]));
        assert!(selection.reach().horizon.is_none());
        let (_, free) = capped_ball([0.0, 150.0, 0.0], false);
        assert_eq!(
            selection.bricks().collect::<BTreeSet<_>>(),
            free.bricks().collect::<BTreeSet<_>>()
        );
        // Rising, it comes on only when the raised camera is 50 m (1000 Voxels) over the sea, not
        // as soon as it is over it (from 300).
        let mut on_at = None;
        for step in 0..180 {
            let y = 150.0 + f64::from(step) * 10.0;
            selection.update(&store, [0.0, y, 0.0]);
            if on_at.is_none() && selection.reach().horizon.is_some() {
                on_at = Some(y);
            }
        }
        let on_at = on_at.expect("it came on");
        assert!((1300.0..1320.0).contains(&on_at), "{on_at}");
        is_a_cut(&selection);
        // Coming down it stays on until the raised camera is under the sea.
        let mut off_at = None;
        for step in 0..180 {
            let y = 1900.0 - f64::from(step) * 10.0;
            selection.update(&store, [0.0, y, 0.0]);
            if off_at.is_none() && selection.reach().horizon.is_none() {
                off_at = Some(y);
            }
        }
        assert!(
            (290.0..310.0).contains(&off_at.expect("it went off")),
            "{off_at:?}"
        );
        is_a_cut(&selection);
    }

    /// The ball with the margins, the camera climbing from under the sea (the cap is off) until
    /// the cap comes on, and then standing; or, from over the sea, going down until it goes off.
    /// The nodes are looked at again `sweep` at a time and `absorb` of those that change are
    /// revisited in each update. Returns the updates it took to absorb the flip, with the cut
    /// checked at each, and what was drawn at the end.
    fn flip_in_updates(sweep: usize, absorb: usize, climbing: bool) -> (usize, BTreeSet<Key>) {
        let margins = Margins {
            radial: 1.25,
            vertical: 100.0,
        };
        let from = if climbing { 150.0 } else { 1900.0 };
        let (store, mut selection) = capped_ball([0.0, from, 0.0], true);
        selection.set_margins(&store, margins);
        selection.sweep_by = sweep;
        selection.absorb_by = absorb;
        assert_eq!(selection.reach().horizon.is_none(), climbing);
        let mut y = from;
        while selection.reach().horizon.is_none() == climbing {
            y += if climbing { 10.0 } else { -10.0 };
            selection.update(&store, [0.0, y, 0.0]);
            is_a_cut(&selection);
            assert!(y > 0.0 && y < 2500.0, "the cap never flipped");
        }
        // The update that found the flip looked at a batch, and not at every node.
        let nodes = selection.nodes();
        let mut updates = 1;
        while selection.rewanted() {
            selection.update(&store, [0.0, y, 0.0]);
            is_a_cut(&selection);
            updates += 1;
            assert!(updates <= nodes, "the sweep does not end");
        }
        is_as_the_rule_says(&selection, "after the sweep");
        (updates, selection.bricks().collect())
    }

    #[test]
    fn a_flip_of_the_cap_looks_at_a_batch_of_nodes_in_each_update_and_ends_where_one_look_does() {
        for climbing in [true, false] {
            let (at_once, drawn) = flip_in_updates(usize::MAX, usize::MAX, climbing);
            // One update looks at every node; the children of the nodes it splits are looked at
            // by the next (they are the sweep's too), and no more.
            assert!(at_once <= 2, "{at_once} updates (climbing: {climbing})");
            let (spread, spread_drawn) = flip_in_updates(50, 20, climbing);
            // The tree has some thousands of nodes: a batch of 50 takes many updates.
            assert!(spread > 20, "{spread} updates (climbing: {climbing})");
            assert!(
                spread_drawn == drawn,
                "the sweep ended in another cut (climbing: {climbing}): {} against {}",
                spread_drawn.len(),
                drawn.len()
            );
        }
    }

    /// The camera at `y` over the ball's centre, up or down in steps of 10 until the cap flips,
    /// then standing until the flip is absorbed (the queue tidied too).
    fn flip_and_settle(store: &BrickStore, selection: &mut Selection, y: &mut f64, climbing: bool) {
        while selection.reach().horizon.is_none() == climbing {
            *y += if climbing { 10.0 } else { -10.0 };
            selection.update(store, [0.0, *y, 0.0]);
            assert!(*y > 0.0 && *y < 2500.0, "the cap never flipped");
        }
        let mut updates = 0;
        while selection.absorbing() || selection.tidying() {
            selection.update(store, [0.0, *y, 0.0]);
            updates += 1;
            assert!(updates < 100_000, "the flip is never absorbed");
        }
    }

    #[test]
    fn flips_of_the_cap_do_not_leave_stale_entries_piling_up_in_the_queue() {
        // Each flip stamps every node again, and the entries that the nodes had are stale: they
        // were gone with `reconsider_all` before the sweep, and are tidied after it.
        let (store, mut selection) = capped_ball([0.0, 150.0, 0.0], true);
        selection.set_margins(
            &store,
            Margins {
                radial: 1.25,
                vertical: 100.0,
            },
        );
        selection.sweep_by = 500;
        selection.absorb_by = 200;
        selection.tidy_by = 300;
        let mut y = 150.0;
        let mut queued = Vec::new();
        for _ in 0..4 {
            flip_and_settle(&store, &mut selection, &mut y, true);
            y += 10.0;
            selection.update(&store, [0.0, y, 0.0]);
            flip_and_settle(&store, &mut selection, &mut y, false);
            queued.push(selection.queued());
            is_a_cut(&selection);
            is_as_the_rule_says(&selection, "after a round trip");
            queue_is_counted(&selection);
        }
        let nodes = selection.nodes();
        // On the main branch a round trip leaves 1.1 entries for each node and stays there.
        for q in &queued {
            assert!(*q < 2 * nodes, "{queued:?} entries for {nodes} nodes");
        }
        assert!(
            queued[3] <= queued[0] + queued[0] / 10,
            "the entries pile up: {queued:?}"
        );
    }

    #[test]
    fn flips_that_come_before_the_sweep_of_the_last_has_ended_leave_no_stale_entries_either() {
        // Each flip restarts the sweep, which stamps the nodes it has got to again: a flip that
        // comes soon leaves the entries of those stamps, and the tidying has to go on through it
        // (without that the queue grew by about a quarter of its size in each round trip here).
        let (store, mut selection) = capped_ball([0.0, 150.0, 0.0], true);
        selection.set_margins(
            &store,
            Margins {
                radial: 1.25,
                vertical: 100.0,
            },
        );
        selection.sweep_by = 300;
        selection.absorb_by = 50;
        selection.tidy_by = 2000;
        let mut y = 150.0;
        let mut climbing = true;
        let mut quick = 0;
        let mut queued = Vec::new();
        for _ in 0..12 {
            // Big steps: the flip comes in a few updates, and the camera goes back at once.
            while selection.reach().horizon.is_none() == climbing {
                y += if climbing { 60.0 } else { -60.0 };
                selection.update(&store, [0.0, y, 0.0]);
                assert!(y > 0.0 && y < 2500.0, "the cap never flipped");
            }
            selection.update(&store, [0.0, y, 0.0]);
            quick += usize::from(!selection.sweep.is_empty());
            if climbing {
                queued.push(selection.queued());
            }
            climbing = !climbing;
        }
        assert!(
            quick >= 10,
            "{quick} of the flips came before the sweep ended"
        );
        // The same point of each round trip: the queue does not grow.
        assert!(
            queued[5] <= queued[1] + queued[1] / 4,
            "the entries pile up in the flips that cut the sweep short: {queued:?}"
        );
        let mut updates = 0;
        while selection.absorbing() || selection.tidying() {
            selection.update(&store, [0.0, y, 0.0]);
            updates += 1;
            assert!(updates < 100_000, "the flips are never absorbed");
        }
        is_a_cut(&selection);
        is_as_the_rule_says(&selection, "after the quick flips");
        let nodes = selection.nodes();
        // On main a flip leaves 1.1 entries for each node (see the test above).
        assert!(
            selection.queued() < 2 * nodes,
            "{} entries for {nodes} nodes",
            selection.queued()
        );
    }

    #[test]
    fn a_camera_that_moves_while_a_flip_is_revisited_is_not_held_back_and_the_flip_is_absorbed() {
        let (store, mut selection) = capped_ball([0.0, 150.0, 0.0], true);
        selection.set_margins(
            &store,
            Margins {
                radial: 1.25,
                vertical: 100.0,
            },
        );
        selection.sweep_by = 300;
        selection.absorb_by = 2;
        let mut y = 150.0;
        while selection.reach().horizon.is_none() {
            y += 10.0;
            selection.update(&store, [0.0, y, 0.0]);
        }
        // Flying on, sideways and up, while the nodes the sweep found are revisited: the backlog
        // does not grow once the sweep is done (what the camera's move makes change is revisited
        // when it does, and is not put in it), and every update is a cut.
        let mut last = None;
        let mut flown = 0;
        while selection.absorbing() {
            y += 2.0;
            let x = f64::from(flown) * 3.0;
            selection.update(&store, [x, y, 0.0]);
            is_a_cut(&selection);
            if selection.sweep.is_empty() {
                if let Some(before) = last {
                    assert!(
                        selection.backlog.len() <= before,
                        "the backlog grew from {before} to {}",
                        selection.backlog.len()
                    );
                }
                last = Some(selection.backlog.len());
            }
            flown += 1;
            assert!(flown < 20_000, "the flip is never absorbed while flying");
        }
        let peak = selection.take_backlog_peak();
        assert!(
            peak > 5,
            "the budget held some back: {peak} at most, {flown} updates"
        );
        is_as_the_rule_says(&selection, "after the flight");
    }

    #[test]
    fn climbing_over_the_horizon_keeps_every_node_as_the_rule_says_and_the_cut_whole() {
        let (store, mut selection) = capped_ball([0.0, SEA + 8.0, 0.0], true);
        selection.set_margins(&store, Margins::standard());
        let mut camera = [0.0, SEA + 8.0, 0.0];
        // Up in small steps (the horizon moves a hundred times as fast as the camera), then
        // over, then down again at a slant.
        for step in 0..1500 {
            let climb = if step < 400 { 0.5 } else { 3.0 };
            camera[1] += climb;
            selection.update(&store, camera);
            if step % 25 == 0 {
                is_as_the_rule_says(&selection, &format!("up {step} {camera:?}"));
            }
        }
        is_a_cut(&selection);
        for step in 0..1400 {
            camera[1] -= 3.0;
            camera[0] += 2.0;
            selection.update(&store, camera);
            if step % 25 == 0 {
                is_as_the_rule_says(&selection, &format!("down {step} {camera:?}"));
            }
        }
        is_a_cut(&selection);
    }

    #[test]
    fn following_a_camera_under_the_cap_draws_what_a_fresh_selection_does_up_to_the_hysteresis() {
        let (store, mut followed) = capped_ball([0.0, SEA + 8.0, 0.0], true);
        let mut camera = [0.0, SEA + 8.0, 0.0];
        for step in 0..600 {
            camera = [
                30.0 * (f64::from(step) * 0.05).sin(),
                SEA + 8.0 + f64::from(step) * 0.7,
                20.0 * (f64::from(step) * 0.07).cos(),
            ];
            followed.update(&store, camera);
        }
        let (_, fresh) = capped_ball(camera, true);
        let at = followed.reach().from(camera);
        let bands = Bands::with_d0(256.0);
        let a: BTreeSet<Key> = followed.bricks().collect();
        let b: BTreeSet<Key> = fresh.bricks().collect();
        for &(level, coord) in a.symmetric_difference(&b) {
            // The brick, or one of those over it, is within a hysteresis of its band or of the
            // edge of the cap.
            let mut near = false;
            let mut c = coord;
            for l in level..=followed.top() {
                let d = brick_distance(camera, l, c);
                near |= (bands.floor(l) > 0.0
                    && (d - bands.floor(l)).abs() <= 0.05 * bands.floor(l) + 1e-6)
                    || at.in_cap(l, c, false) != at.in_cap(l, c, true);
                c = c.parent();
            }
            assert!(near, "{level} {coord:?}");
        }
        assert!(
            a.len().abs_diff(b.len()) * 5 < b.len(),
            "{} {}",
            a.len(),
            b.len()
        );
    }

    #[test]
    fn a_ridge_above_the_sea_is_drawn_from_beyond_the_horizon_of_the_sea() {
        // The brick whose top is high is in the cap from further than the sea's horizon.
        let reach = Reach {
            horizon: Some(Horizon::new(SEA)),
            ..Reach::NONE
        };
        let camera = [0.0, SEA + 8.0, 0.0];
        let at = reach.from(camera);
        // On the surface, 150 Voxels along it from the camera: a level 0 brick is outside the
        // cap (the sea's horizon from 8 up is 80, and the brick's top is 80 more).
        let along = |x: f64, y: f64| {
            let angle = x / y;
            [
                (y * angle.sin() / 8.0).floor() as i32,
                (y * angle.cos() / 8.0).floor() as i32,
                0,
            ]
        };
        let flat_ground = BrickCoord(along(200.0, SEA));
        let on_a_ridge = BrickCoord(along(200.0, SEA + 120.0));
        assert!(!at.in_cap(0, flat_ground, false));
        assert!(
            at.in_cap(0, on_a_ridge, false),
            "a peak 120 up is seen from further"
        );
    }

    #[test]
    fn held_bricks_that_are_buried_are_not_drawn_either() {
        // The floor is a brick of soil on three of rock: of the four layers, the rock under the
        // soil, and not at the edge of the floor, has solid bricks on all six faces... except
        // that nothing is below the lowest, so it has air under it. Only the middle is buried.
        let store = flat(4, 1);
        let selection = select(&store, [0.5, 20.0, 0.5], 1.0e6);
        let layer = |y: i32| selection.bricks().filter(|&(_, c)| c.0[1] == y).count();
        assert_eq!(layer(-1), 8 * 8, "the soil, under the sky");
        assert_eq!(layer(-4), 8 * 8, "the lowest rock, over the air");
        // The two between: the ones at the edge of the floor are drawn, the rest are not.
        assert_eq!(layer(-2), 8 * 8 - 6 * 6);
        assert_eq!(layer(-3), 8 * 8 - 6 * 6);
    }

    #[test]
    fn following_the_camera_draws_what_a_fresh_selection_does_up_to_the_hysteresis() {
        let store = flat(64, 4);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        let mut drawn: BTreeSet<Key> = selection.bricks().collect();
        let mut camera = [0.5, 20.0, 0.5];
        // A walk out, back, round, and up, and jumps.
        let steps: Vec<[f64; 3]> = [([9.0, 0.0, 3.0], 40), ([-13.0, 1.0, -9.0], 60)]
            .into_iter()
            .chain([([0.0, 7.0, 15.0], 30), ([100.0, -3.0, -70.0], 6)])
            .flat_map(|(step, n)| std::iter::repeat_n(step, n))
            .collect();
        for step in steps {
            for a in 0..3 {
                camera[a] += step[a];
            }
            let diff = selection.update(&store, camera);
            // The diff is exactly the change.
            for key in &diff.removed {
                assert!(drawn.remove(key), "removed {key:?} which was not drawn");
            }
            for key in &diff.added {
                assert!(drawn.insert(*key), "added {key:?} which was drawn");
            }
            assert_eq!(drawn, selection.bricks().collect::<BTreeSet<_>>());
            // It is a partition at every step.
            for x in (-500..500).step_by(31) {
                for z in (-500..500).step_by(37) {
                    assert_eq!(covering(&selection, x, z).len(), 1, "{camera:?} {x} {z}");
                }
            }
        }
        // Where it stopped it follows the rule as a fresh selection does, except for bricks
        // within the hysteresis of a boundary, where it stays as it was.
        let bands = Bands::with_d0(D0);
        let fresh: BTreeSet<Key> = select(&store, camera, D0).bricks().collect();
        let mut differ = 0;
        for &(level, coord) in drawn.symmetric_difference(&fresh) {
            differ += 1;
            // The brick, or one of those over it, is within the margin of its floor.
            let mut near = false;
            let mut c = coord;
            for l in level..=selection.top() {
                let d = brick_distance(camera, l, c);
                let floor = bands.floor(l);
                near |= floor > 0.0 && (d - floor).abs() <= 0.05 * floor + 1e-6;
                c = c.parent();
            }
            assert!(near, "{level} {coord:?}");
        }
        // Mostly the same.
        assert!(differ * 4 < drawn.len(), "{differ} of {}", drawn.len());
    }

    /// The bands at the steady `depth` of a camera that is flying, for the tests of #168.
    fn at_speed(depth: f64) -> Bands {
        Bands::with_d0(D0).speed_scaled(depth)
    }

    /// Takes `selection` to `bands` as the scene thread does, a few nodes at a time, and gives
    /// what each look changed.
    fn reband(selection: &mut Selection, store: &BrickStore, bands: Bands) -> Vec<Diff> {
        selection.set_bands(bands);
        let mut diffs = Vec::new();
        while selection.relooking() > 0 {
            diffs.push(selection.relook_some(store, 300, 200));
        }
        diffs
    }

    #[test]
    fn rebanding_in_place_draws_what_a_fresh_selection_for_the_new_bands_does_up_to_the_hysteresis()
    {
        let store = flat(64, 4);
        let camera = [0.5, 20.0, 0.5];
        for depth in [0.5, 1.0, 1.5, 0.0] {
            let mut selection = select(&store, camera, D0);
            let mut drawn: BTreeSet<Key> = selection.bricks().collect();
            // Coarser, and then finer again, from where it is.
            for bands in [at_speed(depth), Bands::with_d0(D0)] {
                for diff in reband(&mut selection, &store, bands) {
                    for key in &diff.removed {
                        assert!(drawn.remove(key), "removed {key:?} which was not drawn");
                    }
                    for key in &diff.added {
                        assert!(drawn.insert(*key), "added {key:?} which was drawn");
                    }
                }
                assert_eq!(selection.bands(), bands);
                assert_eq!(selection.relooking(), 0);
                assert_eq!(drawn, selection.bricks().collect::<BTreeSet<_>>());
                for x in (-500..500).step_by(31) {
                    for z in (-500..500).step_by(37) {
                        assert_eq!(covering(&selection, x, z).len(), 1, "{depth} {x} {z}");
                    }
                }
                let fresh: BTreeSet<Key> = Selection::new(
                    &store,
                    camera,
                    bands,
                    selection.top(),
                    top_bricks(&store, selection.top(), None),
                )
                .0
                .bricks()
                .collect();
                let differ = drawn.symmetric_difference(&fresh).count();
                // The 5% either side of every boundary is a large part of a small scene.
                assert!(
                    differ * 2 < drawn.len(),
                    "{depth}: {differ} of {}",
                    drawn.len()
                );
            }
        }
        // The same bands again change nothing.
        let mut selection = select(&store, camera, D0);
        assert!(reband(&mut selection, &store, Bands::with_d0(D0)).is_empty());
    }

    /// A change to coarser bands is not taken for the camera's move (#168): the camera goes on
    /// at a flight's speed with nothing looked at again, and no update changes more than a few
    /// nodes, though the bands ask for most of the tree to merge. (Parking only the nodes on the
    /// hot list left the rest to come due by the camera's travel, and nearly all did in a round or
    /// two: 900 of 1 180 nodes in one update here, 3.3 million bricks in a follow step at full
    /// size.) What is looked at is the relook's, a few at a time.
    #[test]
    fn a_coarser_change_is_not_taken_for_the_cameras_move_by_an_update_at_a_flights_speed() {
        let store = flat(64, 4);
        let camera = [0.5, 20.0, 0.5];
        let mut selection = select(&store, camera, D0);
        let nodes = selection.nodes();
        selection.set_bands(at_speed(1.5));
        assert!(selection.relooking() > 50);
        let mut at = camera;
        let mut most = 0;
        for _ in 0..100 {
            at[0] += 4.0;
            let diff = selection.update(&store, at);
            most = most.max(diff.added.len() + diff.removed.len());
        }
        eprintln!("the largest update changed {most} bricks of {nodes} nodes");
        assert!(most <= 64, "an update changed {most} bricks");
    }

    #[test]
    fn a_coarser_depth_draws_fewer_bricks_and_each_is_no_finer_than_the_slow_camera_draws() {
        let store = flat(64, 4);
        let camera = [0.5, 20.0, 0.5];
        let slow = select(&store, camera, D0);
        let mut counts = Vec::new();
        for depth in [0.0, 0.5, 1.0, 1.5] {
            let mut fast = select(&store, camera, D0);
            reband(&mut fast, &store, at_speed(depth));
            counts.push(fast.bricks().count());
            for (level, coord) in fast.bricks() {
                // Whatever the fast camera draws, the slow one draws at that level or finer.
                let under = (0..=level).any(|l| {
                    let mut c = coord;
                    for _ in l..level {
                        c = c.children()[0];
                    }
                    slow.draws(l, c) || slow.splits(l, c)
                });
                assert!(under || level == slow.top(), "{level} {coord:?}");
            }
        }
        assert!(counts.windows(2).all(|w| w[1] <= w[0]), "{counts:?}");
        assert!(counts[3] < counts[0], "{counts:?}");
    }

    #[test]
    fn a_change_of_bands_leaves_each_node_on_the_hot_list_once_and_a_second_change_mid_way_loses_none()
     {
        let store = flat(64, 4);
        let camera = [0.5, 20.0, 0.5];
        let mut selection = select(&store, camera, D0);
        for bands in [at_speed(1.0), at_speed(1.5), Bands::with_d0(D0)] {
            reband(&mut selection, &store, bands);
            selection.update(&store, [camera[0] + 1.0, camera[1], camera[2]]);
            let mut seen = HashSet::default();
            for entry in &selection.hot {
                if selection
                    .nodes
                    .get(&entry.key)
                    .is_some_and(|n| n.stamp == entry.stamp)
                {
                    assert!(seen.insert(entry.key), "{:?} is hot twice", entry.key);
                }
            }
        }
        // The bands change again before the nodes the first change listed are looked at: what is
        // left of that list is looked at for the new bands, and the cut is as the rule says.
        let mut selection = select(&store, camera, D0);
        selection.set_bands(at_speed(1.5));
        selection.relook_some(&store, 50, 50);
        assert!(selection.relooking() > 0);
        let bands = at_speed(0.5);
        reband(&mut selection, &store, bands);
        is_a_cut(&selection);
        is_as_the_rule_says(&selection, "after a second change of bands");
        queue_is_counted(&selection);
    }

    /// The tidying of the queue after a flip of the horizon cap (#213) leaves what is kept for a
    /// parked node (#168): a change back to the bands those entries were made for restores the
    /// stamps, and a node whose entries the tidying dropped would be watched by nothing.
    #[test]
    fn a_tidying_pass_keeps_the_entries_of_parked_nodes_and_counts_those_it_drops() {
        let store = flat(64, 4);
        let camera = [0.5, 20.0, 0.5];
        let mut selection = select(&store, camera, D0);
        selection.set_bands(at_speed(1.5));
        let parked = selection.nodes.values().filter(|n| kept(n.stamp)).count();
        assert!(parked > 50, "{parked}");
        let queued = selection.queued();
        selection.tidy = Some((0, 0));
        while selection.tidy.is_some() {
            selection.tidy_some();
        }
        assert_eq!(
            selection.queued(),
            queued,
            "the parked entries were dropped"
        );
        queue_is_counted(&selection);
        // A stale entry (its node has another stamp) is dropped, and counted as dropped.
        let key = *selection.nodes.keys().next().expect("a node");
        selection
            .queue
            .entry(u64::MAX)
            .or_default()
            .push((u64::MAX >> 1, key));
        selection.queued_entries += 1;
        selection.tidy = Some((0, 0));
        while selection.tidy.is_some() {
            selection.tidy_some();
        }
        assert_eq!(selection.queued(), queued);
        queue_is_counted(&selection);
    }

    /// A change back to the bands that the nodes of the last change were parked for, before they
    /// are looked at, has them back as they were, with their entries (#168): there is hardly
    /// anything to look at, and what is drawn is what it was.
    #[test]
    fn a_change_back_to_the_bands_before_the_relook_has_the_parked_nodes_as_they_were() {
        let store = flat(64, 4);
        let camera = [0.5, 20.0, 0.5];
        let mut selection = select(&store, camera, D0);
        let before: BTreeSet<Key> = selection.bricks().collect();
        // A far-off camera move builds nothing: the entries are in the queue, with a slack.
        selection.set_bands(at_speed(1.5));
        let listed = selection.relooking();
        assert!(listed > 50, "{listed}");
        selection.set_bands(Bands::with_d0(D0));
        let left = selection.relooking();
        assert!(left < listed / 4, "{left} of {listed} are looked at again");
        while selection.relooking() > 0 {
            let diff = selection.relook_some(&store, 1_000, usize::MAX);
            assert!(diff.added.is_empty() && diff.removed.is_empty());
        }
        selection.update(&store, camera);
        assert_eq!(selection.bricks().collect::<BTreeSet<Key>>(), before);
        is_a_cut(&selection);
        is_as_the_rule_says(&selection, "after a change and back");
        // The camera moves on and the cut is what the rule asks: nothing was lost.
        for n in 1..=40 {
            selection.update(
                &store,
                [camera[0] + 0.5 * f64::from(n), camera[1], camera[2]],
            );
        }
        is_a_cut(&selection);
        is_as_the_rule_says(&selection, "after moving");
        // A change that is not back (to a third) leaves them to be looked at.
        selection.set_bands(at_speed(1.0));
        selection.set_bands(at_speed(1.5));
        let third = selection.relooking();
        selection.set_bands(at_speed(0.5));
        assert!(selection.relooking() >= third / 2);
        while selection.relooking() > 0 {
            selection.relook_some(&store, 1_000, usize::MAX);
        }
        let c = [camera[0] + 20.0, camera[1], camera[2]];
        selection.update(&store, c);
        is_a_cut(&selection);
        is_as_the_rule_says(&selection, "after three changes");
    }

    /// A margin node (whole, its children resident for the camera that comes) is not listed by a
    /// change to coarser bands, so it is not parked: a camera that comes to it before the relook has
    /// reached the rest has it split by the rule (#168: 59 of the 71 soft misses under the camera
    /// at frames 245 to 289 of the 50 m flight were margin nodes parked by a change of bands, with
    /// their children resident).
    #[test]
    fn a_margin_node_is_split_by_a_camera_that_comes_to_it_before_a_coarser_change_is_looked_at() {
        let store = flat(64, 4);
        let camera = [0.5, 20.0, 0.5];
        let top = top_level(&store);
        let roots = top_bricks(&store, top, None);
        let reach = Reach {
            margins: MARGINS,
            ..Reach::NONE
        };
        let (mut selection, _) =
            Selection::new_reached(&store, camera, Bands::with_d0(D0), top, roots, None, reach);
        let margins = selection.nodes.values().filter(|n| n.margin).count();
        assert!(margins > 10, "{margins} margin nodes");
        selection.set_bands(at_speed(1.5));
        assert!(selection.relooking() > 0);
        for (key, node) in &selection.nodes {
            assert!(
                !node.margin || watched(node.stamp),
                "{key:?} is a margin node and parked"
            );
        }
        // A camera that walks on with nothing of the relook done: a whole node that is watched is
        // not one that the bands split.
        for n in 1..=40 {
            selection.update(
                &store,
                [camera[0] + 4.0 * f64::from(n), camera[1], camera[2]],
            );
            for (&key, node) in &selection.nodes {
                assert!(
                    node.state == State::Split
                        || node.stamp == 0
                        || !selection.drawn_split(key, Some(false)),
                    "step {n}: {key:?} is whole, watched and split by the rule"
                );
            }
        }
    }

    /// Bands that go to and fro, a flight's every change, do not make the hot list and the queue
    /// grow: what was made stale is dropped when the nodes have been looked at (#168).
    #[test]
    fn the_queue_stays_bounded_over_repeated_coarse_and_fine_pairs() {
        let store = flat(64, 4);
        let camera = [0.5, 20.0, 0.5];
        let mut selection = select(&store, camera, D0);
        let nodes = selection.nodes();
        let at_first = selection.queued();
        for n in 0..12u32 {
            for bands in [at_speed(1.5), Bands::with_d0(D0)] {
                selection.set_bands(bands);
                while selection.relooking() > 0 {
                    selection.relook_some(&store, 64, usize::MAX);
                }
                // The camera is not still: a little each time.
                selection.update(
                    &store,
                    [camera[0] + 0.25 * f64::from(n), camera[1], camera[2]],
                );
                queue_is_counted(&selection);
                assert!(
                    selection.queued() <= 3 * selection.nodes(),
                    "{} entries for {} nodes after {n} pairs",
                    selection.queued(),
                    selection.nodes()
                );
            }
        }
        assert!(
            selection.nodes() <= 2 * nodes,
            "{} nodes",
            selection.nodes()
        );
        assert!(
            selection.queued() <= at_first.max(nodes) * 3,
            "{} entries, {at_first} at first",
            selection.queued()
        );
    }

    /// No level flips back and forth at a steady speed (#168): a camera that flies straight at a
    /// speed right on the edge between two steps of the curve (30.1 m/s: the step to 2 levels is
    /// asked for from 30.0), which the scene thread is told with noise of 4% on it (the velocity
    /// is a mean over a tenth of a second of frames, and a frame's time is not exact), draws
    /// exactly what the camera does that is told the exact speed, at every frame: the bands move
    /// once and never again. Without the release hysteresis the noise would flip the step.
    #[test]
    fn no_level_flips_at_a_steady_speed_on_the_edge_of_a_step() {
        let store = flat(64, 4);
        let start = [0.5, 20.0, 0.5];
        let speed = 30.1 * brick_store::VOXELS_PER_METRE;
        let step = speed / 100.0;
        let frame = 0.01;
        let run = |wobble: f64| {
            let mut scale = crate::lod::SpeedScale::AT_REST;
            let mut selection = select(&store, start, D0);
            let mut camera = start;
            let mut drawn = Vec::new();
            let mut rebands = 0;
            for n in 0..400u64 {
                camera[0] += step;
                // The first sample is exact: the flight is told its speed, then the noise.
                let noise = if n == 0 {
                    0.0
                } else {
                    ((n * 7919) % 17) as f64 / 16.0 - 0.5
                };
                if scale.observe(speed * (1.0 + wobble * 2.0 * noise), n as f64 * frame) {
                    rebands += 1;
                    reband(&mut selection, &store, at_speed(scale.depth()));
                }
                selection.update(&store, camera);
                while selection.rewanted() {
                    selection.update(&store, camera);
                }
                drawn.push(selection.bricks().collect::<BTreeSet<Key>>());
            }
            (drawn, rebands, scale.depth())
        };
        let (exact, exact_rebands, depth) = run(0.0);
        let (noisy, noisy_rebands, noisy_depth) = run(0.04);
        // One reband, at the first frame, to the 2 levels that 30.1 m/s holds.
        assert_eq!((exact_rebands, noisy_rebands), (1, 1));
        assert_eq!((depth, noisy_depth), (2.0, 2.0));
        assert!(exact[0].len() > 100);
        for (n, (a, b)) in exact.iter().zip(&noisy).enumerate() {
            assert_eq!(a, b, "frame {n}");
        }
        // A scale with no hysteresis (the step the speed asks for, as it is told) flips on the
        // same noise: this test is about the hysteresis.
        let naive_steps = (0..400u64)
            .map(|n| {
                let noise = ((n * 7919) % 17) as f64 / 16.0 - 0.5;
                (crate::lod::depth_for(speed * (1.0 + 0.04 * 2.0 * noise)) / crate::lod::DEPTH_STEP
                    - 1e-9)
                    .ceil() as u32
            })
            .collect::<Vec<_>>();
        let flips = naive_steps.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(flips > 20, "{flips}");
        // And along the flight no brick is drawn more than twice, once while the camera is
        // coming to it and once when it has gone by: a flip would draw it again.
        let mut draws: HashMap<Key, usize> = HashMap::default();
        let mut before = BTreeSet::new();
        for frame in &exact {
            for key in frame.difference(&before) {
                *draws.entry(*key).or_default() += 1;
            }
            before = frame.clone();
        }
        let most = draws.values().copied().max().unwrap();
        assert!(most <= 2, "a brick was drawn {most} times");
    }

    #[test]
    fn a_camera_that_stays_or_bobs_within_the_margin_changes_nothing() {
        let store = flat(64, 4);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        for i in 0..50 {
            let x = 0.5 + if i % 2 == 0 { 0.0 } else { 0.7 };
            assert!(selection.update(&store, [x, 20.0, 0.5]).is_empty());
        }
        // A camera that bobs about a boundary by less than the hysteresis flips no brick. The
        // level 1 brick at x 128..144 is 128 + 20 * 20 / 256 = 129.6 from the camera at 0.5.
        let mut flips = 0;
        for i in 0..200 {
            let x = 0.5 + 3.0 * if i % 2 == 0 { 1.0 } else { -1.0 };
            let diff = selection.update(&store, [x, 20.0, 0.5]);
            flips += diff.added.len() + diff.removed.len();
        }
        assert_eq!(flips, 0);
    }

    #[test]
    fn a_camera_that_goes_further_than_a_hot_node_in_a_frame_still_draws_each_column_once() {
        // Steps of 40 Voxels a frame are five times what a node is watched every frame for.
        let store = flat(64, 5);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        let mut drawn: BTreeSet<Key> = selection.bricks().collect();
        let mut camera = [0.5, 20.0, 0.5];
        for i in 0..40 {
            camera[0] += 40.0;
            camera[2] -= if i % 2 == 0 { 25.0 } else { -25.0 };
            let diff = selection.update(&store, camera);
            for key in &diff.removed {
                assert!(drawn.remove(key), "removed {key:?} which was not drawn");
            }
            for key in &diff.added {
                assert!(drawn.insert(*key), "added {key:?} which was drawn");
            }
            assert_eq!(drawn, selection.bricks().collect::<BTreeSet<_>>());
            for x in (-500..500).step_by(131) {
                for z in (-500..500).step_by(37) {
                    assert_eq!(covering(&selection, x, z).len(), 1, "{camera:?} {x} {z}");
                }
            }
        }
    }

    #[test]
    fn a_brick_that_could_not_be_split_is_split_when_an_edit_gives_it_bricks_while_the_camera_is_away()
     {
        // Level 1 at the origin is solid, with nothing at level 0 under it: it is whole although
        // the camera is inside its band, and nothing looks at it while the camera stays.
        #[derive(Debug)]
        struct Odd;
        impl brick_store::BrickSource for Odd {
            fn brick(&self, level: usize, coord: BrickCoord) -> Brick {
                if level == 1 && coord == BrickCoord([0, 0, 0]) {
                    Brick::solid(ROCK)
                } else {
                    Brick::AIR
                }
            }
        }
        let mut store = BrickStore::new();
        store.set_source(Odd);
        store.build_lod(3);
        let (top, bands) = (top_level(&store), Bands::with_d0(1000.0));
        let roots = top_bricks(&store, top, Some(([0; 3], 64)));
        let (mut selection, _) = Selection::new(&store, [3.0, 3.0, 3.0], bands, top, roots.clone());
        assert!(selection.draws(1, BrickCoord([0, 0, 0])));
        // Away, out of its band but not its parent's; then the Edit gives it a brick of level 0, and the camera returns.
        selection.update(&store, [2500.0, 3.0, 3.0]);
        let changed = [BrickCoord([0, 0, 0])];
        store.insert(changed[0], Brick::solid(SOIL));
        store.refresh_lod(changed);
        selection.refresh(&store, &changed);
        selection.update(&store, [3.0, 3.0, 3.0]);
        let (fresh, _) = Selection::new(&store, [3.0, 3.0, 3.0], bands, top, roots);
        assert!(fresh.draws(0, BrickCoord([0, 0, 0])));
        assert_eq!(
            selection.bricks().collect::<BTreeSet<_>>(),
            fresh.bricks().collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn only_the_bricks_that_can_change_are_looked_at() {
        // A walk of a Voxel pops the few bricks within a Voxel of a boundary, not all of them.
        let store = flat(64, 5);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        let all = selection.nodes();
        assert!(all > 1000, "{all}");
        let before = selection.queued();
        selection.update(&store, [1.5, 20.0, 0.5]);
        // What was popped was queued again: the queue only changes by those looked at.
        let looked_at = selection.queued().abs_diff(before);
        assert!(looked_at * 20 < all, "{looked_at} of {all}");
    }

    #[test]
    fn an_edit_that_digs_a_brick_out_draws_the_ones_it_uncovers() {
        // A ball of rock of 300 Voxels' radius whose crust is held, as the Planet's is, and
        // whose inside is not: the bricks inside it are the source's, and buried.
        let mut store = BrickStore::new();
        store.set_source(Interior {
            radius: 300,
            material: ROCK,
        });
        for x in -40..40 {
            for y in -40..40 {
                for z in -40..40 {
                    let coord = BrickCoord([x, y, z]);
                    if !coord.within_ball(300) && brick_distance([0.0; 3], 0, coord) < 300.0 {
                        store.insert(coord, Brick::solid(ROCK));
                    }
                }
            }
        }
        store.build_lod(2);
        let top = top_level(&store);
        let roots = |store: &BrickStore| top_bricks(store, top, Some(([0; 3], 400)));
        let far = Bands::with_d0(1.0e6);
        let (mut selection, _) = Selection::new(&store, [0.0, 400.0, 0.0], far, top, roots(&store));
        // The crust brick on top, and the bricks under it, which are not held.
        let skin = BrickCoord([0, 37, 0]);
        let under = BrickCoord([0, 36, 0]);
        assert!(selection.draws(0, skin), "on the crust");
        assert!(!selection.draws(0, under), "buried");
        // Dig the crust brick out.
        store.insert(skin, Brick::AIR);
        store.refresh_lod([skin]);
        let diff = selection.refresh(&store, &[skin]);
        assert!(!selection.draws(0, skin), "air is not drawn");
        assert!(selection.draws(0, under), "the brick under it shows now");
        assert!(diff.added.contains(&(0, under)) && diff.removed.contains(&(0, skin)));
        // A fresh selection draws the same.
        let (fresh, _) = Selection::new(&store, [0.0, 400.0, 0.0], far, top, roots(&store));
        assert_eq!(
            selection.bricks().collect::<BTreeSet<_>>(),
            fresh.bricks().collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn a_brick_without_bricks_under_it_stays_whole_instead_of_leaving_a_hole() {
        // Level 1 reads solid from the source and level 0 reads air from it, as the client's
        // region source does outside the region. However near, it is drawn.
        #[derive(Debug)]
        struct Odd;
        impl brick_store::BrickSource for Odd {
            fn brick(&self, level: usize, coord: BrickCoord) -> Brick {
                if level == 1 && coord == BrickCoord([0, 0, 0]) {
                    Brick::solid(ROCK)
                } else {
                    Brick::AIR
                }
            }
        }
        let mut store = BrickStore::new();
        store.set_source(Odd);
        store.build_lod(1);
        let (selection, _) = Selection::new(
            &store,
            [3.0, 3.0, 3.0],
            Bands::with_d0(1000.0),
            1,
            [BrickCoord([0, 0, 0])],
        );
        assert!(selection.draws(1, BrickCoord([0, 0, 0])));
        assert_eq!(selection.bricks().count(), 1);
    }

    /// A source whose level 1 brick at the origin reads as air, as a coarse brick sampled at the
    /// middle of its Voxels does over a slope, with a brick of rock under it at level 0 that
    /// pokes into it. `sky` is what it says to [`brick_store::BrickSource::air_below`].
    #[derive(Debug)]
    struct Poke {
        sky: bool,
    }

    impl brick_store::BrickSource for Poke {
        fn brick(&self, level: usize, coord: BrickCoord) -> Brick {
            if level == 0 && coord == BrickCoord([1, 0, 1]) {
                Brick::solid(ROCK)
            } else {
                Brick::AIR
            }
        }

        fn air_below(&self, _level: usize, _coord: BrickCoord) -> bool {
            self.sky
        }
    }

    fn poke(sky: bool, camera: [f64; 3], d0: f64) -> Selection {
        let mut store = BrickStore::new();
        store.set_source(Poke { sky });
        store.build_lod(1);
        Selection::new(
            &store,
            camera,
            Bands::with_d0(d0),
            1,
            [BrickCoord([0, 0, 0])],
        )
        .0
    }

    #[test]
    fn a_coarse_brick_that_reads_as_air_with_ground_under_it_is_split_and_the_ground_drawn() {
        // Near, level 1 is split, and the brick of rock that its air hid is drawn at level 0.
        let near = poke(false, [3.0, 3.0, 3.0], 1000.0);
        assert!(near.draws(0, BrickCoord([1, 0, 1])));
        assert_eq!(near.bricks().count(), 1);
        // Far, it is whole, and draws nothing, as air does.
        let far = poke(false, [3000.0, 3.0, 3.0], 1.0);
        assert_eq!(far.bricks().count(), 0);
        assert!(
            far.nodes() > 0,
            "it is in the tree, to be split when it is near"
        );
        // Where the source says that all of it is air, it is not in the tree at all.
        let sky = poke(true, [3.0, 3.0, 3.0], 1000.0);
        assert_eq!(sky.nodes(), 0);
    }

    #[test]
    fn a_coarse_brick_that_reads_as_air_waits_for_its_children_in_a_store_that_streams() {
        let mut store = BrickStore::new();
        store.set_source(Poke { sky: false });
        store.build_lod(1);
        store.stream_from(1);
        let (mut selection, _) = Selection::new(
            &store,
            [3.0, 3.0, 3.0],
            Bands::with_d0(1000.0),
            1,
            [BrickCoord([0, 0, 0])],
        );
        assert_eq!(selection.bricks().count(), 0);
        assert_eq!(selection.take_wanted(), [(1, BrickCoord([0, 0, 0]))]);
        // The loader reads its children from the source, which holds the rock, and the selection
        // splits it and draws them.
        let source = store.source().expect("a source");
        let bricks = brick_store::children_from(&*source, 1, BrickCoord([0, 0, 0]), |_| false);
        store.hold_children(1, BrickCoord([0, 0, 0]), bricks);
        let diff = selection.arrived(&store, [(1, BrickCoord([0, 0, 0]))]);
        assert!(selection.draws(0, BrickCoord([1, 0, 1])));
        assert!(diff.added.contains(&(0, BrickCoord([1, 0, 1]))));
        assert_eq!(selection.waiting(), 0);
    }

    /// A block of solid rock, 40 bricks (320 Voxels) on a side, with a pyramid of five levels.
    fn rock() -> BrickStore {
        let mut store = BrickStore::new();
        for x in 0..40 {
            for y in 0..40 {
                for z in 0..40 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(ROCK));
                }
            }
        }
        store.build_lod(4);
        store
    }

    /// Digs a tunnel along x through the block, `height` Voxels tall from `floor` and `width`
    /// wide from `z0`. Returns the bricks of level 0 it changed.
    fn tunnel_box(
        store: &mut BrickStore,
        floor: i32,
        height: i32,
        z0: i32,
        width: i32,
    ) -> Vec<BrickCoord> {
        let mut changed = BTreeSet::new();
        for x in 0..320 {
            for y in floor..floor + height {
                for z in z0..z0 + width {
                    let voxel = brick_store::VoxelCoord([x, y, z]);
                    changed.insert(voxel.brick());
                    store.set(voxel, None);
                }
            }
        }
        changed.into_iter().collect()
    }

    /// A tunnel `width` Voxels square, through the middle of the block, with its floor at
    /// height `floor`.
    fn tunnel(store: &mut BrickStore, floor: i32, width: i32) -> Vec<BrickCoord> {
        tunnel_box(store, floor, width, 160, width)
    }

    /// The bricks of level 0 that a ray can reach from the air, within `reach` Voxels of
    /// `camera`, as the store has them: those that are neither air nor full, and the full ones
    /// with a Voxel of air against a face. A tunnel's walls are these, and the wall across a
    /// face of a coarse brick, in a brick that is itself full, is among them.
    fn walls(store: &BrickStore, camera: [f64; 3], reach: f64) -> Vec<BrickCoord> {
        let air_against = |c: BrickCoord| {
            (0..6).any(|face| {
                let (axis, upper) = (face / 2, face % 2 == 1);
                let mut n = c;
                n.0[axis] += if upper { 1 } else { -1 };
                let side = if upper { 0 } else { 7 };
                // The layer of the neighbour that touches `c`.
                (0..64).any(|i| {
                    let (u, v) = (i % 8, i / 8);
                    let mut local = [0u8; 3];
                    local[axis] = side;
                    local[(axis + 1) % 3] = u;
                    local[(axis + 2) % 3] = v;
                    store.brick_at(0, n).get(local).is_none()
                })
            })
        };
        store
            .level(0)
            .unwrap()
            .sorted_coords()
            .into_iter()
            .filter(|&c| {
                let brick = store.brick_at(0, c);
                !brick.is_air()
                    && brick_distance(camera, 0, c) <= reach
                    && (!brick.is_full() || air_against(c))
            })
            .collect()
    }

    /// A camera inside a tunnel sees its walls, however deep in solid rock it is and however
    /// narrow it is: at a coarse level the bricks round it read as solid (a coarse Voxel is solid
    /// when half its children are), but they are split by the camera in them, down to level 0.
    #[test]
    fn a_tunnel_through_solid_rock_is_drawn_from_inside_it_whatever_its_depth_and_width() {
        let cases = [
            (40, 10),
            (40, 20),
            (120, 10),
            (160, 10),
            (200, 20),
            (236, 36),
            (150, 4),
            (156, 6),
            (100, 8),
        ];
        for (floor, width) in cases {
            let camera = [
                100.0,
                f64::from(floor) + f64::from(width) / 2.0,
                160.0 + f64::from(width) / 2.0,
            ];
            let bands = Bands::standard(1.0);
            let top = top_level(&rock());

            // Fresh: the tunnel is dug, and then the selection is made.
            let mut dug = rock();
            tunnel(&mut dug, floor, width);
            dug.build_lod(4);
            let roots = top_bricks(&dug, top, None);
            let (fresh, _) = Selection::new(&dug, camera, bands, top, roots);
            let wanted = walls(&dug, camera, 200.0);
            assert!(wanted.len() > 4, "floor {floor} width {width}");
            for &wall in &wanted {
                assert!(
                    fresh.draws(0, wall),
                    "fresh: floor {floor} width {width}: {wall:?}"
                );
            }

            // Refreshed: the selection is of solid rock, and then the tunnel is dug.
            let mut store = rock();
            let roots = top_bricks(&store, top, None);
            let (mut selection, _) = Selection::new(&store, camera, bands, top, roots);
            let changed = tunnel(&mut store, floor, width);
            store.refresh_lod(changed.iter().copied());
            selection.refresh(&store, &changed);
            for &wall in &wanted {
                assert!(
                    selection.draws(0, wall),
                    "refreshed: floor {floor} width {width}: {wall:?}"
                );
            }
        }
    }

    /// A hollow that touches a face of a coarse brick has its wall across that face, in the brick on
    /// the other side. A coarse brick reads as full while it hides a hollow up to half a coarse
    /// Voxel wide, so a brick that reads full with full neighbours is not out of reach unless
    /// they, too, are solid all the way down. The tunnel here is against the faces of bricks of
    /// levels 4 (z = 128), 2 (z = 160) and 3 (z = 192), on either side of them, and the wall
    /// Voxels on both sides of the face must be drawn.
    #[test]
    fn the_wall_across_the_face_of_a_coarse_brick_is_drawn_from_a_tunnel_against_it() {
        let mut cases = Vec::new();
        for face in [128, 160, 192] {
            for (floor, height, width) in [(120, 18, 6), (100, 8, 16), (110, 18, 1), (90, 12, 3)] {
                // The air against the face from below, and from above it.
                cases.push((floor, height, face - width, width));
                cases.push((floor, height, face, width));
            }
        }
        let bands = Bands::standard(1.0);
        let top = top_level(&rock());
        for (floor, height, z0, width) in cases {
            let camera = [
                100.0,
                f64::from(floor) + f64::from(height) / 2.0,
                f64::from(z0) + f64::from(width) / 2.0,
            ];
            let label = format!("floor {floor} height {height} z {z0} width {width}");
            let mut dug = rock();
            tunnel_box(&mut dug, floor, height, z0, width);
            dug.build_lod(4);
            let wanted = walls(&dug, camera, 200.0);
            // The bricks across the face are among them.
            let across = |z: i32| wanted.iter().any(|c| c.0[2] == z.div_euclid(8));
            assert!(across(z0 - 1) && across(z0 + width), "{label}");
            let roots = top_bricks(&dug, top, None);
            let (fresh, _) = Selection::new(&dug, camera, bands, top, roots);
            for &wall in &wanted {
                assert!(fresh.draws(0, wall), "fresh: {label}: {wall:?}");
            }

            let mut store = rock();
            let roots = top_bricks(&store, top, None);
            let (mut selection, _) = Selection::new(&store, camera, bands, top, roots);
            let changed = tunnel_box(&mut store, floor, height, z0, width);
            store.refresh_lod(changed.iter().copied());
            selection.refresh(&store, &changed);
            for &wall in &wanted {
                assert!(selection.draws(0, wall), "refreshed: {label}: {wall:?}");
            }
        }
    }

    /// The rock far from a tunnel is not drawn, so the camera in the tunnel is not paying for the
    /// volume: the bricks in the tree are those round the tunnel and the surface.
    #[test]
    fn the_rock_round_a_tunnel_is_not_in_the_tree() {
        let mut store = rock();
        tunnel(&mut store, 120, 10);
        store.build_lod(4);
        let top = top_level(&store);
        let roots = top_bricks(&store, top, None);
        let (selection, _) = Selection::new(
            &store,
            [100.0, 125.0, 165.0],
            Bands::standard(1.0),
            top,
            roots,
        );
        assert!(
            selection.nodes() < 20_000,
            "{} nodes for 64000 bricks",
            selection.nodes()
        );
    }

    /// A flat world whose pyramid is held all the way up but streams from `floor`: the bricks
    /// under a brick of `floor` or below are resident only when told.
    fn streamed(n: i32, levels: usize, floor: usize) -> BrickStore {
        let mut store = flat(n, levels);
        store.stream_from(floor);
        store
    }

    /// Loads what the selection waits for, and what that makes it wait for, until it waits for
    /// nothing. Returns how many rounds it took.
    /// What a loader does in a frame: what was wanted joins what was asked for and has not come,
    /// and the `most` nearest to the `camera` arrive. The rest keep waiting, as in the client.
    fn arrive_nearest(
        store: &mut BrickStore,
        selection: &mut Selection,
        pending: &mut Vec<Key>,
        camera: [f64; 3],
        most: usize,
    ) {
        pending.extend(selection.take_wanted());
        pending.sort_by(|a, b| {
            let d = |&(l, c): &Key| brick_distance(camera, l, c);
            d(b).total_cmp(&d(a))
        });
        let now: Vec<Key> = pending.split_off(pending.len().saturating_sub(most));
        for &(level, coord) in &now {
            store.set_children_resident(level, coord);
        }
        selection.arrived(store, now);
    }

    fn load_everything(store: &mut BrickStore, selection: &mut Selection) -> usize {
        let mut rounds = 0;
        loop {
            let wanted = selection.take_wanted();
            if wanted.is_empty() {
                return rounds;
            }
            rounds += 1;
            for &(level, coord) in &wanted {
                store.set_children_resident(level, coord);
            }
            selection.arrived(store, wanted);
        }
    }

    /// Counts the bricks of level 0 it is asked for. Under y = -1 (the rock) it proves that all is rock, as
    /// the Planet proves its interior.
    #[derive(Debug)]
    struct Counting(std::sync::Arc<AtomicUsize>);

    impl brick_store::BrickSource for Counting {
        fn brick(&self, level: usize, coord: BrickCoord) -> Brick {
            if level == 0 {
                self.0.fetch_add(1, AtomicOrdering::Relaxed);
            }
            if coord.0[1] < -1 {
                Brick::solid(ROCK)
            } else {
                Brick::AIR
            }
        }

        fn solid_below(&self, _: usize, coord: BrickCoord) -> bool {
            coord.0[1] < -1
        }
    }

    /// Splitting the bricks that arrive classifies them against the bricks beside them, which may
    /// not be resident yet: that reads what the source proves of them and never generates one, on
    /// the frame thread, where the loader's workers are the ones to generate.
    #[test]
    fn taking_in_bricks_does_not_generate_the_bricks_beside_them() {
        let made = std::sync::Arc::new(AtomicUsize::new(0));
        let mut store = flat(4, 4);
        store.set_source(Counting(made.clone()));
        store.stream_from(3);
        let source = store.source().expect("a source");
        // The Guard: the level that the store streams from, held where it is not air.
        for x in -3..3 {
            for y in -3..3 {
                for z in -3..3 {
                    let coord = BrickCoord([x, y, z]);
                    if !store.holds(3, coord) {
                        let brick = source.brick(3, coord);
                        if !brick.is_air() {
                            store.hold(3, coord, brick, source.solid_below(3, coord));
                        }
                    }
                }
            }
        }
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        let mut rounds = 0;
        loop {
            let wanted = selection.take_wanted();
            if wanted.is_empty() {
                break;
            }
            rounds += 1;
            // What a loader does: generates the children, and the store holds them.
            for &(level, coord) in &wanted {
                let bricks = brick_store::children_from(&*source, level, coord, |c| {
                    store.holds(level - 1, c)
                });
                store.hold_children(level, coord, bricks);
            }
            made.store(0, AtomicOrdering::Relaxed);
            selection.arrived(&store, wanted);
            assert_eq!(made.load(AtomicOrdering::Relaxed), 0, "round {rounds}");
        }
        assert!(rounds > 1, "{rounds}");
    }

    /// Rock under y = -8 (the top of the level 0 bricks at y = -2), at every level: a brick wholly
    /// under it is rock and says so, one that has it in is rock too (a coarse brick's air is not
    /// modelled), one wholly over it is air.
    #[derive(Debug)]
    struct Floor;

    impl Floor {
        fn size(level: usize) -> i32 {
            8 << level
        }
    }

    impl brick_store::BrickSource for Floor {
        fn brick(&self, level: usize, coord: BrickCoord) -> Brick {
            if coord.0[1] * Self::size(level) >= -8 {
                Brick::AIR
            } else {
                Brick::solid(ROCK)
            }
        }

        fn solid_below(&self, level: usize, coord: BrickCoord) -> bool {
            (coord.0[1] + 1) * Self::size(level) <= -8
        }
    }

    /// A buried brick that the loader left out is not drawn; an Edit that opens the brick over it
    /// makes it a brick that shows, and it is drawn then, with the Voxels the source gives it.
    #[test]
    fn an_edit_beside_a_buried_brick_that_was_left_out_leads_to_its_being_drawn_with_its_voxels() {
        let mut store = BrickStore::new();
        store.set_source(Floor);
        store.build_lod(3);
        store.stream_from(3);
        let source = store.source().expect("a source");
        for x in -3..3 {
            for y in -3..3 {
                for z in -3..3 {
                    let coord = BrickCoord([x, y, z]);
                    if !store.holds(3, coord) {
                        let brick = source.brick(3, coord);
                        if !brick.is_air() {
                            store.hold(3, coord, brick, source.solid_below(3, coord));
                        }
                    }
                }
            }
        }
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        loop {
            let wanted = selection.take_wanted();
            if wanted.is_empty() {
                break;
            }
            for &(level, coord) in &wanted {
                let bricks = brick_store::children_from(&*source, level, coord, |c| {
                    store.holds(level - 1, c)
                });
                store.hold_children(level, coord, bricks);
            }
            selection.arrived(&store, wanted);
        }
        // The rock is under y = -1: its top layer (y = -2) shows, and the one under (y = -3) is
        // buried, so it was left out: not held, not drawn, and full as far as the store knows.
        let (x, z) = (0, 0);
        let (top, buried) = (BrickCoord([x, -2, z]), BrickCoord([x, -3, z]));
        assert!(store.holds(0, top) && selection.draws(0, top));
        assert!(!store.holds(0, buried), "it was left out");
        assert!(store.reads_full_from_source(0, buried));
        assert!(!selection.draws(0, buried) && !selection.splits(0, buried));

        // An Edit takes out the brick over it (it stays held, as air): now the buried brick has
        // air against its top face.
        store.insert(top, Brick::AIR);
        let diff = selection.refresh(&store, &[top]);
        assert!(selection.draws(0, buried), "it shows now");
        assert!(diff.added.contains(&(0, buried)));
        assert!(!selection.draws(0, top));
        // What it is drawn with is what the source makes of it, generated when it is read.
        assert_eq!(*store.brick_at(0, buried), Brick::solid(ROCK));
    }

    #[test]
    fn a_brick_whose_children_are_not_resident_is_drawn_whole_and_wanted_once() {
        let store = streamed(64, 4, 2);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        // Level 2 is the finest that is drawn: there is no hole, and nothing finer than the
        // Guard's children.
        for (x, z) in [(3, 3), (30, 3), (85, 3), (330, 3), (-505, 3)] {
            let levels = covering(&selection, x, z);
            assert_eq!(levels.len(), 1, "{x} {z}: {levels:?}");
            assert!(levels[0] >= 2, "{x} {z}: {levels:?}");
        }
        let wanted = selection.take_wanted();
        assert!(!wanted.is_empty());
        assert!(wanted.iter().all(|&(level, _)| level == 2), "{wanted:?}");
        assert_eq!(selection.waiting(), wanted.len());
        assert!(selection.take_wanted().is_empty(), "each is listed once");
        // Those under the camera are among them; far ones are wanted only if they split.
        let near = BrickCoord([0, -1, 0]).parent().parent();
        assert!(wanted.contains(&(2, near)), "{near:?}");
        assert!(wanted.len() < 200, "{}", wanted.len());
    }

    #[test]
    fn a_brick_that_is_split_is_said_to_split_and_one_that_is_drawn_whole_is_not() {
        let mut store = streamed(64, 4, 2);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        load_everything(&mut store, &mut selection);
        let (drawn_level, drawn) = selection.bricks().next().expect("something is drawn");
        assert!(!selection.splits(drawn_level, drawn));
        // Every brick over one that is drawn is split.
        let (mut level, mut over) = (drawn_level, drawn);
        while level < selection.top() {
            (level, over) = (level + 1, over.parent());
            assert!(selection.splits(level, over), "{level} {over:?}");
        }
        assert!(!selection.splits(0, BrickCoord([9999, 9999, 9999])));
    }

    #[test]
    fn loading_what_is_wanted_ends_with_what_a_store_that_does_not_stream_draws() {
        let mut store = streamed(64, 4, 2);
        for camera in [[0.5, 20.0, 0.5], [300.0, 5.0, -200.0], [127.9, 3.0, 128.1]] {
            let mut selection = select(&store, camera, D0);
            let rounds = load_everything(&mut store, &mut selection);
            assert!(
                rounds >= 2,
                "level 2 down to level 0 takes rounds: {rounds}"
            );
            assert_eq!(selection.waiting(), 0);
            let resident: BTreeSet<Key> = selection.bricks().collect();
            let whole: BTreeSet<Key> = select(&flat(64, 4), camera, D0).bricks().collect();
            assert_eq!(resident, whole, "{camera:?}");
        }
    }

    #[test]
    fn there_is_no_hole_while_a_band_fills_and_the_camera_moves() {
        let mut store = streamed(64, 4, 2);
        let mut camera = [0.5, 20.0, 0.5];
        let mut selection = select(&store, camera, D0);
        let mut pending: Vec<Key> = Vec::new();
        for step in 0..60 {
            camera[0] += 9.0;
            camera[2] -= 4.0;
            selection.update(&store, camera);
            // Some of what is wanted arrives each step, the nearest few.
            arrive_nearest(&mut store, &mut selection, &mut pending, camera, 3);
            for x in (-400..500).step_by(23) {
                for z in (-500..400).step_by(29) {
                    assert_eq!(
                        covering(&selection, x, z).len(),
                        1,
                        "step {step} {camera:?} {x} {z}"
                    );
                }
            }
        }
        arrive_nearest(&mut store, &mut selection, &mut pending, camera, usize::MAX);
        load_everything(&mut store, &mut selection);
        assert_eq!(selection.waiting(), 0);
    }

    #[test]
    fn a_wanted_brick_that_is_taken_off_is_wanted_again_when_the_bands_split_it_again() {
        let store = streamed(64, 4, 2);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        let wanted = selection.take_wanted();
        for &key in &wanted {
            selection.unwant(key);
        }
        assert_eq!(selection.waiting(), 0);
        // Out of reach, and back: the bricks that split are looked at, and are wanted again.
        selection.update(&store, [5000.0, 20.0, 0.5]);
        selection.update(&store, [0.5, 20.0, 0.5]);
        let again = selection.take_wanted();
        assert!(again.contains(&(2, BrickCoord([0, -1, 0]).parent().parent())));
    }

    #[test]
    fn the_nearest_stuck_of_a_large_set_is_a_bound_that_is_never_further_than_the_nearest() {
        let mut store = streamed(64, 4, 2);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        let mut pending: Vec<Key> = Vec::new();
        selection.many = 0;
        let mut camera = [0.5, 20.0, 0.5];
        let mut tight = 0;
        for step in 0..80 {
            camera[0] += 2.5;
            camera[2] -= 1.0;
            selection.update(&store, camera);
            // Some of what is wanted arrives, the nearest few.
            arrive_nearest(&mut store, &mut selection, &mut pending, camera, 2);
            let bound = selection.nearest_stuck();
            let exact = selection.nearest_stuck_in_full();
            for level in 0..LEVELS {
                match (bound[level], exact[level]) {
                    (Some(b), Some(e)) => {
                        assert!(b <= e + 1e-9, "step {step} level {level}: {b} past {e}");
                        tight += usize::from(e - b < 1e-6);
                    }
                    (None, Some(e)) => panic!("step {step} level {level}: no bound for {e}"),
                    _ => {}
                }
            }
        }
        assert!(tight >= 1, "the bound is not just zero: {tight}");
    }

    #[test]
    fn a_brick_that_is_merged_into_its_parent_while_it_waits_is_cancelled() {
        let store = streamed(64, 4, 2);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        let wanted = selection.take_wanted();
        assert!(!wanted.is_empty());
        assert!(selection.take_cancelled().is_empty());
        // The camera goes far enough that bricks it wanted are not wanted split any more.
        selection.update(&store, [3000.0, 20.0, 0.5]);
        let cancelled = selection.take_cancelled();
        assert!(!cancelled.is_empty());
        assert!(
            cancelled.iter().all(|key| wanted.contains(key)),
            "{cancelled:?}"
        );
        assert!(
            cancelled
                .iter()
                .all(|&(level, coord)| !selection.draws(level, coord))
        );
        assert!(selection.take_cancelled().is_empty(), "each is said once");
    }

    #[test]
    fn a_brick_that_is_wanted_again_before_the_cancellation_is_taken_is_not_cancelled() {
        // Its node is gone, and comes back, between two calls: the loader must not be told to stop
        // generating the children of a brick that waits for them (it kept the earlier request, and
        // the cancellation killed it, so the brick waited for ever).
        let store = streamed(64, 4, 2);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        let wanted = selection.take_wanted();
        let (back, gone) = (wanted[0], wanted[1]);
        selection.forget(back);
        selection.forget(gone);
        selection.want(back);
        assert_eq!(selection.take_cancelled(), [gone]);
        assert_eq!(selection.take_wanted(), [back]);
        assert_eq!(selection.waiting(), wanted.len() - 1);
    }

    #[test]
    fn the_wanted_can_be_taken_a_few_at_a_time_and_none_is_lost() {
        let store = streamed(64, 4, 2);
        let mut selection = select(&store, [0.5, 20.0, 0.5], D0);
        let total = selection.waiting();
        let mut got = Vec::new();
        loop {
            let some = selection.take_wanted_up_to(7);
            assert!(some.len() <= 7);
            if some.is_empty() {
                break;
            }
            got.extend(some);
        }
        assert_eq!(got.len(), total);
        let distinct: BTreeSet<_> = got.iter().collect();
        assert_eq!(distinct.len(), total);
        assert!(selection.take_wanted().is_empty());
    }

    const MARGINS: Margins = Margins {
        radial: 1.25,
        vertical: 0.0,
    };

    /// Whether the children of the whole bricks that are drawn at `level` are resident, those at
    /// `from` to `to` voxels from `camera` (in distance of their boxes), as (resident, all).
    fn resident_between(
        store: &BrickStore,
        selection: &Selection,
        camera: [f64; 3],
        level: usize,
        (from, to): (f64, f64),
    ) -> (usize, usize) {
        let ring: Vec<Key> = selection
            .bricks()
            .filter(|&(l, c)| l == level && (from..to).contains(&brick_distance(camera, l, c)))
            .collect();
        let resident = ring
            .iter()
            .filter(|&&(l, c)| store.children_resident(l, c))
            .count();
        (resident, ring.len())
    }

    #[test]
    fn a_margin_has_the_children_of_bricks_about_to_split_resident_and_draws_none_of_them() {
        let camera = [0.5, 20.0, 0.5];
        let mut plain_store = streamed(64, 4, 2);
        let mut plain = select(&plain_store, camera, D0);
        load_everything(&mut plain_store, &mut plain);
        let mut store = streamed(64, 4, 2);
        let mut selection = select(&store, camera, D0);
        selection.set_margins(&store, MARGINS);
        load_everything(&mut store, &mut selection);
        assert_eq!(selection.waiting(), 0);
        // The same bricks are drawn: a margin is residency, not drawing.
        let drawn: BTreeSet<Key> = selection.bricks().collect();
        assert_eq!(drawn, plain.bricks().collect::<BTreeSet<Key>>());
        // Level 2 starts at 128 and its bricks are split under it. Between 128 and 150 (inside
        // 1.25 · 128 = 160 with the hysteresis) the margin has the children resident, and a store
        // without one has not.
        let (margin, all) = resident_between(&store, &selection, camera, 2, (130.0, 150.0));
        assert!(all > 10 && margin == all, "{margin} of {all}");
        let (without, all) = resident_between(&plain_store, &plain, camera, 2, (130.0, 150.0));
        assert!(without == 0 && all > 10, "{without} of {all}");
        // Beyond the margin there is nothing extra.
        let (far, all) = resident_between(&store, &selection, camera, 2, (175.0, 400.0));
        assert!(far == 0 && all > 10, "{far} of {all}");
    }

    #[test]
    fn with_a_margin_the_camera_walks_into_a_band_without_a_soft_miss() {
        let start = [0.5, 20.0, 0.5];
        let mut counts = Vec::new();
        for margins in [Margins::NONE, MARGINS] {
            let mut store = streamed(64, 4, 2);
            let mut selection = select(&store, start, D0);
            selection.set_margins(&store, margins);
            load_everything(&mut store, &mut selection);
            assert_eq!(selection.soft_misses(), 0);
            // A step of 8 Voxels at a time (level 1 has a margin of 12), with some bricks arriving after each, as a loader that
            // makes less than the camera asks of it does.
            let mut camera = start;
            let mut pending: Vec<Key> = Vec::new();
            let mut misses = 0;
            for _ in 0..24 {
                camera[0] += 8.0;
                selection.update(&store, camera);
                misses += selection.soft_misses();
                arrive_nearest(&mut store, &mut selection, &mut pending, camera, 200);
            }
            counts.push(misses);
        }
        assert!(counts[0] > 0, "the walk crosses bands: {counts:?}");
        assert_eq!(counts[1], 0, "the margin was there first: {counts:?}");
    }

    #[test]
    fn a_brick_that_leaves_the_margin_before_its_children_come_is_given_up_on() {
        let camera = [0.5, 20.0, 0.5];
        let store = streamed(64, 4, 2);
        let mut selection = select(&store, camera, D0);
        selection.take_wanted();
        selection.set_margins(&store, MARGINS);
        let wanted = selection.take_wanted();
        assert!(!wanted.is_empty());
        let margin_only: Vec<Key> = wanted
            .iter()
            .copied()
            .filter(|&(l, c)| !Bands::with_d0(D0).split(l, brick_distance(camera, l, c), None))
            .collect();
        assert!(!margin_only.is_empty());
        selection.take_cancelled();
        // Gone far enough that none of it is in reach.
        selection.update(&store, [3000.0, 20.0, 0.5]);
        let cancelled = selection.take_cancelled();
        for key in &margin_only {
            assert!(cancelled.contains(key), "{key:?}");
        }
    }

    #[test]
    fn a_margin_brick_the_loader_dropped_is_wanted_again_by_the_next_update() {
        let camera = [0.5, 20.0, 0.5];
        let store = streamed(64, 4, 2);
        let mut selection = select(&store, camera, D0);
        selection.take_wanted();
        selection.set_margins(&store, MARGINS);
        let margin_only: Vec<Key> = selection
            .take_wanted()
            .into_iter()
            .filter(|&(l, c)| !Bands::with_d0(D0).split(l, brick_distance(camera, l, c), None))
            .collect();
        assert!(!margin_only.is_empty());
        // The loader was behind the camera and gave them up, though the camera is in the margin
        // still.
        for &key in &margin_only {
            selection.unwant(key);
        }
        assert!(selection.take_wanted().is_empty());
        // Whether the camera moves or not.
        assert!(selection.rewanted());
        selection.update(&store, camera);
        assert!(!selection.rewanted());
        let again = selection.take_wanted();
        for key in &margin_only {
            assert!(again.contains(key), "{key:?} is stranded");
        }
    }

    #[test]
    fn the_lead_wants_the_children_of_bricks_where_the_camera_is_going() {
        let camera = [0.5, 20.0, 0.5];
        let ahead = [300.0, 20.0, 0.5];
        let mut store = streamed(64, 4, 2);
        let mut selection = select(&store, camera, D0);
        load_everything(&mut store, &mut selection);
        selection.set_ahead([Some(ahead), None]);
        let diff = selection.update(&store, [1.5, 20.0, 0.5]);
        assert!(diff.is_empty(), "a lead draws nothing");
        let wanted = selection.take_wanted();
        assert!(!wanted.is_empty());
        // What the lead wants is near where the camera is going, and is not a brick that the bands
        // split for the camera.
        let bands = Bands::with_d0(D0);
        for &(level, coord) in &wanted {
            assert!(!bands.split(level, brick_distance(camera, level, coord), None));
            assert!(
                bands.split(level, brick_distance(ahead, level, coord), Some(true)),
                "{level} {coord:?}"
            );
        }
    }

    #[test]
    fn a_brick_that_is_split_with_its_children_gone_from_the_store_is_a_hole_and_none_is_otherwise()
    {
        let camera = [0.5, 20.0, 0.5];
        let mut store = streamed(64, 4, 2);
        let mut selection = select(&store, camera, D0);
        load_everything(&mut store, &mut selection);
        assert_eq!(selection.holes(&store), 0);
        assert_eq!(selection.soft_misses_by_level(), [0; LEVELS]);
        let (level, coord) = selection
            .nodes
            .iter()
            .find(|&(&(level, _), node)| level == 1 && node.state == State::Split)
            .map(|(&key, _)| key)
            .expect("the camera is near enough to split a brick of level 1");
        // Eviction does this only to what is not drawn split: a store that did it anyway is what the
        // count is there to find.
        assert!(store.evict_children(level, coord, |_, _| false).is_some());
        assert_eq!(selection.holes(&store), 1);
        // The look shared among threads counts what one does, whatever the threads.
        for threads in [1, 2, 3, 4, 7] {
            assert_eq!(selection.holes_by(&store, threads), 1, "{threads} threads");
        }
    }
}
