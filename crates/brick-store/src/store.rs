//! Sparse hash index of bricks keyed by integer brick coordinate.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::brick::{Brick, Material};
use crate::coord::{BrickCoord, VoxelCoord};
use crate::lod::{BrickSource, downsample};

/// The Planet's bricks. A brick that is absent reads as air.
///
/// The index is a `HashMap`, whose iteration order is random per process, so it is never
/// exposed: use [`Self::sorted_coords`] wherever order matters.
///
/// A brick that is not held can read as something other than air: see [`BrickSource`].
/// [`Self::brick`], [`Self::get`] and [`Self::content_hash`] only see the bricks held;
/// [`Self::brick_at`] and [`Self::voxel_at`] read what is there, at any level.
///
/// A store can also carry a pyramid of coarser mip levels (ADR 0002), built by
/// [`Self::build_lod`]. Level `n` is a store of its own, in bricks of Voxels `2^n` times the
/// size of level 0's. A coarse brick is held exactly when it covers a brick held at level 0,
/// and is then the downsampling of the eight bricks below it, each read as [`Self::brick_at`]
/// does. The pyramid is not updated by [`Self::insert`], [`Self::remove`] or [`Self::set`]:
/// after changing level 0 bricks, tell it which with [`Self::refresh_lod`]. `sim::apply` does.
#[derive(Clone, Debug, Default)]
pub struct BrickStore {
    bricks: HashMap<BrickCoord, Brick>,
    /// In the stores of the coarse levels: the bricks held there that have only solid Voxels of
    /// level 0 under them, exactly (see [`BrickStore::solid_below`]). Empty at level 0.
    solid_below: HashSet<BrickCoord>,
    /// Mip levels 1 and up, finest first. Empty when there is no pyramid; the stores in it have
    /// no pyramid or source of their own.
    coarse: Vec<BrickStore>,
    source: Option<Arc<dyn BrickSource>>,
    /// Which source this is, for the answers that are remembered (see
    /// [`BrickStore::source_solid_below`]): 0 for none, and different for every source set.
    source_id: u64,
    /// Set for a store whose bricks are streamed in (see [`Self::stream_from`]).
    streaming: Option<Streaming>,
    /// What [`Self::reserve_resident`] was asked for before the store streamed, which
    /// [`Self::stream_from`] makes the resident set with room for.
    resident_room: usize,
    /// How much the store may hold once it streams (see [`Self::set_budget`]).
    budget: Option<Budget>,
    /// The bricks of level 0 that an Edit changed after the store began to stream, which eviction
    /// keeps (see [`Self::evict_children`]).
    diverged: Diverged,
}

/// How much a store that streams may hold, counted as [`BrickStore::streamed_len`] does. Eviction
/// (ADR 0004) starts when the store holds more than [`Budget::limit`], and goes on until it holds
/// no more than [`Budget::bricks`], so that it is not run again for every brick that arrives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    pub bricks: usize,
    /// How far over `bricks` the store may go before eviction starts, as a fraction of it: 0.1
    /// to 0.2 (ADR 0004). What it holds then is what was wanted lately.
    pub overflow: f64,
}

impl Budget {
    /// The most the store holds before eviction starts.
    pub fn limit(&self) -> usize {
        (self.bricks as f64 * (1.0 + self.overflow)) as usize
    }
}

/// What the source said of a brick's [`BrickSource::solid_below`], for one thread to remember.
#[derive(Clone, Copy, Default)]
struct Remembered {
    source: u64,
    coord: [i32; 3],
    level: u8,
    solid: bool,
}

/// Answers remembered per thread: a direct-mapped table, in which a brick takes the place of
/// whichever one had its slot. It is small, since a thread allocates it on first use, and the
/// threads that are started for one job each (a selection made from scratch) use it
/// too: the bricks asked about again and again are the few round the ones that just arrived.
const REMEMBERED: usize = 1 << 12;

thread_local! {
    static REMEMBERED_SOLID: RefCell<Vec<Remembered>> = const { RefCell::new(Vec::new()) };
}

/// What `ask` says for the brick at `coord` of `level` of the source `source` (a nonzero
/// [`BrickStore::source_id`]), asking it only if this thread has not been told lately. The source
/// is asked about the same few bricks again and again while the scene looks at the bricks round
/// the ones that just arrived, and an answer takes a microsecond or two to work out from the
/// world map: more than the rest of looking at a brick.
///
/// Both answers are remembered, so [`BrickSource::solid_below`] must be a pure function of the
/// brick for a source that is used through a store. The table is not borrowed while `ask` runs,
/// so `ask` may use the store again.
fn remembered(source: u64, level: usize, coord: BrickCoord, ask: impl FnOnce() -> bool) -> bool {
    let mut slot = (level as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for c in coord.0 {
        slot = (slot.rotate_left(21) ^ u64::from(c as u32)).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    }
    let slot = (slot >> 40) as usize & (REMEMBERED - 1);
    let found = REMEMBERED_SOLID.with(|table| {
        let mut table = table.borrow_mut();
        if table.is_empty() {
            table.resize(REMEMBERED, Remembered::default());
        }
        let entry = table[slot];
        (entry.source == source && entry.level as usize == level && entry.coord == coord.0)
            .then_some(entry.solid)
    });
    if let Some(solid) = found {
        return solid;
    }
    let solid = ask();
    REMEMBERED_SOLID.with(|table| {
        table.borrow_mut()[slot] = Remembered {
            source,
            coord: coord.0,
            level: level as u8,
            solid,
        };
    });
    solid
}

/// How full one table is: its entries, and how many it has room for before it grows (and moves
/// every entry). Tables that are reserved up front are watched with these (see
/// [`BrickStore::reserve`]); a capacity that is larger than it was is a growth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableSize {
    pub name: String,
    pub len: usize,
    pub capacity: usize,
    /// Bytes the table has allocated for that capacity (zero for what is not memory).
    pub bytes: usize,
}

impl TableSize {
    /// A count that takes no memory of its own.
    pub fn new(name: impl Into<String>, len: usize, capacity: usize) -> Self {
        Self {
            name: name.into(),
            len,
            capacity,
            bytes: 0,
        }
    }

    /// A hash table of entries of type `T` (the key and the value of a map): its buckets are the
    /// next power of two over eight sevenths of its capacity, with a byte of control each.
    pub fn hash<T>(name: impl Into<String>, len: usize, capacity: usize) -> Self {
        let buckets = if capacity == 0 {
            0
        } else {
            (capacity * 8 / 7).next_power_of_two()
        };
        Self {
            bytes: buckets * (std::mem::size_of::<T>() + 1),
            ..Self::new(name, len, capacity)
        }
    }

    /// A vector of `T`.
    pub fn list<T>(name: impl Into<String>, len: usize, capacity: usize) -> Self {
        Self {
            bytes: capacity * std::mem::size_of::<T>(),
            ..Self::new(name, len, capacity)
        }
    }
}

/// The units whose children are resident, in parts that can each be looked at on their own (see
/// [`BrickStore::eviction_candidates`]): a unit is in the part its coordinates hash to.
const SHARDS: usize = 4096;

#[derive(Clone, Debug)]
struct Resident {
    shards: Vec<HashMap<(usize, BrickCoord), Stamp>>,
    len: usize,
}

impl Resident {
    fn with_capacity(room: usize) -> Self {
        let mut resident = Self {
            shards: (0..SHARDS).map(|_| HashMap::new()).collect(),
            len: 0,
        };
        resident.reserve(room);
        resident
    }

    fn shard(key: &(usize, BrickCoord)) -> usize {
        let [x, y, z] = key.1.0.map(|c| u64::from(c as u32));
        let mut h = (key.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for c in [x, y, z] {
            h = (h.rotate_left(23) ^ c).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        }
        (h >> 40) as usize % SHARDS
    }

    /// Room for `room` more units, a part's share of it and a tenth over: the parts fill unevenly.
    fn reserve(&mut self, room: usize) {
        let each = room.div_ceil(SHARDS) * 11 / 10;
        for shard in &mut self.shards {
            shard.reserve(each);
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn capacity(&self) -> usize {
        self.shards.iter().map(HashMap::capacity).sum()
    }

    fn contains(&self, key: &(usize, BrickCoord)) -> bool {
        self.shards[Self::shard(key)].contains_key(key)
    }

    fn insert(&mut self, key: (usize, BrickCoord), stamp: Stamp) {
        if self.shards[Self::shard(&key)].insert(key, stamp).is_none() {
            self.len += 1;
        }
    }

    fn remove(&mut self, key: &(usize, BrickCoord)) {
        if self.shards[Self::shard(key)].remove(key).is_some() {
            self.len -= 1;
        }
    }

    fn part(&self, part: usize) -> impl Iterator<Item = (&(usize, BrickCoord), &Stamp)> {
        self.shards[part].iter()
    }
}

/// A unit that [`BrickStore::eviction_candidates`] found may be taken away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// How many levels out of its band it is, 0 in the band.
    pub gap: u32,
    /// The last pass at which it was wanted.
    pub stamp: u64,
    pub level: usize,
    pub coord: BrickCoord,
}

impl Candidate {
    /// The unit, as `(level, coord)`.
    pub fn unit(&self) -> (usize, BrickCoord) {
        (self.level, self.coord)
    }

    /// What the candidates are put in order by, first to be taken first: those furthest out of
    /// their band, and of those the longest unwanted.
    pub fn order(&self) -> (std::cmp::Reverse<u32>, u64, usize, BrickCoord) {
        (
            std::cmp::Reverse(self.gap),
            self.stamp,
            self.level,
            self.coord,
        )
    }
}

/// The bricks of level 0 that an Edit changed after the store began to stream (the diverged
/// bricks: not what the source makes), and how many are under each unit, so that what eviction
/// must keep is looked up and not worked out.
#[derive(Clone, Debug, Default)]
struct Diverged {
    bricks: HashSet<BrickCoord>,
    /// Bricks that became diverged (true) or stopped being, in order, that `under` does not count
    /// yet: noting a brick is one lookup, which is what the thread that makes Edits pays.
    pending: Vec<(BrickCoord, bool)>,
    /// For each unit `(level, coord)`, of every level from 1 to the Guard's: how many diverged
    /// bricks are under it.
    under: HashMap<(usize, BrickCoord), u32>,
}

impl Diverged {
    fn mark(&mut self, coord: BrickCoord) {
        if self.bricks.insert(coord) {
            self.pending.push((coord, true));
        }
    }

    fn unmark(&mut self, coord: BrickCoord) {
        if self.bricks.remove(&coord) {
            self.pending.push((coord, false));
        }
    }

    /// Counts what is pending, a brick at a time, until `until`. Whether it is all counted.
    fn sync(&mut self, floor: usize, until: Option<std::time::Instant>) -> bool {
        let mut counted = 0;
        for &(coord, added) in &self.pending {
            let mut over = coord;
            for level in 1..=floor {
                over = over.parent();
                if added {
                    *self.under.entry((level, over)).or_default() += 1;
                } else if let Some(n) = self.under.get_mut(&(level, over)) {
                    *n -= 1;
                    if *n == 0 {
                        self.under.remove(&(level, over));
                    }
                }
            }
            counted += 1;
            if counted % 32 == 0 && until.is_some_and(|t| std::time::Instant::now() >= t) {
                break;
            }
        }
        self.pending.drain(..counted);
        self.pending.is_empty()
    }

    /// Whether the unit `(level, coord)` holds a diverged brick, is over one, or is beside a unit
    /// that is. A coarse brick is made from the Voxels beside the bricks it covers (see
    /// [`BrickStore::refresh_lod`]), so what an Edit changed at a level is in the units over it and
    /// the ones beside them, and none of these can be made again from the source. (What is
    /// changed at a level is within one unit of the unit over the Edit, at every level: the
    /// neighbours of a brick that changed are within two bricks, and a parent is half as far.)
    fn pinned(&self, level: usize, coord: BrickCoord) -> bool {
        if self.under.is_empty() {
            return false;
        }
        (0..27).any(|step: i32| {
            let at =
                [0, 1, 2].map(|a| coord.0[a].saturating_add(step / 3i32.pow(a as u32) % 3 - 1));
            self.under.contains_key(&(level, BrickCoord(at)))
        })
    }
}

/// What a store that is streamed in keeps of what is resident.
#[derive(Clone, Debug)]
struct Streaming {
    /// Levels above this one are always resident: the Guard.
    floor: usize,
    /// The bricks `(level, coord)` whose eight children (the bricks of the level below that they
    /// cover) are resident, and when each was last wanted.
    resident: Resident,
    /// The passes of eviction so far (see [`BrickStore::begin_eviction_pass`]).
    clock: Stamp,
    /// Bricks removed by [`BrickStore::evict_children`], since the store began to stream.
    evicted: u64,
}

/// A tick of [`Streaming::clock`], which a unit was last wanted at. It is atomic so that
/// [`BrickStore::eviction_candidates`] can note the units that are wanted while only reading the
/// store, as the scene thread does without stopping the frame thread's reads.
#[derive(Debug, Default)]
struct Stamp(AtomicU64);

impl Clone for Stamp {
    fn clone(&self) -> Self {
        Self::new(self.get())
    }
}

impl Stamp {
    fn new(tick: u64) -> Self {
        Self(AtomicU64::new(tick))
    }

    fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    fn set(&self, tick: u64) {
        self.0.store(tick, Ordering::Relaxed);
    }
}

/// The six bricks that share a face with `coord`.
fn touching(coord: BrickCoord) -> impl Iterator<Item = BrickCoord> {
    (0..6).map(move |face| {
        let mut next = coord.0;
        next[face / 2] = if face % 2 == 0 {
            next[face / 2].saturating_sub(1)
        } else {
            next[face / 2].saturating_add(1)
        };
        BrickCoord(next)
    })
}

/// The parents of `bricks`.
fn moved_parents(bricks: &BTreeSet<BrickCoord>) -> BTreeSet<BrickCoord> {
    bricks.iter().map(|c| c.parent()).collect()
}

/// A [`BrickStore::refresh_lod`] in the middle: see [`BrickStore::step_lod`].
#[derive(Debug)]
pub struct LodWork {
    refresh: LodRefresh,
    beside: bool,
    /// The coarse level being made, from 1.
    level: usize,
    phase: LodPhase,
    /// The bricks of the level below that changed, and whose `solid_below` did, for this level.
    moved: BTreeSet<BrickCoord>,
    flipped: BTreeSet<BrickCoord>,
    /// The parents of `moved`, which are made from the source if they have no children held.
    direct: BTreeSet<BrickCoord>,
    /// The bricks of this level still to remake, the next last, and those to work `solid_below`
    /// out again of.
    queue: Vec<BrickCoord>,
    flag_queue: Vec<BrickCoord>,
}

#[derive(Clone, Copy, Debug)]
enum LodPhase {
    Start,
    Bricks,
    Flags,
}

impl LodWork {
    fn new(changed: BTreeSet<BrickCoord>, beside: bool) -> Self {
        Self {
            refresh: LodRefresh::default(),
            beside,
            level: 1,
            phase: LodPhase::Start,
            moved: changed,
            flipped: BTreeSet::new(),
            direct: BTreeSet::new(),
            queue: Vec::new(),
            flag_queue: Vec::new(),
        }
    }

    /// What the refresh did, once [`BrickStore::step_lod`] has said it is done.
    pub fn finish(self) -> LodRefresh {
        self.refresh
    }
}

/// What [`BrickStore::refresh_lod`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LodRefresh {
    /// How many coarse bricks it remade: the ancestors of the bricks it was told had changed,
    /// and those of the bricks touching them, since a coarse brick reads the Voxels beside the
    /// bricks it is made from. It stops at the first level at which no brick reads differently or
    /// changes whether it is held.
    pub remade: usize,
    /// The coarse bricks that read differently afterwards, as `(level, coordinate)`, finest
    /// level first and ascending within a level. These are the ones to send on to wherever
    /// the coarse levels are drawn from.
    pub changed: Vec<(usize, BrickCoord)>,
}

impl BrickStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of bricks held.
    pub fn len(&self) -> usize {
        self.bricks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bricks.is_empty()
    }

