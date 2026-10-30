//! Camera-centred rings: the way a [`Scene`] holds the Planet at every distance from the camera
//! (ADR 0002).
//!
//! A scene of rings has a level for each of the first mips of the brick store, and each level
//! holds a box of [`RING_REGIONS`] regions on each edge, centred on the camera. A region of mip
//! `n` is `64 << n` Voxels across, so each ring is twice as wide as the ring inside it, and it
//! holds about as many bricks: the surface of the Planet crosses the box of each ring in about
//! the same number of bricks, however far across the ring is. That is what keeps GPU memory
//! bounded whatever the size of the Planet and wherever the camera is.
//!
//! The box of a ring contains the box of the ring inside it, and the finest ring contains the
//! camera. The walk in `trace.wgsl` goes through the rings finest first, and takes the nearest
//! hit. In each ring it skips the regions that the box of the ring inside it covers (the
//! ring's *hole*: [`Scene::hole`]), so every part of space is seen at one ring only, finer near
//! the camera and coarser far from it, and a ray that is clear of one ring's Voxels carries on
//! into the next with no gap. The bricks of a ring under its hole are packed all the same and
//! never reached; leaving them out would mean packing the ring again whenever the finer one
//! moved.
//!
//! The buffers are allocated once, for every region the boxes could hold and a budget of words
//! for bricks that all the rings share ([`Room`] says how much for each ring), so they are the
//! same size however full the boxes are and wherever the camera goes. [`Scene::recentre`] moves
//! the boxes with the camera, and touches only the regions that enter or leave a box: the words
//! of those that leave are handed to the ones that enter. [`Scene::update`] takes Edits into
//! every ring at once, from the bricks of level 0 that they changed, and rewrites only the
//! bricks that differ.
//!
//! The one way this can fail is that the words run out ([`RingsFull`]): the Planet is denser than
//! the budget, or Edits have made it so (they only turn uniform bricks into mixed ones, which
//! take words). A scene that has failed is not to be used again: its boxes and its pending
//! patch no longer match what is on the GPU, so the caller stops.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;

use brick_store::{Brick, BrickCoord, BrickStore, BrickView, Material};

use super::{
    Level, MAX_LEVELS, REGION_BRICKS, REGION_VOXELS, REGION_WORDS, RING_REGIONS, SOLID_FLAG, Scene,
    SceneUpdate, bit_of, encode_mixed, region_of,
};

/// How many words the buffers of a scene of rings have, for each ring. The rings share what they
/// have, so a ring that is fuller than the others is no trouble while the others are emptier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Room {
    /// Words of `brick_refs` for each ring: one for every brick it lists that is not inside a
    /// region of one Material.
    pub refs_per_ring: usize,
    /// Words of `bricks` for each ring. A brick of two Materials with a surface in it takes 34
    /// to 50 words, so the default is room for 40 to 60 thousand of them in each ring.
    pub words_per_ring: usize,
}

impl Default for Room {
    fn default() -> Self {
        Self {
            refs_per_ring: 1 << 18,
            words_per_ring: 1 << 21,
        }
    }
}

/// The rings do not fit in their buffers: the Planet is denser than they were sized for, or the
/// words that are free are in pieces too small for a region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingsFull;

impl fmt::Display for RingsFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the rings do not fit in their buffers")
    }
}

impl std::error::Error for RingsFull {}

/// Hands out ranges of the words of one buffer, first fit, and takes them back.
#[derive(Debug)]
struct Pool {
    /// The free ranges, ascending, and never touching: ones that touch are one range.
    free: Vec<Range<usize>>,
}

impl Pool {
    fn new(capacity: usize) -> Self {
        Self {
            free: std::iter::once(0..capacity).collect(),
        }
    }

    /// The start of `len` words that are free, which are now taken, if there are that many in a
    /// row.
    fn take(&mut self, len: usize) -> Option<usize> {
        let at = self.free.iter().position(|r| r.len() >= len)?;
        let start = self.free[at].start;
        if self.free[at].len() == len {
            self.free.remove(at);
        } else {
            self.free[at].start += len;
        }
        Some(start)
    }

    /// Gives `len` words from `start` back, joining them to the free ranges they touch.
    fn give(&mut self, start: usize, len: usize) {
        let at = self.free.partition_point(|r| r.start < start);
        let after = self.free.get(at).is_some_and(|r| r.start == start + len);
        let before = at > 0 && self.free[at - 1].end == start;
        match (before, after) {
            (true, true) => {
                self.free[at - 1].end = self.free[at].end;
                self.free.remove(at);
            }
            (true, false) => self.free[at - 1].end = start + len,
            (false, true) => self.free[at].start = start,
            (false, false) => self.free.insert(at, start..start + len),
        }
    }
}

/// What a scene of rings keeps besides the buffers.
#[derive(Debug)]
pub(super) struct Rings {
    room: Room,
    /// The records of `regions` that no region uses. There is one for every region the boxes
    /// could hold, so a region always finds one when it enters a box.
    free: Vec<usize>,
    refs: Pool,
    data: Pool,
    /// The words of a brick on their way into `bricks`.
    scratch: Vec<u32>,
    /// What has been written since the last patch was handed out.
    patch: Patch,
}

