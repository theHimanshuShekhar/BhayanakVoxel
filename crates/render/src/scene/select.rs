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

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use brick_store::{Brick, BrickCoord, BrickStore, BrickView};

use crate::lod::{Bands, LEVELS, brick_distance};

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
    /// The total camera travel at which its state is next looked at, infinite for none.
    due: f64,
}

/// An entry of the queue: when to look at a node.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Due(f64, Key);

impl Eq for Due {}

impl PartialOrd for Due {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Due {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0).then(self.1.cmp(&other.1))
    }
}

/// What changed in what is drawn: the bricks that are drawn now and were not, and the bricks
/// that were drawn and are not. A brick that came and went in the same change is in neither.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub added: HashSet<Key>,
    pub removed: HashSet<Key>,
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
    /// In the tree: not buried, and not air with nothing under it.
    present: bool,
    /// Drawn if it is whole: not air.
    draws: bool,
}

/// Whether every Voxel of `brick` is solid.
fn is_full(brick: &Brick) -> bool {
    match brick.view() {
        BrickView::Air => false,
        BrickView::Solid(_) => true,
        BrickView::Mixed { occupancy, .. } => occupancy.iter().all(|&w| w == u64::MAX),
    }
}

/// The bricks that share a face with `coord`.
fn faces(coord: BrickCoord) -> impl Iterator<Item = BrickCoord> {
    (0..6).map(move |face| {
        let mut next = coord.0;
        next[face / 2] += if face % 2 == 0 { -1 } else { 1 };
        BrickCoord(next)
    })
}

/// What the brick at `coord` of `level` is to the tree.
///
/// Air is out of it, unless the store holds the brick, which means it holds bricks under it that
/// may not be air (a coarse brick is air if under half its Voxels are solid). A brick that is
/// solid with solid bricks on all six faces is out of it too, held or not: no ray can reach it,
/// and what is under it is out of reach in the same way. An Edit that opens a brick is one that
/// changes class, which [`Selection::refresh`] sees: it looks again at the bricks changed and
/// those beside them.
///
/// At a coarse level "solid" means a majority of each Voxel's children are, so a brick can hide
/// air thinner than half a Voxel of its level: a cave that narrow, wholly inside buried bricks,
/// is not drawn from within. It is under a pixel wide at the distance the level is used from.
fn probe(store: &BrickStore, level: usize, coord: BrickCoord) -> Probe {
    let held = store.level(level).is_some_and(|l| l.brick(coord).is_some());
    let brick = store.brick_at(level, coord);
    if brick.is_air() {
        return Probe {
            present: held,
            draws: false,
        };
    }
    let buried = is_full(&brick) && faces(coord).all(|n| is_full(&store.brick_at(level, n)));
    Probe {
        present: !buried,
        draws: true,
    }
}

