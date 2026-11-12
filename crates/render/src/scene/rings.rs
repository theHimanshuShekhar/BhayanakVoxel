//! Rings: the way a [`Scene`] holds the Planet at every distance from the camera (ADR 0002).
//!
//! A scene of rings has a level for each level of the brick store's pyramid that the LOD bands
//! use (see [`crate::lod`]), and each level lists only the bricks that the bands select for it
//! ([`Selection`]): a sparse set of regions of its own, whatever its distance and however the
//! camera moves, not a box. A region lists just the bricks of it that are selected, so a region
//! half in the band of one level and half in the next is listed by both, with different bricks.
//!
//! The bands partition space, so every part of it is seen at one level. The walk in
//! `trace.wgsl` goes through the levels finest first and takes the nearest hit. It skips the
//! *hole* of a level: the regions inside the ball about the camera that no brick of the level
//! reaches (a cube in it, [`Scene::hole`]), so a ray that starts near the camera goes to the
//! level that has something without stepping through empty regions.
//!
//! A level is only walked inside the smallest box of regions that has all the regions it lists
//! (`Level::occupied_first` and `occupied_end`, kept up to date by every operation that changes
//! them). Levels that have nothing, and the sky above and below the crust, then cost a clip and
//! no steps.
//!
//! The buffers are allocated with room to spare ([`Room`] says how much when it is given;
//! otherwise it is what the bricks selected need and half as much again), so they keep their
//! size as the camera moves, unless one fills. [`Scene::recentre`] follows the camera: it asks the selection what
//! changed and packs again only the regions that gained or lost a brick. [`Scene::update`] takes
//! Edits in, from the bricks of level 0 that they changed, and rewrites only the bricks that
//! differ.
//!
//! **Layout.** The words of a region's mixed bricks are kept together, in the order of the region's
//! mask, whatever order the camera brought the bricks in: a fresh scene packs them so, and a region
//! that gains, loses or resizes a brick is written to a new run (see [`Scene::repack_region`]).
//! Bricks that are next to each other in space are then next to each other in memory, which the
//! reference's incoherent bounce rays gain from. `crates/render/tests/layout.rs` measures it.
//!
//! The way this can fail is that the words run out ([`RingsFull`]), for a scene that was given its
//! [`Room`]: it fails, and is not to be used again, since its selection and its pending patch no
//! longer match what is on the GPU. A scene that was sized by the bricks it needs grows the one
//! buffer that ran out, with room to spare (a quarter, and at least a mebiword; the table of one
//! level, which is a hash table, is made again bigger at the end of `table` with its regions listed
//! in it again), and says so in the next patch ([`RingPatch::sizes`]): the GPU copy makes that
//! buffer bigger and copies what it had, and no other buffer is touched (A3 of
//! `docs/research/scheduling-and-upload.md`). Only a jump of the camera or other bands pack all of
//! the scene again ([`SceneUpdate::Rebuilt`]), which makes the GPU copy new buffers.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::ops::Range;

use brick_store::{Brick, BrickCoord, BrickStore, BrickView, TableSize};

use super::select::{self, Selection};
use super::{
    Bound, Level, MAX_LEVELS, REGION_BRICKS, REGION_FIRST_REF, REGION_VOXELS, REGION_WORDS,
    SOLID_FLAG, Scene, SceneUpdate, bit_of, encode_mixed, region_of,
};
use crate::lod::{AHEAD, Bands, LEVELS, Margins, Reach};
use crate::tracer::SceneHeader;

/// How many words the buffers of a scene of rings have, for each level, when the scene is given
/// room instead of being sized by the bricks it holds. The levels share what they have, except
/// for the regions, which each level has its own table for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Room {
    /// Regions each level can list.
    pub regions_per_ring: usize,
    /// Words of `brick_refs` for each level: one for every brick it lists that is not inside a
    /// region of one Material.
    pub refs_per_ring: usize,
    /// Words of `bricks` for each level. A brick of two Materials with a surface in it takes 34
    /// to 50 words, so the default is room for 40 to 60 thousand of them in each level.
    pub words_per_ring: usize,
}

impl Default for Room {
    fn default() -> Self {
        Self {
            regions_per_ring: 4096,
            refs_per_ring: 1 << 18,
            words_per_ring: 1 << 21,
        }
    }
}

/// The rings do not fit in their buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingsFull {
    /// The Planet is denser than the buffers were sized for, or the words that are free are in
    /// pieces too small for a region, and the scene was given its room (a scene sized by its bricks
    /// grows the buffer that is full instead).
    Room,
    /// A buffer would have to be longer than the device allows one to be
    /// ([`Scene::set_buffer_limit`]), even with the growth cut down to what is needed.
    BufferLimit {
        part: Part,
        /// The length, in words, that the buffer needs.
        needed: usize,
        /// The most words a buffer may have.
        limit: usize,
    },
}

impl fmt::Display for RingsFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Room => write!(f, "the rings do not fit in their buffers"),
            Self::BufferLimit {
                part,
                needed,
                limit,
            } => write!(
                f,
                "the rings need {needed} words in the {part:?} buffer, more than the device's \
                 limit of {limit} words a buffer"
            ),
        }
    }
}

impl std::error::Error for RingsFull {}

/// Hands out ranges of the words of one buffer, first fit by address, and takes them back: a
/// request gets the free range with the lowest start that is long enough, cut from its front, so
/// that the bricks of a scene lie together in the buffer (the reference's bounce rays are
/// incoherent, and read better from a buffer that is packed than from one that is scattered).
///
/// The free ranges are kept in address order in blocks of a few hundred, each with the length of
/// its longest range. A buffer that has had bricks written and freed for a long time is in tens of
/// thousands of pieces, and looking along them for one that fits is what a frame cannot afford:
/// this looks along the blocks, then along one.
#[derive(Debug)]
struct Pool {
    /// Never empty blocks; in address order, and so are the ranges in them. Ranges never touch:
    /// ones that touch are one range.
    blocks: Vec<Block>,
}

/// Most ranges a block of a [`Pool`] holds before it is split in two.
const BLOCK: usize = 512;

#[derive(Debug)]
struct Block {
    ranges: Vec<Range<usize>>,
    /// The length of the longest of `ranges`.
    longest: usize,
}

impl Block {
    fn new(ranges: Vec<Range<usize>>) -> Self {
        let mut block = Self { ranges, longest: 0 };
        block.measure();
        block
    }

    fn measure(&mut self) {
        self.longest = self.ranges.iter().map(Range::len).max().unwrap_or(0);
    }
}

impl Pool {
    fn new(capacity: usize) -> Self {
        let blocks = if capacity > 0 {
            vec![Block::new(std::iter::once(0..capacity).collect())]
        } else {
            Vec::new()
        };
        Self { blocks }
    }

    /// One past the last word in use of the `capacity` the pool was made for: where the free range
    /// that reaches the end of the buffer begins.
    fn used_end(&self, capacity: usize) -> usize {
        match self.blocks.last().and_then(|b| b.ranges.last()) {
            Some(r) if r.end >= capacity => r.start.min(capacity),
            _ => capacity,
        }
    }

    /// The free ranges, ascending.
    fn free(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.blocks.iter().flat_map(|b| b.ranges.iter().cloned())
    }

    /// The start of `len` words that are free, which are now taken, if there are that many in a
    /// row.
    fn take(&mut self, len: usize) -> Option<usize> {
        let bi = self.blocks.iter().position(|b| b.longest >= len)?;
        let block = &mut self.blocks[bi];
        let at = block.ranges.iter().position(|r| r.len() >= len)?;
        let start = block.ranges[at].start;
        if block.ranges[at].len() == len {
            block.ranges.remove(at);
        } else {
            block.ranges[at].start += len;
        }
        if block.ranges.is_empty() {
            self.blocks.remove(bi);
        } else {
            block.measure();
        }
        Some(start)
    }

    /// Where the range that starts at `start` is, or would be put: the block and the place in it,
    /// which is the end of the block if every range in it starts earlier.
    fn place(&self, start: usize) -> (usize, usize) {
        let bi = self
            .blocks
            .partition_point(|b| b.ranges[0].start < start)
            .saturating_sub(1);
        let ri = self
            .blocks
            .get(bi)
            .map_or(0, |b| b.ranges.partition_point(|r| r.start < start));
        (bi, ri)
    }

    /// Gives `len` words from `start` back, joining them to the free ranges they touch.
    fn give(&mut self, start: usize, len: usize) {
        let (bi, ri) = self.place(start);
        let end = start + len;
        // The range before, which is the one before `ri` in its block (a block that is not the
        // first is only chosen if its first range starts before `start`), and the range after, which
        // may be the first of the next block.
        let before = (ri > 0).then(|| (bi, ri - 1));
        let after = match self.blocks.get(bi) {
            Some(b) if ri < b.ranges.len() => Some((bi, ri)),
            _ if bi + 1 < self.blocks.len() => Some((bi + 1, 0)),
            _ => None,
        };
        let touches_before = before.filter(|&(b, r)| self.blocks[b].ranges[r].end == start);
        let touches_after = after.filter(|&(b, r)| self.blocks[b].ranges[r].start == end);
        match (touches_before, touches_after) {
            (Some((b, r)), Some((ab, ar))) => {
                let joined = self.blocks[ab].ranges[ar].end;
                self.blocks[b].ranges[r].end = joined;
                self.blocks[b].measure();
                self.blocks[ab].ranges.remove(ar);
                if self.blocks[ab].ranges.is_empty() {
                    self.blocks.remove(ab);
                } else {
                    self.blocks[ab].measure();
                }
            }
            (Some((b, r)), None) => {
                self.blocks[b].ranges[r].end = end;
                self.blocks[b].measure();
            }
            (None, Some((b, r))) => {
                self.blocks[b].ranges[r].start = start;
                self.blocks[b].measure();
            }
            (None, None) => self.insert(bi, ri, start..end),
        }
    }

    /// Puts `range` at place `ri` of block `bi`, splitting the block if it is full.
    fn insert(&mut self, bi: usize, ri: usize, range: Range<usize>) {
        let Some(block) = self.blocks.get_mut(bi) else {
            self.blocks.push(Block::new(vec![range]));
            return;
        };
        block.longest = block.longest.max(range.len());
        block.ranges.insert(ri, range);
        if block.ranges.len() > BLOCK {
            let upper = block.ranges.split_off(BLOCK / 2);
            block.measure();
            self.blocks.insert(bi + 1, Block::new(upper));
        }
    }
}

/// How a scene of rings is sized.
#[derive(Clone, Copy, Debug)]
enum Sizing {
    /// By a [`Room`] that was given: a scene that does not fit is an error.
    Given(Room),
    /// By what the bricks selected need, and this many times as much, so that the camera can go
    /// on to where there are more, and at least this many words of `brick_refs` and of `bricks`.
    /// A buffer that is full grows.
    Fitted(f64, (usize, usize)),
}

/// What a scene of rings keeps besides the buffers.
#[derive(Debug)]
pub(super) struct Rings {
    sizing: Sizing,
    /// The records of `regions` that no region uses. There is one for every region that the
    /// levels can list, so a region always finds one.
    free: Vec<usize>,
    /// Regions each level can list, and how many it does.
    capacity: Vec<usize>,
    listed: Vec<usize>,
    refs: Pool,
    data: Pool,
    /// The words of a brick on their way into `bricks`.
    scratch: Vec<u32>,
    /// What has been written since the last patch was handed out.
    patch: Patch,
    selection: Selection,
    /// The ball the selection looked in for the Planet (the scene's own bound may be taken off).
    ball: Bound,
    /// The coarsest level with a brick, as the header last said.
    coarsest: Option<usize>,
    /// What [`Scene::reserve`] made the selection's tables roomy for (nodes, waiting, stuck), for
    /// the bands the selection had then: a scene made again starts its selection empty, and is
    /// given this again.
    reserved: Option<Reserved>,
    /// Why the scene was last made again, if it was.
    rebuilt: Option<Rebuild>,
    /// The lengths of the buffers that the last patch said (see [`RingPatch::sizes`]).
    reported: Lengths,
    /// The most words a buffer may have (see [`Scene::set_buffer_limit`]).
    limit_words: usize,
}

/// The room [`Scene::reserve`] was asked to make in the selection, and the bands' `d0` it was for.
#[derive(Clone, Copy, Debug)]
struct Reserved {
    nodes: usize,
    waiting: usize,
    stuck: usize,
    d0: f64,
}

/// Why a scene of rings was packed again and uploaded in full (a buffer that fills only grows) ([`SceneUpdate::Rebuilt`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rebuild {
    /// Other LOD bands were asked for (a new bias): expected.
    Bands,
    /// The camera jumped further than the rings follow: expected.
    Jump,
}

/// A [`RingPatch`] in the making.
#[derive(Debug, Default)]
struct Patch {
    /// Regions unlisted since the last patch, by level and key: a level whose box one of them
    /// was on the edge of may have a smaller one now.
    unlisted: Vec<(usize, [i32; 3])>,
    regions: BTreeSet<usize>,
    table: BTreeSet<usize>,
    brick_refs: Vec<Range<usize>>,
    bricks: Vec<Range<usize>>,
    levels: bool,
}

/// The lengths of the four buffers of a scene of rings, in words. Each grows on its own, when it
/// is the one that ran out of room (see [`Scene::take_words`] and `grow_level`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lengths {
    pub table: usize,
    pub regions: usize,
    pub brick_refs: usize,
    pub bricks: usize,
}

impl Lengths {
    /// The lengths of the buffers of `scene`.
    pub fn of(scene: &Scene) -> Self {
        Self {
            table: scene.table.len(),
            regions: scene.regions.len(),
            brick_refs: scene.brick_refs.len(),
            bricks: scene.bricks.len(),
        }
    }
}