/// A [`RingPatch`] in the making.
#[derive(Debug, Default)]
struct Patch {
    regions: BTreeSet<usize>,
    table: BTreeSet<usize>,
    brick_refs: Vec<Range<usize>>,
    bricks: Vec<Range<usize>>,
    levels: bool,
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
    /// Whether the boxes in [`Scene::levels`] moved.
    pub levels: bool,
}

impl RingPatch {
    /// Words that copying this patch to the GPU writes, apart from the boxes.
    pub fn words(&self) -> usize {
        let ranges = |r: &[Range<usize>]| r.iter().map(Range::len).sum::<usize>();
        self.regions.len() * REGION_WORDS
            + self.table.len() * 4
            + ranges(&self.brick_refs)
            + ranges(&self.bricks)
    }
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

/// What a ring has in one region.
enum Content<'a> {
    /// Every brick is one Material.
    Uniform(Material),
    /// The bricks that are not air, as (mask bit, brick), by mask bit.
    Bricks(Vec<(usize, Cow<'a, Brick>)>),
}

/// What `store` has in the region at `key` of mip `level`, or `None` if it is all air.
fn region_content(store: &BrickStore, level: usize, key: [i32; 3]) -> Option<Content<'_>> {
    let first = key.map(|k| k * REGION_BRICKS);
    let bricks: Vec<_> = (0..512)
        .map(|bit| {
            let coord = BrickCoord(std::array::from_fn(|a| {
                first[a] + (bit >> (3 * a) & 7) as i32
            }));
            (bit, store.brick_at(level, coord))
        })
        .filter(|(_, brick)| !brick.is_air())
        .collect();
    let uniform = match bricks.first()?.1.view() {
        BrickView::Solid(material) if bricks.len() == 512 => bricks
            .iter()
            .all(|(_, b)| matches!(b.view(), BrickView::Solid(m) if m == material))
            .then_some(material),
        _ => None,
    };
    Some(match uniform {
        Some(material) => Content::Uniform(material),
        None => Content::Bricks(bricks),
    })
}

/// The regions of a box that starts at region `first`.
fn keys_of(first: [i32; 3]) -> impl Iterator<Item = [i32; 3]> {
    (0..RING_REGIONS.pow(3)).map(move |i| {
        let at = [
            i % RING_REGIONS,
            i / RING_REGIONS % RING_REGIONS,
            i / RING_REGIONS.pow(2),
        ];
        std::array::from_fn(|a| first[a] + at[a])
    })
}

/// Whether the region at `key` is in the box that starts at region `first`.
fn inside(first: [i32; 3], key: [i32; 3]) -> bool {
    (0..3).all(|a| key[a] >= first[a] && key[a] < first[a] + RING_REGIONS)
}

/// Regions of a box from its start that the camera may be in without the box moving: the box
/// moves when the camera leaves these, and then puts the camera in the middle.
const SLACK: std::ops::RangeInclusive<i32> = (RING_REGIONS / 2 - 2)..=(RING_REGIONS / 2 + 1);

/// Where the boxes of `levels` rings start, in regions of their own level, for a camera at
/// `camera`, given where they start now (`current`, empty at the start). A box stays where it is
/// on an axis while the camera is in the regions of [`SLACK`] counted from its start, so a
/// camera that hovers about a plane between two regions does not move the box with every
/// crossing. Otherwise the region that holds the camera is put in the middle, give or take one.
///
/// A start is even, so the box of a ring is a whole number of regions of the next. With the
/// camera in the regions 6 to 9 of every box, the box of a ring is inside the box of the ring
/// outside it by at least three of its regions on every side, whatever the camera: the region
/// that holds the camera at one level is the half of a region at the next. A test checks it.
fn place(camera: [f64; 3], levels: usize, current: &[[i32; 3]]) -> Vec<[i32; 3]> {
    (0..levels)
        .map(|level| {
            let size = f64::from(REGION_VOXELS << level);
            std::array::from_fn(|a| {
                let region = (camera[a] / size).floor() as i32;
                match current.get(level) {
                    Some(first) if SLACK.contains(&(region - first[a])) => first[a],
                    _ => (region - (RING_REGIONS / 2 - 1)).div_euclid(2) * 2,
                }
            })
        })
        .collect()
}

impl Scene {
    /// Camera-centred rings: the first `levels` mips of `store` (fewer if it has fewer, and at
    /// most [`MAX_LEVELS`]), each holding the [`RING_REGIONS`] regions along each edge that are
    /// centred on `camera`, a position in Voxels. The store must have its pyramid
    /// ([`BrickStore::build_lod`]). Every level is read through [`BrickStore::brick_at`], so the
    /// store's source stands in for the bricks it does not hold, as the core of the Planet does.
    ///
    /// The module's docs say how rings are laid out and walked. [`Scene::recentre`] keeps them
    /// centred on the camera and [`Scene::update`] takes Edits in. This fails if the rings do
    /// not fit in their buffers.
    pub fn rings(store: &BrickStore, camera: [f64; 3], levels: usize) -> Result<Scene, RingsFull> {
        Scene::rings_in(store, camera, levels, Room::default())
    }