    pub fn brick(&self, coord: BrickCoord) -> Option<&Brick> {
        self.bricks.get(&coord)
    }

    /// The brick at `coord` to change in place, if one is held: one lookup for all the Voxels of
    /// the brick that are written (see [`Brick::clear`] and [`Brick::fill`]). It is for level 0
    /// and does not refresh the coarse levels, as [`Self::set`] does not: tell
    /// [`Self::refresh_lod`].
    ///
    /// In a store that streams this is how an Edit changes a brick that is held, so the brick is
    /// diverged, and kept from eviction ([`Self::evict_children`]).
    pub fn brick_mut(&mut self, coord: BrickCoord) -> Option<&mut Brick> {
        let brick = self.bricks.get_mut(&coord)?;
        if self.streaming.is_some() {
            self.diverged.mark(coord);
        }
        Some(brick)
    }

    /// Stores `brick` at `coord`, replacing any brick there. In a store that streams this is how
    /// an Edit puts in a brick that was not held (bricks that are streamed in are held with
    /// [`Self::hold`]): it is then diverged, and kept from eviction ([`Self::evict_children`]).
    pub fn insert(&mut self, coord: BrickCoord, brick: Brick) {
        self.bricks.insert(coord, brick);
        if self.streaming.is_some() {
            self.diverged.mark(coord);
        }
    }

    /// Removes the brick at `coord`, which then reads as the source gives it, or as air with none.
    ///
    /// In a store that streams, a brick that is not held reads as air once its parent's children
    /// are resident (all of them that are not air or proven solid are held), so removing one there puts back what
    /// the source makes of it, whether or not they are resident yet (they may arrive after, and
    /// not hold a brick that was held when they were asked for): this is how an Edit is taken
    /// back. [`Self::discard`] removes without that.
    pub fn remove(&mut self, coord: BrickCoord) -> Option<Brick> {
        let removed = self.bricks.remove(&coord);
        self.diverged.unmark(coord);
        if self.streaming.is_some()
            && let Some(source) = &self.source
            && !source.solid_below(0, coord)
        {
            let generated = source.brick(0, coord);
            if !generated.is_air() {
                self.bricks.insert(coord, generated);
            }
        }
        removed
    }

    /// Removes the brick at `coord` and nothing else, for a brick that was only held for a moment
    /// to work on, and was known to read as the source gives it when it was.
    pub fn discard(&mut self, coord: BrickCoord) -> Option<Brick> {
        self.diverged.unmark(coord);
        self.bricks.remove(&coord)
    }

    /// FNV-1a hash of the Planet's content: the coordinate and [`Brick::content_hash`] of every
    /// brick held, in ascending coordinate order. Bricks held as air count: inside the Planet's
    /// core a brick that is not held reads as rock, so one dug out to air differs from one never
    /// touched, and two stores that differ only in that must not hash alike.
    pub fn content_hash(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for coord in self.sorted_coords() {
            let brick = &self.bricks[&coord];
            let bytes = coord
                .0
                .into_iter()
                .flat_map(i32::to_le_bytes)
                .chain(brick.content_hash().to_le_bytes());
            for byte in bytes {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    /// Sets what a brick reads as where none is held, replacing any source, and remakes the
    /// pyramid to match. Set it before holding bricks that depend on it.
    pub fn set_source(&mut self, source: impl BrickSource + 'static) {
        static SOURCES: AtomicU64 = AtomicU64::new(0);
        self.source = Some(Arc::new(source));
        self.source_id = SOURCES.fetch_add(1, Ordering::Relaxed) + 1;
        self.build_lod(self.coarse.len());
    }

    /// The brick at `coord` of mip `level`, as it reads: the one held, or else what the source
    /// gives, or else air. Level 0 is this store's own bricks. Panics if there is no such level.
    pub fn brick_at(&self, level: usize, coord: BrickCoord) -> Cow<'_, Brick> {
        match self.level(level).expect("no such level").brick(coord) {
            Some(brick) => Cow::Borrowed(brick),
            None => Cow::Owned(self.unheld(level, coord)),
        }
    }

    /// Whether the source says that every Voxel of level 0 under the brick at `coord` of `level`
    /// is solid, for a brick that is not held (see [`BrickSource::solid_below`]).
    pub fn source_solid_below(&self, level: usize, coord: BrickCoord) -> bool {
        let Some(source) = &self.source else {
            return false;
        };
        if self.source_id == 0 {
            return source.solid_below(level, coord);
        }
        remembered(self.source_id, level, coord, || {
            source.solid_below(level, coord)
        })
    }

    /// Whether the store has a source that does not say that every Voxel of level 0 under the
    /// brick at `coord` of `level` is air (see [`BrickSource::air_below`]): a brick that reads
    /// as air may then have ground under it. Without a source a brick that is not held has
    /// nothing under it.
    pub fn may_have_ground_below(&self, level: usize, coord: BrickCoord) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| !source.air_below(level, coord))
    }

    /// Holds `brick` as the brick at `coord` of mip `level`, replacing any held there: the way a
    /// coarse brick that was generated directly (see [`BrickSource`]) gets into the pyramid, as
    /// [`Self::insert`] does for level 0. Like it, this does not touch the levels above: the
    /// brick is what the caller says it is. Whether it is solid all the way down is what the
    /// source says. Panics if there is no such level.
    pub fn insert_at(&mut self, level: usize, coord: BrickCoord, brick: Brick) {
        let solid = level > 0 && self.source_solid_below(level, coord);
        self.hold(level, coord, brick, solid);
    }

    /// [`Self::insert_at`], with whether every Voxel of level 0 under the brick is solid given
    /// (what the source says, see [`BrickSource::solid_below`]), for a caller that worked it out
    /// on several threads.
    pub fn hold(&mut self, level: usize, coord: BrickCoord, brick: Brick, solid: bool) {
        if level == 0 {
            self.bricks.insert(coord, brick);
            return;
        }
        let store = &mut self.coarse[level - 1];
        store.bricks.insert(coord, brick);
        if solid {
            store.solid_below.insert(coord);
        } else {
            store.solid_below.remove(&coord);
        }
    }

    /// Makes room for `held[level]` more bricks held at each level (as many entries as there are
    /// levels given, from level 0), so that the tables do not double, and move every entry, in
    /// the middle of a frame. Allocating a table commits little of it (its control bytes, on
    /// Linux), but hashing spreads the entries over every page, so it is in memory in full once a
    /// few percent of it is used; on Windows the allocation commits all of it at once.
    pub fn reserve(&mut self, held: &[usize]) {
        for (level, &n) in held.iter().enumerate() {
            if level == 0 {
                self.bricks.reserve(n);
            } else if let Some(store) = self.coarse.get_mut(level - 1) {
                store.bricks.reserve(n);
                store.solid_below.reserve(n);
            }
        }
    }

    /// Makes room for `resident` more bricks whose children are resident, as [`Self::reserve`]
    /// does for the bricks held. Before the store streams, the room is kept for the resident set
    /// that [`Self::stream_from`] starts.
    pub fn reserve_resident(&mut self, resident: usize) {
        match &mut self.streaming {
            Some(streaming) => streaming.resident.reserve(resident),
            None => self.resident_room = self.resident_room.max(resident),
        }
    }

    /// The size of every table that [`Self::reserve`] and [`Self::reserve_resident`] make room
    /// in: the bricks held at each level, the coarse levels' `solid_below`, and the resident set.
    pub fn table_sizes(&self) -> Vec<TableSize> {
        let mut sizes = vec![TableSize::hash::<(BrickCoord, Brick)>(
            "store level 0 bricks",
            self.bricks.len(),
            self.bricks.capacity(),
        )];
        for (level, store) in self.coarse.iter().enumerate() {
            let level = level + 1;
            sizes.push(TableSize::hash::<(BrickCoord, Brick)>(
                format!("store level {level} bricks"),
                store.bricks.len(),
                store.bricks.capacity(),
            ));
            sizes.push(TableSize::hash::<BrickCoord>(
                format!("store level {level} solid_below"),
                store.solid_below.len(),
                store.solid_below.capacity(),
            ));
        }
        if let Some(streaming) = &self.streaming {
            sizes.push(TableSize::hash::<((usize, BrickCoord), Stamp)>(
                "store resident",
                streaming.resident.len(),
                streaming.resident.capacity(),
            ));
        }
        sizes
    }

    /// Makes this a store whose bricks are streamed in: the eight bricks under a brick of a level
    /// above `floor` are always resident (the Guard is every brick of the levels from `floor` up),
    /// and those under a brick of `floor` or below are resident once
    /// [`Self::set_children_resident`] says so. A renderer splits a brick only when its children
    /// are resident ([`Self::children_resident`]), and draws it whole, a coarser frame, until then. A
    /// store that does not stream has everything resident.
    pub fn stream_from(&mut self, floor: usize) {
        self.streaming = Some(Streaming {
            floor,
            resident: Resident::with_capacity(self.resident_room),
            clock: Stamp::default(),
            evicted: 0,
        });
    }

    /// Whether the eight bricks under the brick at `coord` of `level` are resident, so that they
    /// can be read without generating anything. Always true unless the store streams.
    pub fn children_resident(&self, level: usize, coord: BrickCoord) -> bool {
        self.streaming
            .as_ref()
            .is_none_or(|s| level > s.floor || s.resident.contains(&(level, coord)))
    }

    /// Records that the eight bricks under the brick at `coord` of `level` are resident: the ones
    /// that are drawable are held (not air, not buried: see [`crate::children_to_generate`]; the
    /// rest read as the source gives them). Does nothing unless the store streams.
    pub fn set_children_resident(&mut self, level: usize, coord: BrickCoord) {
        if let Some(s) = &mut self.streaming {
            let now = s.clock.get();
            s.resident.insert((level, coord), Stamp::new(now));
        }
    }

    /// Sets how much the store may hold. It only takes effect once the store streams.
    pub fn set_budget(&mut self, budget: Budget) {
        self.budget = Some(budget);
    }

    /// How much the store may hold, if it was told ([`Self::set_budget`]).
    pub fn budget(&self) -> Option<Budget> {
        self.budget
    }