/// What changed in a scene of rings, for copying to a GPU copy of it. Everything else is as
/// before.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RingPatch {
    /// The records of `regions` that were written, by index.
    pub regions: Vec<usize>,
    /// The slots of `table` that were written, by index.
    pub table: Vec<usize>,
    /// The words of `brick_refs` and `bricks` that were written, as ascending ranges that do not
    /// touch.
    pub brick_refs: Vec<Range<usize>>,
    pub bricks: Vec<Range<usize>>,
    /// Whether [`Scene::levels`] changed: the boxes walked, the holes, or the ball that rays are
    /// clipped to.
    pub levels: bool,
    /// The lengths the buffers have now, if any of them grew (they never shrink): a copy of the
    /// scene makes the buffers that are shorter this long, with what they had, before it writes
    /// the words above. A buffer that is not named here has not changed in length.
    pub sizes: Option<Lengths>,
}

/// The buffers of a scene of rings, for naming the part of one to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Table,
    Regions,
    BrickRefs,
    Bricks,
}

/// What copying a [`RingPatch`] to the GPU costs: the bytes of the copies, and how many there are
/// (each is a copy on the GPU with a barrier of its own), the header's included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UploadCost {
    pub bytes: u64,
    pub writes: u32,
}

impl UploadCost {
    /// This and `other`, both.
    pub fn plus(self, other: UploadCost) -> UploadCost {
        UploadCost {
            bytes: self.bytes + other.bytes,
            writes: self.writes + other.writes,
        }
    }
}

/// Words that a gap between two words written may have, for one copy to write them both: the
/// words in the gap are written again as they are. A copy costs a barrier and a command, a
/// thousand words 4 KB at several GB/s, so it is cheaper to write a few thousand words twice
/// than to make another copy. A first value, set from that arithmetic and not from a measurement:
/// the bench was not run with other values, so there is no figure of what joining saves or costs
/// (docs/measurements.md has only the copies per frame that it gives, 43 at the most on the
/// test Planet's walk).
pub const COPY_GAP: usize = 1024;