    /// [`Scene::rings`] with buffers of another size.
    pub fn rings_in(
        store: &BrickStore,
        camera: [f64; 3],
        levels: usize,
        room: Room,
    ) -> Result<Scene, RingsFull> {
        let levels = levels.min(store.level_count()).min(MAX_LEVELS);
        Scene::with_boxes(store, &place(camera, levels, &[]), room)
    }

    /// Rings with their boxes starting at the regions `firsts`, one for each level.
    fn with_boxes(store: &BrickStore, firsts: &[[i32; 3]], room: Room) -> Result<Scene, RingsFull> {
        let per_level = RING_REGIONS.pow(3) as usize;
        // Half full at most, so searches for a region end quickly.
        let slots = (per_level * 2).next_power_of_two();
        let levels = firsts.len();
        let mut scene = Scene {
            levels: (firsts.iter().enumerate())
                .map(|(level, first)| Level {
                    origin: first.map(|f| f * (REGION_VOXELS << level)),
                    dims: [RING_REGIONS as u32; 3],
                    table_base: (level * slots) as u32,
                    table_mask: slots as u32 - 1,
                })
                .collect(),
            table: vec![0; levels * slots * 4],
            regions: vec![0; levels * per_level * REGION_WORDS],
            brick_refs: vec![0; levels * room.refs_per_ring],
            bricks: vec![0; levels * room.words_per_ring],
            rings: Some(Rings {
                room,
                free: (0..levels * per_level).rev().collect(),
                refs: Pool::new(levels * room.refs_per_ring),
                data: Pool::new(levels * room.words_per_ring),
                scratch: Vec::new(),
                patch: Patch::default(),
            }),
            ..Scene::default()
        };
        for (level, first) in firsts.iter().enumerate() {
            for key in keys_of(*first) {
                scene.add_region(store, level, key)?;
            }
        }
        scene.take_patch();
        Ok(scene)
    }

    /// Where each level's box starts, in regions of that level.
    pub fn box_starts(&self) -> Vec<[i32; 3]> {
        (self.levels.iter().enumerate())
            .map(|(level, l)| l.first_region(level))
            .collect()
    }

    /// A scene of rings packed again from scratch from `store`, with the same boxes and room: what
    /// the scene should show, with the words in order.
    pub fn rebuilt_rings(&self, store: &BrickStore) -> Result<Scene, RingsFull> {
        let room = self.rings.as_ref().map_or_else(Room::default, |r| r.room);
        Scene::with_boxes(store, &self.box_starts(), room)
    }

    /// Whether this is a scene of rings (see [`Scene::rings`]).
    pub fn is_rings(&self) -> bool {
        self.rings.is_some()
    }

    /// Moves the rings' boxes to centre on `camera` (a position in Voxels), if the camera has
    /// left the middle of one of them: a box moves two regions at a time, and the camera may be
    /// in any of four regions of it without it moving (see `SLACK`). The regions that leave a box
    /// are dropped and those that enter it are packed from `store`; nothing else is touched.
    /// Returns what changed, or `None` if nothing did (and if this is not a scene of rings).
    ///
    /// If the regions that enter do not fit, the scene is packed again from scratch, which is
    /// reported as [`SceneUpdate::Rebuilt`]. It is an error only if they do not fit even then, and
    /// then the scene is half moved and not to be used again.
    pub fn recentre(
        &mut self,
        store: &BrickStore,
        camera: [f64; 3],
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        if self.rings.is_none() {
            return Ok(None);
        }
        let current = self.box_starts();
        let wanted = place(camera, current.len(), &current);
        if wanted == current {
            return Ok(None);
        }
        Ok(Some(match self.move_boxes(store, &current, &wanted) {
            Ok(patch) => SceneUpdate::Rings(patch),
            Err(RingsFull) => {
                let room = self.rings.as_ref().map_or_else(Room::default, |r| r.room);
                *self = Scene::with_boxes(store, &wanted, room)?;
                SceneUpdate::Rebuilt
            }
        }))
    }

    /// Moves the box of each level from the start in `current` to the one in `wanted`.
    fn move_boxes(
        &mut self,
        store: &BrickStore,
        current: &[[i32; 3]],
        wanted: &[[i32; 3]],
    ) -> Result<RingPatch, RingsFull> {
        for (level, (&old, &new)) in current.iter().zip(wanted).enumerate() {
            if old == new {
                continue;
            }
            for key in keys_of(old).filter(|&key| !inside(new, key)) {
                self.drop_region(level, key);
            }
            self.levels[level].origin = new.map(|f| f * (REGION_VOXELS << level));
            self.patch().levels = true;
            for key in keys_of(new).filter(|&key| !inside(old, key)) {
                self.add_region(store, level, key)?;
            }
        }
        Ok(self.take_patch())
    }