    /// What the budget is of, for a store that streams: the bricks held under the Guard (the levels
    /// below its first, see [`Self::stream_from`]) and the bricks whose children are resident, an
    /// entry each, which is all that a brick of air costs. Zero unless the store streams.
    pub fn streamed_len(&self) -> usize {
        self.streaming.as_ref().map_or(0, |s| {
            s.resident.len()
                + (0..s.floor)
                    .filter_map(|level| self.level(level))
                    .map(|level| level.bricks.len())
                    .sum::<usize>()
        })
    }

    /// Whether the store holds more than its budget's [`Budget::limit`].
    pub fn over_limit(&self) -> bool {
        self.budget
            .is_some_and(|budget| self.streamed_len() > budget.limit())
    }

    /// Whether the store holds more than the [`Budget::bricks`] of its budget, which eviction
    /// that has started goes on until it does not.
    pub fn over_budget(&self) -> bool {
        self.budget
            .is_some_and(|budget| self.streamed_len() > budget.bricks)
    }

    /// Bricks removed by [`Self::evict_children`] since the store began to stream.
    pub fn evicted_len(&self) -> u64 {
        self.streaming.as_ref().map_or(0, |s| s.evicted)
    }

    /// The bricks of level 0 that an Edit changed since the store began to stream: the diverged
    /// bricks, which are kept from eviction.
    pub fn diverged_len(&self) -> usize {
        self.diverged.bricks.len()
    }

    /// Whether diverged bricks have come or gone that [`Self::sync_diverged`] has not counted yet,
    /// so that [`Self::evict_children`] does not know what to keep.
    pub fn diverged_pending(&self) -> bool {
        !self.diverged.pending.is_empty()
    }

    /// Counts the bricks that became diverged, or stopped being, since this was last called, under
    /// the units over them, for `until` at the most (one is always counted). Whether it is done. A
    /// thread that evicts calls it before it does ([`Self::diverged_pending`]), so that the thread
    /// that makes the Edits only notes the brick it changed, and does not walk up the pyramid for
    /// it.
    pub fn sync_diverged(&mut self, until: Option<std::time::Instant>) -> bool {
        match &self.streaming {
            Some(s) => self.diverged.sync(s.floor, until),
            None => true,
        }
    }

    /// Starts a pass of eviction: returns its number, which stamps the units that
    /// [`Self::eviction_candidates`] finds wanted. A unit's stamp is the last pass at which it was
    /// wanted, so "least recently used" means wanted at the earliest pass.
    pub fn begin_eviction_pass(&self) -> u64 {
        self.streaming.as_ref().map_or(0, |s| {
            let pass = s.clock.get() + 1;
            s.clock.set(pass);
            pass
        })
    }

    /// How many parts [`Self::eviction_candidates`] looks at the resident units in.
    pub const EVICTION_PARTS: usize = SHARDS;

    /// Looks at one `part` of the units whose children are resident, of the [`Self::EVICTION_PARTS`],
    /// and adds to `out` those that eviction may take away: not wanted (`wanted` says which are,
    /// and they are stamped with the `pass`), not the Guard's, and not over or beside a diverged
    /// brick. `gap` says how far a unit is out of its band, 0 for in the band. It only reads the
    /// store, and a part is small (a four-thousandth of the units), so a thread that shares the store
    /// can let go of it between parts: a unit that arrives meanwhile is looked at in the next pass.
    pub fn eviction_candidates(
        &self,
        part: usize,
        pass: u64,
        wanted: impl Fn(usize, BrickCoord) -> bool,
        gap: impl Fn(usize, BrickCoord) -> u32,
        out: &mut Vec<Candidate>,
    ) {
        let Some(s) = &self.streaming else {
            return;
        };
        for (&(level, coord), stamp) in s.resident.part(part) {
            if level > s.floor {
                continue;
            }
            if wanted(level, coord) {
                stamp.set(pass);
            } else if !self.diverged.pinned(level, coord) {
                out.push(Candidate {
                    gap: gap(level, coord),
                    stamp: stamp.get(),
                    level,
                    coord,
                });
            }
        }
    }

    /// Takes away the eight bricks under the brick at `coord` of `level`, and notes that they are
    /// not resident, so that a renderer draws the brick whole and asks for them again when the
    /// bands split it: how a store that streams stays within its budget. Returns how many bricks
    /// it removed (air is not held, so often fewer than eight), or `None`, with nothing done,
    /// when it must not:
    /// - their children are not resident, or are the Guard's;
    /// - they hold or are over or beside a diverged brick (the journal is the source of truth, but
    ///   until it has everything the store has the only copy of an Edit, and a coarse brick over
    ///   one is made from it, not from the source), or such bricks are not counted yet
    ///   ([`Self::sync_diverged`]);
    /// - `wanted` says they are wanted: the caller's drawn set and what is in flight;
    /// - a brick under them has its children resident (those go first: the units are a tree, and
    ///   none is left under one that is gone).
    ///
    /// They come back as the source gives them, by [`Self::hold_children`], when they are needed.
    pub fn evict_children(
        &mut self,
        level: usize,
        coord: BrickCoord,
        wanted: impl Fn(usize, BrickCoord) -> bool,
    ) -> Option<usize> {
        let s = self.streaming.as_ref()?;
        if level == 0
            || level > s.floor
            || !s.resident.contains(&(level, coord))
            || self.diverged_pending()
            || self.diverged.pinned(level, coord)
            || wanted(level, coord)
            || (level > 1
                && coord
                    .children()
                    .iter()
                    .any(|&c| s.resident.contains(&(level - 1, c))))
        {
            return None;
        }
        let mut removed = 0;
        for child in coord.children() {
            let store = if level == 1 {
                &mut *self
            } else {
                &mut self.coarse[level - 2]
            };
            removed += usize::from(store.bricks.remove(&child).is_some());
            store.solid_below.remove(&child);
        }
        let s = self.streaming.as_mut().expect("it was streaming");
        s.resident.remove(&(level, coord));
        s.evicted += removed as u64;
        Some(removed)
    }

    /// Holds the bricks of `bricks` as the bricks under the brick at `coord` of `level` (at least
    /// 1), except those already held, which are never replaced (an Edit's are among them), and
    /// records that its children are resident (see [`Self::children_resident`]). The others read as
    /// the source gives them, so `bricks` is what [`crate::children_from`] returned for them. Returns
    /// how many it held.
    pub fn hold_children(
        &mut self,
        level: usize,
        coord: BrickCoord,
        bricks: Vec<(BrickCoord, Brick)>,
    ) -> usize {
        let below = level - 1;
        let bricks = bricks
            .into_iter()
            .map(|(child, brick)| {
                let solid =
                    !self.holds(below, child) && below > 0 && self.source_solid_below(below, child);
                (child, brick, solid)
            })
            .collect();
        self.hold_children_solid(level, coord, bricks)
    }

    /// [`Self::hold_children`], with whether each brick is solid all the way down given (what
    /// [`crate::children_with_solid`] returns), for a caller that worked it out on another thread.
    pub fn hold_children_solid(
        &mut self,
        level: usize,
        coord: BrickCoord,
        bricks: Vec<(BrickCoord, Brick, bool)>,
    ) -> usize {
        let below = level - 1;
        let mut held = 0;
        for (child, brick, solid) in bricks {
            if self.holds(below, child) {
                continue;
            }
            self.hold(below, child, brick, solid);
            held += 1;
        }
        self.set_children_resident(level, coord);
        held
    }

    /// Whether the brick at `coord` of `level`, which the caller has found is not held, is full as
    /// far as the store knows without generating it: it streams, its parent's children are
    /// resident, and the source proves it solid all the way down. That is a brick that the loader
    /// left out as buried (see [`crate::children_to_generate`]), but also one that is drawn since
    /// an Edit exposed it, and one whose Edit was undone ([`Self::remove`]). Reading it
    /// ([`Self::unheld_brick`]) generates it, which is exact and costs a brick, so a reader that
    /// only needs to know that it is full (the renderer's tree) asks this first.
    pub fn reads_full_from_source(&self, level: usize, coord: BrickCoord) -> bool {
        self.streaming.is_some()
            && self.source.is_some()
            && self.children_resident(level + 1, coord.parent())
            && self.source_solid_below(level, coord)
    }

    /// The brick at `coord` of mip `level`, which the caller has found is not held, as it reads
    /// ([`Self::brick_at`] without looking for it again).
    pub fn unheld_brick(&self, level: usize, coord: BrickCoord) -> Brick {
        self.unheld(level, coord)
    }

    /// What a brick that is not held reads as: a handle to the source, which a thread that
    /// generates bricks for this store can use without the store.
    pub fn source(&self) -> Option<Arc<dyn BrickSource>> {
        self.source.clone()
    }

    /// Whether the brick at `coord` of mip `level` is held. Only whether it is: a brick that is
    /// not held reads as the source gives it.
    pub fn holds(&self, level: usize, coord: BrickCoord) -> bool {
        self.level(level)
            .is_some_and(|l| l.bricks.contains_key(&coord))
    }

    /// Whether the brick at `coord` of mip `level` is full (see [`Brick::is_full`]), as far as is
    /// known without generating it: in a store that streams, a brick that is not held is full
    /// only if the source says it is solid all the way down. Otherwise the same as
    /// `brick_at(level, coord).is_full()`.
    pub fn is_full_known(&self, level: usize, coord: BrickCoord) -> bool {
        match self.level(level).and_then(|l| l.bricks.get(&coord)) {
            Some(brick) => brick.is_full(),
            None if self.streaming.is_some() => self.source_solid_below(level, coord),
            None => self.brick_at(level, coord).is_full(),
        }
    }

    /// Whether every Voxel of level 0 under the brick at `coord` of mip `level` is solid, exactly
    /// for a brick that is held. A coarse brick that reads as full may still have air under it,
    /// since a coarse Voxel is solid when half of its children are: the pyramid keeps this beside
    /// the brick, made with it. Where the store holds no brick the source says (see
    /// [`BrickSource::solid_below`]): exact when it says so, and otherwise not known, at level 0
    /// too in a store that streams (see [`Self::is_full_known`]), which does not generate a brick
    /// to find out. A caller that asks beside many bricks can rely on that.
    pub fn solid_below(&self, level: usize, coord: BrickCoord) -> bool {
        let Some(store) = self.level(level) else {
            return false;
        };
        match store.bricks.get(&coord) {
            Some(brick) if level == 0 => brick.is_full(),
            Some(_) => store.solid_below.contains(&coord),
            None if level == 0 => self.is_full_known(0, coord),
            None => self.source_solid_below(level, coord),
        }
    }

    /// The Voxel at `coord` of mip `level`, as [`Self::brick_at`] reads it: `None` for air.
    pub fn voxel_at(&self, level: usize, coord: VoxelCoord) -> Option<Material> {
        self.brick_at(level, coord.brick()).get(coord.local())
    }

    /// Replaces the pyramid with `coarse_levels` levels above this one. The Planet needs about
    /// 20 or 21 to reach from 5 cm Voxels to its disc.
    pub fn build_lod(&mut self, coarse_levels: usize) {
        self.coarse = vec![BrickStore::new(); coarse_levels];
        self.remake_above(self.bricks.keys().copied().collect(), false);
    }

    /// Number of mip levels, counting this one as level 0: 1 when there is no pyramid.
    pub fn level_count(&self) -> usize {
        1 + self.coarse.len()
    }

    /// The bricks held at mip level `level`, or `None` beyond the pyramid. Level 0 is this
    /// store. Reading it ignores the source: use [`Self::brick_at`] for that.
    pub fn level(&self, level: usize) -> Option<&BrickStore> {
        match level {
            0 => Some(self),
            n => self.coarse.get(n - 1),
        }
    }

    /// Makes the pyramid match the level 0 bricks at `changed` after they were changed (held,
    /// replaced or removed), by remaking only the coarse bricks over them and over the bricks
    /// that touch them, as far up as any brick reads differently. Does nothing without a
    /// pyramid. The result is the pyramid that [`Self::build_lod`] would make.
    pub fn refresh_lod(&mut self, changed: impl IntoIterator<Item = BrickCoord>) -> LodRefresh {
        self.remake_above(changed.into_iter().collect(), true)
    }

