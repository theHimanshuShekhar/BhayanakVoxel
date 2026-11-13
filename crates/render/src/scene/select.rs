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
//! **Incremental.** The distance of a brick's box changes no faster than the camera moves, so a
//! brick that is `s` from where its decision would change cannot change it until the camera has
//! travelled `s` (the path length, not the displacement). Each node is queued for the total
//! travel at which that is first possible, and [`Selection::update`] looks only at those that
//! are due: the work in a frame is the bricks that may change level, not all of them.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use brick_store::{Brick, BrickCoord, BrickStore, TableSize};

use crate::lod::{Bands, HYSTERESIS, LEVELS, Margins, Placed, Reach, brick_distance};

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

/// What looking at an entry that was due in the queue came to.
#[cfg_attr(test, derive(Debug, PartialEq))]
enum Due {
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

/// How far the camera (and the places it looks ahead to) must go before the node `key`, `distance`
/// from the camera, could change: what is drawn, or for a whole node whether it is in reach of
/// `reach` (`margin` says whether it is).
fn node_slack(
    bands: Bands,
    reach: Placed,
    key: Key,
    distance: f64,
    split: bool,
    margin: bool,
) -> f64 {
    let drawn = bands.slack(key.0, distance, split);
    if split || reach.is_none() || key.0 == 0 {
        return drawn;
    }
    let floor = bands.floor(key.0);
    let edge = floor
        * if margin {
            1.0 + HYSTERESIS
        } else {
            1.0 - HYSTERESIS
        };
    drawn.min((reach.distance(key.0, key.1) - edge).abs())
}

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
fn plan(store: &BrickStore, bands: Bands, camera: [f64; 3], key: Key, p: Probe) -> Plan {
    let (level, coord) = key;
    let distance = brick_distance(camera, level, coord);
    if bands.split(level, distance, None) {
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
    roots.into_iter().collect()
}

/// The bricks drawn, as a cut through the pyramid that follows a camera. See the module docs.
#[derive(Clone, Debug)]
pub struct Selection {
    bands: Bands,
    /// The coarsest level, which is drawn from where it starts to infinity.
    top: usize,
    camera: [f64; 3],
    /// What residency looks at besides the bands (see [`Selection::set_margins`]).
    reach: Reach,
    /// Total length of the camera's path since the selection began, and of the places it was
    /// looking ahead to (a node's distance changes no faster than that sum).
    travelled: f64,
    /// A margin brick was given up on and is on the hot list for the next [`Selection::update`],
    /// which is to run whether the camera has moved or not.
    rewanted: bool,
    nodes: HashMap<Key, Node>,
    /// The nodes that are not hot, by the bucket of the camera's travel in which they are to be
    /// looked at to see if they have become hot, with the stamp they were queued with (an entry for
    /// a node that was queued again since is stale).
    queue: BTreeMap<u64, Vec<(u64, Key)>>,
    /// The nodes that could change within [`HOT`] travel of the camera.
    hot: Vec<Hot>,
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
        assert!(top < LEVELS && top < store.level_count());
        let mut selection = Selection {
            bands,
            top,
            camera,
            reach: Reach::NONE,
            travelled: 0.0,
            rewanted: false,
            nodes: HashMap::default(),
            queue: BTreeMap::new(),
            hot: Vec::new(),
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
        let mut diff = Diff::default();
        // Level by level, from the top, so that the bricks of a level are looked at on all the
        // threads at once.
        let roots: Vec<BrickCoord> = roots.into_iter().collect();
        let probes = par_map(&roots, |&coord| probe(store, top, coord));
        let mut frontier: Vec<(Key, Probe)> = roots
            .iter()
            .zip(probes)
            .filter(|(_, p)| p.present)
            .map(|(&coord, p)| ((top, coord), p))
            .collect();
        while !frontier.is_empty() {
            let (bands, camera) = (selection.bands, selection.camera);
            let plans = par_map(&frontier, |&(key, p)| plan(store, bands, camera, key, p));
            let mut next = Vec::new();
            for (&(key, p), plan) in frontier.iter().zip(plans) {
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

    /// Nodes of the tree: the bricks drawn, those split, and air that may have something in it.
    pub fn nodes(&self) -> usize {
        self.nodes.len()
    }

    /// Entries in the queue and among the hot nodes: the stale ones are counted too.
    pub fn queued(&self) -> usize {
        self.queue.values().map(Vec::len).sum::<usize>() + self.hot.len()
    }

    /// Adds `key`, which is in the tree and whose parent is split (or is the top), and what is
    /// under it if it is to be split.
    fn build(&mut self, store: &BrickStore, key: Key, p: Probe, diff: &mut Diff) {
        let distance = brick_distance(self.camera, key.0, key.1);
        match plan(store, self.bands, self.camera, key, p) {
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
    /// stuck, so that the tables do not double in the middle of a frame.
    pub fn reserve(&mut self, nodes: usize, waiting: usize, stuck: usize) {
        self.nodes.reserve(nodes);
        self.waiting.reserve(waiting);
        self.stuck.reserve(stuck);
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
    /// children arrived. Whatever is generating them can stop.
    pub fn take_cancelled(&mut self) -> Vec<Key> {
        std::mem::take(&mut self.cancelled)
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
                && self
                    .bands
                    .split(level, brick_distance(self.camera, level, coord), None)
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
    pub fn holes(&self, store: &BrickStore) -> usize {
        self.nodes
            .iter()
            .filter(|&(&(level, coord), node)| {
                node.state == State::Split && !store.children_resident(level, coord)
            })
            .count()
    }

    /// Takes `key` off the bricks that wait, because its children will not be generated after all (the
    /// camera has gone from it): it is listed again if the bands want it split later.
    pub fn unwant(&mut self, key: Key) {
        if self.waiting.remove(&key)
            && let Some(node) = self.nodes.get_mut(&key)
            && node.state == State::Whole
            && (node.stamp == 0 || node.margin)
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
        self.look_again(store);
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
    }

    /// Looks at every whole brick again, for a change of [`Reach`].
    fn look_again(&mut self, store: &BrickStore) {
        let keys: Vec<Key> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.state == State::Whole)
            .map(|(&key, _)| key)
            .collect();
        let at = self.reach.from(self.camera);
        for key in keys {
            self.settle_margin(store, at, key);
            if self.nodes.get(&key).is_some_and(|node| node.stamp != 0) {
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
        let inside = self.bands.split(level, reach, Some(node.margin));
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
            if !self.bands.split(level - 1, reach, None) || !probe(store, level - 1, child).present
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
        node_slack(
            self.bands,
            self.reach.from(self.camera),
            key,
            distance,
            false,
            margin,
        )
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
                    self.want_below(store, self.reach.from(self.camera), key);
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
        self.settle_margin(store, self.reach.from(self.camera), key);
        let stuck = self.bands.split(key.0, distance, Some(false));
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
            self.bands.slack(key.0, distance, true)
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
            });
        } else {
            // At least half of `HOT` on, so that a node that is just out of reach is not looked at
            // again and again.
            let at = self.travelled + (slack - HOT).max(HOT / 2.0);
            self.queue
                .entry((at / BUCKET) as u64)
                .or_default()
                .push((stamp, key));
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
                if self.bands.split(level, distance, whole_was) {
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
                    if self.bands.split(level, distance, Some(false)) {
                        self.unwatch(key);
                        return;
                    }
                }
                self.settle_margin(store, self.reach.from(self.camera), key);
                self.queue_node(key, distance, false);
            }
            State::Split => {
                if self.bands.split(level, distance, Some(true)) {
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
        self.rewanted = false;
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
            popped.extend(entry.remove());
        }
        let (bands, camera) = (self.bands, self.camera);
        let reach = self.reach.from(camera);
        // Whether a node `distance` from the camera changes: what is drawn, or (for a whole node)
        // whether it is in reach.
        let changes = |key: Key, distance: f64, split: bool, margin: bool| {
            if split {
                !bands.split(key.0, distance, Some(true))
            } else {
                bands.split(key.0, distance, Some(false))
                    || (!reach.is_none()
                        && bands.split(key.0, reach.distance(key.0, key.1), Some(margin)) != margin)
            }
        };
        let slack = |key: Key, distance: f64, split: bool, margin: bool| {
            node_slack(bands, reach, key, distance, split, margin)
        };
        let mut changing: Vec<(u64, Key)> = Vec::new();
        let hot = std::mem::take(&mut self.hot);
        let mut stay = Vec::with_capacity(hot.len());
        let mut cooled = Vec::new();
        for entry in hot {
            let distance = brick_distance(camera, entry.key.0, entry.key.1);
            if changes(entry.key, distance, entry.split, entry.margin) {
                changing.push((entry.stamp, entry.key));
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
                Due::Changes => changing.push((stamp, key)),
                Due::Later {
                    split,
                    margin,
                    slack,
                } => self.watch(key, stamp, split, margin, slack),
            }
        }
        for (stamp, key) in changing {
            if self.nodes.get(&key).is_some_and(|n| n.stamp == stamp) {
                self.revisit(store, key, &mut diff);
            }
        }
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
                } else if self.nodes.get(&key).is_some_and(|n| n.stamp == 0) {
                    // Left unwatched while it could not be split, and outside the band now, so
                    // nothing else will look at it when the camera comes back.
                    self.queue_node(key, distance, false);
                }
            }
            None if p.present => {
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

    fn select(store: &BrickStore, camera: [f64; 3], d0: f64) -> Selection {
        let top = top_level(store);
        let roots = top_bricks(store, top, None);
        Selection::new(store, camera, Bands::with_d0(d0), top, roots).0
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
    }
}