    /// [`Scene::update`] for rings. The bricks over each changed brick are packed again at every
    /// level, wherever they are in the ring's box; the store has remade them already (see
    /// [`BrickStore::refresh_lod`]). A brick whose words are what they were is not written again.
    /// A region that a box has but the scene lacks, because it was all air, is added.
    ///
    /// If the words run out the scene is packed again from scratch ([`SceneUpdate::Rebuilt`]),
    /// which frees what the Edits left; it is an error only if they do not fit even then, and
    /// then the scene is half changed and not to be used again.
    pub(super) fn update_rings(
        &mut self,
        store: &BrickStore,
        changed: &[BrickCoord],
    ) -> Result<SceneUpdate, RingsFull> {
        Ok(match self.repack_regions(store, changed) {
            Ok(patch) => SceneUpdate::Rings(patch),
            Err(RingsFull) => {
                *self = self.rebuilt_rings(store)?;
                SceneUpdate::Rebuilt
            }
        })
    }

    fn repack_regions(
        &mut self,
        store: &BrickStore,
        changed: &[BrickCoord],
    ) -> Result<RingPatch, RingsFull> {
        // The bricks over the changed ones that are in a box, by level and region.
        let mut by_region: BTreeMap<(usize, [i32; 3]), BTreeSet<BrickCoord>> = BTreeMap::new();
        for level in 0..self.levels.len() {
            let first = self.levels[level].first_region(level);
            for brick in changed {
                let over = BrickCoord(brick.0.map(|c| c >> level));
                if inside(first, region_of(over)) {
                    by_region
                        .entry((level, region_of(over)))
                        .or_default()
                        .insert(over);
                }
            }
        }
        for ((level, key), bricks) in by_region {
            self.repack_region(store, level, key, &bricks)?;
        }
        Ok(self.take_patch())
    }

    /// Brings the region at `key` of mip `level` up to date with `store` for its bricks
    /// `changed`.
    fn repack_region(
        &mut self,
        store: &BrickStore,
        level: usize,
        key: [i32; 3],
        changed: &BTreeSet<BrickCoord>,
    ) -> Result<(), RingsFull> {
        let Some(index) = self.find_region(level, key) else {
            // It was all air, so it was not listed; it may not be now.
            return self.add_region(store, level, key);
        };
        let old: Vec<Option<u32>> = (0..512).map(|bit| self.entry(index, bit)).collect();
        let mut new = old.clone();
        for &coord in changed {
            let bit = bit_of(coord);
            let brick = store.brick_at(level, coord);
            new[bit] = match brick.view() {
                BrickView::Air => None,
                view => Some(self.put_brick(view, old[bit])?),
            };
        }
        if new == old {
            return Ok(());
        }
        // What the region no longer refers to is free, and so is its list of entries.
        for (&before, &after) in old.iter().zip(&new) {
            if let Some(entry) = before.filter(|&e| Some(e) != after) {
                self.release_brick(entry);
            }
        }
        self.release_refs(index);
        let bricks: Vec<(usize, u32)> = (0..512).filter_map(|bit| Some((bit, new[bit]?))).collect();
        if bricks.is_empty() {
            self.unlist_region(level, key, index);
            return Ok(());
        }
        let first = bricks[0].1;
        let at = index * REGION_WORDS;
        if bricks.len() == 512 && first & SOLID_FLAG != 0 && bricks.iter().all(|b| b.1 == first) {
            self.regions[at..at + 16].fill(u32::MAX);
            self.regions[at + 16] = first;
        } else {
            self.write_entries(index, &bricks)?;
        }
        self.patch().regions.insert(index);
        Ok(())
    }

    /// Packs the region at `key` of mip `level` as `store` has it and lists it. A region of only
    /// air is not listed.
    fn add_region(
        &mut self,
        store: &BrickStore,
        level: usize,
        key: [i32; 3],
    ) -> Result<(), RingsFull> {
        let Some(content) = region_content(store, level, key) else {
            return Ok(());
        };
        let rings = self.rings.as_mut().expect("a scene of rings");
        let index = rings.free.pop().ok_or(RingsFull)?;
        let at = index * REGION_WORDS;
        match content {
            Content::Uniform(material) => {
                self.regions[at..at + 16].fill(u32::MAX);
                self.regions[at + 16] = SOLID_FLAG | u32::from(material.0);
            }
            Content::Bricks(bricks) => {
                let mut entries = Vec::with_capacity(bricks.len());
                for (bit, brick) in bricks {
                    entries.push((bit, self.put_brick(brick.view(), None)?));
                }
                self.write_entries(index, &entries)?;
            }
        }
        let slot = self.insert_region(level, key, index as u32);
        let patch = self.patch();
        patch.regions.insert(index);
        patch.table.insert(slot);
        Ok(())
    }

    /// Unlists the region at `key` of mip `level`, if the scene has it, and frees what it took.
    fn drop_region(&mut self, level: usize, key: [i32; 3]) {
        let Some(index) = self.find_region(level, key) else {
            return;
        };
        for bit in 0..512 {
            if let Some(entry) = self.entry(index, bit) {
                self.release_brick(entry);
            }
        }
        self.release_refs(index);
        self.unlist_region(level, key, index);
    }

    /// Takes the region at `key` of mip `level` out of the table, and its record `index` back.
    fn unlist_region(&mut self, level: usize, key: [i32; 3], index: usize) {
        let mut slots = BTreeSet::new();
        self.remove_region(level, key, &mut slots);
        let rings = self.rings.as_mut().expect("a scene of rings");
        rings.patch.table.extend(slots);
        rings.patch.regions.remove(&index);
        rings.free.push(index);
    }