/// What a brick of `level` that is to be split is split into, given that it is `p`.
///
/// The bricks under it that are in the tree. If there are none because they are all buried, it is
/// buried too: nothing under it can be seen, so nothing is drawn. If there are none because they
/// are air, or the store has nothing under it, and it is not air, it stays whole, which is a
/// coarser frame than the rule asks for, rather than a hole.
fn decide(store: &BrickStore, level: usize, coord: BrickCoord, p: Probe) -> Plan {
    let all: Vec<(BrickCoord, Probe)> = coord
        .children()
        .into_iter()
        .map(|child| (child, probe(store, level - 1, child)))
        .collect();
    let present: Vec<_> = all.iter().filter(|(_, c)| c.present).copied().collect();
    if !present.is_empty() || all.iter().any(|(_, c)| c.draws) || !p.draws {
        Plan::Split(present)
    } else {
        Plan::Whole
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

/// What to do with a brick that is in the tree.
enum Plan {
    /// Draw it whole.
    Whole,
    /// Split it into these bricks of the level below, which are in the tree.
    Split(Vec<(BrickCoord, Probe)>),
}

/// Whether the brick `key`, which is `p` and is new, is drawn whole or split.
fn plan(store: &BrickStore, bands: Bands, camera: [f64; 3], key: Key, p: Probe) -> Plan {
    let (level, coord) = key;
    let distance = brick_distance(camera, level, coord);
    if bands.split(level, distance, None) {
        decide(store, level, coord, p)
    } else {
        Plan::Whole
    }
}

/// The coarsest level that is looked at in `store`.
pub fn top_level(store: &BrickStore) -> usize {
    (LEVELS - 1).min(store.level_count() - 1)
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
    if let Some((centre, radius)) = ball {
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
    /// Total length of the camera's path since the selection began.
    travelled: f64,
    nodes: HashMap<Key, Node>,
    queue: BinaryHeap<Reverse<Due>>,
    /// Bricks drawn at each level.
    drawn: [usize; LEVELS],
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
            travelled: 0.0,
            nodes: HashMap::new(),
            queue: BinaryHeap::new(),
            drawn: [0; LEVELS],
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
                    Plan::Whole => selection.make_whole(key, distance, p, &mut diff),
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

    /// Every brick drawn, in no order.
    pub fn bricks(&self) -> impl Iterator<Item = Key> + '_ {
        self.nodes
            .iter()
            .filter(|(_, n)| n.state == State::Whole && n.draws)
            .map(|(&key, _)| key)
    }

    /// Nodes of the tree: the bricks drawn, those split, and air that may have something in it.
    pub fn nodes(&self) -> usize {
        self.nodes.len()
    }

    /// Nodes waiting in the queue, the stale ones included.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Adds `key`, which is in the tree and whose parent is split (or is the top), and what is
    /// under it if it is to be split.
    fn build(&mut self, store: &BrickStore, key: Key, p: Probe, diff: &mut Diff) {
        let distance = brick_distance(self.camera, key.0, key.1);
        match plan(store, self.bands, self.camera, key, p) {
            Plan::Whole => self.make_whole(key, distance, p, diff),
            Plan::Split(children) => self.split(store, key, distance, children, diff),
        }
    }

    /// Makes `key` a node that is split.
    fn mark_split(&mut self, key: Key, distance: f64) {
        self.nodes.insert(
            key,
            Node {
                state: State::Split,
                draws: false,
                due: f64::INFINITY,
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
    fn make_whole(&mut self, key: Key, distance: f64, p: Probe, diff: &mut Diff) {
        self.nodes.insert(
            key,
            Node {
                state: State::Whole,
                draws: p.draws,
                due: f64::INFINITY,
            },
        );
        if p.draws {
            self.drawn[key.0] += 1;
            diff.add(key);
        }
        self.queue_node(key, distance, false);
    }

    /// Queues `key` for when it may change, given its `distance` now.
    fn queue_node(&mut self, key: Key, distance: f64, split: bool) {
        let slack = self.bands.slack(key.0, distance, split);
        let due = self.travelled + slack;
        if let Some(node) = self.nodes.get_mut(&key) {
            node.due = due;
        }
        if due.is_finite() {
            self.queue.push(Reverse(Due(due, key)));
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
                if node.draws {
                    self.drawn[level - 1] -= 1;
                    diff.remove(key);
                }
                if node.state == State::Split {
                    self.remove_under(key, diff);
                }
            }
        }
    }

    /// Looks again at `key`, which is due.
    fn revisit(&mut self, store: &BrickStore, key: Key, diff: &mut Diff) {
        let Some(node) = self.nodes.get(&key).copied() else {
            return;
        };
        let (level, coord) = key;
        let distance = brick_distance(self.camera, level, coord);
        match node.state {
            State::Whole => {
                if self.bands.split(level, distance, Some(false)) {
                    let p = Probe {
                        present: true,
                        draws: node.draws,
                    };
                    if let Plan::Split(children) = decide(store, level, coord, p) {
                        if node.draws {
                            self.drawn[level] -= 1;
                            diff.remove(key);
                        }
                        self.split(store, key, distance, children, diff);
                        return;
                    }
                }
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
                    self.make_whole(key, distance, p, diff);
                } else {
                    self.nodes.remove(&key);
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
        let mut diff = Diff::default();
        while let Some(&Reverse(Due(due, key))) = self.queue.peek() {
            if due > self.travelled {
                break;
            }
            self.queue.pop();
            // An entry for a node that has gone, or has been queued again since, is stale.
            if self.nodes.get(&key).is_some_and(|n| n.due == due) {
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
                if node.draws {
                    self.drawn[level] -= 1;
                    diff.remove(key);
                }
                self.remove_under(key, diff);
            }
            Some(node) if node.state == State::Whole => {
                if node.draws != p.draws {
                    if p.draws {
                        self.drawn[level] += 1;
                        diff.add(key);
                    } else {
                        self.drawn[level] -= 1;
                        diff.remove(key);
                    }
                    if let Some(node) = self.nodes.get_mut(&key) {
                        node.draws = p.draws;
                    }
                }
                // It may have bricks to be split into now, which it did not have.
                self.revisit(store, key, diff);
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
}