    /// FNV-1a hash of the pyramid: the number and [`Self::content_hash`] of each coarse level,
    /// finest first. Level 0 is left to `content_hash`, so what the two hash is disjoint.
    pub fn lod_hash(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for (n, level) in (1u32..).zip(&self.coarse) {
            let bytes = n
                .to_le_bytes()
                .into_iter()
                .chain(level.content_hash().to_le_bytes());
            for byte in bytes {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    /// Remakes the coarse bricks over the level 0 bricks `changed`, level by level.
    ///
    /// A coarse brick is made from the eight bricks it covers, and from the Voxels beside them:
    /// whether a Voxel of the eight is exposed to air depends on the Voxels next to it, in the
    /// bricks that touch theirs (see [`downsample`]). So a brick that changed makes the coarse
    /// bricks over it and over the six bricks touching it be remade, when `beside` is set. It
    /// is not set to build every brick afresh, where all are remade anyway.
    ///
    /// A level is remade only from the bricks of the level below that changed, held or read
    /// differently. When none did, nothing above it can have, and the rest is left alone.
    fn remake_above(&mut self, changed: BTreeSet<BrickCoord>, beside: bool) -> LodRefresh {
        let mut work = LodWork::new(changed, beside);
        let finished = self.step_lod(&mut work, None);
        debug_assert!(finished);
        work.refresh
    }

    /// Starts a [`BrickStore::refresh_lod`] that is done in steps ([`BrickStore::step_lod`]), so
    /// that a thread that shares the store behind a lock can let go of it between steps.
    pub fn begin_lod(&self, changed: impl IntoIterator<Item = BrickCoord>) -> LodWork {
        LodWork::new(changed.into_iter().collect(), true)
    }

    /// Does the next part of `work`, a coarse brick at a time, until it is done or `until` has
    /// passed (one coarse brick is always made). Returns whether it is done, and then
    /// [`LodWork::finish`] has what it did. Run to the end it is [`BrickStore::refresh_lod`],
    /// whatever the steps and whatever is read between them, as long as the Voxels of level 0 and
    /// the source are not changed by anything but a later refresh in between.
    ///
    /// The coarse levels are between the old pyramid and the new one until it is done: nothing
    /// but the thread that steps should read them.
    pub fn step_lod(&mut self, work: &mut LodWork, until: Option<std::time::Instant>) -> bool {
        loop {
            if work.level > self.coarse.len() {
                return true;
            }
            match work.phase {
                LodPhase::Start => {
                    let level = work.level;
                    let over: BTreeSet<_> = work
                        .moved
                        .iter()
                        .flat_map(|&coord| {
                            let touching =
                                work.beside.then(|| touching(coord)).into_iter().flatten();
                            std::iter::once(coord).chain(touching)
                        })
                        .map(BrickCoord::parent)
                        .collect();
                    let flags: BTreeSet<_> = work.flipped.iter().map(|c| c.parent()).collect();
                    work.direct = moved_parents(&work.moved);
                    work.flag_queue = over.union(&flags).copied().collect();
                    work.queue = over.into_iter().collect();
                    work.queue.reverse();
                    work.moved = BTreeSet::new();
                    work.flipped = BTreeSet::new();
                    work.phase = LodPhase::Bricks;
                    debug_assert!(level >= 1);
                }
                LodPhase::Bricks => {
                    let level = work.level;
                    while let Some(coord) = work.queue.pop() {
                        let held = self.remake(level, coord, work.direct.contains(&coord));
                        work.refresh.remade += 1;
                        let (was_held, before) = match held {
                            Some(brick) => (true, brick),
                            None => (false, self.unheld(level, coord)),
                        };
                        let now_held = self.coarse[level - 1].bricks.contains_key(&coord);
                        let read_differently = before != *self.brick_at(level, coord);
                        if read_differently {
                            work.refresh.changed.push((level, coord));
                        }
                        if read_differently || was_held != now_held {
                            work.moved.insert(coord);
                        }
                        if until.is_some_and(|t| std::time::Instant::now() >= t) {
                            if work.queue.is_empty() {
                                work.phase = LodPhase::Flags;
                            }
                            return false;
                        }
                    }
                    work.phase = LodPhase::Flags;
                }
                LodPhase::Flags => {
                    let level = work.level;
                    for coord in std::mem::take(&mut work.flag_queue) {
                        if self.remake_solid_below(level, coord) {
                            work.flipped.insert(coord);
                        }
                    }
                    if work.moved.is_empty() && work.flipped.is_empty() {
                        return true;
                    }
                    work.level += 1;
                    work.phase = LodPhase::Start;
                }
            }
        }
    }

    /// Remakes brick `coord` of coarse level `level` from the eight bricks of the level below:
    /// held if any of them is held, and not held otherwise. Returns the brick that was held.
    ///
    /// In a store that streams, a coarse brick may be held with none of its children held: the
    /// source made it (the Guard, and the bricks of a streamed band). Then it is not remade from
    /// them, which would read them as air. If `direct` (it is over a brick that changed, say one
    /// that an Edit taken back is not held any more) it is made as the source makes it, and
    /// otherwise it is left as it is.
    fn remake(&mut self, level: usize, coord: BrickCoord, direct: bool) -> Option<Brick> {
        let finer = self.level(level - 1).expect("the level below exists");
        let children = coord.children();
        let any_held = children.iter().any(|c| finer.bricks.contains_key(c));
        if !any_held && self.streaming.is_some() {
            let kept = self.coarse[level - 1].bricks.get(&coord).cloned();
            if direct && let Some(source) = &self.source {
                let made = source.brick(level, coord);
                let coarse = &mut self.coarse[level - 1].bricks;
                if made.is_air() {
                    coarse.remove(&coord);
                } else {
                    coarse.insert(coord, made);
                }
            }
            return kept;
        }
        let brick = any_held.then(|| {
            let read = children.map(|child| self.brick_at(level - 1, child));
            let around = |at: [i32; 3]| {
                self.brick_at(
                    level - 1,
                    BrickCoord([0, 1, 2].map(|a| children[0].0[a] + at[a])),
                )
            };
            downsample(read.each_ref().map(|c| &**c), around)
        });
        let coarse = &mut self.coarse[level - 1].bricks;
        match brick {
            Some(brick) => coarse.insert(coord, brick),
            None => coarse.remove(&coord),
        }
    }

    /// Works out [`Self::solid_below`] of the held brick `coord` of coarse level `level` again,
    /// from the brick and the eight under it. Returns whether it changed.
    fn remake_solid_below(&mut self, level: usize, coord: BrickCoord) -> bool {
        let full = self.coarse[level - 1]
            .bricks
            .get(&coord)
            .is_some_and(Brick::is_full);
        let solid = full
            && coord
                .children()
                .iter()
                .all(|&c| self.solid_below(level - 1, c));
        let store = &mut self.coarse[level - 1];
        let was_solid = if solid {
            !store.solid_below.insert(coord)
        } else {
            store.solid_below.remove(&coord)
        };
        solid != was_solid
    }

    /// What the brick at `coord` of mip `level` reads as where none is held there.
    fn unheld(&self, level: usize, coord: BrickCoord) -> Brick {
        match &self.source {
            // In a store that streams, the bricks under a parent whose children are resident are all
            // held but the air, and what the source proves solid (which includes the buried ones
            // that [`crate::children_to_generate`] leaves out): to read the rest is not to
            // generate them.
            Some(source)
                if self.streaming.is_some()
                    && self.children_resident(level + 1, coord.parent())
                    && !self.source_solid_below(level, coord) =>
            {
                Brick::AIR
            }
            Some(source) => source.brick(level, coord),
            None => Brick::AIR,
        }
    }

    /// Coordinates of all bricks held, in ascending order.
    pub fn sorted_coords(&self) -> Vec<BrickCoord> {
        let mut coords: Vec<_> = self.bricks.keys().copied().collect();
        coords.sort_unstable();
        coords
    }

    /// The Voxel at `coord`: `None` for air, including where no brick is held.
    pub fn get(&self, coord: VoxelCoord) -> Option<Material> {
        self.brick(coord.brick())?.get(coord.local())
    }

    /// Sets the Voxel at `coord`, creating its brick if needed. Setting air where no brick is
    /// held allocates nothing.
    pub fn set(&mut self, coord: VoxelCoord, voxel: Option<Material>) {
        let brick = coord.brick();
        if voxel.is_none() && !self.bricks.contains_key(&brick) {
            return;
        }
        if self.streaming.is_some() {
            self.diverged.mark(brick);
        }
        self.bricks
            .entry(brick)
            .or_insert(Brick::AIR)
            .set(coord.local(), voxel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lod::Interior;

    const ROCK: Material = Material(3);

    /// Counts the bricks it is asked for, which are rock, and proves rock below y = 0.
    #[derive(Debug, Default)]
    struct Counting(std::sync::atomic::AtomicUsize);

    impl BrickSource for Counting {
        fn brick(&self, _: usize, _: BrickCoord) -> Brick {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Brick::solid(ROCK)
        }
        fn solid_below(&self, _: usize, coord: BrickCoord) -> bool {
            coord.0[1] < 0
        }
    }

    /// Says what a brick is solid below by its height, and counts the times it is asked.
    #[derive(Debug)]
    struct Asked {
        asked: Arc<std::sync::atomic::AtomicUsize>,
        deep_is_solid: bool,
    }

    impl BrickSource for Asked {
        fn brick(&self, _: usize, _: BrickCoord) -> Brick {
            Brick::AIR
        }
        fn solid_below(&self, _: usize, coord: BrickCoord) -> bool {
            self.asked
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (coord.0[1] < 0) == self.deep_is_solid
        }
    }

    #[test]
    fn what_the_source_says_of_a_brick_is_asked_once_and_remembered_per_source() {
        use std::sync::atomic::Ordering::Relaxed;

        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut store = BrickStore::new();
        store.set_source(Asked {
            asked: asked.clone(),
            deep_is_solid: true,
        });
        let (deep, shallow) = (BrickCoord([3, -2, 1]), BrickCoord([3, 2, 1]));
        for _ in 0..5 {
            assert!(store.source_solid_below(2, deep));
            assert!(!store.source_solid_below(2, shallow));
        }
        assert_eq!(asked.load(Relaxed), 2);
        // Another source answers for itself, whatever the first said of the same brick.
        let mut other = BrickStore::new();
        other.set_source(Asked {
            asked: asked.clone(),
            deep_is_solid: false,
        });
        assert!(!other.source_solid_below(2, deep));
        assert!(other.source_solid_below(2, shallow));
    }

    #[test]
    fn holding_children_with_their_solid_below_is_holding_children() {
        let mut a = BrickStore::new();
        a.set_source(Interior {
            radius: 1000,
            material: ROCK,
        });
        a.build_lod(2);
        a.stream_from(1);
        let mut b = a.clone();
        let parent = BrickCoord([0, 0, 0]);
        let source = a.source().unwrap();
        let bricks = crate::children_from(&*source, 2, parent, |_| false);
        let with = crate::children_with_solid(&*source, 2, parent, |_| false);
        assert_eq!(
            a.hold_children(2, parent, bricks),
            b.hold_children_solid(2, parent, with)
        );
        for c in parent.children() {
            assert_eq!(a.solid_below(1, c), b.solid_below(1, c));
            assert_eq!(a.brick_at(1, c).is_full(), b.brick_at(1, c).is_full());
        }
        assert!(b.children_resident(2, parent));
    }

    #[test]
    fn a_child_left_out_as_buried_is_not_held_and_reads_as_the_source_gives_it() {
        let mut store = BrickStore::new();
        store.set_source(Counting::default());
        store.build_lod(2);
        store.stream_from(1);
        let source = store.source().unwrap();
        // Children at y = -2 (buried) and y = -1 (drawn, with y = 0 above, not known solid).
        let parent = BrickCoord([0, -1, 0]);
        let bricks = crate::children_from(&*source, 1, parent, |_| false);
        assert_eq!(bricks.len(), 4);
        let before = store.reads_full_from_source(0, BrickCoord([0, -2, 0]));
        assert!(
            !before,
            "it is not so until its parent's children are resident"
        );
        assert_eq!(store.hold_children(1, parent, bricks), 4);
        for child in parent.children() {
            let buried = child.0[1] == -2;
            assert_eq!(
                !store.holds(0, child) && store.reads_full_from_source(0, child),
                buried,
                "{child:?}"
            );
            assert_eq!(store.holds(0, child), !buried, "{child:?}");
            assert!(store.solid_below(0, child));
            assert_eq!(*store.brick_at(0, child), Brick::solid(ROCK), "{child:?}");
        }
        assert_eq!(
            store.streamed_len(),
            5,
            "the unit and the four held, not the four left out"
        );
    }

    #[test]
    fn an_empty_store_reads_air_everywhere() {
        let store = BrickStore::new();
        assert_eq!(store.get(VoxelCoord([5, -5, 1_000_000])), None);
        assert!(store.is_empty());
    }

    #[test]
    fn set_then_get_by_voxel_coordinate_including_negatives() {
        let mut store = BrickStore::new();
        for v in [[0, 0, 0], [-1, -1, -1], [7, 8, -9], [-100, 200, -300]] {
            store.set(VoxelCoord(v), Some(ROCK));
            assert_eq!(store.get(VoxelCoord(v)), Some(ROCK), "{v:?}");
        }
        assert_eq!(store.get(VoxelCoord([1, 0, 0])), None);
        assert_eq!(store.len(), 4);
    }

    #[test]
    fn voxels_on_either_side_of_zero_live_in_different_bricks() {
        let mut store = BrickStore::new();
        store.set(VoxelCoord([-1, 0, 0]), Some(ROCK));
        store.set(VoxelCoord([0, 0, 0]), Some(Material(4)));
        assert_eq!(
            store.sorted_coords(),
            [BrickCoord([-1, 0, 0]), BrickCoord([0, 0, 0])]
        );
        assert_eq!(store.get(VoxelCoord([-1, 0, 0])), Some(ROCK));
        assert_eq!(store.get(VoxelCoord([0, 0, 0])), Some(Material(4)));
    }

    #[test]
    fn setting_air_in_empty_space_allocates_no_brick() {
        let mut store = BrickStore::new();
        store.set(VoxelCoord([9, 9, 9]), None);
        assert!(store.is_empty());
    }

    #[test]
    fn remove_makes_a_brick_read_as_air_again() {
        let mut store = BrickStore::new();
        store.set(VoxelCoord([1, 2, 3]), Some(ROCK));
        assert!(store.remove(BrickCoord([0, 0, 0])).is_some());
        assert_eq!(store.get(VoxelCoord([1, 2, 3])), None);
        assert!(store.remove(BrickCoord([0, 0, 0])).is_none());
    }

    #[test]
    fn content_hash_ignores_insertion_order() {
        let (mut a, mut b) = (BrickStore::new(), BrickStore::new());
        a.insert(BrickCoord([1, 0, 0]), Brick::solid(ROCK));
        a.insert(BrickCoord([-1, 0, 0]), Brick::solid(Material(4)));
        b.insert(BrickCoord([-1, 0, 0]), Brick::solid(Material(4)));
        b.insert(BrickCoord([1, 0, 0]), Brick::solid(ROCK));
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn content_hash_tells_a_brick_held_as_air_from_one_not_held() {
        let (mut held, mut absent) = (BrickStore::new(), BrickStore::new());
        for store in [&mut held, &mut absent] {
            store.insert(BrickCoord([1, 0, 0]), Brick::solid(ROCK));
        }
        held.insert(BrickCoord([9, 9, 9]), Brick::AIR);
        assert_ne!(held.content_hash(), absent.content_hash());
    }

    #[test]
    fn content_hash_sees_where_a_brick_is_and_what_is_in_it() {
        let hash = |coord, material| {
            let mut store = BrickStore::new();
            store.insert(BrickCoord(coord), Brick::solid(Material(material)));
            store.content_hash()
        };
        let base = hash([1, 0, 0], 3);
        assert_ne!(base, hash([0, 1, 0], 3));
        assert_ne!(base, hash([1, 0, 0], 4));
        assert_ne!(base, BrickStore::new().content_hash());
    }

    #[test]
    fn sorted_coords_ignore_insertion_order() {
        let mut store = BrickStore::new();
        for c in [[2, 0, 0], [-1, 5, 0], [0, 0, 1], [-1, 4, 7]] {
            store.insert(BrickCoord(c), Brick::solid(ROCK));
        }
        assert_eq!(
            store.sorted_coords(),
            [
                BrickCoord([-1, 4, 7]),
                BrickCoord([-1, 5, 0]),
                BrickCoord([0, 0, 1]),
                BrickCoord([2, 0, 0]),
            ]
        );
    }

    /// A 16-Voxel cube of solid ROCK: the eight level 0 bricks 0..=1 on each axis, which are
    /// the children of level 1 brick (0, 0, 0).
    fn block() -> BrickStore {
        let mut store = BrickStore::new();
        for i in 0..8 {
            store.insert(BrickCoord([i & 1, i >> 1 & 1, i >> 2]), Brick::solid(ROCK));
        }
        store
    }

    /// 4×4×4 solid bricks about the origin, in three Materials that vary from brick to brick.
    fn banded() -> BrickStore {
        let mut store = BrickStore::new();
        for i in 0..64 {
            let at = [i % 4 - 2, i / 4 % 4 - 2, i / 16 - 2];
            let material = Material(1 + at.iter().sum::<i32>().rem_euclid(3) as u16);
            store.insert(BrickCoord(at), Brick::solid(material));
        }
        store
    }

    /// The coarse bricks, as `(level, coordinate)`, that read differently in the two stores.
    fn differing(before: &BrickStore, after: &BrickStore) -> Vec<(usize, BrickCoord)> {
        let mut differing = Vec::new();
        for level in 1..before.level_count() {
            let mut coords = before.level(level).unwrap().sorted_coords();
            coords.extend(after.level(level).unwrap().sorted_coords());
            coords.sort();
            coords.dedup();
            differing.extend(
                coords
                    .into_iter()
                    .filter(|&c| before.brick_at(level, c) != after.brick_at(level, c))
                    .map(|c| (level, c)),
            );
        }
        differing
    }

    /// The pyramid that `build_lod` makes of `store`'s level 0 bricks and source.
    fn rebuilt(store: &BrickStore) -> BrickStore {
        let mut again = store.clone();
        again.build_lod(store.level_count() - 1);
        again
    }

    #[test]
    fn a_store_without_a_pyramid_has_one_level_and_refreshing_it_does_nothing() {
        let mut store = block();
        assert_eq!(store.level_count(), 1);
        assert!(store.level(1).is_none());
        assert_eq!(
            store.refresh_lod([BrickCoord([0, 0, 0])]),
            LodRefresh::default()
        );
        assert_eq!(store.lod_hash(), BrickStore::new().lod_hash());
    }

    #[test]
    fn level_0_of_a_pyramid_is_the_store_itself() {
        let mut store = block();
        store.build_lod(3);
        assert_eq!(store.level_count(), 4);
        assert_eq!(store.level(0).unwrap().len(), 8);
        assert!(store.level(4).is_none());
    }

    #[test]
    fn a_pyramid_does_not_change_the_hash_of_level_0() {
        let plain = block();
        let mut pyramid = block();
        pyramid.build_lod(4);
        assert_eq!(pyramid.content_hash(), plain.content_hash());
        assert_ne!(pyramid.lod_hash(), plain.lod_hash());
    }

    #[test]
    fn a_cube_of_stone_shrinks_by_half_at_each_level() {
        let mut store = block();
        store.build_lod(4);
        let level = |n| store.level(n).unwrap();
        // Level 1 brick 0 is the eight bricks as one solid brick.
        assert_eq!(level(1).sorted_coords(), [BrickCoord([0; 3])]);
        assert!(level(1).brick(BrickCoord([0; 3])).unwrap().is_uniform());
        // Each level up, the cube is half as many Voxels wide: 8, 4, 2 then 1.
        for (n, width) in [(1, 8), (2, 4), (3, 2), (4, 1)] {
            for v in 0..9 {
                let inside = v < width;
                let want = inside.then_some(ROCK);
                assert_eq!(
                    store.voxel_at(n, VoxelCoord([v, v, v])),
                    want,
                    "level {n} Voxel {v}"
                );
                assert_eq!(
                    store.voxel_at(n, VoxelCoord([v, 0, width])),
                    None,
                    "level {n} Voxel {v}"
                );
            }
        }
        assert_eq!(level(4).len(), 1);
    }

    #[test]
    fn a_lone_voxel_is_too_few_to_show_in_the_levels_above() {
        let mut store = BrickStore::new();
        store.set(VoxelCoord([3, 3, 3]), Some(ROCK));
        store.build_lod(3);
        // The coarse bricks are held, as they cover a held brick, but read as air.
        for n in 1..=3 {
            assert!(store.brick_at(n, BrickCoord([0; 3])).is_air(), "level {n}");
        }
    }

    #[test]
    fn building_again_replaces_the_old_pyramid() {
        let mut store = block();
        store.build_lod(4);
        let four = store.lod_hash();
        store.build_lod(1);
        assert_eq!(store.level_count(), 2);
        let mut fresh = block();
        fresh.build_lod(1);
        assert_eq!(store.lod_hash(), fresh.lod_hash());
        assert_ne!(store.lod_hash(), four);
        // What is left of level 1 is its content, and not just the right number of levels.
        assert!(store.brick_at(1, BrickCoord([0; 3])).is_uniform());
        assert_eq!(store.voxel_at(1, VoxelCoord([7, 7, 7])), Some(ROCK));
        store.build_lod(0);
        assert_eq!(store.level_count(), 1);
        assert_eq!(store.lod_hash(), BrickStore::new().lod_hash());
    }

    #[test]
    fn refreshing_remakes_the_ancestors_of_a_changed_brick_and_reports_what_differs() {
        let mut store = block();
        store.build_lod(4);
        let before = store.clone();
        store.insert(BrickCoord([1, 0, 1]), Brick::AIR);
        let refresh = store.refresh_lod([BrickCoord([1, 0, 1])]);
        // Four per level: the ancestor, and the ones over the bricks beside it that are not in
        // its parent. The hole is seen up to level 3, so level 4 is remade and then it stops.
        assert_eq!(refresh.remade, 4 * 4);
        // The hole shows up to level 3, and at level 4 it is one child in eight: still rock.
        assert_eq!(
            refresh.changed,
            [
                (1, BrickCoord([0; 3])),
                (2, BrickCoord([0; 3])),
                (3, BrickCoord([0; 3]))
            ]
        );
        assert_eq!(refresh.changed, differing(&before, &store));
        assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash());
    }

    #[test]
    fn a_refresh_done_in_steps_is_the_refresh_done_at_once() {
        for hole in [[1, 0, 1], [0, 0, 0], [3, 1, 2]] {
            let mut once = block();
            once.build_lod(4);
            let mut stepped = once.clone();
            for store in [&mut once, &mut stepped] {
                store.insert(BrickCoord(hole), Brick::AIR);
            }
            let want = once.refresh_lod([BrickCoord(hole)]);
            let mut work = stepped.begin_lod([BrickCoord(hole)]);
            let mut steps = 0;
            // A deadline that has passed: one coarse brick a step.
            while !stepped.step_lod(&mut work, Some(std::time::Instant::now())) {
                steps += 1;
            }
            assert!(steps > 3, "{steps}");
            assert_eq!(work.finish(), want);
            assert_eq!(stepped.lod_hash(), once.lod_hash());
            assert_eq!(stepped.lod_hash(), rebuilt(&stepped).lod_hash());
        }
    }

    #[test]
    fn refreshing_a_brick_that_did_not_change_reports_nothing() {
        let mut store = block();
        store.build_lod(4);
        let refresh = store.refresh_lod([BrickCoord([1, 0, 1])]);
        assert_eq!(refresh.remade, 4);
        assert!(refresh.changed.is_empty());
    }

    #[test]
    fn bricks_that_share_ancestors_or_touch_ones_that_do_are_remade_once() {
        let mut store = block();
        store.build_lod(4);
        // None of these change anything, so only level 1 is remade: the parents of the bricks
        // and of the bricks touching them. Bricks -1 and 0 never share a parent, and each of the
        // two touches the other, so the parents are those of -2 to 1 along x, and the y and z
        // of the two bricks and of the ones beside them.
        let refresh = store.refresh_lod([BrickCoord([-1, 0, 0]), BrickCoord([0, 0, 0])]);
        assert_eq!(refresh.remade, 6);
        // Four children of one parent, its lower layer: that parent and the one beyond each of
        // its faces but the top, which the upper layer of children is in.
        let refresh =
            store.refresh_lod([[0, 0, 0], [1, 0, 0], [0, 1, 0], [1, 1, 0]].map(BrickCoord));
        assert_eq!(refresh.remade, 6);
        // Twenty-seven neighbours span two parents per axis: eight, and twelve more that only
        // the bricks touching the cube are in (a brick 2 along one axis, and not along two).
        let cube = (0..27).map(|i| BrickCoord([i % 3 - 1, i / 3 % 3 - 1, i / 9 - 1]));
        assert_eq!(store.refresh_lod(cube).remade, 20);
    }

    #[test]
    fn those_ancestors_are_all_that_differs_when_the_bricks_really_changed() {
        let mut store = block();
        for far in [[40, 0, 0], [-33, -33, 17], [2, 2, 2]] {
            store.insert(BrickCoord(far), Brick::solid(Material(5)));
        }
        store.build_lod(4);
        let before = store.clone();
        // The eight bricks around the origin, where -1 and 0 never share an ancestor.
        let around: Vec<_> = (0..8)
            .map(|i| BrickCoord([i & 1, i >> 1 & 1, i >> 2].map(|b| b - 1)))
            .collect();
        for &coord in &around {
            store.insert(coord, Brick::AIR);
        }
        store.insert(BrickCoord([0, 0, 0]), Brick::solid(Material(4)));
        let refresh = store.refresh_lod(around.iter().copied());
        assert_eq!(refresh.remade, 8 * 4);

        let differing = differing(&before, &store);
        assert!(!differing.is_empty());
        assert_eq!(refresh.changed, differing);
        // Everything that differs is over the bricks changed: coarse bricks -1 and 0 on each axis.
        let over = |&(_, c): &(usize, BrickCoord)| c.0.iter().all(|&a| a == 0 || a == -1);
        assert!(differing.iter().all(over), "{differing:?}");
    }

    #[test]
    fn a_refreshed_pyramid_is_the_one_a_rebuild_would_make() {
        let mut store = banded();
        store.build_lod(5);
        for centre in [
            [0, 0, 0],
            [1, 1, 1],
            [-8, -8, -8],
            [-1, 0, 7],
            [-2, -3, -4],
            [9, 8, 7],
        ] {
            let mut changed = BTreeSet::new();
            for d in 0..27 {
                let at = [0, 1, 2].map(|a| centre[a] + [d % 3, d / 3 % 3, d / 9][a] - 1);
                store.set(VoxelCoord(at), None);
                changed.insert(VoxelCoord(at).brick());
            }
            store.refresh_lod(changed);
            assert_eq!(
                store.lod_hash(),
                rebuilt(&store).lod_hash(),
                "after {centre:?}"
            );
        }
    }

    const SOIL: Material = Material(9);
    const BEDROCK: Material = Material(2);

    /// Ground up to z = 16 over the 16 Voxel wide square about (8, 8): bedrock with a skin of
    /// soil at z = 15. Its bricks are the four at z = 1 and four at z = 0, which are the
    /// children of level 1 brick (0, 0, 0).
    fn ground() -> BrickStore {
        let mut store = BrickStore::new();
        for i in 0..8 {
            let at = BrickCoord([i & 1, i >> 1 & 1, i >> 2]);
            let mut brick = Brick::solid(BEDROCK);
            if at.0[2] == 1 {
                for v in 0..64 {
                    brick.set([v % 8, v / 8, 7], Some(SOIL));
                }
            }
            store.insert(at, brick);
        }
        store
    }

    #[test]
    fn a_coarse_brick_is_remade_when_a_brick_next_to_its_children_changes() {
        let mut store = ground();
        store.build_lod(3);
        // Air over the soil, so that it is the surface.
        assert_eq!(store.voxel_at(1, VoxelCoord([3, 3, 7])), Some(SOIL));
        // Level 0 brick (0, 0, 2) is not a child of level 1 brick (0, 0, 0), but it is above
        // two of them. With bedrock on it the soil is no longer the surface there.
        store.insert(BrickCoord([0, 0, 2]), Brick::solid(BEDROCK));
        let refresh = store.refresh_lod([BrickCoord([0, 0, 2])]);
        assert_eq!(store.voxel_at(1, VoxelCoord([3, 3, 7])), Some(BEDROCK));
        assert!(
            refresh.changed.contains(&(1, BrickCoord([0; 3]))),
            "{refresh:?}"
        );
        assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash());
        // And the same on the way back.
        store.remove(BrickCoord([0, 0, 2]));
        store.refresh_lod([BrickCoord([0, 0, 2])]);
        assert_eq!(store.voxel_at(1, VoxelCoord([3, 3, 7])), Some(SOIL));
        assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash());
    }

    /// Ground 128 Voxels across, with its surface at z = `top`: 4 Voxels of soil, and bedrock
    /// under them as far down as z = -48.
    fn slab(top: i32) -> BrickStore {
        let material = |z: i32| match z {
            z if z >= top => None,
            z if z >= top - 4 => Some(SOIL),
            _ => Some(BEDROCK),
        };
        let mut store = BrickStore::new();
        for bz in -6..=(top - 1).div_euclid(8) {
            let layer: Vec<_> = (0..8).map(|v| material(8 * bz + v)).collect();
            let mut brick = Brick::AIR;
            for i in 0..512 {
                brick.set([i % 8, i / 8 % 8, i / 64].map(|a| a as u8), layer[i / 64]);
            }
            for bx in -8..8 {
                for by in -8..8 {
                    store.insert(BrickCoord([bx, by, bz]), brick.clone());
                }
            }
        }
        store
    }

    #[test]
    fn soil_four_voxels_thick_over_rock_reads_as_soil_at_every_level_wherever_its_surface_is() {
        // The surface at every height from 18 to 34, which puts it on every boundary between
        // coarse Voxels from level 1 to level 4 and on those of bricks (24 and 32), and has it
        // between them otherwise. Bedrock has the lower ID, so a tie never favours the soil.
        for top in 18..=34 {
            let mut store = slab(top);
            store.build_lod(5);
            for level in 1..=5 {
                // The highest solid Voxel in the column over the middle of the slab.
                let surface = (-8..=(top >> level) + 1)
                    .rev()
                    .find_map(|z| store.voxel_at(level, VoxelCoord([0, 0, z])));
                assert_eq!(surface, Some(SOIL), "surface at {top}, level {level}");
            }
        }
    }

    #[test]
    fn coarse_bricks_are_held_exactly_over_held_bricks() {
        let mut store = block();
        store.build_lod(2);
        assert_eq!(store.level(1).unwrap().len(), 1);
        assert_eq!(store.level(2).unwrap().len(), 1);
        // Held as air, a brick still has its ancestors held, reading as air.
        for coord in store.sorted_coords() {
            store.insert(coord, Brick::AIR);
        }
        store.refresh_lod(store.sorted_coords());
        assert_eq!(store.level(2).unwrap().len(), 1);
        assert!(store.brick_at(1, BrickCoord([0; 3])).is_air());
        assert!(store.brick_at(2, BrickCoord([0; 3])).is_air());
        // Removed, they have none.
        for coord in store.sorted_coords() {
            store.remove(coord);
        }
        store.refresh_lod(block().sorted_coords());
        assert!(store.level(1).unwrap().is_empty());
        assert!(store.level(2).unwrap().is_empty());
        assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash());
    }

    /// An interior of radius 100, and so with coarse bricks over its edge from level 4 up.
    const INTERIOR: Interior = Interior {
        radius: 100,
        material: ROCK,
    };

    #[test]
    fn a_source_decides_what_a_brick_that_is_not_held_reads_as() {
        let mut store = BrickStore::new();
        store.build_lod(3);
        assert!(store.brick_at(2, BrickCoord([0; 3])).is_air());
        store.set_source(INTERIOR);
        // Wholly inside the ball: level 0 brick 0, and level 2 brick 0 (Voxels 0..=31).
        assert_eq!(store.voxel_at(0, VoxelCoord([3, 3, 3])), Some(ROCK));
        assert_eq!(store.voxel_at(2, VoxelCoord([7, 7, 7])), Some(ROCK));
        // Not wholly inside: level 0 brick 20 (Voxels 160..), and level 3 brick 4 (Voxels 256..).
        assert_eq!(store.voxel_at(0, VoxelCoord([160, 0, 0])), None);
        assert_eq!(store.voxel_at(3, VoxelCoord([32, 0, 0])), None);
        // The held bricks are only those held: reading as rock does not hold it.
        assert_eq!(store.get(VoxelCoord([3, 3, 3])), None);
        assert!(store.is_empty() && store.level(1).unwrap().is_empty());
    }

    #[test]
    fn digging_a_brick_out_of_the_interior_shows_in_the_levels_over_it() {
        let mut store = BrickStore::new();
        store.set_source(INTERIOR);
        store.build_lod(2);

        // A level 0 brick dug out to air: an eighth of level 1 brick 0 and of level 2 brick 0,
        // with the rock of the interior around it.
        store.insert(BrickCoord([0; 3]), Brick::AIR);
        let refresh = store.refresh_lod([BrickCoord([0; 3])]);
        // Four a level: the corner of eight parents that the brick is at touches four.
        assert_eq!(refresh.remade, 2 * 4);
        assert_eq!(
            refresh.changed,
            [(1, BrickCoord([0; 3])), (2, BrickCoord([0; 3]))]
        );
        assert_eq!(store.voxel_at(1, VoxelCoord([3, 3, 3])), None);
        assert_eq!(store.voxel_at(1, VoxelCoord([4, 0, 0])), Some(ROCK));
        assert_eq!(store.voxel_at(2, VoxelCoord([1, 1, 1])), None);
        assert_eq!(store.voxel_at(2, VoxelCoord([2, 0, 0])), Some(ROCK));

        // Rock put back: the coarse bricks are held still, but read as they did at first.
        store.insert(BrickCoord([0; 3]), Brick::solid(ROCK));
        let refresh = store.refresh_lod([BrickCoord([0; 3])]);
        assert_eq!(refresh.changed.len(), 2);
        assert_eq!(store.voxel_at(1, VoxelCoord([3, 3, 3])), Some(ROCK));
        // Taken away, they are gone, and the pyramid is as bare as when it was built.
        store.remove(BrickCoord([0; 3]));
        store.refresh_lod([BrickCoord([0; 3])]);
        assert!(store.level(1).unwrap().is_empty() && store.level(2).unwrap().is_empty());
        let mut bare = BrickStore::new();
        bare.set_source(INTERIOR);
        bare.build_lod(2);
        assert_eq!(store.lod_hash(), bare.lod_hash());
    }

    #[test]
    fn taking_back_an_edit_leaves_the_pyramid_as_it_was_even_over_the_edge_of_the_interior() {
        let mut store = BrickStore::new();
        store.set_source(INTERIOR);
        store.build_lod(9);
        let bare = store.lod_hash();
        // Level 4 and up have coarse bricks that are only partly inside the ball. Digging at
        // the origin remakes them from the bricks around, which the source says are air.
        store.insert(BrickCoord([0; 3]), Brick::AIR);
        let dug = store.refresh_lod([BrickCoord([0; 3])]);
        assert_eq!(dug.remade, 9 * 4);
        assert_ne!(store.lod_hash(), bare);
        assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash());
        store.remove(BrickCoord([0; 3]));
        let undone = store.refresh_lod([BrickCoord([0; 3])]);
        assert_eq!(undone.remade, 9 * 4);
        assert_eq!(store.lod_hash(), bare);
        assert_eq!(dug.changed.len(), undone.changed.len());
    }

    /// Rock out to `rock` Voxels from the origin. Bricks wholly within `core` are held only if
    /// `hold_core`.
    fn planet(rock: i32, core: i32, hold_core: bool) -> BrickStore {
        let mut store = BrickStore::new();
        for i in 0..12 * 12 * 12 {
            let coord = BrickCoord([i % 12 - 6, i / 12 % 12 - 6, i / 144 - 6]);
            if !hold_core && coord.within_ball(core) {
                continue;
            }
            let mut brick = Brick::AIR;
            for v in 0..512 {
                let local = [v & 7, v >> 3 & 7, v >> 6].map(|a| a as u8);
                let at = coord.voxel(local).0.map(i64::from);
                if at.iter().map(|a| a * a).sum::<i64>() <= i64::from(rock * rock) {
                    brick.set(local, Some(ROCK));
                }
            }
            if !brick.is_air() {
                store.insert(coord, brick);
            }
        }
        store
    }

    #[test]
    fn an_interior_with_its_crust_held_makes_the_pyramid_of_a_planet_that_holds_it_all() {
        // The core and the crust it needs, but not the bricks wholly inside the core.
        let mut implied = planet(36, 24, false);
        let core = Interior {
            radius: 24,
            material: ROCK,
        };
        implied.set_source(core);
        implied.build_lod(7);
        let mut whole = planet(36, 24, true);
        whole.build_lod(7);
        assert!(whole.len() > implied.len() + 10);

        for level in 1..=7 {
            let mut coords = whole.level(level).unwrap().sorted_coords();
            coords.extend(implied.level(level).unwrap().sorted_coords());
            coords.sort();
            coords.dedup();
            assert!(!coords.is_empty());
            for coord in coords {
                assert_eq!(
                    whole.brick_at(level, coord),
                    implied.brick_at(level, coord),
                    "level {level} {coord:?}"
                );
            }
        }
    }

    #[test]
    fn lod_hash_ignores_the_order_the_bricks_were_inserted_in() {
        let mut backwards = BrickStore::new();
        for coord in block().sorted_coords().into_iter().rev() {
            backwards.insert(coord, Brick::solid(ROCK));
        }
        let mut forwards = block();
        forwards.build_lod(3);
        backwards.build_lod(3);
        assert_eq!(forwards.lod_hash(), backwards.lod_hash());
    }

    #[test]
    fn lod_hash_sees_the_material_in_a_coarse_brick_and_the_number_of_levels() {
        let hash = |material, levels| {
            let mut store = BrickStore::new();
            store.insert(BrickCoord([5, 5, 5]), Brick::solid(Material(material)));
            store.build_lod(levels);
            store.lod_hash()
        };
        assert_ne!(hash(3, 3), hash(4, 3));
        assert_ne!(hash(3, 3), hash(3, 2));
    }

    /// A block of rock 4 bricks on a side, with a pyramid of two levels.
    fn rock_block() -> BrickStore {
        let mut store = BrickStore::new();
        for x in 0..4 {
            for y in 0..4 {
                for z in 0..4 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(ROCK));
                }
            }
        }
        store.build_lod(2);
        store
    }

    #[test]
    fn a_coarse_brick_is_solid_below_only_if_every_voxel_under_it_is() {
        let mut store = rock_block();
        let top = BrickCoord([0, 0, 0]);
        assert!(
            store.solid_below(0, top) && store.solid_below(1, top) && store.solid_below(2, top)
        );
        // A hole of one Voxel does not change what the coarse bricks read as, which is a majority,
        // but they are no longer solid all the way down: not the ones over it.
        let before = store.brick_at(2, top).into_owned();
        let hole = VoxelCoord([13, 13, 13]).brick();
        store.set(VoxelCoord([13, 13, 13]), None);
        let refresh = store.refresh_lod([hole]);
        assert_eq!(
            *store.brick_at(2, top),
            before,
            "the level 2 brick reads as it did"
        );
        assert!(refresh.remade >= 2);
        assert!(!store.solid_below(0, hole));
        assert!(!store.solid_below(1, hole.parent()));
        assert!(!store.solid_below(2, top));
        assert!(store.solid_below(0, BrickCoord([2, 2, 2])));
        assert!(store.solid_below(1, BrickCoord([1, 1, 1])));
        // Refreshing keeps it as building the pyramid afresh makes it, with the hole.
        let mut fresh = store.clone();
        fresh.build_lod(2);
        assert_eq!(store.coarse[0].solid_below, fresh.coarse[0].solid_below);
        assert_eq!(store.coarse[1].solid_below, fresh.coarse[1].solid_below);
        // Filled again, it is.
        store.set(VoxelCoord([13, 13, 13]), Some(ROCK));
        store.refresh_lod([hole]);
        assert!(store.solid_below(2, top));
        let mut fresh = store.clone();
        fresh.build_lod(2);
        assert_eq!(store.coarse[1].solid_below, fresh.coarse[1].solid_below);
    }

    #[test]
    fn where_the_store_holds_nothing_the_source_says_what_is_solid_below() {
        let mut ball = BrickStore::new();
        ball.set_source(Interior {
            radius: 100,
            material: ROCK,
        });
        ball.build_lod(2);
        assert!(ball.solid_below(2, BrickCoord([0, 0, 0])));
        assert!(!ball.solid_below(2, BrickCoord([5, 5, 5])));
    }

    #[test]
    fn air_is_unproven_unless_there_is_a_source_that_says_it_is_air_below() {
        let mut ball = BrickStore::new();
        // Without a source a brick that is not held has nothing under it.
        assert!(!ball.may_have_ground_below(2, BrickCoord([0, 0, 0])));
        ball.set_source(Interior {
            radius: 100,
            material: ROCK,
        });
        // Over the edge of the ball a brick may have some of it under it; wholly outside it, none.
        assert!(ball.may_have_ground_below(2, BrickCoord([0, 0, 0])));
        assert!(ball.may_have_ground_below(2, BrickCoord([3, 0, 0])));
        assert!(!ball.may_have_ground_below(2, BrickCoord([5, 5, 5])));
        assert!(!ball.may_have_ground_below(0, BrickCoord([20, 0, 0])));
    }

    #[test]
    fn a_coarse_brick_inserted_directly_is_held_and_solid_below_is_what_the_source_says() {
        let mut store = BrickStore::new();
        store.set_source(Interior {
            radius: 100,
            material: ROCK,
        });
        store.build_lod(2);
        let (inside, outside) = (BrickCoord([0, 0, 0]), BrickCoord([5, 5, 5]));
        store.insert_at(2, inside, Brick::solid(Material(4)));
        store.insert_at(2, outside, Brick::solid(Material(4)));
        assert_eq!(*store.brick_at(2, inside), Brick::solid(Material(4)));
        assert_eq!(store.level(2).unwrap().len(), 2);
        assert!(store.solid_below(2, inside));
        assert!(!store.solid_below(2, outside));
        // Level 0 is the store's own.
        store.insert_at(0, outside, Brick::solid(ROCK));
        assert_eq!(store.get(VoxelCoord([40, 40, 40])), Some(ROCK));
    }

    #[test]
    fn a_store_that_streams_has_the_children_of_its_guard_resident_and_the_rest_when_told() {
        let mut store = BrickStore::new();
        let c = BrickCoord([1, 2, 3]);
        assert!(
            store.children_resident(5, c),
            "a store that does not stream has all"
        );
        store.stream_from(3);
        assert!(
            store.children_resident(4, c),
            "above the floor is the Guard"
        );
        assert!(!store.children_resident(3, c));
        assert!(!store.children_resident(1, c));
        store.set_children_resident(1, c);
        assert!(store.children_resident(1, c));
        assert!(!store.children_resident(1, BrickCoord([1, 2, 4])));
    }

    #[test]
    fn room_for_the_resident_set_is_kept_whether_it_is_asked_for_before_or_after_streaming_starts()
    {
        let room = |store: &BrickStore| {
            let sizes = store.table_sizes();
            let resident = sizes.iter().find(|t| t.name == "store resident").unwrap();
            (resident.len, resident.capacity)
        };
        let mut store = BrickStore::new();
        store.build_lod(2);
        store.reserve(&[1000, 500]);
        // Asked for before the store streams, the room is kept for the resident set that starts.
        store.reserve_resident(100_000);
        store.stream_from(1);
        let (_, capacity) = room(&store);
        assert!(capacity >= 100_000);
        for i in 0..50_000 {
            store.set_children_resident(1, BrickCoord([i, 0, 0]));
        }
        assert_eq!(room(&store), (50_000, capacity), "it did not grow");
        // Asked for after, it is made more roomy.
        store.reserve_resident(100_000);
        assert!(room(&store).1 >= 150_000);
        let sizes = store.table_sizes();
        let level_0 = sizes.iter().find(|t| t.name == "store level 0 bricks");
        assert!(level_0.is_some_and(|t| t.capacity >= 1000));
    }

    #[test]
    fn a_store_that_streams_does_not_generate_to_know_whether_a_neighbour_is_full() {
        use std::sync::atomic::Ordering::Relaxed;

        let source = Arc::new(Counting::default());
        let mut store = BrickStore::new();
        store.source = Some(source.clone());
        let (deep, shallow) = (BrickCoord([0, -1, 0]), BrickCoord([0, 1, 0]));
        // Not streaming: it reads the brick, which is full.
        assert!(store.is_full_known(0, shallow));
        assert_eq!(source.0.load(Relaxed), 1);
        store.stream_from(0);
        assert!(store.is_full_known(0, deep), "the source proves it");
        assert!(!store.is_full_known(0, shallow), "not known, not read");
        assert_eq!(source.0.load(Relaxed), 1);
        store.insert(shallow, Brick::solid(ROCK));
        assert!(store.is_full_known(0, shallow), "held bricks are known");
    }

    #[test]
    fn a_store_that_streams_does_not_generate_to_know_whether_a_brick_is_solid_below() {
        use std::sync::atomic::Ordering::Relaxed;

        let source = Arc::new(Counting::default());
        let mut store = BrickStore::new();
        store.source = Some(source.clone());
        store.stream_from(1);
        let (deep, shallow) = (BrickCoord([0, -1, 0]), BrickCoord([0, 1, 0]));
        // The children of their parents are not resident: a brick not held is not read.
        assert!(store.solid_below(0, deep), "the source proves it");
        assert!(!store.solid_below(0, shallow), "not known, not read");
        assert_eq!(source.0.load(Relaxed), 0);
        // Resident, a brick that is not held is air, or proven solid by the source.
        store.set_children_resident(1, deep.parent());
        store.set_children_resident(1, shallow.parent());
        assert!(store.solid_below(0, deep));
        assert!(!store.solid_below(0, shallow));
        assert_eq!(source.0.load(Relaxed), 0, "nothing was generated");
        store.insert(shallow, Brick::solid(ROCK));
        assert!(store.solid_below(0, shallow), "held bricks are known");
    }

    #[test]
    fn loading_the_children_of_a_brick_holds_the_ones_not_air_and_never_replaces_a_held_one() {
        let mut store = BrickStore::new();
        store.set_source(Interior {
            radius: 100,
            material: ROCK,
        });
        store.build_lod(2);
        store.stream_from(2);
        let parent = BrickCoord([1, 0, 0]);
        let edited = parent.children()[3];
        store.insert_at(1, edited, Brick::solid(Material(9)));
        let source = store.source().unwrap();
        // The skipped one is the edited one, as a loader that checks what is held does.
        let bricks = crate::children_from(&*source, 2, parent, |c| store.holds(1, c));
        assert!(bricks.iter().all(|(c, _)| *c != edited));
        assert!(!store.children_resident(2, parent));
        // Held late, with the edited one in the list too: it stays as it is.
        let mut all = bricks.clone();
        all.push((edited, Brick::solid(ROCK)));
        let held = store.hold_children(2, parent, all);
        assert_eq!(held, bricks.len());
        assert!(store.children_resident(2, parent));
        assert_eq!(*store.brick_at(1, edited), Brick::solid(Material(9)));
        for (c, b) in &bricks {
            assert!(store.holds(1, *c));
            assert_eq!(*store.brick_at(1, *c), *b);
        }
        // A parent over air has nothing to hold, and is resident.
        let sky = BrickCoord([50, 50, 50]);
        assert_eq!(store.hold_children(2, sky, Vec::new()), 0);
        assert!(store.children_resident(2, sky));
    }

    /// Rock under y = 0 at every level, and air above: a source that is not provably solid anywhere.
    #[derive(Debug)]
    struct Slab;

    impl BrickSource for Slab {
        fn brick(&self, _: usize, coord: BrickCoord) -> Brick {
            if coord.0[1] < 0 {
                Brick::solid(ROCK)
            } else {
                Brick::AIR
            }
        }
    }

    #[test]
    fn an_edit_leaves_a_coarse_brick_that_was_streamed_in_beside_it_held_as_it_was() {
        let mut store = BrickStore::new();
        store.set_source(Slab);
        store.build_lod(3);
        store.stream_from(3);
        // A level 1 brick held with none of its children held, as a streamed band holds them,
        // beside the one over the brick that is edited.
        let beside = BrickCoord([1, -1, 0]);
        let streamed = Brick::solid(Material(9));
        store.insert_at(1, beside, streamed.clone());
        let edited = BrickCoord([1, -1, 0]);
        store.insert(edited, Brick::solid(ROCK));
        store.set(VoxelCoord([8, -8, 0]), None);
        store.refresh_lod([edited]);
        assert!(
            store.holds(1, beside),
            "the brick beside the Edit is still held"
        );
        assert_eq!(*store.brick_at(1, beside), streamed);
        // The one over the Edit was remade from the brick that is held.
        assert!(store.holds(1, edited.parent()));
        assert_ne!(*store.brick_at(1, edited.parent()), Brick::AIR);
    }

    #[test]
    fn taking_back_a_brick_in_a_streamed_store_puts_back_what_the_source_makes_of_it() {
        let mut store = BrickStore::new();
        store.set_source(Slab);
        store.build_lod(2);
        store.stream_from(1);
        let coord = BrickCoord([3, -2, 0]);
        // An Edit made the brick before its parent's children were resident...
        store.insert(coord, Brick::AIR);
        store.set_children_resident(1, coord.parent());
        // ...and the brick not held reads as air in a parent that is resident.
        assert!(store.brick_at(0, BrickCoord([3, -1, 0])).is_air());
        // Taken back, it reads as the source makes it, as it does where nothing streams.
        store.remove(coord);
        assert_eq!(*store.brick_at(0, coord), Brick::solid(ROCK));
        // A brick that is air to the source stays air.
        let sky = BrickCoord([3, 5, 0]);
        store.insert(sky, Brick::solid(ROCK));
        store.remove(sky);
        assert!(store.brick_at(0, sky).is_air());
    }

    #[test]
    fn an_edit_taken_back_while_its_parents_children_are_still_being_made_is_put_back_all_the_same()
    {
        let mut store = BrickStore::new();
        store.set_source(Slab);
        store.build_lod(2);
        store.stream_from(1);
        let (parent, coord) = (BrickCoord([1, -1, 0]), BrickCoord([3, -2, 0]));
        assert_eq!(coord.parent(), parent);
        // An Edit holds the brick; the loader skips it, being held when the job was made.
        store.insert(coord, Brick::AIR);
        let others = crate::children_from(&*store.source().unwrap(), 1, parent, |c| c == coord);
        // The Edit is taken back before the children arrive...
        store.remove(coord);
        // ...and then they do.
        store.hold_children(1, parent, others);
        assert_eq!(*store.brick_at(0, coord), Brick::solid(ROCK));
        // A raw removal puts nothing back.
        store.insert(coord, Brick::AIR);
        store.discard(coord);
        assert!(!store.holds(0, coord));
    }

    /// A store with a pyramid of `levels` levels that streams from `floor` over [`Slab`], with
    /// the children of the units `units` resident (as the loader holds them).
    fn resident(levels: usize, floor: usize, units: &[(usize, BrickCoord)]) -> BrickStore {
        let mut store = BrickStore::new();
        store.set_source(Slab);
        store.build_lod(levels - 1);
        store.stream_from(floor);
        for &(level, coord) in units {
            let kids = crate::children_from(&*store.source().unwrap(), level, coord, |_| false);
            store.hold_children(level, coord, kids);
        }
        store
    }

    /// The units that eviction may take, in the order to take them: all the parts looked at, and
    /// the candidates put in order.
    fn order(
        store: &BrickStore,
        wanted: impl Fn(usize, BrickCoord) -> bool,
        gap: impl Fn(usize, BrickCoord) -> u32,
    ) -> Vec<(usize, BrickCoord)> {
        let pass = store.begin_eviction_pass();
        let mut found = Vec::new();
        for part in 0..BrickStore::EVICTION_PARTS {
            store.eviction_candidates(part, pass, &wanted, &gap, &mut found);
        }
        found.sort_unstable_by_key(Candidate::order);
        found.iter().map(Candidate::unit).collect()
    }

    const NEVER: fn(usize, BrickCoord) -> bool = |_, _| false;
    const NO_GAP: fn(usize, BrickCoord) -> u32 = |_, _| 0;

    #[test]
    fn evicting_the_children_of_a_unit_takes_them_and_their_residence_and_they_can_come_back() {
        let unit = (1, BrickCoord([0, -1, 0]));
        let mut store = resident(4, 3, &[unit]);
        let held = store.len();
        assert_eq!(held, 8);
        assert_eq!(store.streamed_len(), held + 1, "the bricks and the unit");
        assert!(store.children_resident(unit.0, unit.1));
        let before = store.brick_at(0, unit.1.children()[3]).into_owned();

        assert_eq!(store.evict_children(unit.0, unit.1, NEVER), Some(8));
        assert_eq!(store.len(), 0);
        assert_eq!(store.streamed_len(), 0);
        assert_eq!(store.evicted_len(), 8);
        assert!(!store.children_resident(unit.0, unit.1));
        assert_eq!(
            store.evict_children(unit.0, unit.1, NEVER),
            None,
            "nothing is resident to take"
        );

        // Held again as the loader does, it reads as before.
        let kids = crate::children_from(&*store.source().unwrap(), unit.0, unit.1, |_| false);
        store.hold_children(unit.0, unit.1, kids);
        assert_eq!(*store.brick_at(0, unit.1.children()[3]), before);
    }

    #[test]
    fn a_unit_of_the_levels_under_the_guard_is_evicted_with_its_coarse_bricks_and_their_flags() {
        let unit = (2, BrickCoord([0, -1, 0]));
        let mut store = resident(4, 3, &[unit]);
        let child = unit.1.children()[0];
        assert!(store.holds(1, child));
        store.hold(1, child, Brick::solid(ROCK), true);
        assert!(store.level(1).unwrap().solid_below.contains(&child));
        assert_eq!(store.evict_children(unit.0, unit.1, NEVER), Some(8));
        assert!(!store.holds(1, child));
        assert!(!store.level(1).unwrap().solid_below.contains(&child));
    }

    #[test]
    fn the_guard_is_never_listed_or_evicted() {
        // Levels 3 and 4 are the Guard of a store of five levels that streams from 3.
        let mut store = resident(5, 3, &[]);
        let top = BrickCoord([0, -1, 0]);
        store.insert_at(4, top, Brick::solid(ROCK));
        for child in top.children() {
            store.insert_at(3, child, Brick::solid(ROCK));
        }
        // Even told that the children of a Guard brick are resident, they are not listed or taken.
        store.set_children_resident(4, top);
        assert!(order(&store, NEVER, NO_GAP).is_empty());
        assert_eq!(store.evict_children(4, top, NEVER), None);
        assert!(top.children().iter().all(|&c| store.holds(3, c)));
        assert!(store.holds(4, top));
        // The units of the first level of the Guard hold the bricks under it, and not the Guard.
        let first = (3, top.children()[0]);
        let kids = crate::children_from(&*store.source().unwrap(), first.0, first.1, |_| false);
        store.hold_children(first.0, first.1, kids);
        assert_eq!(store.evict_children(first.0, first.1, NEVER), Some(8));
        assert!(top.children().iter().all(|&c| store.holds(3, c)));
    }

    #[test]
    fn a_unit_the_caller_wants_is_not_listed_or_evicted_and_what_it_does_not_want_is() {
        let (a, b) = ((1, BrickCoord([0, -1, 0])), (1, BrickCoord([5, -1, 0])));
        let mut store = resident(4, 3, &[a, b]);
        let drawn = |level, coord| (level, coord) == a;
        assert_eq!(order(&store, drawn, NO_GAP), vec![b]);
        assert_eq!(store.evict_children(a.0, a.1, drawn), None);
        assert!(store.children_resident(a.0, a.1));
        assert_eq!(store.evict_children(b.0, b.1, drawn), Some(8));
        assert_eq!(store.len(), 8, "a's children are held");
    }

    #[test]
    fn the_units_over_a_unit_that_is_wanted_are_kept_by_it() {
        let drawn = (1, BrickCoord([0, -1, 0]));
        let (middle, top) = ((2, drawn.1.parent()), (3, drawn.1.parent().parent()));
        let mut store = resident(5, 4, &[drawn, middle, top]);
        // Only the unit of the brick that is drawn is wanted: the units over it are not taken from
        // under it, though the caller did not say so of them.
        let wanted = |level, coord| (level, coord) == drawn;
        assert_eq!(order(&store, wanted, NO_GAP).len(), 2);
        assert_eq!(store.evict_children(top.0, top.1, wanted), None);
        assert_eq!(store.evict_children(middle.0, middle.1, wanted), None);
        assert!(store.children_resident(drawn.0, drawn.1));
        // With the unit under them taken, they can go, from the lowest.
        assert_eq!(store.evict_children(drawn.0, drawn.1, NEVER), Some(8));
        assert_eq!(store.evict_children(middle.0, middle.1, NEVER), Some(8));
        assert_eq!(store.evict_children(top.0, top.1, NEVER), Some(8));
    }

    #[test]
    fn a_unit_the_caller_says_a_job_is_in_flight_for_is_kept() {
        // The caller wants the unit of a brick that has a job, and the unit that the brick is a
        // child of, which the job's arrival would be under.
        let job = (1, BrickCoord([1, -1, 0]));
        let over = (2, job.1.parent());
        let beside = (2, BrickCoord([9, -1, 0]));
        let mut store = resident(5, 4, &[over, beside]);
        let in_flight = |level, coord| (level, coord) == job || (level, coord) == over;
        assert_eq!(order(&store, in_flight, NO_GAP), vec![beside]);
        assert_eq!(store.evict_children(over.0, over.1, in_flight), None);
        assert!(store.holds(1, job.1));
        assert_eq!(store.evict_children(beside.0, beside.1, in_flight), Some(8));
    }

    #[test]
    fn a_brick_an_edit_changed_and_the_units_over_it_and_beside_it_are_kept_until_it_is_taken_back()
    {
        let near = (1, BrickCoord([0, -1, 0]));
        let far = (1, BrickCoord([40, -1, 0]));
        let mut store = resident(6, 4, &[near, far]);
        let coarse = (2, BrickCoord([0, -1, 0]));
        let kids = crate::children_from(&*store.source().unwrap(), coarse.0, coarse.1, |_| false);
        store.hold_children(coarse.0, coarse.1, kids);
        let edited = near.1.children()[2];
        store.insert(edited, Brick::solid(Material(9)));
        assert_eq!(store.diverged_len(), 1);
        store.sync_diverged(None);
        // The Edit's brick, and the unit over it and over those beside it, are kept...
        assert_eq!(store.evict_children(near.0, near.1, NEVER), None);
        assert!(store.holds(0, edited));
        assert!(
            order(&store, NEVER, NO_GAP)
                .iter()
                .all(|u| *u != near && *u != coarse)
        );
        // ...and so is the coarser unit over it, while a unit far from it goes.
        assert_eq!(store.evict_children(coarse.0, coarse.1, NEVER), None);
        assert_eq!(store.evict_children(far.0, far.1, NEVER), Some(8));
        // Taken back, the Edit is not one any more.
        store.remove(edited);
        assert_eq!(store.diverged_len(), 0);
        store.sync_diverged(None);
        assert_eq!(store.evict_children(near.0, near.1, NEVER), Some(8));
        assert_eq!(store.evict_children(coarse.0, coarse.1, NEVER), Some(8));
    }

    #[test]
    fn every_way_to_change_a_brick_in_place_makes_it_an_edit() {
        let unit = (1, BrickCoord([0, -1, 0]));
        let mut store = resident(4, 3, &[unit]);
        let [a, _, c, ..] = unit.1.children();
        store.brick_mut(a).expect("a brick of rock is held");
        assert!(store.brick_mut(BrickCoord([99, 99, 99])).is_none());
        assert_eq!(
            store.diverged_len(),
            1,
            "a brick that is not held is not an Edit"
        );
        store.set(c.origin(), None);
        assert_eq!(store.diverged_len(), 2);
        store.sync_diverged(None);
        assert_eq!(store.evict_children(unit.0, unit.1, NEVER), None);
    }

    #[test]
    fn the_unit_beside_an_edit_is_kept_too_since_the_coarse_brick_over_it_reads_the_edit() {
        // A brick on the face of its parent has a neighbour in the next unit over.
        let (here, next) = ((1, BrickCoord([0, -1, 0])), (1, BrickCoord([1, -1, 0])));
        let mut store = resident(4, 3, &[here, next]);
        store.insert(BrickCoord([1, -1, 0]), Brick::solid(Material(9)));
        store.sync_diverged(None);
        assert_eq!(store.evict_children(here.0, here.1, NEVER), None);
        assert_eq!(store.evict_children(next.0, next.1, NEVER), None);
    }

    #[test]
    fn nothing_is_evicted_while_diverged_bricks_are_not_counted_and_then_they_are_kept() {
        let unit = (1, BrickCoord([0, -1, 0]));
        let mut store = resident(4, 3, &[unit]);
        // An Edit far away: until it is counted the store does not know what to keep.
        store.insert(BrickCoord([60, -1, 0]), Brick::solid(Material(9)));
        assert!(store.diverged_pending());
        assert_eq!(store.evict_children(unit.0, unit.1, NEVER), None);
        assert!(store.sync_diverged(None));
        assert!(!store.diverged_pending());
        assert_eq!(store.evict_children(unit.0, unit.1, NEVER), Some(8));
        // A later one in the unit is kept when it is counted.
        let kids = crate::children_from(&*store.source().unwrap(), unit.0, unit.1, |_| false);
        store.hold_children(unit.0, unit.1, kids);
        store.insert(unit.1.children()[1], Brick::solid(Material(9)));
        store.sync_diverged(None);
        assert_eq!(store.evict_children(unit.0, unit.1, NEVER), None);
    }

    #[test]
    fn counting_diverged_bricks_can_be_done_in_steps_and_a_brick_back_and_forth_counts_once() {
        let mut store = resident(4, 3, &[]);
        let bricks: Vec<_> = (0..200).map(|x| BrickCoord([x * 30, -1, 0])).collect();
        for &c in &bricks {
            store.insert(c, Brick::solid(Material(9)));
        }
        // Each step counts at least one brick, and a deadline in the past is one step.
        let past = std::time::Instant::now();
        let mut steps = 0;
        while !store.sync_diverged(Some(past)) {
            steps += 1;
            assert!(steps < 200);
        }
        assert!(steps > 1);
        let unit = (1, bricks[5].parent());
        assert!(store.diverged.pinned(unit.0, unit.1));
        // Taken back and made again, and counted only at the end: counted once.
        store.remove(bricks[5]);
        store.insert(bricks[5], Brick::solid(Material(9)));
        store.remove(bricks[5]);
        store.sync_diverged(None);
        assert!(!store.diverged.pinned(unit.0, unit.1));
        assert_eq!(store.diverged_len(), 199);
    }

    #[test]
    fn bricks_held_before_streaming_began_are_not_edits() {
        let mut store = BrickStore::new();
        store.set_source(Slab);
        store.build_lod(3);
        // The spawn ring is inserted before the store streams.
        let ring = BrickCoord([0, -1, 0]);
        store.insert(ring, Brick::solid(ROCK));
        store.stream_from(3);
        let unit = (1, ring.parent());
        let kids = crate::children_from(&*store.source().unwrap(), unit.0, unit.1, |c| c == ring);
        store.hold_children(unit.0, unit.1, kids);
        assert_eq!(store.diverged_len(), 0);
        assert_eq!(store.evict_children(unit.0, unit.1, NEVER), Some(8));
    }

    #[test]
    fn a_unit_with_units_under_it_waits_for_them() {
        let (coarse, fine) = ((2, BrickCoord([0, -1, 0])), (1, BrickCoord([1, -1, 1])));
        let mut store = resident(4, 3, &[coarse, fine]);
        assert_eq!(store.evict_children(coarse.0, coarse.1, NEVER), None);
        assert_eq!(store.evict_children(fine.0, fine.1, NEVER), Some(8));
        assert_eq!(store.evict_children(coarse.0, coarse.1, NEVER), Some(8));
    }

    #[test]
    fn the_units_furthest_out_of_their_band_go_first_and_then_the_least_recently_wanted() {
        let units: Vec<_> = (0..4).map(|x| (1, BrickCoord([x * 4, -1, 0]))).collect();
        let store = resident(4, 3, &units);
        // Units 0 and 1 are wanted at the first look, 2 and 3 are not; then 0 is wanted again.
        let first = |level, coord| (level, coord) == units[0] || (level, coord) == units[1];
        let listed = order(&store, first, NO_GAP);
        assert_eq!(listed.len(), 2);
        assert!(listed.contains(&units[2]) && listed.contains(&units[3]));
        let second = |level, coord| (level, coord) == units[0];
        let listed = order(&store, second, NO_GAP);
        // 1 was wanted more lately than 2 and 3, which were not wanted since they arrived.
        assert_eq!(listed.last(), Some(&units[1]), "{listed:?}");
        // A unit that is further out of its band goes before them all.
        let out = |level, coord| u32::from((level, coord) == units[1]);
        assert_eq!(order(&store, NEVER, out)[0], units[1]);
    }

    #[test]
    fn the_budget_says_when_to_start_and_when_to_stop() {
        let units: Vec<_> = (0..10).map(|x| (1, BrickCoord([x * 4, -1, 0]))).collect();
        let mut store = resident(4, 3, &units);
        assert_eq!(store.streamed_len(), 90);
        assert!(!store.over_limit());
        store.set_budget(Budget {
            bricks: 50,
            overflow: 0.2,
        });
        assert_eq!(store.budget().unwrap().limit(), 60);
        assert!(store.over_limit() && store.over_budget());
        for unit in order(&store, NEVER, NO_GAP) {
            if !store.over_budget() {
                break;
            }
            store.evict_children(unit.0, unit.1, NEVER);
        }
        assert_eq!(store.streamed_len(), 45, "five units of nine are gone");
        assert!(!store.over_limit() && !store.over_budget());
    }
}