    /// Writes the mask and the list of entries of the region record at `index`, for `bricks`
    /// (mask bit and entry, by mask bit). The list takes words from `brick_refs`.
    fn write_entries(&mut self, index: usize, bricks: &[(usize, u32)]) -> Result<(), RingsFull> {
        let rings = self.rings.as_mut().expect("a scene of rings");
        let first_ref = rings.refs.take(bricks.len()).ok_or(RingsFull)?;
        rings
            .patch
            .brick_refs
            .push(first_ref..first_ref + bricks.len());
        let mut mask = [0u32; 16];
        for (i, &(bit, entry)) in bricks.iter().enumerate() {
            mask[bit / 32] |= 1 << (bit % 32);
            self.brick_refs[first_ref + i] = entry;
        }
        let at = index * REGION_WORDS;
        self.regions[at..at + 16].copy_from_slice(&mask);
        self.regions[at + 16] = first_ref as u32;
        Ok(())
    }

    /// Writes `view`, which is not air, and returns its entry for `brick_refs`. A mixed brick
    /// takes words from `bricks`, unless `old` is the entry of a mixed brick with the same words,
    /// which is kept.
    fn put_brick(&mut self, view: BrickView<'_>, old: Option<u32>) -> Result<u32, RingsFull> {
        let BrickView::Mixed {
            occupancy,
            palette,
            bits,
            indices,
        } = view
        else {
            return match view {
                BrickView::Solid(material) => Ok(SOLID_FLAG | u32::from(material.0)),
                _ => unreachable!("air bricks are skipped"),
            };
        };
        let old = old.filter(|&entry| entry & SOLID_FLAG == 0);
        let old_len = old.map(|entry| self.mixed_words(entry));
        let rings = self.rings.as_mut().expect("a scene of rings");
        rings.scratch.clear();
        encode_mixed(&mut rings.scratch, occupancy, palette, bits, indices);
        let len = rings.scratch.len();
        if let (Some(old), Some(old_len)) = (old, old_len)
            && old_len == len
            && self.bricks[old as usize..][..len] == rings.scratch[..]
        {
            return Ok(old);
        }
        let at = rings.data.take(len).ok_or(RingsFull)?;
        rings.patch.bricks.push(at..at + len);
        self.bricks[at..at + len].copy_from_slice(&rings.scratch);
        Ok(at as u32)
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
            let first_ref = self.regions[index * REGION_WORDS + 16] as usize;
            let rings = self.rings.as_mut().expect("a scene of rings");
            rings.refs.give(first_ref, refs);
        }
    }

    /// The regions of the box of mip `level` that a finer ring holds instead, as the first region
    /// and the one after the last, in regions of `level`: none for level 0, and for a scene that
    /// is not rings. The walk leaves them to the finer ring.
    pub fn hole(&self, level: usize) -> ([i32; 3], [i32; 3]) {
        match level.checked_sub(1).and_then(|l| self.levels.get(l)) {
            Some(finer) if self.rings.is_some() => {
                // The finer box starts on an even region, which is the start of a region here.
                let first = finer.first_region(level - 1).map(|f| f >> 1);
                (first, first.map(|f| f + RING_REGIONS / 2))
            }
            _ => ([0; 3], [0; 3]),
        }
    }

    /// Bytes of the scene's buffers that hold something. The buffers of rings are allocated
    /// whole, so for them this is how full they are, and the size of the GPU copy is that of the
    /// buffers.
    pub fn used_bytes(&self) -> u64 {
        let words = match &self.rings {
            Some(rings) => {
                let free = |pool: &Pool| pool.free.iter().map(Range::len).sum::<usize>();
                self.table.len() + self.regions.len() + self.brick_refs.len() + self.bricks.len()
                    - rings.free.len() * REGION_WORDS
                    - free(&rings.refs)
                    - free(&rings.data)
            }
            None => {
                self.table.len() + self.regions.len() + self.brick_refs.len() + self.bricks.len()
                    - self.garbage
            }
        };
        words as u64 * 4
    }

    /// Bricks the scene lists at mip `level`, the ones under the box of a finer ring included.
    /// Those inside a region of one Material are not listed: they cost nothing.
    pub fn level_bricks(&self, level: usize) -> usize {
        let Some(level) = self.levels.get(level) else {
            return 0;
        };
        let slots = level.table_base as usize..=(level.table_base + level.table_mask) as usize;
        slots
            .filter_map(|slot| self.table[slot * 4 + 3].checked_sub(1))
            .map(|index| self.region_refs(index as usize))
            .sum()
    }

    /// What has been written since the last patch.
    fn patch(&mut self) -> &mut Patch {
        &mut self.rings.as_mut().expect("a scene of rings").patch
    }