/// The ranges of `ranges` (ascending) with those that are `gap` words apart or less made one.
fn joined(ranges: impl IntoIterator<Item = Range<usize>>, gap: usize) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for range in ranges.into_iter().filter(|r| !r.is_empty()) {
        match out.last_mut() {
            Some(last) if range.start <= last.end + gap => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

impl RingPatch {
    /// Words that copying this patch to the GPU writes, apart from the header.
    pub fn words(&self) -> usize {
        let ranges = |r: &[Range<usize>]| r.iter().map(Range::len).sum::<usize>();
        self.regions.len() * REGION_WORDS
            + self.table.len() * 4
            + ranges(&self.brick_refs)
            + ranges(&self.bricks)
    }

    /// The copies that bring a GPU copy up to date with this patch, in the order to make them:
    /// the words of bricks, then the lists of entries, the records, and the table, as ranges of
    /// words of each buffer. Ranges that are [`COPY_GAP`] words apart or less are one copy, so a
    /// patch of thousands of small writes is a few dozen copies; the words between are written as
    /// the scene has them, which is what the GPU copy has too, since it is brought up to date by
    /// every patch before this one.
    pub fn copies(&self) -> Vec<(Part, Range<usize>)> {
        let slots = self.table.iter().map(|&s| s * 4..s * 4 + 4);
        let records = self
            .regions
            .iter()
            .map(|&r| r * REGION_WORDS..(r + 1) * REGION_WORDS);
        let mut out = Vec::new();
        for (part, ranges) in [
            (Part::Bricks, joined(self.bricks.iter().cloned(), COPY_GAP)),
            (
                Part::BrickRefs,
                joined(self.brick_refs.iter().cloned(), COPY_GAP),
            ),
            (Part::Regions, joined(records, COPY_GAP)),
            (Part::Table, joined(slots, COPY_GAP)),
        ] {
            out.extend(ranges.into_iter().map(|range| (part, range)));
        }
        out
    }

    /// What [`RingPatch::copies`] and the header cost to write.
    pub fn cost(&self) -> UploadCost {
        let copies = self.copies();
        let header = u64::from(self.levels);
        UploadCost {
            bytes: copies.iter().map(|(_, r)| r.len() as u64 * 4).sum::<u64>()
                + header * size_of::<SceneHeader>() as u64,
            writes: copies.len() as u32 + header as u32,
        }
    }
}

impl RingPatch {
    /// Whether copying this patch writes nothing.
    pub fn is_empty(&self) -> bool {
        self.words() == 0 && !self.levels
    }

    /// This patch and then `later`: what a copy of the scene needs to be brought up to date through
    /// both, written from the scene as it is after both.
    pub fn merge(&mut self, later: RingPatch) {
        let union = |mut a: Vec<usize>, b: Vec<usize>| {
            a.extend(b);
            a.sort_unstable();
            a.dedup();
            a
        };
        self.regions = union(std::mem::take(&mut self.regions), later.regions);
        self.table = union(std::mem::take(&mut self.table), later.table);
        let both = |mut a: Vec<Range<usize>>, b: Vec<Range<usize>>| {
            a.extend(b);
            merged(a)
        };
        self.brick_refs = both(std::mem::take(&mut self.brick_refs), later.brick_refs);
        self.bricks = both(std::mem::take(&mut self.bricks), later.bricks);
        self.levels |= later.levels;
        // The buffers only grow, so the later lengths are the longer.
        self.sizes = later.sizes.or(self.sizes);
    }
}

/// A [`RingPatch`] with the words it says were written, as the scene has them, and the header: all
/// that a copy of the scene (see [`Scene::mirror`]) needs to show what the scene shows, though
/// the scene has gone on since. The words are copied out, so that the scene can be written to
/// again, and a later copy of a patch that this one is part of ([`RingPatch::merge`]) says what
/// the scene is by then.
#[derive(Clone, Debug, Default)]
pub struct PatchCopy {
    pub patch: RingPatch,
    regions: Vec<u32>,
    table: Vec<u32>,
    brick_refs: Vec<u32>,
    bricks: Vec<u32>,
    levels: Vec<Level>,
    bound: Option<Bound>,
    coarsest: Option<usize>,
}

/// `ranges` in ascending order, with those that overlap or touch made one.
fn merged(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::new();
    for range in ranges.into_iter().filter(|r| !r.is_empty()) {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

/// What a brick of a region is to be, when the region is packed again.
enum Want {
    /// Not in the region.
    Absent,
    /// One Material throughout: its entry in `brick_refs`.
    Solid(u32),
    /// Mixed, with these words (a range of the words made for the region).
    Mixed(Range<usize>),
}

/// The bricks of `selection` or of `keys`, by the level and region they are in.
type ByRegion = BTreeMap<(usize, [i32; 3]), BTreeSet<BrickCoord>>;

fn group(keys: impl IntoIterator<Item = (usize, BrickCoord)>) -> ByRegion {
    let mut by_region = ByRegion::new();
    for (level, coord) in keys {
        by_region
            .entry((level, region_of(coord)))
            .or_default()
            .insert(coord);
    }
    by_region
}

/// How far the camera can go in one move, in `d0`, before the scene is made again instead of
/// patched. Patching costs about in proportion to the distance (8 ms for 100 Voxels, 90 ms for
/// 1000, 170 ms for 2000 on the test Planet), a new scene about 200 ms wherever it is, and less
/// the further out the camera is, and they meet at about two `d0` (2056 Voxels).
const JUMP: f64 = 2.0;

/// The least a buffer that has run out of room grows by, in words (1 MiB).
const GROW_LEAST: usize = 1 << 18;

/// How many words to add to a buffer of `len` words that has no `needed` words in a row: a
/// quarter of it, and at least [`GROW_LEAST`], so that a buffer that is filling grows a few times
/// and not at every region, each time by what the allocation and the copy on the GPU are worth.
/// The buffer may not be longer than `limit`: the slack is cut down to what the limit leaves, and
/// to nothing, if `needed` is all that it leaves or more. `None` if `needed` does not fit.
fn grow_by(len: usize, needed: usize, limit: usize) -> Option<usize> {
    let room = limit.saturating_sub(len);
    (needed <= room).then(|| needed.max(len / 4).max(GROW_LEAST).min(room))
}

/// Regions of bricks read on the threads before they are packed.
const PREFETCH_REGIONS: usize = 256;

/// Regions that the bricks of one level can be in, at the most, wherever the camera is: those of
/// its band, which is about as many at every level (a ring twice as wide has bricks twice as wide;
/// see ADR 0002), three times over for the layers of the crust; or those of the ball the Planet
/// is in, if that is less.
fn shell_regions(bands: Bands, bound: Bound, level: usize) -> usize {
    let size = f64::from(REGION_VOXELS << level);
    let band = std::f64::consts::PI * (2.0 * bands.d0() / f64::from(REGION_VOXELS)).powi(2);
    let ball = 4.0 * std::f64::consts::PI * ((f64::from(bound.radius) / size) + 2.0).powi(2);
    (3.0 * band.min(ball)) as usize
}

/// Regions to leave room for in a table of a level that lists `regions`.
fn regions_for(regions: usize, headroom: f64) -> usize {
    (regions as f64 * headroom) as usize + 64
}

impl Scene {
    /// Rings for a Planet inside `bound`: the bricks of `store` that the LOD `bands` select for a
    /// camera at `camera`, a position in Voxels, each level a sparse set of regions of its own.
    /// The store must have its pyramid ([`BrickStore::build_lod`]); the levels are those of it, at
    /// most [`crate::lod::LEVELS`]. Every level is read through [`BrickStore::brick_at`], so the
    /// store's source stands in for the bricks it does not hold, as the core of the Planet does.
    ///
    /// The module's docs say how rings are laid out and walked. [`Scene::recentre`] keeps them
    /// following the camera and [`Scene::update`] takes Edits in. The buffers are the size that
    /// the bricks need, and grow by packing again if the camera goes where they need more.
    pub fn rings(
        store: &BrickStore,
        camera: [f64; 3],
        bands: Bands,
        bound: Bound,
    ) -> Result<Scene, RingsFull> {
        Scene::rings_sized(store, camera, bands, bound, Sizing::Fitted(2.0, (0, 0)))
    }

    /// [`Scene::rings`] with buffers of at least `refs` words of `brick_refs` and `words` of
    /// `bricks`, for a scene that is going to grow a lot without being packed again (see
    /// [`Scene::arrive`]): the size it is expected to reach, which the caller knows.
    pub fn rings_reserving(
        store: &BrickStore,
        camera: [f64; 3],
        bands: Bands,
        bound: Bound,
        refs: usize,
        words: usize,
    ) -> Result<Scene, RingsFull> {
        Scene::rings_sized(
            store,
            camera,
            bands,
            bound,
            Sizing::Fitted(2.0, (refs, words)),
        )
    }

    /// [`Scene::rings`] with buffers of the size `room` says. That is all the room there will
    /// be: a scene that needs more is an error ([`RingsFull`]).
    pub fn rings_in(
        store: &BrickStore,
        camera: [f64; 3],
        bands: Bands,
        bound: Bound,
        room: Room,
    ) -> Result<Scene, RingsFull> {
        Scene::rings_sized(store, camera, bands, bound, Sizing::Given(room))
    }

    fn rings_sized(
        store: &BrickStore,
        camera: [f64; 3],
        bands: Bands,
        bound: Bound,
        sizing: Sizing,
    ) -> Result<Scene, RingsFull> {
        let top = select::top_level(store);
        let roots = select::top_bricks(store, top, Some((bound.centre, bound.radius)));
        let (selection, _) = Selection::new(store, camera, bands, top, roots);
        Scene::pack_rings(store, selection, bound, sizing)
    }

    /// A scene of rings that shows `selection`, with buffers sized as `sizing` says.
    fn pack_rings(
        store: &BrickStore,
        selection: Selection,
        bound: Bound,
        sizing: Sizing,
    ) -> Result<Scene, RingsFull> {
        let by_region = group(selection.bricks());
        let levels = selection.top() + 1;
        let mut wanted = vec![0; levels];
        for &(level, _) in by_region.keys() {
            wanted[level] += 1;
        }
        let (capacity, refs, words) = match sizing {
            Sizing::Given(room) => (
                vec![room.regions_per_ring; levels],
                room.refs_per_ring * levels,
                room.words_per_ring * levels,
            ),
            // The words are not known until the bricks are packed: they are packed into buffers
            // that grow, and the room is made after.
            // A level that has few regions now may have many once the camera has moved (each level
            // is about as full as the others at the surface of a Planet), so none is sized by
            // less than half of what the busiest has, nor by less than what its band can hold
            // (see [`shell_regions`]).
            Sizing::Fitted(headroom, _) => (
                {
                    let most = wanted.iter().copied().max().unwrap_or(0);
                    let bands = selection.bands();
                    (0..levels)
                        .map(|level| {
                            let n = wanted[level].max(most / 2);
                            regions_for(n.max(shell_regions(bands, bound, level)), headroom)
                        })
                        .collect()
                },
                0,
                0,
            ),
        };
        let fitted = matches!(sizing, Sizing::Fitted(..));
        let mut scene = Scene::empty_rings(selection, bound, sizing, capacity, refs, words);
        // Reading the bricks is what takes the time (they may be generated), so a chunk of regions
        // is read on all the threads, and then packed.
        let regions: Vec<_> = by_region.into_iter().collect();
        for chunk in regions.chunks(PREFETCH_REGIONS) {
            let read = select::par_map(chunk, |((level, _), bricks)| {
                bricks
                    .iter()
                    .map(|&coord| (coord, store.brick_at(*level, coord).into_owned()))
                    .collect::<HashMap<_, _>>()
            });
            for (((level, key), bricks), read) in chunk.iter().zip(&read) {
                scene.repack_region(store, *level, *key, bricks, Some(read))?;
            }
        }
        if let (true, Sizing::Fitted(headroom, at_least)) = (fitted, sizing) {
            scene.make_room(headroom, at_least);
        }
        scene.sync_header();
        scene.take_patch();
        Ok(scene)
    }

    /// A scene of rings with nothing listed yet.
    fn empty_rings(
        selection: Selection,
        bound: Bound,
        sizing: Sizing,
        capacity: Vec<usize>,
        refs: usize,
        words: usize,
    ) -> Scene {
        let levels = capacity.len();
        let mut base = 0;
        let level_headers: Vec<Level> = capacity
            .iter()
            .map(|&regions| {
                // Half full at most, so searches for a region end quickly.
                let slots = (regions * 2).next_power_of_two().max(2);
                let level = Level {
                    table_base: base as u32,
                    table_mask: slots as u32 - 1,
                    ..Level::default()
                };
                base += slots;
                level
            })
            .collect();
        let records: usize = capacity.iter().sum();
        // Fitted rings pack into buffers that grow, with room taken from the pools as if there
        // were no end to them.
        let unbounded = matches!(sizing, Sizing::Fitted(..));
        let pool = |words: usize| Pool::new(if unbounded { usize::MAX / 4 } else { words });
        Scene {
            levels: level_headers,
            table: vec![0; base * 4],
            regions: vec![0; records * REGION_WORDS],
            brick_refs: vec![0; refs],
            bricks: vec![0; words],
            bound: Some(bound),
            rings: Some(Rings {
                sizing,
                free: (0..records).rev().collect(),
                capacity,
                listed: vec![0; levels],
                refs: pool(refs),
                data: pool(words),
                scratch: Vec::new(),
                patch: Patch::default(),
                selection,
                ball: bound,
                coarsest: None,
                reserved: None,
                rebuilt: None,
                reported: Lengths::default(),
                limit_words: usize::MAX,
            }),
            ..Scene::default()
        }
    }

    /// Makes the buffers of a scene that was packed into growing ones the size they will stay:
    /// what is used, and `headroom` times as much, but not less than `at_least`.
    fn make_room(&mut self, headroom: f64, at_least: (usize, usize)) {
        let rings = self.rings.as_mut().expect("a scene of rings");
        let room =
            |used: usize, least: usize| ((used as f64 * headroom) as usize + 4096).max(least);
        let (refs, words) = (
            room(self.brick_refs.len(), at_least.0),
            room(self.bricks.len(), at_least.1),
        );
        rings.refs = Pool::new(refs);
        rings.refs.take(self.brick_refs.len());
        rings.data = Pool::new(words);
        rings.data.take(self.bricks.len());
        self.brick_refs.resize(refs, 0);
        self.bricks.resize(words, 0);
    }

    /// The scene packed again from scratch: what `selection` is, with the words in order. The
    /// buffers are sized as they were, or, for a scene that was sized by its bricks, again by
    /// them and `headroom` times as much.
    fn repacked(&self, store: &BrickStore, headroom: Option<f64>) -> Result<Scene, RingsFull> {
        let rings = self.rings.as_ref().expect("a scene of rings");
        let sizing = match (rings.sizing, headroom) {
            (Sizing::Fitted(_, at_least), Some(headroom)) => Sizing::Fitted(headroom, at_least),
            (sizing, _) => sizing,
        };
        let bound = self.bound.is_some().then_some(rings.ball);
        let mut scene = Scene::pack_rings(store, rings.selection.clone(), rings.ball, sizing)?;
        scene.bound = bound;
        scene.set_buffer_limit(rings.limit_words);
        Ok(scene)
    }

    /// A scene of rings packed again from scratch from `store`, with the same bricks selected and
    /// room: what the scene should show, with the words in order.
    pub fn rebuilt_rings(&self, store: &BrickStore) -> Result<Scene, RingsFull> {
        self.repacked(store, None)
    }

    /// The ball the selection of a scene of rings looks in, which a scene made again starts from.
    pub(super) fn rings_ball(&self) -> Option<Bound> {
        self.rings.as_ref().map(|r| r.ball)
    }

    /// Sets the ball of [`Scene::rings_ball`], if this is a scene of rings.
    pub(super) fn set_rings_ball(&mut self, ball: Bound) {
        if let Some(rings) = &mut self.rings {
            rings.ball = ball;
        }
    }

    /// A scene that has the words of this scene of rings, for showing what it shows without the
    /// selection or the room to change: the copy that [`Scene::paste`] brings up to date, as the
    /// patches that follow say. The words past the last one in use are not copied, but the buffers
    /// are the size of this scene's, so that every index is in them. Panics if this is not a
    /// scene of rings.
    pub fn mirror(&self) -> Scene {
        let rings = self.rings.as_ref().expect("a scene of rings");
        // All of the buffer is written, not left as untouched zero pages: the pages that the
        // changes land in later would otherwise be found by faults, which is milliseconds on the
        // thread that pastes them.
        let held = |words: &[u32], pool: &Pool| {
            let mut copy = Vec::with_capacity(words.len());
            copy.extend_from_slice(&words[..pool.used_end(words.len())]);
            copy.resize(words.len(), 0);
            copy
        };
        Scene {
            levels: self.levels.clone(),
            table: self.table.clone(),
            regions: self.regions.clone(),
            brick_refs: held(&self.brick_refs, &rings.refs),
            bricks: held(&self.bricks, &rings.data),
            bound: self.bound,
            shown_coarsest: rings.selection.coarsest(),
            shown_rebuilt: rings.rebuilt,
            ..Scene::default()
        }
    }

    /// The words that `patch`, which this scene of rings made (or merged patches that it made, the
    /// latest last), wrote, as the scene has them now, with the header.
    pub fn copy_patch(&self, patch: RingPatch) -> PatchCopy {
        self.copy_patch_into(patch, PatchCopy::default())
    }

    /// [`Scene::copy_patch`], into the buffers of `spare`, a copy that is of no more use: so that
    /// the room it has is used again, and not given back and asked for again.
    pub fn copy_patch_into(&self, patch: RingPatch, mut copy: PatchCopy) -> PatchCopy {
        copy.regions.clear();
        copy.regions.extend(patch.regions.iter().flat_map(|&i| {
            self.regions[i * REGION_WORDS..][..REGION_WORDS]
                .iter()
                .copied()
        }));
        copy.table.clear();
        copy.table.extend(
            patch
                .table
                .iter()
                .flat_map(|&i| self.table[i * 4..][..4].iter().copied()),
        );
        for (out, words, ranges) in [
            (&mut copy.brick_refs, &self.brick_refs, &patch.brick_refs),
            (&mut copy.bricks, &self.bricks, &patch.bricks),
        ] {
            out.clear();
            for range in ranges {
                out.extend_from_slice(&words[range.clone()]);
            }
        }
        copy.levels.clone_from(&self.levels);
        copy.bound = self.bound;
        copy.coarsest = self.coarsest_drawn();
        copy.patch = patch;
        copy
    }

    /// Writes `copy` into this scene, a [`Scene::mirror`] of the scene of rings that made it (or
    /// one that was brought up to date with every copy before it): what is written is what the
    /// scene of rings had written, and the header is as it was.
    ///
    /// Returns the time it took to make the buffers that grew longer, which is what this copy
    /// costs a growth beyond what writing the words costs: the new words are written as zero, and
    /// that is a few milliseconds for each 100 MB.
    pub fn paste(&mut self, copy: &PatchCopy) -> std::time::Duration {
        let patch = &copy.patch;
        let lengthened = std::time::Instant::now();
        // A buffer that grew is longer before anything is written to it.
        if let Some(sizes) = patch.sizes {
            for (buffer, len) in [
                (&mut self.table, sizes.table),
                (&mut self.regions, sizes.regions),
                (&mut self.brick_refs, sizes.brick_refs),
                (&mut self.bricks, sizes.bricks),
            ] {
                if buffer.len() < len {
                    buffer.resize(len, 0);
                }
            }
        }
        let lengthened = lengthened.elapsed();
        for (&i, words) in patch.regions.iter().zip(copy.regions.chunks(REGION_WORDS)) {
            self.regions[i * REGION_WORDS..][..REGION_WORDS].copy_from_slice(words);
        }
        for (&i, words) in patch.table.iter().zip(copy.table.chunks(4)) {
            self.table[i * 4..][..4].copy_from_slice(words);
        }
        for (buffer, ranges, words) in [
            (&mut self.brick_refs, &patch.brick_refs, &copy.brick_refs),
            (&mut self.bricks, &patch.bricks, &copy.bricks),
        ] {
            let mut at = 0;
            for range in ranges {
                buffer[range.clone()].copy_from_slice(&words[at..at + range.len()]);
                at += range.len();
            }
        }
        self.levels.clone_from(&copy.levels);
        self.bound = copy.bound;
        self.shown_coarsest = copy.coarsest;
        lengthened
    }

    /// Sets the most words that a buffer of this scene of rings may have: the device's limit on a
    /// storage buffer. A buffer that is full grows by no more than that leaves (and by less than
    /// its slack, if need be), and a scene that needs more is [`RingsFull::BufferLimit`], not a
    /// buffer that the GPU cannot make. A scene made again from this one has it too.
    pub fn set_buffer_limit(&mut self, words: usize) {
        if let Some(rings) = &mut self.rings {
            rings.limit_words = words;
        }
    }

    /// Whether this is a scene of rings (see [`Scene::rings`]).
    pub fn is_rings(&self) -> bool {
        self.rings.is_some()
    }

    /// The LOD bands the rings are selected by, if this is a scene of rings.
    pub fn bands(&self) -> Option<Bands> {
        self.rings.as_ref().map(|r| r.selection.bands())
    }

    /// Selects the bricks again with other `bands` (a new bias, or another image height), from
    /// the camera where it is, and packs the scene again. Reported as [`SceneUpdate::Rebuilt`].
    pub fn set_bands(
        &mut self,
        store: &BrickStore,
        bands: Bands,
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        let Some(rings) = &self.rings else {
            return Ok(None);
        };
        if rings.selection.bands() == bands {
            return Ok(None);
        }
        let camera = rings.selection.camera();
        self.rebuilt_at(store, camera, bands, Rebuild::Bands)
            .map(Some)
    }

    /// The scene made again for `camera` and `bands`, in place, from nothing: what a jump of the
    /// camera or another bias asks for. Reported as [`SceneUpdate::Rebuilt`]. The selection is
    /// made roomy again as [`Scene::reserve`] did, in proportion to `d0` squared if the bands
    /// changed.
    fn rebuilt_at(
        &mut self,
        store: &BrickStore,
        camera: [f64; 3],
        bands: Bands,
        why: Rebuild,
    ) -> Result<SceneUpdate, RingsFull> {
        let rings = self.rings.as_ref().expect("a scene of rings");
        let (ball, sizing, clipped) = (rings.ball, rings.sizing, self.bound.is_some());
        let (reserved, reach) = (rings.reserved, rings.selection.reach());
        let changed = self.changed;
        *self = Scene::rings_sized(store, camera, bands, ball, sizing)?;
        self.changed = changed;
        self.bound = clipped.then_some(ball);
        if !reach.is_none()
            && let Some(rings) = &mut self.rings
        {
            rings.selection.set_ahead(reach.ahead);
            rings.selection.set_margins(store, reach.margins);
        }
        if let (Some(reserved), Some(rings)) = (reserved, &mut self.rings) {
            let scale = (bands.d0() / reserved.d0).powi(2);
            let by = |n: usize| (n as f64 * scale) as usize;
            let now = Reserved {
                nodes: by(reserved.nodes),
                waiting: by(reserved.waiting),
                stuck: by(reserved.stuck),
                d0: bands.d0(),
            };
            rings.selection.reserve(now.nodes, now.waiting, now.stuck);
            rings.reserved = Some(now);
        }
        self.rings.as_mut().expect("a scene of rings").rebuilt = Some(why);
        Ok(SceneUpdate::Rebuilt)
    }

    /// Why the scene was last packed again ([`SceneUpdate::Rebuilt`]), if it was and is rings.
    pub fn rebuild_reason(&self) -> Option<Rebuild> {
        self.rings
            .as_ref()
            .map_or(self.shown_rebuilt, |r| r.rebuilt)
    }

    /// The size of everything of a scene of rings that is made roomy up front: the selection's
    /// tables, the words of `brick_refs` and of `bricks` in use against what the buffers hold,
    /// and the regions each level lists against what it can (these are fitted again whenever the
    /// scene is packed again, not reserved, and grow, each on its own, when they fill). Empty for a scene that is
    /// not rings.
    pub fn table_sizes(&self) -> Vec<TableSize> {
        let Some(rings) = &self.rings else {
            return Vec::new();
        };
        let mut sizes = rings.selection.table_sizes();
        let (refs, data) = self.used_words().unwrap_or_default();
        for (name, used, words) in [
            ("scene brick_refs words", refs, self.brick_refs.len()),
            ("scene bricks words", data, self.bricks.len()),
        ] {
            sizes.push(TableSize::list::<u32>(name, used, words));
        }
        for (level, (listed, capacity)) in rings.listed.iter().zip(&rings.capacity).enumerate() {
            sizes.push(TableSize::new(
                format!("scene level {level} regions"),
                *listed,
                *capacity,
            ));
        }
        sizes
    }

    /// Bricks drawn at `level` (see [`Selection::count`]): the part of the scene that the level
    /// is. Zero for a scene that is not rings.
    pub fn level_bricks(&self, level: usize) -> usize {
        self.rings
            .as_ref()
            .map_or(0, |rings| rings.selection.count(level))
    }

    /// The level of the brick that is drawn where the Voxel of level 0 at `voxel` is, if one is.
    /// A copy of a scene of rings ([`Scene::mirror`]) finds it in the regions it lists.
    pub fn level_at(&self, voxel: [i32; 3]) -> Option<usize> {
        let brick = |level: usize| BrickCoord(voxel.map(|v| v >> (level + 3)));
        match &self.rings {
            Some(rings) => {
                (0..self.levels.len()).find(|&level| rings.selection.draws(level, brick(level)))
            }
            None => (0..self.levels.len()).find(|&level| {
                let coord = brick(level);
                self.find_region(level, region_of(coord))
                    .is_some_and(|index| {
                        let bit = bit_of(coord);
                        self.regions[index * REGION_WORDS + bit / 32] >> (bit % 32) & 1 != 0
                    })
            }),
        }
    }

    /// Whether a scene of rings splits the brick at `coord` of `level` into the eight bricks
    /// under it (see [`Selection::splits`]), so that they are in use.
    pub fn splits(&self, level: usize, coord: BrickCoord) -> bool {
        self.rings
            .as_ref()
            .is_some_and(|rings| rings.selection.splits(level, coord))
    }

    /// Where the camera is that a scene of rings was last moved to, in Voxels.
    pub fn camera(&self) -> Option<[f64; 3]> {
        self.rings.as_ref().map(|r| r.selection.camera())
    }

    /// Follows the camera to `camera`, a position in Voxels: the bricks that the bands select
    /// change as it moves, and only the regions that lose or gain one are packed again. Returns
    /// what changed, or `None` if nothing did (and if this is not a scene of rings).
    ///
    /// If the regions do not fit, the buffer that is full grows (see the module docs). It is an
    /// error only for a scene that was given its room, which is then half moved and not to be
    /// used again. A jump is reported as [`SceneUpdate::Rebuilt`].
    pub fn recentre(
        &mut self,
        store: &BrickStore,
        camera: [f64; 3],
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        let Some(rings) = &mut self.rings else {
            return Ok(None);
        };
        // A camera that has gone far has most of the bricks to change, which patching does
        // slower than making the scene again: past about two d0 (measured on the test Planet).
        let bands = rings.selection.bands();
        let from = rings.selection.camera();
        let jumped = (0..3)
            .map(|a| (camera[a] - from[a]).powi(2))
            .sum::<f64>()
            .sqrt();
        if jumped > JUMP * bands.d0() {
            return self
                .rebuilt_at(store, camera, bands, Rebuild::Jump)
                .map(Some);
        }
        let diff = rings.selection.update(store, camera);
        self.show_diff(store, &diff)
    }

    /// Packs again the regions that `diff` (what the selection changed) touches. Returns what
    /// changed, or `None` if nothing did; the buffer that is full grows if the regions do not fit.
    fn show_diff(
        &mut self,
        store: &BrickStore,
        diff: &select::Diff,
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        self.changed += (diff.added.len() + diff.removed.len()) as u64;
        let attempt = self
            .apply(
                store,
                group(diff.added.iter().chain(&diff.removed).copied()),
            )
            .map(|patch| {
                let nothing = patch.words() == 0 && !patch.levels;
                (!nothing).then_some(patch)
            });
        attempt.map(|patch| patch.map(SceneUpdate::Rings))
    }

    /// The bricks that are drawn whole although the bands split them, because the bricks under
    /// them are not resident (see [`BrickStore::stream_from`]), that became so since this was last
    /// called, as `(level, coordinate)`. Load their eight children, mark them with
    /// [`BrickStore::set_children_resident`], and tell the scene with [`Scene::arrive`]. Empty for
    /// a scene that is not rings.
    pub fn take_wanted(&mut self) -> Vec<(usize, BrickCoord)> {
        self.rings
            .as_mut()
            .map_or_else(Vec::new, |r| r.selection.take_wanted())
    }

    /// [`Scene::take_wanted`], but at most `limit` of them; the rest are for the next call.
    pub fn take_wanted_up_to(&mut self, limit: usize) -> Vec<(usize, BrickCoord)> {
        self.rings
            .as_mut()
            .map_or_else(Vec::new, |r| r.selection.take_wanted_up_to(limit))
    }

    /// Makes room for `nodes` nodes of the selection, `waiting` bricks waiting and `stuck` bricks
    /// stuck (see [`Selection::reserve`]).
    pub fn reserve(&mut self, nodes: usize, waiting: usize, stuck: usize) {
        if let Some(rings) = &mut self.rings {
            rings.selection.reserve(nodes, waiting, stuck);
            rings.reserved = Some(Reserved {
                nodes,
                waiting,
                stuck,
                d0: rings.selection.bands().d0(),
            });
        }
    }

    /// How many bricks wait for the bricks under them to be resident.
    pub fn waiting(&self) -> usize {
        self.rings.as_ref().map_or(0, |r| r.selection.waiting())
    }

    /// Whether the selection has a brick to look at again that no camera move will bring (see
    /// [`Selection::rewanted`]).
    pub fn rewanted(&self) -> bool {
        self.rings.as_ref().is_some_and(|r| r.selection.rewanted())
    }

    /// Nodes of the selection: what the scene wants, drawn or split or in reach (see
    /// [`Selection::nodes`]).
    pub fn nodes(&self) -> usize {
        self.rings.as_ref().map_or(0, |r| r.selection.nodes())
    }

    /// The soft misses of this moment (see [`Selection::soft_misses`]).
    pub fn soft_misses(&self) -> usize {
        self.rings.as_ref().map_or(0, |r| r.selection.soft_misses())
    }

    /// The soft misses of this moment by the level of the brick that is drawn whole: the bricks of
    /// each Ring that the bands ask to be finer.
    pub fn soft_misses_by_level(&self) -> [usize; LEVELS] {
        self.rings
            .as_ref()
            .map_or([0; LEVELS], |r| r.selection.soft_misses_by_level())
    }

    /// Bricks that are split in the selection although the bricks under them are not resident in
    /// `store`: the scene draws their children, and the store does not hold them, so a ray that
    /// reaches the brick finds nothing where there is ground. Zero unless eviction took what the
    /// scene still draws, which it never does (see [`BrickStore::evict_children`]).
    pub fn holes(&self, store: &BrickStore) -> usize {
        self.rings.as_ref().map_or(0, |r| r.selection.holes(store))
    }

    /// How many bricks drawn have come or gone in the selections that packed this scene since it
    /// was made, which is how much the scene's packing has been asked to change. Cumulative: a
    /// frame's share is the difference between two reads.
    pub fn bricks_changed(&self) -> u64 {
        self.changed
    }

    /// What residency looks at besides the bands (see [`Selection::reach`]).
    pub fn reach(&self) -> Reach {
        self.rings
            .as_ref()
            .map_or(Reach::NONE, |r| r.selection.reach())
    }

    /// Wants the children of the bricks within `margins` resident, without drawing them (see
    /// [`Selection::set_margins`]). Kept when the scene is made again for other bands or a jump.
    pub fn set_margins(&mut self, store: &BrickStore, margins: Margins) {
        if let Some(rings) = &mut self.rings {
            rings.selection.set_margins(store, margins);
        }
    }

    /// Where the camera is going, for prefetch (see [`Selection::set_ahead`]).
    pub fn set_ahead(&mut self, ahead: [Option<[f64; 3]>; AHEAD]) {
        if let Some(rings) = &mut self.rings {
            rings.selection.set_ahead(ahead);
        }
    }

    /// The bricks of [`Scene::take_wanted`] that are not wanted any more: their nodes are gone
    /// (merged into a coarser brick, or buried) before their children arrived.
    pub fn take_cancelled(&mut self) -> Vec<(usize, BrickCoord)> {
        self.rings
            .as_mut()
            .map_or_else(Vec::new, |r| r.selection.take_cancelled())
    }

    /// Gives up waiting for the children of `key`, one of the bricks of [`Scene::take_wanted`],
    /// because they will not be generated: it is listed again when the bands split it again.
    pub fn unwant(&mut self, key: (usize, BrickCoord)) {
        if let Some(rings) = &mut self.rings {
            rings.selection.unwant(key);
        }
    }

    /// Splits the bricks `parents`, whose children are resident, as the bands say. Returns
    /// what changed, or `None` if nothing did; like [`Scene::recentre`] it grows the buffer that is
    /// full if the regions do not fit.
    pub fn arrive(
        &mut self,
        store: &BrickStore,
        parents: &[(usize, BrickCoord)],
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        let Some(rings) = &mut self.rings else {
            return Ok(None);
        };
        let diff = rings.selection.arrived(store, parents.iter().copied());
        self.show_diff(store, &diff)
    }

    /// Packs again the regions of `by_region`, in the bricks it lists, and brings the header up
    /// to date, and hands out the patch.
    fn apply(&mut self, store: &BrickStore, by_region: ByRegion) -> Result<RingPatch, RingsFull> {
        for ((level, key), bricks) in by_region {
            self.repack_region(store, level, key, &bricks, None)?;
        }
        self.sync_header();
        Ok(self.take_patch())
    }

    /// [`Scene::update`] for rings. The selection takes in what the Edit did to what is drawn
    /// (a brick may have become air, or solid with no air beside it, or the reverse), and the
    /// bricks over each changed brick that are drawn are packed again at every level, wherever
    /// they are; the store has remade them already (see [`BrickStore::refresh_lod`]). A brick
    /// whose words are what they were is not written again.
    ///
    /// If the words run out the buffer that is full grows (a scene that was given its room is
    /// [`RingsFull`]): the words that the Edits left free are used first, and a buffer that is in
    /// pieces too small for a run grows, and is not packed again.
    pub(super) fn update_rings(
        &mut self,
        store: &BrickStore,
        changed: &[BrickCoord],
    ) -> Result<SceneUpdate, RingsFull> {
        let rings = self.rings.as_mut().expect("a scene of rings");
        let diff = rings.selection.refresh(store, changed);
        let mut touched = group(diff.added.iter().chain(&diff.removed).copied());
        // The bricks over the changed ones, and over the ones beside them, which read the
        // Voxels across their border (ADR 0002), that are drawn.
        let mut around: BTreeSet<BrickCoord> = BTreeSet::new();
        for brick in changed {
            around.insert(*brick);
            for axis in 0..3 {
                for step in [-1, 1] {
                    let mut next = brick.0;
                    next[axis] += step;
                    around.insert(BrickCoord(next));
                }
            }
        }
        for level in 0..self.levels.len() {
            for brick in &around {
                let over = BrickCoord(brick.0.map(|c| c >> level));
                if self
                    .rings
                    .as_ref()
                    .is_some_and(|r| r.selection.draws(level, over))
                {
                    touched
                        .entry((level, region_of(over)))
                        .or_default()
                        .insert(over);
                }
            }
        }
        self.apply(store, touched).map(SceneUpdate::Rings)
    }

    /// Brings the regions of `level` of `key` up to date with the bricks that are selected: the
    /// bricks `touch` are written again (if their words differ) or dropped, and the other bricks
    /// of the region are as they were. A region left with no bricks is unlisted, and one that has
    /// some and was not listed is.
    ///
    /// **Where the words go.** The words of a region's mixed bricks are kept together, in the order
    /// of the region's mask, in one run of `bricks`: so that bricks that are next to each other in
    /// space are next to each other in memory, and a ray that goes through a region reads one place
    /// (what the reference's incoherent bounce rays gain from; see the module docs). A brick that
    /// reads differently but takes the same words is written where it is. Any other change to a
    /// region's mixed bricks (one that comes, goes, or takes other words) writes the region's
    /// bricks again, to a new run, and gives the old words back. The place of a run does not depend
    /// on what was done before beyond which words are free: the first free range that is long
    /// enough, lowest first.
    fn repack_region(
        &mut self,
        store: &BrickStore,
        level: usize,
        key: [i32; 3],
        touch: &BTreeSet<BrickCoord>,
        read: Option<&HashMap<BrickCoord, Brick>>,
    ) -> Result<(), RingsFull> {
        let found = self.find_region(level, key);
        let old_entries = found.map(|index| self.region_entries(index));
        let old_at = |bit: usize| old_entries.as_ref().and_then(|e| e[bit]);
        // The words of the bricks that are to be mixed, end to end, and what each touched brick is
        // to be.
        let mut words = std::mem::take(&mut self.rings.as_mut().expect("a scene of rings").scratch);
        words.clear();
        let mut wanted: Vec<(usize, Want)> = Vec::with_capacity(touch.len());
        for &coord in touch {
            let bit = bit_of(coord);
            let drawn = self
                .rings
                .as_ref()
                .is_some_and(|r| r.selection.draws(level, coord));
            let want = if drawn {
                let brick = match read.and_then(|r| r.get(&coord)) {
                    Some(brick) => Cow::Borrowed(brick),
                    None => store.brick_at(level, coord),
                };
                match brick.view() {
                    BrickView::Air => Want::Absent,
                    BrickView::Solid(material) => Want::Solid(SOLID_FLAG | u32::from(material.0)),
                    BrickView::Mixed {
                        occupancy,
                        palette,
                        bits,
                        indices,
                    } => {
                        let from = words.len();
                        encode_mixed(&mut words, occupancy, palette, bits, indices);
                        Want::Mixed(from..words.len())
                    }
                }
            } else {
                Want::Absent
            };
            wanted.push((bit, want));
        }
        // The bricks that are to be something else than they are.
        wanted.retain(|(bit, want)| {
            let old = old_at(*bit);
            match want {
                Want::Absent => old.is_some(),
                Want::Solid(entry) => old != Some(*entry),
                Want::Mixed(range) => {
                    !old.is_some_and(|entry| self.has_words(entry, &words[range.clone()]))
                }
            }
        });
        let result = self.write_region(level, key, found, old_entries.as_deref(), &wanted, &words);
        words.clear();
        self.rings.as_mut().expect("a scene of rings").scratch = words;
        result
    }

    /// Whether the brick with `brick_refs` entry `entry` is mixed, with these `words`.
    fn has_words(&self, entry: u32, words: &[u32]) -> bool {
        entry & SOLID_FLAG == 0
            && self.mixed_words(entry) == words.len()
            && self.bricks[entry as usize..][..words.len()] == *words
    }

    /// Writes the words of the mixed bricks of a region, those of `changed` that are to be (the
    /// ranges of `words`) and those that are there, which are copied, to one run of `bricks` in
    /// the order of the mask, and gives back all the words the bricks had and the region's list of
    /// entries. Puts the entry of each of them in `new`.
    fn write_run(
        &mut self,
        old: &[Option<u32>],
        new: &mut [Option<u32>],
        changed: &[(usize, Want)],
        words: &[u32],
        found: Option<usize>,
    ) -> Result<(), RingsFull> {
        let mut run: Vec<u32> = Vec::new();
        let mut starts: Vec<(usize, usize)> = Vec::new();
        let mut change_of = [usize::MAX; 512];
        for (i, (bit, _)) in changed.iter().enumerate() {
            change_of[*bit] = i;
        }
        for bit in 0..512 {
            let from = run.len();
            match changed.get(change_of[bit]) {
                Some((_, Want::Mixed(range))) => run.extend_from_slice(&words[range.clone()]),
                Some(_) => continue,
                None => match old[bit] {
                    Some(entry) if entry & SOLID_FLAG == 0 => {
                        let len = self.mixed_words(entry);
                        run.extend_from_slice(&self.bricks[entry as usize..][..len]);
                    }
                    _ => continue,
                },
            }
            starts.push((bit, from));
        }
        // What the region no longer refers to is free, and so is its list of entries. The bricks
        // that were mixed are all free: they are in the run.
        for entry in old.iter().flatten() {
            self.release_brick(*entry);
        }
        if let Some(index) = found {
            self.release_refs(index);
        }
        if run.is_empty() {
            return Ok(());
        }
        let at = self.take_words(Part::Bricks, run.len())?;
        self.patch().bricks.push(at..at + run.len());
        self.bricks[at..at + run.len()].copy_from_slice(&run);
        for (bit, from) in starts {
            new[bit] = Some((at + from) as u32);
        }
        Ok(())
    }

    /// Makes the region of `level` at `key`, which is at record `found` of `regions` if the scene
    /// lists it, with `old` as the `brick_refs` entry of each of its 512 bricks, have the bricks
    /// `changed` (by mask bit) as they say, whose words (for the mixed) are the ranges of `words`.
    /// See [`Scene::repack_region`].
    fn write_region(
        &mut self,
        level: usize,
        key: [i32; 3],
        found: Option<usize>,
        old: Option<&[Option<u32>]>,
        changed: &[(usize, Want)],
        words: &[u32],
    ) -> Result<(), RingsFull> {
        if changed.is_empty() {
            return Ok(());
        }
        // The usual case: the same bricks are in the region, and some of them read differently, with
        // the words they had or none. Then the list of entries stays where it is, and only those
        // words are written, where they were.
        let stays = |&(bit, ref want): &(usize, Want)| match (want, old.and_then(|e| e[bit])) {
            (Want::Absent, _) | (_, None) => false,
            (Want::Solid(_), Some(_)) => true,
            (Want::Mixed(range), Some(entry)) => {
                entry & SOLID_FLAG == 0 && self.mixed_words(entry) == range.len()
            }
        };
        if let (Some(index), Some(old)) = (found, old) {
            let uniform = self.regions[index * REGION_WORDS + REGION_FIRST_REF] & SOLID_FLAG != 0;
            if !uniform && changed.iter().all(stays) {
                let first_ref = self.regions[index * REGION_WORDS + REGION_FIRST_REF] as usize;
                let rank = |bit: usize| old[..bit].iter().filter(|e| e.is_some()).count();
                for (bit, want) in changed {
                    let before = old[*bit].expect("the brick is there");
                    match want {
                        Want::Solid(entry) => {
                            self.release_brick(before);
                            let at = first_ref + rank(*bit);
                            self.brick_refs[at] = *entry;
                            self.patch().brick_refs.push(at..at + 1);
                        }
                        Want::Mixed(range) => {
                            let at = before as usize;
                            self.bricks[at..at + range.len()]
                                .copy_from_slice(&words[range.clone()]);
                            self.patch().bricks.push(at..at + range.len());
                        }
                        Want::Absent => unreachable!("the brick stays"),
                    }
                }
                return Ok(());
            }
        }
        // Bricks that go, that are or turn solid, and bricks that read differently with the words
        // they had, need no new words: the rest of the region stays where it is, and only its
        // list of entries is written, which is enough for a region that loses bricks, as it does
        // where the bands take it over, or gains bricks that are one Material.
        let moves = changed
            .iter()
            .any(|change| matches!(change.1, Want::Mixed(_)) && !stays(change));
        let old_all: Vec<Option<u32>> = old.map_or_else(|| vec![None; 512], <[_]>::to_vec);
        let mut new = old_all.clone();
        for (bit, want) in changed {
            new[*bit] = match want {
                Want::Absent => None,
                Want::Solid(entry) => Some(*entry),
                // Where the words will be, in the run that is made below.
                Want::Mixed(_) => Some(0),
            };
        }
        if moves {
            self.write_run(old_all.as_slice(), &mut new, changed, words, found)?;
        } else {
            // The words that are given up are free; the others are where they were.
            for (bit, want) in changed {
                match (want, old_all[*bit]) {
                    (Want::Mixed(range), Some(entry)) => {
                        let at = entry as usize;
                        self.bricks[at..at + range.len()].copy_from_slice(&words[range.clone()]);
                        self.patch().bricks.push(at..at + range.len());
                        new[*bit] = Some(entry);
                    }
                    (_, Some(entry)) => self.release_brick(entry),
                    (_, None) => {}
                }
            }
            if let Some(index) = found {
                self.release_refs(index);
            }
        }
        let bricks: Vec<(usize, u32)> = (0..512).filter_map(|bit| Some((bit, new[bit]?))).collect();
        if bricks.is_empty() {
            if let Some(index) = found {
                self.unlist_region(level, key, index);
            }
            return Ok(());
        }
        let index = match found {
            Some(index) => index,
            None => {
                let rings = self.rings.as_ref().expect("a scene of rings");
                if rings.listed[level] >= rings.capacity[level] {
                    self.grow_level(level)?;
                }
                let rings = self.rings.as_mut().expect("a scene of rings");
                rings.listed[level] += 1;
                rings.free.pop().ok_or(RingsFull::Room)?
            }
        };
        let first = bricks[0].1;
        let at = index * REGION_WORDS;
        if bricks.len() == 512 && first & SOLID_FLAG != 0 && bricks.iter().all(|b| b.1 == first) {
            self.regions[at..at + REGION_FIRST_REF].fill(u32::MAX);
            self.regions[at + REGION_FIRST_REF] = first;
        } else {
            self.write_entries(index, &bricks)?;
        }
        let new_slot = found
            .is_none()
            .then(|| self.insert_region(level, key, index as u32));
        let patch = self.patch();
        patch.regions.insert(index);
        patch.table.extend(new_slot);
        Ok(())
    }

    /// The `brick_refs` entry of each of the 512 bricks of the region record at `index`, in one
    /// pass over its mask.
    fn region_entries(&self, index: usize) -> Vec<Option<u32>> {
        let record = &self.regions[index * REGION_WORDS..][..REGION_WORDS];
        let first_ref = record[REGION_FIRST_REF];
        if first_ref & SOLID_FLAG != 0 {
            return vec![Some(first_ref); 512];
        }
        let mut rank = 0;
        (0..512)
            .map(|bit| {
                (record[bit / 32] >> (bit % 32) & 1 != 0).then(|| {
                    rank += 1;
                    self.brick_refs[(first_ref + rank - 1) as usize]
                })
            })
            .collect()
    }

    /// Takes the region at `key` of mip `level` out of the table, and its record `index` back.
    fn unlist_region(&mut self, level: usize, key: [i32; 3], index: usize) {
        let mut slots = BTreeSet::new();
        self.remove_region(level, key, &mut slots);
        let rings = self.rings.as_mut().expect("a scene of rings");
        rings.listed[level] -= 1;
        rings.patch.table.extend(slots);
        rings.patch.unlisted.push((level, key));
        rings.patch.regions.remove(&index);
        rings.free.push(index);
    }

    /// Writes the mask and the list of entries of the region record at `index`, for `bricks`
    /// (mask bit and entry, by mask bit). The list takes words from `brick_refs`.
    fn write_entries(&mut self, index: usize, bricks: &[(usize, u32)]) -> Result<(), RingsFull> {
        let first_ref = self.take_words(Part::BrickRefs, bricks.len())?;
        self.patch()
            .brick_refs
            .push(first_ref..first_ref + bricks.len());
        let mut mask = [0u32; 16];
        for (i, &(bit, entry)) in bricks.iter().enumerate() {
            mask[bit / 32] |= 1 << (bit % 32);
            self.brick_refs[first_ref + i] = entry;
        }
        let at = index * REGION_WORDS;
        self.regions[at..at + REGION_FIRST_REF].copy_from_slice(&mask);
        self.regions[at + REGION_FIRST_REF] = first_ref as u32;
        Ok(())
    }

    /// The start of `len` words of `brick_refs` or of `bricks` (`part`) that are now taken. When
    /// there is no room for them in one piece, a scene that was sized by its bricks grows that
    /// buffer, and no other (see [`grow_by`]); one that was given its room is [`RingsFull`].
    fn take_words(&mut self, part: Part, len: usize) -> Result<usize, RingsFull> {
        let rings = self.rings.as_mut().expect("a scene of rings");
        let (pool, buffer) = match part {
            Part::Bricks => (&mut rings.data, &mut self.bricks),
            Part::BrickRefs => (&mut rings.refs, &mut self.brick_refs),
            Part::Table | Part::Regions => unreachable!("not handed out by a pool"),
        };
        let at = match pool.take(len) {
            Some(at) => at,
            None if matches!(rings.sizing, Sizing::Fitted(..)) => {
                let old = buffer.len();
                // The words free at the end of the buffer count towards the `len` in a row.
                let by = grow_by(old, len, rings.limit_words).ok_or(RingsFull::BufferLimit {
                    part,
                    needed: old + len,
                    limit: rings.limit_words,
                })?;
                buffer.resize(old + by, 0);
                // The words it adds join the free ones at its end, so that there are `len` in a row.
                pool.give(old, by);
                pool.take(len).expect("the words just added")
            }
            None => return Err(RingsFull::Room),
        };
        // A scene that is being packed from nothing takes words as if there were no end.
        if buffer.len() < at + len {
            buffer.resize(at + len, 0);
        }
        Ok(at)
    }

    /// Makes room for more regions in `level`, and for no other level: its part of `table` is
    /// made a bigger one if it is full (the regions are listed in it again, at the end of the
    /// buffer, and the part it had is left, unused), and `regions` has records for them. A scene
    /// that was given its room is [`RingsFull`].
    fn grow_level(&mut self, level: usize) -> Result<(), RingsFull> {
        let rings = self.rings.as_mut().expect("a scene of rings");
        if !matches!(rings.sizing, Sizing::Fitted(..)) {
            return Err(RingsFull::Room);
        }
        let capacity = rings.capacity[level];
        // At least one record more than it has, and room for one more region listed.
        let least = (rings.listed[level] + 1).max(capacity + 1);
        let (table_len, regions_len) = (self.table.len(), self.regions.len());
        let limit = rings.limit_words;
        // Room for the slack, and, if the limit does not leave that, for no more than is needed.
        let sized = |target: usize| {
            let slots = (target * 2).next_power_of_two();
            // The part is a power of two of slots, half full at most: all of that is room.
            let new_capacity = slots / 2;
            let moves = slots as u32 - 1 != self.levels[level].table_mask;
            let table = table_len + if moves { slots * 4 } else { 0 };
            let regions = regions_len + (new_capacity - capacity) * REGION_WORDS;
            (slots, new_capacity, table, regions)
        };
        let mut chosen = sized((capacity + capacity / 4 + 64).max(least));
        if chosen.2 > limit || chosen.3 > limit {
            chosen = sized(least);
        }
        let (slots, new_capacity, table, regions) = chosen;
        for (part, needed) in [(Part::Table, table), (Part::Regions, regions)] {
            if needed > limit {
                return Err(RingsFull::BufferLimit {
                    part,
                    needed,
                    limit,
                });
            }
        }
        let first_new = self.regions.len() / REGION_WORDS;
        let extra = new_capacity - capacity;
        self.regions
            .resize(self.regions.len() + extra * REGION_WORDS, 0);
        // The records that were free are used before these.
        rings
            .free
            .splice(0..0, (first_new..first_new + extra).rev());
        rings.capacity[level] = new_capacity;
        let was = self.levels[level];
        if slots as u32 - 1 != was.table_mask {
            let mut moved = Vec::with_capacity(rings.listed[level]);
            for slot in was.table_base..=was.table_base + was.table_mask {
                let at = slot as usize * 4;
                let entry = &self.table[at..at + 4];
                if entry[3] != 0 {
                    moved.push((
                        [entry[0], entry[1], entry[2]].map(|k| k as i32),
                        entry[3] - 1,
                    ));
                }
            }
            let base = self.table.len() / 4;
            self.table.resize(self.table.len() + slots * 4, 0);
            self.levels[level].table_base = base as u32;
            self.levels[level].table_mask = slots as u32 - 1;
            let written: Vec<usize> = moved
                .into_iter()
                .map(|(key, index)| self.insert_region(level, key, index))
                .collect();
            let patch = self.patch();
            patch.table.extend(written);
            patch.levels = true;
        }
        Ok(())
    }

    /// Makes room for more regions in `level`, as a level that lists more than its table has room
    /// for does, until the table has moved, and returns the update that says so. For a test of
    /// what the GPU does with a table that grew, which a scene seldom needs to.
    #[doc(hidden)]
    pub fn grow_table_of(&mut self, level: usize) -> Result<SceneUpdate, RingsFull> {
        let slots = self.levels[level].table_mask;
        while self.levels[level].table_mask == slots {
            self.grow_level(level)?;
        }
        Ok(SceneUpdate::Rings(self.take_patch()))
    }

    /// Gives back the words of `bricks` that the brick with `entry` has.
    fn release_brick(&mut self, entry: u32) {
        let words = self.mixed_words(entry);
        if words > 0 {
            let rings = self.rings.as_mut().expect("a scene of rings");
            rings.data.give(entry as usize, words);
        }
    }

    /// Gives back the words of `brick_refs` that the region record at `index` lists its bricks
    /// in.
    fn release_refs(&mut self, index: usize) {
        let refs = self.region_refs(index);
        if refs > 0 {
            let first_ref = self.regions[index * REGION_WORDS + REGION_FIRST_REF] as usize;
            let rings = self.rings.as_mut().expect("a scene of rings");
            rings.refs.give(first_ref, refs);
        }
    }

    /// The regions of the box of mip `level` that no brick of it reaches, as the first region
    /// and the one after the last, in regions of `level`: a cube about the camera that is inside
    /// the ball that no brick of the level is in. None for level 0, and for a scene that is not
    /// rings. The walk skips them.
    pub fn hole(&self, level: usize) -> ([i32; 3], [i32; 3]) {
        self.levels
            .get(level)
            .map_or(([0; 3], [0; 3]), |l| (l.hole_first, l.hole_end))
    }

    /// The level of the coarsest brick drawn, if the scene is rings with any.
    pub(super) fn coarsest_drawn(&self) -> Option<usize> {
        match &self.rings {
            Some(rings) => rings.selection.coarsest(),
            None => self.shown_coarsest,
        }
    }

    /// Brings the parts of the header that follow the camera and the bricks up to date: the
    /// holes, and the ball that rays are clipped to (which depends on the coarsest level with a
    /// brick). Marks the patch if either changed.
    fn sync_header(&mut self) {
        let rings = self.rings.as_mut().expect("a scene of rings");
        let camera = rings.selection.camera();
        let bands = rings.selection.bands();
        // The ball that no brick of a level is in: as far as its band says, or as far as the
        // nearest brick that is drawn whole though it is nearer than that.
        let stuck = rings.selection.nearest_stuck();
        let radii: Vec<f64> = (0..self.levels.len())
            .map(|level| stuck[level].unwrap_or(bands.clear_radius(level)))
            .collect();
        let mut changed = false;
        for (level, radius) in radii.into_iter().enumerate() {
            // The cube inside that ball, in whole regions of the level.
            let half = radius / 3f64.sqrt();
            let size = f64::from(REGION_VOXELS << level);
            let first: [i32; 3] =
                std::array::from_fn(|a| ((camera[a] - half) / size).ceil() as i32);
            let end: [i32; 3] = std::array::from_fn(|a| ((camera[a] + half) / size).floor() as i32);
            let (first, end) = if (0..3).all(|a| end[a] > first[a]) {
                (first, end)
            } else {
                ([0; 3], [0; 3])
            };
            let l = &mut self.levels[level];
            changed |= (l.hole_first, l.hole_end) != (first, end);
            (l.hole_first, l.hole_end) = (first, end);
        }
        let coarsest = self.coarsest_drawn();
        let rings = self.rings.as_mut().expect("a scene of rings");
        changed |= rings.coarsest != coarsest;
        rings.coarsest = coarsest;
        if changed {
            rings.patch.levels = true;
        }
    }

    /// Words of `brick_refs` and of `bricks` that a scene of rings has in use, if it is one.
    pub fn used_words(&self) -> Option<(usize, usize)> {
        let rings = self.rings.as_ref()?;
        let free = |pool: &Pool, len: usize| {
            len - pool
                .free()
                .map(|r| r.end.min(len).saturating_sub(r.start.min(len)))
                .sum::<usize>()
        };
        Some((
            free(&rings.refs, self.brick_refs.len()),
            free(&rings.data, self.bricks.len()),
        ))
    }

    /// For each level, the words of `brick_refs` and of `bricks` that the bricks drawn at it take
    /// (zero for a scene that is not rings): a brick that is air takes none, one in a region of a
    /// single Material takes none of either, and a brick of one Material takes a reference and no
    /// words. They add up to [`Scene::used_words`].
    pub fn words_by_level(&self) -> [(usize, usize); LEVELS] {
        let mut by_level = [(0, 0); LEVELS];
        let Some(rings) = &self.rings else {
            return by_level;
        };
        for (level, coord) in rings.selection.bricks() {
            let Some(index) = self.find_region(level, region_of(coord)) else {
                continue;
            };
            if self.regions[index * REGION_WORDS + 16] & SOLID_FLAG != 0 {
                continue;
            }
            if let Some(entry) = self.entry(index, bit_of(coord))
                && let Some(words) = by_level.get_mut(level)
            {
                words.0 += 1;
                words.1 += self.mixed_words(entry);
            }
        }
        by_level
    }

    /// Bytes the scene's buffers take, used or not: what the GPU copy of a scene of rings
    /// reserves, and the host holds as well.
    pub fn reserved_bytes(&self) -> u64 {
        (self.table.len() + self.regions.len() + self.brick_refs.len() + self.bricks.len()) as u64
            * 4
    }

    /// Bytes of the scene's buffers that hold something. The buffers of rings are allocated
    /// whole, so for them this is how full they are, and the size of the GPU copy is that of the
    /// buffers.
    pub fn used_bytes(&self) -> u64 {
        let words = match &self.rings {
            Some(rings) => {
                let free = |pool: &Pool| {
                    pool.free()
                        .map(|r| r.end.min(usize::MAX / 8).saturating_sub(r.start))
                        .sum::<usize>()
                };
                let (refs, data) = (
                    free(&rings.refs).min(self.brick_refs.len()),
                    free(&rings.data).min(self.bricks.len()),
                );
                self.table.len() + self.regions.len() + self.brick_refs.len() + self.bricks.len()
                    - rings.free.len() * REGION_WORDS
                    - refs
                    - data
            }
            None => {
                self.table.len() + self.regions.len() + self.brick_refs.len() + self.bricks.len()
                    - self.garbage
            }
        };
        words as u64 * 4
    }

    /// What has been written since the last patch.
    fn patch(&mut self) -> &mut Patch {
        &mut self.rings.as_mut().expect("a scene of rings").patch
    }

    /// The level whose part of `table` has `slot`, if one does: the slots of a part that a level
    /// has left for a bigger one (see `grow_level`) are no level's.
    fn level_of_slot(&self, slot: usize) -> Option<usize> {
        self.levels.iter().position(|l| {
            (l.table_base as usize..=(l.table_base + l.table_mask) as usize).contains(&slot)
        })
    }

    /// Grows the occupied box of `level` to hold the regions in the table `slots`, which are
    /// listed, and says whether it changed. The box is what [`Scene::occupy`] would find if no
    /// region was unlisted since it last did.
    fn occupy_more(&mut self, level: usize, slots: &[usize]) -> bool {
        let l = &self.levels[level];
        let empty = l.occupied_first == l.occupied_end;
        let (mut first, mut end) = if empty {
            ([i32::MAX; 3], [i32::MIN; 3])
        } else {
            (l.occupied_first, l.occupied_end)
        };
        for &slot in slots {
            let entry = &self.table[slot * 4..][..4];
            for a in 0..3 {
                first[a] = first[a].min(entry[a] as i32);
                end[a] = end[a].max(entry[a] as i32 + 1);
            }
        }
        let l = &mut self.levels[level];
        let changed = (l.occupied_first, l.occupied_end) != (first, end);
        (l.occupied_first, l.occupied_end) = (first, end);
        l.origin = first.map(|f| f << (6 + level));
        l.dims = std::array::from_fn(|a| (end[a] - first[a]) as u32);
        changed
    }

    /// Fits the occupied box of each of `levels` to the regions it lists now, and says whether
    /// any changed.
    fn occupy(&mut self, levels: &BTreeSet<usize>) -> bool {
        let mut changed = false;
        for &level in levels {
            let Level {
                table_base,
                table_mask,
                ..
            } = self.levels[level];
            let (mut first, mut end) = ([i32::MAX; 3], [i32::MIN; 3]);
            for slot in table_base..=table_base + table_mask {
                let entry = &self.table[slot as usize * 4..][..4];
                if entry[3] != 0 {
                    for a in 0..3 {
                        first[a] = first[a].min(entry[a] as i32);
                        end[a] = end[a].max(entry[a] as i32 + 1);
                    }
                }
            }
            let (first, end) = if first[0] == i32::MAX {
                ([0; 3], [0; 3])
            } else {
                (first, end)
            };
            let l = &mut self.levels[level];
            changed |= (l.occupied_first, l.occupied_end) != (first, end);
            (l.occupied_first, l.occupied_end) = (first, end);
            l.origin = first.map(|f| f << (6 + level));
            l.dims = std::array::from_fn(|a| (end[a] - first[a]) as u32);
        }
        changed
    }

    /// Hands out what has been written since the last call.
    fn take_patch(&mut self) -> RingPatch {
        let mut patch = std::mem::take(self.patch());
        if !patch.table.is_empty() {
            // A level whose slots written are all regions listed, none unlisted, has only gained:
            // its box grows to hold them, and the table need not be searched for the new one.
            let mut grown: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
            // A region taken out only changes the box if it was on its edge.
            let mut rescan = BTreeSet::new();
            for &(level, key) in &patch.unlisted {
                let l = &self.levels[level];
                if (0..3).any(|a| key[a] <= l.occupied_first[a] || key[a] >= l.occupied_end[a] - 1)
                {
                    rescan.insert(level);
                }
            }
            for &slot in &patch.table {
                if self.table[slot * 4 + 3] != 0
                    && let Some(level) = self.level_of_slot(slot)
                {
                    grown.entry(level).or_default().push(slot);
                }
            }
            for (level, slots) in grown {
                if !rescan.contains(&level) {
                    patch.levels |= self.occupy_more(level, &slots);
                }
            }
            patch.levels |= self.occupy(&rescan);
        }
        let now = Lengths::of(self);
        let rings = self.rings.as_mut().expect("a scene of rings");
        let sizes = (now != rings.reported).then_some(now);
        rings.reported = now;
        RingPatch {
            regions: patch.regions.into_iter().collect(),
            table: patch.table.into_iter().collect(),
            brick_refs: merged(patch.brick_refs),
            bricks: merged(patch.bricks),
            levels: patch.levels,
            sizes,
        }
    }
}

const _: () = assert!(MAX_LEVELS >= crate::lod::LEVELS);
const _: () = assert!(REGION_BRICKS == 8);

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use brick_store::{Material, VoxelCoord};
    use planet_gen::generate_region;

    use super::*;

    const LEVELS: usize = 5;
    /// Above the test Planet's +y pole.
    const ABOVE: [f64; 3] = [0.0, 1560.0, 0.0];

    /// Bands on a scale that the ground patch has several levels in: level 1 from 128 Voxels.
    fn bands() -> Bands {
        Bands::with_d0(64.0)
    }

    fn bound() -> Bound {
        Bound {
            centre: [0; 3],
            radius: planet_gen::MAX_RADIUS,
        }
    }

    /// The ground near the +y pole, Voxels x -440..=447, y 1360..=1575, z -360..=263, with its
    /// pyramid.
    fn ground() -> &'static BrickStore {
        static GROUND: OnceLock<BrickStore> = OnceLock::new();
        GROUND.get_or_init(|| {
            let mut store = generate_region(
                0xB4A7_A9AC,
                BrickCoord([-55, 170, -45]),
                BrickCoord([55, 196, 32]),
            );
            store.build_lod(LEVELS - 1);
            store
        })
    }

    fn rings_at(camera: [f64; 3]) -> Scene {
        Scene::rings(ground(), camera, bands(), bound()).unwrap()
    }

    #[test]
    fn the_words_of_the_levels_add_up_to_the_words_in_use() {
        let mut scene = rings_at(ABOVE);
        // After the camera has moved too, so that words were freed and taken again.
        let far = [ABOVE[0] + 300.0, ABOVE[1], ABOVE[2] - 200.0];
        scene.recentre(ground(), far).unwrap();
        let by_level = scene.words_by_level();
        let (refs, words) = by_level
            .iter()
            .fold((0, 0), |(r, w), &(lr, lw)| (r + lr, w + lw));
        assert_eq!(Some((refs, words)), scene.used_words());
        assert!(words > 0 && by_level.iter().filter(|&&(r, _)| r > 0).count() > 2);
    }

    /// What a scene shows of every region it lists, by level and region: its Material if it is
    /// all one, else the words of each brick by mask bit. It does not depend on where in the
    /// buffers anything is.
    #[allow(clippy::type_complexity)]
    fn shown(scene: &Scene) -> BTreeMap<(usize, [i32; 3]), (Option<u32>, Vec<(usize, Vec<u32>)>)> {
        let mut all = BTreeMap::new();
        for (level, l) in scene.levels.iter().enumerate() {
            for slot in l.table_base..=l.table_base + l.table_mask {
                let entry = &scene.table[slot as usize * 4..][..4];
                if entry[3] == 0 {
                    continue;
                }
                let key = [entry[0], entry[1], entry[2]].map(|k| k as i32);
                let index = entry[3] as usize - 1;
                let first_ref = scene.regions[index * REGION_WORDS + REGION_FIRST_REF];
                let region = if first_ref & SOLID_FLAG != 0 {
                    (Some(first_ref & 0xffff), Vec::new())
                } else {
                    let bricks = (0..512)
                        .filter_map(|bit| {
                            let entry = scene.entry(index, bit)?;
                            let words = match scene.mixed_words(entry) {
                                0 => vec![entry],
                                n => scene.bricks[entry as usize..][..n].to_vec(),
                            };
                            Some((bit, words))
                        })
                        .collect();
                    (None, bricks)
                };
                all.insert((level, key), region);
            }
        }
        all
    }

    /// The buffers of a scene, as a GPU copy of it holds them.
    #[derive(Clone, PartialEq, Debug)]
    struct Copy {
        table: Vec<u32>,
        regions: Vec<u32>,
        brick_refs: Vec<u32>,
        bricks: Vec<u32>,
    }

    impl Copy {
        fn of(scene: &Scene) -> Self {
            Self {
                table: scene.table.clone(),
                regions: scene.regions.clone(),
                brick_refs: scene.brick_refs.clone(),
                bricks: scene.bricks.clone(),
            }
        }

        /// What a GPU copy holds after `patch` is written to it from `scene`, as the tracer does.
        fn patched(mut self, patch: &RingPatch, scene: &Scene) -> Self {
            // A buffer that grew is made longer first, keeping what it had.
            if let Some(sizes) = patch.sizes {
                self.table.resize(sizes.table, 0);
                self.regions.resize(sizes.regions, 0);
                self.brick_refs.resize(sizes.brick_refs, 0);
                self.bricks.resize(sizes.bricks, 0);
            }
            for &slot in &patch.table {
                self.table[slot * 4..][..4].copy_from_slice(&scene.table[slot * 4..][..4]);
            }
            for &index in &patch.regions {
                let at = index * REGION_WORDS;
                self.regions[at..at + REGION_WORDS]
                    .copy_from_slice(&scene.regions[at..][..REGION_WORDS]);
            }
            for range in &patch.brick_refs {
                self.brick_refs[range.clone()].copy_from_slice(&scene.brick_refs[range.clone()]);
            }
            for range in &patch.bricks {
                self.bricks[range.clone()].copy_from_slice(&scene.bricks[range.clone()]);
            }
            self
        }
    }

    fn patch_of(update: Option<SceneUpdate>) -> RingPatch {
        match update {
            Some(SceneUpdate::Rings(patch)) => patch,
            other => panic!("the rings were patched: {other:?}"),
        }
    }

    #[test]
    fn each_level_lists_the_bricks_the_bands_select_for_it_and_no_other() {
        let scene = rings_at(ABOVE);
        let selection = &scene.rings.as_ref().unwrap().selection;
        let mut listed = 0;
        for ((level, key), (uniform, bricks)) in shown(&scene) {
            assert!(level < scene.levels.len());
            // A uniform region is 512 bricks, and every one of them is drawn.
            let n = if uniform.is_some() { 512 } else { bricks.len() };
            listed += n;
            let first = key.map(|k| k * REGION_BRICKS);
            let drawn = (0..512)
                .filter(|&bit| {
                    let coord =
                        BrickCoord(std::array::from_fn(|a| first[a] + (bit >> (3 * a) & 7)));
                    selection.draws(level, coord)
                })
                .count();
            assert_eq!(drawn, n, "level {level} region {key:?}");
        }
        let drawn: usize = (0..LEVELS).map(|l| scene.level_bricks(l)).sum();
        assert_eq!(listed, drawn);
        assert!(scene.level_bricks(0) > 100 && scene.level_bricks(1) > 100);
        assert!(scene.levels[0].occupied_end[1] > scene.levels[0].occupied_first[1]);
    }

    #[test]
    fn the_hole_of_a_level_lists_no_region() {
        for camera in [ABOVE, [200.0, 1700.0, -100.0], [0.0, 3000.0, 0.0]] {
            let scene = rings_at(camera);
            assert_eq!(scene.hole(0), ([0; 3], [0; 3]));
            for level in 0..scene.levels.len() {
                let (first, end) = scene.hole(level);
                let size = f64::from(REGION_VOXELS << level);
                let radius = bands().clear_radius(level);
                for a in 0..3 {
                    // The hole is within the ball about the camera that no brick is in.
                    if end[a] > first[a] {
                        let half = f64::from(end[a] - first[a]) * size / 2.0;
                        assert!(half * 3f64.sqrt() <= radius + 2.0 * size, "{level}");
                    }
                }
                for ((lvl, key), _) in shown(&scene) {
                    let inside = (0..3).all(|a| key[a] >= first[a] && key[a] < end[a]);
                    assert!(!(lvl == level && inside), "level {level} lists {key:?}");
                }
            }
        }
    }

    #[test]
    fn rings_that_follow_the_camera_show_what_rings_built_where_it_ends_do() {
        let mut scene = rings_at([-420.0, 1560.0, -300.0]);
        let mut copy = Copy::of(&scene);
        let mut moves = 0;
        let mut at = [-420.0, 1560.0, -300.0];
        for step in 0..120 {
            at = [
                at[0] + 7.5 + f64::from(step % 5),
                1560.0 + 300.0 * (f64::from(step) / 15.0).sin(),
                at[2] + 4.0,
            ];
            match scene.recentre(ground(), at).unwrap() {
                None => {}
                Some(SceneUpdate::Rings(patch)) => {
                    moves += 1;
                    copy = copy.patched(&patch, &scene);
                }
                Some(SceneUpdate::Rebuilt) => copy = Copy::of(&scene),
                Some(other) => panic!("step {step}: {other:?}"),
            }
            assert_eq!(copy, Copy::of(&scene), "step {step}");
        }
        assert!(moves > 20, "{moves}");
        // What it shows is a selection that has followed the camera, which a new one differs from
        // only within the hysteresis: so compare with the same selection packed from scratch.
        assert_eq!(
            shown(&scene),
            shown(&scene.rebuilt_rings(ground()).unwrap())
        );
    }

    #[test]
    fn a_camera_that_does_not_move_changes_nothing_and_a_small_move_writes_little() {
        let mut scene = rings_at(ABOVE);
        assert_eq!(scene.recentre(ground(), ABOVE), Ok(None));
        let total = scene.used_bytes() as usize / 4;
        let mut written = 0;
        for i in 1..=5 {
            let at = [f64::from(i) * 2.0, 1560.0, 0.0];
            if let Some(update) = scene.recentre(ground(), at).unwrap() {
                written += patch_of(Some(update)).words();
            }
        }
        // 10 Voxels of walking: a small part of the scene is written.
        assert!(written * 4 < total, "{written} words of {total}");
    }

    /// Digs a ball of Voxels out of `store` and returns the bricks of level 0 it changed.
    fn dig(store: &mut BrickStore, centre: [i32; 3], radius: i32) -> Vec<BrickCoord> {
        let mut changed = BTreeSet::new();
        for x in -radius..=radius {
            for y in -radius..=radius {
                for z in -radius..=radius {
                    let voxel = VoxelCoord([centre[0] + x, centre[1] + y, centre[2] + z]);
                    if x * x + y * y + z * z <= radius * radius && store.get(voxel).is_some() {
                        changed.insert(voxel.brick());
                        store.set(voxel, None);
                    }
                }
            }
        }
        changed.into_iter().collect()
    }

    #[test]
    fn an_edit_shows_in_every_level_as_a_new_scene_would_and_writes_only_what_changed() {
        let mut store = ground().clone();
        let mut scene = Scene::rings(&store, ABOVE, bands(), bound()).unwrap();
        let mut copy = Copy::of(&scene);
        let changed = dig(&mut store, [20, 1510, -30], 14);
        assert!(!changed.is_empty());
        let refresh = store.refresh_lod(changed.iter().copied());
        assert!(refresh.changed.iter().any(|&(level, _)| level >= 2));

        let SceneUpdate::Rings(patch) = scene.update(&store, &changed).unwrap() else {
            panic!("an edit patches the rings");
        };
        assert_eq!(
            shown(&scene),
            shown(&Scene::rings(&store, ABOVE, bands(), bound()).unwrap())
        );
        copy = copy.patched(&patch, &scene);
        assert_eq!(copy, Copy::of(&scene));
        // A crater 28 Voxels across touches a handful of bricks in each level.
        assert!(patch.words() < 30_000, "{} words", patch.words());

        // The same Edit again finds every brick as it is, so writes none of them.
        let SceneUpdate::Rings(again) = scene.update(&store, &changed).unwrap() else {
            panic!("an edit patches the rings");
        };
        assert!(again.bricks.is_empty(), "{again:?}");
        assert_eq!(copy, Copy::of(&scene));
    }

    /// Words of `bricks` that the scene's bricks take.
    fn words_taken(scene: &Scene) -> usize {
        let free: usize = scene
            .rings
            .as_ref()
            .unwrap()
            .data
            .free()
            .map(|r| r.len())
            .sum();
        scene.bricks.len() - free
    }

    #[test]
    fn a_scene_that_was_given_too_little_room_is_an_error_and_one_that_was_not_grows_the_buffer_that_filled()
     {
        let scene = rings_at(ABOVE);
        let tiny = Room {
            words_per_ring: words_taken(&scene) / LEVELS / 2,
            ..Room::default()
        };
        assert!(Scene::rings_in(ground(), ABOVE, bands(), bound(), tiny).is_err());
        // A fitted scene that has to hold more than it was sized for grows the buffer that is full.
        let mut store = ground().clone();
        let mut small = Scene::rings(&store, ABOVE, bands(), bound()).unwrap();
        let before = Lengths::of(&small);
        let copy = Copy::of(&small);
        let spare: usize = small
            .rings
            .as_ref()
            .unwrap()
            .data
            .free()
            .map(|r| r.len())
            .sum();
        // Take the spare words, so that the next brick that needs some does not fit.
        assert!(small.rings.as_mut().unwrap().data.take(spare).is_some());
        let changed = dig(&mut store, [20, 1510, -30], 14);
        store.refresh_lod(changed.iter().copied());
        let patch = patch_of(Some(small.update(&store, &changed).unwrap()));
        let after = patch.sizes.expect("a buffer grew");
        // Only `bricks` was full, and it grew by what growing says and not by a repack of all.
        assert_eq!(
            (after.table, after.regions, after.brick_refs),
            (before.table, before.regions, before.brick_refs)
        );
        assert!(after.bricks >= before.bricks + GROW_LEAST, "{after:?}");
        assert_eq!(after, Lengths::of(&small));
        assert_eq!(copy.patched(&patch, &small), Copy::of(&small));
        assert_eq!(
            shown(&small),
            shown(&Scene::rings(&store, ABOVE, bands(), bound()).unwrap())
        );
        // The next patch does not say it again.
        let later = dig(&mut store, [20, 1510, -30], 3);
        store.refresh_lod(later.iter().copied());
        assert_eq!(
            patch_of(Some(small.update(&store, &later).unwrap())).sizes,
            None
        );
    }

    #[test]
    fn a_level_that_lists_more_regions_than_its_table_has_room_for_grows_its_table_alone() {
        let mut scene = rings_at(ABOVE);
        let level = 2;
        let others = |scene: &Scene| -> Vec<(u32, u32)> {
            scene
                .levels
                .iter()
                .enumerate()
                .filter(|&(l, _)| l != level)
                .map(|(_, l)| (l.table_base, l.table_mask))
                .collect()
        };
        let (shown_before, others_before) = (shown(&scene), others(&scene));
        let before = Lengths::of(&scene);
        let copy = Copy::of(&scene);
        let capacity = scene.rings.as_ref().unwrap().capacity[level];
        let slots = scene.levels[level].table_mask + 1;
        // Grow until the table has to move.
        while scene.levels[level].table_mask + 1 == slots {
            scene.grow_level(level).unwrap();
        }
        assert!(scene.rings.as_ref().unwrap().capacity[level] > capacity);
        let patch = scene.take_patch();
        let after = patch.sizes.expect("buffers grew");
        // The brick buffers are as they were, and every other level's table is where it was.
        assert_eq!(
            (after.brick_refs, after.bricks),
            (before.brick_refs, before.bricks)
        );
        assert!(after.table > before.table && after.regions > before.regions);
        assert_eq!(others(&scene), others_before);
        assert!(patch.levels);
        // The regions are found where they were, and the GPU copy has them in the new table.
        assert_eq!(shown(&scene), shown_before);
        assert_eq!(copy.patched(&patch, &scene), Copy::of(&scene));
        // Every record is free or listed: the new ones are there for the level to use.
        let rings = scene.rings.as_ref().unwrap();
        assert_eq!(
            rings.free.len() + rings.listed.iter().sum::<usize>(),
            scene.regions.len() / REGION_WORDS
        );
    }

    #[test]
    fn a_scene_that_was_given_its_room_does_not_grow_a_table() {
        let mut scene =
            Scene::rings_in(ground(), ABOVE, bands(), bound(), Room::default()).unwrap();
        assert_eq!(scene.grow_level(1), Err(RingsFull::Room));
    }

    #[test]
    fn a_growth_is_cut_to_the_devices_limit_when_what_is_needed_fits_and_is_a_named_error_when_not()
    {
        let mut scene = rings_at(ABOVE);
        let spare: usize = scene
            .rings
            .as_ref()
            .unwrap()
            .data
            .free()
            .map(|r| r.len())
            .sum();
        // Take the spare words, so that the next that are asked for make the buffer grow.
        assert!(scene.rings.as_mut().unwrap().data.take(spare).is_some());
        let len = scene.bricks.len();
        // A limit that leaves less than the least a buffer grows by (and than a quarter of it).
        let limit = len + 1000;
        const { assert!(1000 < GROW_LEAST) };
        scene.set_buffer_limit(limit);
        // What is needed fits, so the buffer grows, by what the limit leaves and no more.
        scene.take_words(Part::Bricks, 500).unwrap();
        assert_eq!(scene.bricks.len(), limit);
        assert_eq!(patch_sizes_of(&mut scene).bricks, limit);
        // What is needed does not: a named error, and the buffer is as it was.
        assert_eq!(
            scene.take_words(Part::Bricks, 2000),
            Err(RingsFull::BufferLimit {
                part: Part::Bricks,
                needed: limit + 2000,
                limit
            })
        );
        assert_eq!(scene.bricks.len(), limit);
        // A table that would have to be longer than the limit is the same error.
        scene.set_buffer_limit(scene.table.len());
        assert!(matches!(
            scene.grow_level(2),
            Err(RingsFull::BufferLimit { .. })
        ));
        assert!(
            RingsFull::BufferLimit {
                part: Part::Table,
                needed: 2,
                limit: 1
            }
            .to_string()
            .contains("limit")
        );
    }

    /// The lengths that the scene's next patch says, if it says any.
    fn patch_sizes_of(scene: &mut Scene) -> Lengths {
        scene.take_patch().sizes.expect("a buffer grew")
    }

    #[test]
    fn another_bias_selects_other_bricks() {
        let mut scene = rings_at(ABOVE);
        assert_eq!(scene.set_bands(ground(), bands()), Ok(None));
        let before: usize = (0..LEVELS).map(|l| scene.level_bricks(l)).sum();
        let loose = Bands::with_d0(32.0);
        assert_eq!(
            scene.set_bands(ground(), loose),
            Ok(Some(SceneUpdate::Rebuilt))
        );
        assert_eq!(scene.bands(), Some(loose));
        let after: usize = (0..LEVELS).map(|l| scene.level_bricks(l)).sum();
        // Half the distances for every level: fewer, coarser bricks.
        assert!(after < before, "{after} against {before}");
    }

    #[test]
    fn a_scene_made_again_has_its_selection_made_roomy_again_and_says_why() {
        let room = |scene: &Scene, name: &str| {
            scene
                .table_sizes()
                .into_iter()
                .find(|t| t.name == name)
                .unwrap()
                .capacity
        };
        let mut scene = rings_at(ABOVE);
        assert_eq!(scene.rebuild_reason(), None);
        scene.reserve(200_000, 100_000, 50_000);
        let (nodes, waiting, stuck) = (
            room(&scene, "selection nodes"),
            room(&scene, "selection waiting"),
            room(&scene, "selection stuck"),
        );
        assert!(nodes >= 200_000 && waiting >= 100_000 && stuck >= 50_000);
        // Another bias: made again from nothing, and roomy again, in proportion to d0 squared
        // (d0 here is a half, so the room is a quarter).
        let loose = Bands::with_d0(bands().d0() / 2.0);
        assert_eq!(
            scene.set_bands(ground(), loose),
            Ok(Some(SceneUpdate::Rebuilt))
        );
        assert_eq!(scene.rebuild_reason(), Some(Rebuild::Bands));
        assert!(room(&scene, "selection nodes") >= 50_000);
        assert!(room(&scene, "selection waiting") >= 25_000);
        assert!(room(&scene, "selection stuck") >= 12_500);
        // A jump keeps what it had.
        let far = scene
            .rings
            .as_ref()
            .unwrap()
            .selection
            .camera()
            .map(|c| c + 900.0);
        let before = room(&scene, "selection nodes");
        assert_eq!(
            scene.recentre(ground(), far),
            Ok(Some(SceneUpdate::Rebuilt))
        );
        assert_eq!(scene.rebuild_reason(), Some(Rebuild::Jump));
        assert!(room(&scene, "selection nodes") >= before);
    }

    #[test]
    fn the_clip_radius_allows_for_the_coarsest_voxel_drawn() {
        let scene = rings_at(ABOVE);
        let coarsest = scene.coarsest_drawn().unwrap();
        let radius = scene.clip_radius().unwrap();
        let want = planet_gen::MAX_RADIUS as f32
            + (1u32 << coarsest) as f32 * 3f32.sqrt()
            + crate::scene::BOUND_SLACK;
        assert_eq!(radius, want);
    }

    /// The first solid Voxel straight down from above `voxel`, if rays reach it.
    fn seen_from_above(scene: &Scene, voxel: VoxelCoord) -> Option<[i32; 3]> {
        let (cell, origin) = crate::camera::split([
            f64::from(voxel.0[0]) + 0.5,
            f64::from(voxel.0[1]) + 60.5,
            f64::from(voxel.0[2]) + 0.5,
        ]);
        let ray = crate::reference::Ray {
            cell,
            origin,
            dir: [0.0, -1.0, 0.0],
        };
        crate::reference::trace(scene, &ray).map(|hit| hit.voxel)
    }

    #[test]
    fn matter_placed_above_the_ball_grows_it_so_that_rays_are_not_clipped_short_of_it() {
        let mut store = ground().clone();
        let radius = bound().radius;
        // A cube of 32 Voxels (4 bricks) a side, aligned to the coarse levels so that it is
        // solid at each of them, over the ball, with nothing round it.
        let low = (radius + 24 + 31) / 32 * 32;
        let camera = [16.0, f64::from(low) + 80.0, 16.0];
        let mut scene = Scene::rings(&store, camera, bands(), bound()).unwrap();
        let top = VoxelCoord([16, low + 31, 16]);
        assert_ne!(seen_from_above(&scene, top), Some(top.0));
        let mut changed = BTreeSet::new();
        for x in 0..32 {
            for y in low..low + 32 {
                for z in 0..32 {
                    let voxel = VoxelCoord([x, y, z]);
                    store.set(voxel, Some(Material(7)));
                    changed.insert(voxel.brick());
                }
            }
        }
        let changed: Vec<_> = changed.into_iter().collect();
        store.refresh_lod(changed.iter().copied());
        scene.update(&store, &changed).unwrap();
        // A clip at the ball cuts the cube off; the grown ball does not.
        assert_eq!(seen_from_above(&scene, top), Some(top.0));
        let grown = scene.bound.unwrap();
        assert!(grown.radius >= low + 32, "{grown:?}");
        assert_eq!(grown.centre, bound().centre);
        assert!(scene.clip_radius().unwrap() > grown.radius as f32);

        // A scene made again (the camera jumped, or the bias changed) keeps the grown ball.
        let far = [camera[0] + 900.0, camera[1], camera[2]];
        assert_eq!(scene.recentre(&store, far), Ok(Some(SceneUpdate::Rebuilt)));
        assert_eq!(scene.bound, Some(grown));
        let near = [camera[0], camera[1], camera[2]];
        assert_eq!(scene.recentre(&store, near), Ok(Some(SceneUpdate::Rebuilt)));
        assert_eq!(seen_from_above(&scene, top), Some(top.0));
    }

    #[test]
    fn digging_and_placing_inside_the_ball_leave_it_as_it_was() {
        let mut store = ground().clone();
        let mut scene = rings_at(ABOVE);
        let top = VoxelCoord([0, bound().radius - 10, 0]);
        let changed = [top.brick()];
        store.set(top, Some(Material(7)));
        store.refresh_lod(changed);
        scene.update(&store, &changed).unwrap();
        store.set(top, None);
        store.refresh_lod(changed);
        scene.update(&store, &changed).unwrap();
        assert_eq!(scene.bound, Some(bound()));
    }

    #[test]
    fn removing_regions_leaves_the_others_where_a_search_finds_them() {
        // Enough regions in one table that many of them collided.
        let mut store = BrickStore::new();
        let keys: Vec<[i32; 3]> = (0..1500).map(|i| [i % 37, i / 37 % 5, i / 185]).collect();
        for key in &keys {
            store.insert(
                BrickCoord(key.map(|k| k * 8)),
                brick_store::Brick::solid(Material(1)),
            );
        }
        let mut scene = Scene::new(&store, None);
        let mut touched = BTreeSet::new();
        for key in keys.iter().step_by(3) {
            assert!(scene.remove_region(0, *key, &mut touched).is_some());
        }
        assert!(!touched.is_empty());
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(scene.find_region(0, *key).is_some(), i % 3 != 0, "{key:?}");
        }
        assert_eq!(scene.remove_region(0, keys[0], &mut touched), None);
    }

    #[test]
    fn a_pool_hands_out_words_in_a_row_and_joins_what_comes_back() {
        let mut pool = Pool::new(100);
        let (a, b, c) = (pool.take(30), pool.take(30), pool.take(30));
        assert_eq!((a, b, c), (Some(0), Some(30), Some(60)));
        assert_eq!(pool.take(20), None, "only ten words are left");
        pool.give(30, 30);
        assert_eq!(pool.take(31), None, "the hole is thirty long");
        pool.give(0, 30);
        pool.give(60, 30);
        assert_eq!(pool.free().next(), Some(0..100));
        assert_eq!(pool.free().count(), 1);
        assert_eq!(pool.take(100), Some(0));
    }

    /// The pool as it was before it was in blocks: a list of ranges in address order, searched
    /// from the front, which is what places bricks in the buffer as the GPU timings were made with.
    struct Linear(Vec<Range<usize>>);

    impl Linear {
        fn take(&mut self, len: usize) -> Option<usize> {
            let at = self.0.iter().position(|r| r.len() >= len)?;
            let start = self.0[at].start;
            if self.0[at].len() == len {
                self.0.remove(at);
            } else {
                self.0[at].start += len;
            }
            Some(start)
        }

        fn give(&mut self, start: usize, len: usize) {
            let at = self.0.partition_point(|r| r.start < start);
            let after = self.0.get(at).is_some_and(|r| r.start == start + len);
            let before = at > 0 && self.0[at - 1].end == start;
            match (before, after) {
                (true, true) => {
                    self.0[at - 1].end = self.0[at].end;
                    self.0.remove(at);
                }
                (true, false) => self.0[at - 1].end = start + len,
                (false, true) => self.0[at].start = start,
                (false, false) => self.0.insert(at, start..start + len),
            }
        }
    }

    #[test]
    fn a_pool_places_every_request_where_a_linear_first_fit_does() {
        // Long enough to have many more ranges than a block holds, to split and empty blocks, and
        // to join ranges across them.
        let mut rng = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move |n: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 16) as usize % n
        };
        let capacity = 400_000;
        let (mut pool, mut linear) = (
            Pool::new(capacity),
            Linear(std::iter::once(0..capacity).collect()),
        );
        let mut held: Vec<(usize, usize)> = Vec::new();
        let mut most_ranges = 0;
        for step in 0..60_000 {
            // Fill up for a while, then free more than is taken, then hold steady.
            let free_chance = match step {
                0..20_000 => 35,
                20_000..40_000 => 60,
                _ => 50,
            };
            if !held.is_empty() && next(100) < free_chance {
                let (start, len) = held.swap_remove(next(held.len()));
                pool.give(start, len);
                linear.give(start, len);
            } else {
                let len = match next(10) {
                    0 => 1 + next(2000),
                    _ => 1 + next(40),
                };
                let (a, b) = (pool.take(len), linear.take(len));
                assert_eq!(a, b, "step {step}, a request of {len}");
                held.extend(a.map(|start| (start, len)));
            }
            if step % 997 == 0 {
                assert_eq!(pool.free().collect::<Vec<_>>(), linear.0, "step {step}");
                most_ranges = most_ranges.max(linear.0.len());
            }
        }
        assert_eq!(pool.free().collect::<Vec<_>>(), linear.0);
        assert!(most_ranges > 2 * BLOCK, "{most_ranges} ranges at the most");
        assert!(pool.blocks.iter().all(|b| !b.ranges.is_empty()));
    }

    #[test]
    fn ranges_that_touch_or_overlap_are_written_as_one() {
        assert_eq!(
            merged(vec![10..20, 0..5, 5..8, 18..25, 30..30]),
            [0..8, 10..25]
        );
    }

    /// A brick that is whole because there is nothing under it to split it into is found by a ray
    /// even when it is nearer than its level is used from, where its level's hole would be.
    #[test]
    fn a_brick_that_cannot_be_split_is_not_in_the_hole_of_its_level() {
        #[derive(Debug)]
        struct Odd;
        impl brick_store::BrickSource for Odd {
            fn brick(&self, level: usize, coord: BrickCoord) -> brick_store::Brick {
                if level == 1 && coord == BrickCoord([0, 0, 0]) {
                    brick_store::Brick::solid(Material(1))
                } else {
                    brick_store::Brick::AIR
                }
            }
        }
        let mut store = BrickStore::new();
        store.set_source(Odd);
        store.build_lod(1);
        let bound = Bound {
            centre: [0; 3],
            radius: 100,
        };
        // 40 Voxels above the brick (which is 16 across), and level 1 is used from 2000 away.
        let camera = [8.5, 40.5, 8.5];
        let scene = Scene::rings(&store, camera, Bands::with_d0(1000.0), bound).unwrap();
        assert_eq!(scene.level_bricks(1), 1);
        let (first, end) = scene.hole(1);
        assert!(
            (0..3).any(|a| end[a] <= first[a]) || first[1] > 0 || end[1] <= 0,
            "{first:?} {end:?}"
        );
        let (cell, origin) = crate::camera::split(camera);
        let ray = crate::reference::Ray {
            cell,
            origin,
            dir: [0.0, -1.0, 0.0],
        };
        let hit = crate::reference::trace(&scene, &ray).expect("the brick is hit");
        assert_eq!(hit.voxel[1], 15);
    }

    #[test]
    fn a_camera_that_jumps_gets_a_new_scene_and_one_that_walks_gets_a_patch() {
        let mut scene = rings_at(ABOVE);
        // d0 is 64 Voxels here: a jump is more than 96.
        let near = [60.0, 1560.0, 0.0];
        assert!(matches!(
            scene.recentre(ground(), near),
            Ok(None | Some(SceneUpdate::Rings(_)))
        ));
        let far = [400.0, 1600.0, 0.0];
        assert_eq!(
            scene.recentre(ground(), far),
            Ok(Some(SceneUpdate::Rebuilt))
        );
        assert_eq!(
            shown(&scene),
            shown(&Scene::rings(ground(), far, bands(), bound()).unwrap())
        );
        // The ball that rays are clipped to is as it was.
        assert_eq!(scene.bound, Some(bound()));
        // And it carries on from there.
        assert!(matches!(
            scene.recentre(ground(), [410.0, 1600.0, 0.0]),
            Ok(None | Some(SceneUpdate::Rings(_)))
        ));
    }
}