    /// Hands out what has been written since the last call.
    fn take_patch(&mut self) -> RingPatch {
        let patch = std::mem::take(self.patch());
        RingPatch {
            regions: patch.regions.into_iter().collect(),
            table: patch.table.into_iter().collect(),
            brick_refs: merged(patch.brick_refs),
            bricks: merged(patch.bricks),
            levels: patch.levels,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use brick_store::VoxelCoord;
    use planet_gen::generate_region;

    use super::*;

    const LEVELS: usize = 5;
    /// Above the test Planet's +y pole.
    const ABOVE: [f64; 3] = [0.0, 1560.0, 0.0];

    /// The ground near the +y pole, Voxels x -440..=447, y 1360..=1575, z -360..=263, with its
    /// pyramid. Its bricks reach into the first few rings.
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
        Scene::rings(ground(), camera, LEVELS).unwrap()
    }

    /// Voxels of level 0 across a region of mip `level`.
    fn region_size(level: usize) -> f64 {
        f64::from(REGION_VOXELS << level)
    }

    /// What a scene shows of every region of its boxes, by level and region: its Material if it
    /// is all one, else the words of each brick by mask bit. It does not depend on where in the
    /// buffers anything is.
    #[allow(clippy::type_complexity)]
    fn shown(scene: &Scene) -> BTreeMap<(usize, [i32; 3]), (Option<u32>, Vec<(usize, Vec<u32>)>)> {
        let mut all = BTreeMap::new();
        for (level, l) in scene.levels.iter().enumerate() {
            for key in keys_of(l.first_region(level)) {
                let Some(index) = scene.find_region(level, key) else {
                    continue;
                };
                let first_ref = scene.regions[index * REGION_WORDS + 16];
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
    fn the_rings_are_boxes_around_the_camera_each_twice_as_wide_as_the_one_inside() {
        for camera in [ABOVE, [-300.5, 1523.25, 77.0], [5000.0, -3000.0, 0.25]] {
            let scene = rings_at(camera);
            assert_eq!(scene.levels.len(), LEVELS);
            for (level, l) in scene.levels.iter().enumerate() {
                assert_eq!(l.dims, [RING_REGIONS as u32; 3]);
                let width = f64::from(RING_REGIONS) * region_size(level);
                for (a, (&at, &origin)) in camera.iter().zip(&l.origin).enumerate() {
                    let middle = f64::from(origin) + width / 2.0;
                    assert!(
                        (at - middle).abs() <= region_size(level),
                        "level {level} axis {a}: the camera is at {at} and a box {width} across at {origin}",
                    );
                }
            }
        }
    }

    #[test]
    fn each_box_contains_the_box_inside_it_wherever_the_camera_goes() {
        // Walks, turns, jitter about the planes between regions of every level, and jumps, across
        // zero. The boxes carry on from where they were, as they do in a flight.
        let mut state = 5u64;
        let mut rng = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 40) as f64 / (1u64 << 24) as f64
        };
        let mut camera = [-9000.0, 700.0, 3000.0];
        let mut step = [137.3, -61.9, 29.1];
        let mut boxes: Vec<[i32; 3]> = Vec::new();
        for i in 0..4000 {
            boxes = place(camera, LEVELS, &boxes);
            for (level, first) in boxes.iter().enumerate() {
                let size = region_size(level);
                for (a, &start) in first.iter().enumerate() {
                    let region = (camera[a] / size).floor() as i32;
                    assert!(
                        (RING_REGIONS / 2 - 2..=RING_REGIONS / 2 + 1).contains(&(region - start)),
                        "step {i}, level {level}, axis {a}: camera in region {region} of a box at {start}"
                    );
                    assert_eq!(start % 2, 0, "the start of a box is even");
                }
            }
            for level in 1..LEVELS {
                for (a, (inner, outer)) in boxes[level - 1].iter().zip(boxes[level]).enumerate() {
                    // Both in regions of the inner level: the outer box is twice as wide, and the
                    // inner box is at least three of its regions inside it on every side.
                    let (inner, outer) = (*inner, 2 * outer);
                    assert!(
                        inner >= outer + 3 && inner + RING_REGIONS + 3 <= outer + 2 * RING_REGIONS,
                        "step {i}, level {level}, axis {a}: {inner} in {outer}"
                    );
                }
            }
            match i % 400 {
                // Jitter across the plane between two regions of level 0.
                100..200 => camera[0] = 575.5 + f64::from(i % 2),
                // A jump to anywhere in the 200000 Voxels about the origin.
                399 => camera = [0, 1, 2].map(|_| (rng() - 0.5) * 200_000.0),
                // Back and forth along an axis.
                200..300 => camera[2] += (rng() - 0.5) * 300.0,
                _ => {
                    for (at, by) in camera.iter_mut().zip(&mut step) {
                        *at += *by;
                        *by *= 1.003;
                    }
                }
            }
        }
    }

    #[test]
    fn a_camera_that_jitters_across_a_boundary_between_regions_moves_nothing_after_the_first_time()
    {
        // A plane between regions of every level, and of the odd and the even regions.
        for level in 0..LEVELS {
            for k in [3, 4] {
                let plane = f64::from(k) * region_size(level);
                let at = |x: f64| [x, 1560.0, 0.0];
                let mut scene = rings_at(at(plane - 0.5));
                for x in [
                    plane + 0.5,
                    plane - 0.5,
                    plane + 0.5,
                    plane - 1.5,
                    plane + 1.5,
                ] {
                    assert_eq!(
                        scene.recentre(ground(), at(x)),
                        Ok(None),
                        "level {level}, plane {k}, x {x}"
                    );
                }
            }
        }
        // And the first crossing that is out of the slack moves it once, and then it rests.
        let mut scene = rings_at([0.0, 1560.0, 0.0]);
        let x = 10.0 * region_size(0);
        assert!(
            scene
                .recentre(ground(), [x, 1560.0, 0.0])
                .unwrap()
                .is_some()
        );
        for x in [x + 1.0, x - 1.0, x + 70.0, x - 70.0] {
            assert_eq!(scene.recentre(ground(), [x, 1560.0, 0.0]), Ok(None));
        }
    }

    #[test]
    fn the_hole_of_a_ring_is_the_box_of_the_ring_inside_it() {
        let scene = rings_at([-3000.0, 1560.0, 25.0]);
        assert_eq!(scene.hole(0), ([0; 3], [0; 3]));
        for level in 1..LEVELS {
            let (first, end) = scene.hole(level);
            let finer = scene.levels[level - 1];
            for a in 0..3 {
                // The same Voxels, in regions of this level.
                let voxels = |region: i32| i64::from(region) * i64::from(REGION_VOXELS << level);
                assert_eq!(voxels(first[a]), i64::from(finer.origin[a]));
                assert_eq!(
                    voxels(end[a]),
                    i64::from(finer.origin[a])
                        + i64::from(RING_REGIONS) * region_size(level - 1) as i64
                );
            }
        }
        // An ordinary scene has no hole.
        assert_eq!(Scene::new(ground(), None).hole(0), ([0; 3], [0; 3]));
    }

    #[test]
    fn the_buffers_are_the_same_size_whatever_the_rings_hold() {
        let full = rings_at(ABOVE);
        let mut bare = BrickStore::new();
        bare.build_lod(LEVELS - 1);
        let empty = Scene::rings(&bare, ABOVE, LEVELS).unwrap();
        let sizes = |s: &Scene| {
            (
                s.table.len(),
                s.regions.len(),
                s.brick_refs.len(),
                s.bricks.len(),
            )
        };
        assert_eq!(sizes(&full), sizes(&empty));
        assert!(full.used_bytes() > empty.used_bytes());
        let allocated = sizes(&full);
        assert!(
            full.used_bytes() < 4 * (allocated.0 + allocated.1 + allocated.2 + allocated.3) as u64
        );
    }

    #[test]
    fn a_camera_that_stays_in_the_middle_of_the_boxes_moves_nothing() {
        let mut scene = rings_at(ABOVE);
        // Level 0 regions are 64 Voxels across and the camera is in region 0 of x and z.
        assert_eq!(scene.recentre(ground(), [63.0, 1560.0, 63.0]), Ok(None));
        assert_eq!(scene.recentre(ground(), [-1.0, 1561.0, -1.0]), Ok(None));
        assert!(matches!(
            scene.recentre(ground(), [0.0, 1200.0, 0.0]),
            Ok(Some(SceneUpdate::Rings(_)))
        ));
    }

    #[test]
    fn rings_that_are_moved_show_what_rings_built_where_they_end_do() {
        let mut scene = rings_at([-420.0, 1560.0, -300.0]);
        for (to, moved) in [
            ([300.0, 1560.0, 0.0], true),
            ([300.0, 1561.0, 1.0], false),
            ([-200.0, 1400.0, 200.0], true),
            ([-300.0, 2900.0, 200.0], true),
        ] {
            let update = scene.recentre(ground(), to).unwrap();
            assert_eq!(update.is_some(), moved, "{to:?}");
            // A box moves when the camera leaves its middle, not for every region it crosses, so
            // it is not where a new scene would put it: but it is the scene that has those boxes.
            assert_eq!(
                shown(&scene),
                shown(&scene.rebuilt_rings(ground()).unwrap()),
                "{to:?}"
            );
        }
    }

    #[test]
    fn a_move_writes_what_a_copy_needs_and_leaves_regions_that_stay_alone() {
        let mut scene = rings_at([-400.0, 1560.0, 0.0]);
        let before = Copy::of(&scene);
        let boxes = scene.box_starts();
        let patch = patch_of(scene.recentre(ground(), [300.0, 1560.0, 0.0]).unwrap());
        assert!(patch.levels);

        // A copy that was written the patch is the scene.
        assert_eq!(before.clone().patched(&patch, &scene), Copy::of(&scene));

        // Regions that stay in a box are not touched: not their record, not their slot in the
        // table, not the words of their bricks.
        let written = |ranges: &[Range<usize>], words: Range<usize>| {
            ranges
                .iter()
                .any(|r| r.start < words.end && words.start < r.end)
        };
        let mut stayed = 0;
        for (level, first) in boxes.iter().enumerate() {
            for key in keys_of(*first) {
                let Some(index) = scene
                    .find_region(level, key)
                    .filter(|_| inside(scene.levels[level].first_region(level), key))
                else {
                    continue;
                };
                stayed += 1;
                assert!(!patch.regions.contains(&index), "{level} {key:?}");
                let refs = scene.region_refs(index);
                let first_ref = scene.regions[index * REGION_WORDS + 16] as usize;
                assert!(
                    !written(&patch.brick_refs, first_ref..first_ref + refs),
                    "{level} {key:?}"
                );
                for bit in 0..512 {
                    if let Some(entry) = scene.entry(index, bit) {
                        let words = scene.mixed_words(entry);
                        let entry = entry as usize;
                        assert!(
                            !written(&patch.bricks, entry..entry + words),
                            "{level} {key:?}"
                        );
                    }
                }
            }
        }
        assert!(stayed > 100, "{stayed} regions stayed");
    }

    #[test]
    fn a_small_move_writes_a_small_part_of_the_scene() {
        let mut scene = rings_at([-100.0, 1560.0, 0.0]);
        let total = scene.used_bytes() as usize / 4;
        let patch = patch_of(scene.recentre(ground(), [100.0, 1560.0, 0.0]).unwrap());
        assert!(patch.words() > 0);
        assert!(
            patch.words() * 8 < total,
            "{} words written of {total}",
            patch.words()
        );
    }

    #[test]
    fn a_long_flight_never_grows_the_buffers_and_ends_where_a_new_scene_would() {
        let start = [-420.0, 1560.0, -300.0];
        let mut scene = rings_at(start);
        let mut copy = Copy::of(&scene);
        let sizes = (
            scene.table.len(),
            scene.regions.len(),
            scene.brick_refs.len(),
            scene.bricks.len(),
        );
        let mut at = start;
        for step in 0..150 {
            at = [
                at[0] + 7.5 + f64::from(step % 5),
                1560.0 + 400.0 * (f64::from(step) / 20.0).sin(),
                at[2] + 4.0,
            ];
            match scene.recentre(ground(), at).unwrap() {
                None => {}
                Some(SceneUpdate::Rings(patch)) => copy = copy.patched(&patch, &scene),
                Some(other) => panic!("step {step}: {other:?}"),
            }
            assert_eq!(
                sizes,
                (
                    scene.table.len(),
                    scene.regions.len(),
                    scene.brick_refs.len(),
                    scene.bricks.len()
                )
            );
            assert_eq!(copy, Copy::of(&scene), "step {step}");
        }
        assert_eq!(
            shown(&scene),
            shown(&scene.rebuilt_rings(ground()).unwrap())
        );
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
    fn an_edit_shows_in_every_ring_as_a_new_scene_would_and_writes_only_what_changed() {
        let mut store = ground().clone();
        let mut scene = Scene::rings(&store, ABOVE, LEVELS).unwrap();
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
            shown(&Scene::rings(&store, ABOVE, LEVELS).unwrap())
        );
        copy = copy.patched(&patch, &scene);
        assert_eq!(copy, Copy::of(&scene));
        // A crater 28 Voxels across touches a handful of bricks in each ring.
        assert!(patch.words() < 12_000, "{} words", patch.words());
        assert!(!patch.levels);

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
            .free
            .iter()
            .map(Range::len)
            .sum();
        scene.bricks.len() - free
    }

    /// Room for a ring set of `scene`, and `spare` words more.
    fn room_for(scene: &Scene, spare: usize) -> Room {
        Room {
            words_per_ring: (words_taken(scene) + spare).div_ceil(LEVELS),
            ..Room::default()
        }
    }

    #[test]
    fn edits_that_need_more_words_than_the_rings_have_are_an_error_and_not_a_panic() {
        let mut store = ground().clone();
        let room = room_for(&rings_at(ABOVE), 300);
        let mut scene = Scene::rings_in(&store, ABOVE, LEVELS, room).unwrap();
        // Digging turns bricks of rock into bricks of Voxels, which take words.
        let changed = dig(&mut store, [20, 1510, -30], 14);
        store.refresh_lod(changed.iter().copied());
        assert_eq!(scene.update(&store, &changed), Err(RingsFull));
    }

    #[test]
    fn rings_that_move_to_where_there_is_more_than_they_have_room_for_are_an_error() {
        let orbit = [0.0, 3600.0, 3000.0];
        let room = room_for(&rings_at(orbit), 300);
        let mut scene = Scene::rings_in(ground(), orbit, LEVELS, room).unwrap();
        assert_eq!(scene.recentre(ground(), ABOVE), Err(RingsFull));
    }

    #[test]
    fn removing_regions_leaves_the_others_where_a_search_finds_them() {
        // Enough regions in one table that many of them collided.
        let mut store = BrickStore::new();
        let keys: Vec<[i32; 3]> = (0..1500).map(|i| [i % 37, i / 37 % 5, i / 185]).collect();
        for key in &keys {
            store.insert(BrickCoord(key.map(|k| k * 8)), Brick::solid(Material(1)));
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
        assert_eq!(pool.free, std::iter::once(0..100).collect::<Vec<_>>());
        assert_eq!(pool.take(100), Some(0));
    }

    #[test]
    fn ranges_that_touch_or_overlap_are_written_as_one() {
        assert_eq!(
            merged(vec![10..20, 0..5, 5..8, 18..25, 30..30]),
            [0..8, 10..25]
        );
    }
}
