//! The emissive Voxels of a scene as lights both views sample directly (#54, ADR 0005, "Emissive
//! Voxels"): a list of boxes, and for each region of the Planet a bounded list of the boxes near it.
//!
//! An **emitter** is an axis-aligned box of Voxels of level 0 that are all one Material that gives
//! off light (`emissive_intensity` above 0): the emissive Voxels of a brick, merged greedily into as
//! few boxes as it takes, so that a lamp of 4 x 4 x 4 Voxels is one light and not 64. A point of
//! the box's surface is seen from a shaded point over a **solid angle**, and the shaders sample
//! that solid angle (`emitters.wgsl`) so that an emitter close to a surface cannot make a bright
//! speckle: the density of a direction is the same wherever in the solid angle it points.
//!
//! The lists are **per region**: the world is cut into cells of one region (64 Voxels, 3.2 m) and
//! a cell's list is the [`MAX_PER_CELL`] boxes nearest its centre among those in the cell and its
//! 26 neighbours. A shaded point takes the list of the cell it is in. A box that is in no list that
//! reaches a point is found there only by the bounce rays, which count it in full: the estimator
//! of the Reference weighs the two against each other (multiple importance sampling) only where
//! the list has the box, so the sum is unbiased wherever the list is cut.
//!
//! The scan reads the packed scene, level 0 only (a coarser level shows far Voxels at a size the
//! lists do not describe): the emissive Voxels of mixed bricks, bricks that are one Material, and
//! regions that are one Material. [`MAX_BOXES`] bounds the work and the buffer; boxes past it are
//! left to the bounce rays too.
//!
//! The buffer is one array of words, read by the shaders as `emitters` (keep the layout in step with
//! `emitters.wgsl`):
//!
//! | words                          | content                                                      |
//! |--------------------------------|--------------------------------------------------------------|
//! | 0                              | the cell table's slot count minus one                        |
//! | 1                              | the number of boxes                                          |
//! | 2, 3                           | where `lists` and `boxes` start, in words                    |
//! | 4 ..                           | the table: 4 words a slot, the cell's coordinate (3 x i32), then `count << 24 \| first` (0 for an empty slot) |
//! | `lists` ..                     | the boxes of every cell's list, by index                     |
//! | `boxes` ..                     | 8 words a box: its first Voxel (3 x i32), its Material, one past its last Voxel (3 x i32), and a pad |

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use crate::scene::{
    REGION_FIRST_REF, REGION_VOXELS, REGION_WORDS, SOLID_FLAG, Scene, SceneUpdate, hash_region,
};

/// The most boxes in a cell's list. Keep in step with `EMITTER_LIST_MAX` in `emitters.wgsl`.
pub const MAX_PER_CELL: usize = 8;
/// The most boxes a scene's buffer holds.
pub const MAX_BOXES: usize = 1 << 15;
/// Voxels along a cell: a region.
pub const CELL_VOXELS: i32 = REGION_VOXELS;
/// Words of a box in the buffer.
pub const BOX_WORDS: usize = 8;
/// The words before the table.
const HEADER_WORDS: usize = 4;

/// A box of Voxels of one Material that gives off light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EmitterBox {
    /// The first Voxel, and one past the last, in Voxels of level 0.
    pub min: [i32; 3],
    pub max: [i32; 3],
    pub material: u16,
}

impl EmitterBox {
    fn centre2(&self) -> [i64; 3] {
        std::array::from_fn(|a| i64::from(self.min[a]) + i64::from(self.max[a]))
    }
}

/// The emitters of a scene, packed for the GPU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Emitters {
    /// The buffer's words (see the module's table).
    pub words: Vec<u32>,
    /// The boxes in it.
    pub boxes: usize,
}

impl Emitters {
    /// No emitters: a buffer a shader can read, whose lists are all empty.
    pub fn none() -> Self {
        Self::pack(&[])
    }

    /// The emitters of `scene` (level 0), for the Materials whose id is true in `emissive`.
    pub fn of(scene: &Scene, emissive: &[bool]) -> Self {
        EmitterIndex::scan(scene, emissive).emitters()
    }

    /// The buffer for `boxes`, whose lists are made as the scene's are.
    pub fn pack(boxes: &[EmitterBox]) -> Self {
        EmitterIndex::of_boxes(boxes).emitters()
    }

    /// The buffer for `boxes`, with the lists `lists` (each box in a list is one of `boxes`).
    fn assemble(boxes: &[EmitterBox], lists: &BTreeMap<[i32; 3], Vec<usize>>) -> Self {
        let slots = (lists.len() * 2).next_power_of_two().max(2);
        let lists_base = HEADER_WORDS + slots * 4;
        let list_words: usize = lists.values().map(Vec::len).sum();
        let boxes_base = lists_base + list_words;
        let mut words = vec![0u32; boxes_base + boxes.len() * BOX_WORDS];
        words[0] = slots as u32 - 1;
        words[1] = boxes.len() as u32;
        words[2] = lists_base as u32;
        words[3] = boxes_base as u32;
        let mut next = 0usize;
        for (cell, list) in lists {
            let mut slot = hash_region(*cell) as usize & (slots - 1);
            while words[HEADER_WORDS + slot * 4 + 3] != 0 {
                slot = (slot + 1) & (slots - 1);
            }
            let at = HEADER_WORDS + slot * 4;
            for a in 0..3 {
                words[at + a] = cell[a] as u32;
            }
            words[at + 3] = (list.len() as u32) << 24 | next as u32;
            for &index in list {
                words[lists_base + next] = index as u32;
                next += 1;
            }
        }
        for (i, b) in boxes.iter().enumerate() {
            let at = boxes_base + i * BOX_WORDS;
            for a in 0..3 {
                words[at + a] = b.min[a] as u32;
                words[at + 4 + a] = b.max[a] as u32;
            }
            words[at + 3] = u32::from(b.material);
        }
        Self {
            words,
            boxes: boxes.len(),
        }
    }

    /// The boxes in the buffer, in order.
    pub fn boxes(&self) -> Vec<EmitterBox> {
        let base = self.words[3] as usize;
        (0..self.boxes)
            .map(|i| {
                let at = base + i * BOX_WORDS;
                let w = &self.words[at..at + BOX_WORDS];
                EmitterBox {
                    min: [w[0] as i32, w[1] as i32, w[2] as i32],
                    max: [w[4] as i32, w[5] as i32, w[6] as i32],
                    material: w[3] as u16,
                }
            })
            .collect()
    }

    /// The boxes of the list of the cell at `cell` (a region's coordinate), the way the shaders
    /// find it.
    pub fn list(&self, cell: [i32; 3]) -> Vec<EmitterBox> {
        let mask = self.words[0] as usize;
        let lists = self.words[2] as usize;
        let boxes = self.boxes();
        let mut slot = hash_region(cell) as usize & mask;
        for _ in 0..=mask {
            let at = HEADER_WORDS + slot * 4;
            let packed = self.words[at + 3];
            if packed == 0 {
                return Vec::new();
            }
            if self.words[at..at + 3] == cell.map(|c| c as u32) {
                let (count, first) = ((packed >> 24) as usize, (packed & 0xff_ffff) as usize);
                return (0..count)
                    .map(|i| boxes[self.words[lists + first + i] as usize])
                    .collect();
            }
            slot = (slot + 1) & mask;
        }
        Vec::new()
    }
}

/// The emitters of a scene's level 0 by region, kept from one update of the scene to the next, so
/// that an update rescans the regions it changed and not the whole scene (a streamed patch of the
/// Planet changes a few of them, an Edit one or two). The boxes are those of the module's account;
/// a region's boxes are found by [`region_boxes`], and a cell's list (a cell is a region: they
/// are the same size, and a box's cell is the region it is in) is made again for the 27 cells
/// round every region whose boxes changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmitterIndex {
    /// The boxes of each region of level 0 that has any, by the region's coordinate.
    by_region: BTreeMap<[i32; 3], Vec<EmitterBox>>,
    /// The list of each cell that has one: its [`MAX_PER_CELL`] nearest boxes, nearest first.
    lists: BTreeMap<[i32; 3], Vec<EmitterBox>>,
}

/// What [`EmitterIndex::apply`] found changed in level 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applied {
    /// Whether any region's boxes are other than they were: the buffer is to be made again.
    pub emitters_changed: bool,
    /// The regions of level 0 whose records or bricks the update wrote.
    pub touched: Touched,
}

/// Which regions of level 0 an update wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touched {
    Nothing,
    /// The regions from the first coordinate to the second, inclusive.
    Regions([i32; 3], [i32; 3]),
    /// The scene was made again: any of it.
    Everything,
}

/// The cell's squared distance to a box's centre, in half Voxels, and the box: what a list is
/// ordered by (the box's fields break a tie, so that the same boxes give the same list).
fn list_order(cell: [i32; 3], b: &EmitterBox) -> (i64, [i32; 3], [i32; 3], u16) {
    let centre2 = b.centre2();
    let from_centre: i64 = (0..3)
        .map(|a| {
            let centre = i64::from(cell[a]) * i64::from(CELL_VOXELS) * 2 + i64::from(CELL_VOXELS);
            (centre2[a] - centre).pow(2)
        })
        .sum();
    (from_centre, b.min, b.max, b.material)
}

fn neighbours(cell: [i32; 3]) -> impl Iterator<Item = [i32; 3]> {
    (0..27).map(move |d| std::array::from_fn(|a| cell[a] + (d / 3i32.pow(a as u32)) % 3 - 1))
}

impl EmitterIndex {
    /// The emitters of all of level 0 of `scene`.
    pub fn scan(scene: &Scene, emissive: &[bool]) -> Self {
        let mut index = Self::default();
        let emits = |material: u32| emissive.get(material as usize).copied().unwrap_or(false);
        if emissive.iter().any(|&e| e)
            && let Some(level) = scene.levels.first()
        {
            for slot in 0..=level.table_mask as usize {
                let at = (level.table_base as usize + slot) * 4;
                let entry = scene.table[at + 3] as usize;
                if entry == 0 {
                    continue;
                }
                let key = std::array::from_fn(|a| scene.table[at + a] as i32);
                let mut boxes = Vec::new();
                region_boxes(scene, key, entry - 1, &emits, &mut boxes);
                if !boxes.is_empty() {
                    index.by_region.insert(key, boxes);
                }
            }
        }
        index.make_all_lists();
        index
    }

    /// An index of `boxes`, each in the region its centre is in.
    fn of_boxes(boxes: &[EmitterBox]) -> Self {
        let mut index = Self::default();
        for b in boxes {
            let centre2 = b.centre2();
            let home: [i32; 3] =
                std::array::from_fn(|a| (centre2[a] / 2).div_euclid(i64::from(CELL_VOXELS)) as i32);
            index.by_region.entry(home).or_default().push(*b);
        }
        index.make_all_lists();
        index
    }

    /// Every list, from the boxes: each box goes to its own cell and the 26 about it.
    fn make_all_lists(&mut self) {
        let mut cells: BTreeMap<[i32; 3], Vec<EmitterBox>> = BTreeMap::new();
        for (&home, boxes) in &self.by_region {
            for cell in neighbours(home) {
                cells.entry(cell).or_default().extend(boxes);
            }
        }
        for (&cell, list) in &mut cells {
            list.sort_unstable_by_key(|b| list_order(cell, b));
            list.truncate(MAX_PER_CELL);
        }
        self.lists = cells;
    }

    /// Makes the list of `cell` again from the regions about it.
    fn make_list(&mut self, cell: [i32; 3]) {
        let mut list: Vec<EmitterBox> = neighbours(cell)
            .filter_map(|n| self.by_region.get(&n))
            .flatten()
            .copied()
            .collect();
        list.sort_unstable_by_key(|b| list_order(cell, b));
        list.truncate(MAX_PER_CELL);
        if list.is_empty() {
            self.lists.remove(&cell);
        } else {
            self.lists.insert(cell, list);
        }
    }

    /// Takes in what `update` changed in `scene`: rescans the regions of level 0 whose record it
    /// wrote, or whose bricks' references it wrote (a brick that changed has a new reference), and
    /// drops the regions the scene no longer has. The work is the table's slots, a few words a
    /// slot, and the regions it rescans; nothing of a region that did not change is read.
    pub fn apply(&mut self, scene: &Scene, emissive: &[bool], update: &SceneUpdate) -> Applied {
        let (written, refs): (Vec<usize>, Vec<Range<usize>>) = match update {
            SceneUpdate::Rebuilt => {
                let fresh = Self::scan(scene, emissive);
                let emitters_changed = fresh.by_region != self.by_region;
                *self = fresh;
                return Applied {
                    emitters_changed,
                    touched: Touched::Everything,
                };
            }
            SceneUpdate::Patched {
                regions,
                brick_refs_from,
                ..
            } => (
                regions.clone(),
                std::iter::once(*brick_refs_from..usize::MAX).collect(),
            ),
            SceneUpdate::Rings(patch) => (patch.regions.clone(), patch.brick_refs.clone()),
        };
        let mut written = written;
        written.sort_unstable();
        let emits = |material: u32| emissive.get(material as usize).copied().unwrap_or(false);
        let any_emissive = emissive.iter().any(|&e| e);
        let mut touched: Option<([i32; 3], [i32; 3])> = None;
        let mut seen = 0usize;
        let mut rescanned: Vec<([i32; 3], Vec<EmitterBox>)> = Vec::new();
        if let Some(level) = scene.levels.first() {
            for slot in 0..=level.table_mask as usize {
                let at = (level.table_base as usize + slot) * 4;
                let entry = scene.table[at + 3] as usize;
                if entry == 0 {
                    continue;
                }
                let key: [i32; 3] = std::array::from_fn(|a| scene.table[at + a] as i32);
                if self.by_region.contains_key(&key) {
                    seen += 1;
                }
                let record = &scene.regions[(entry - 1) * REGION_WORDS..][..REGION_WORDS];
                let first = record[REGION_FIRST_REF];
                let mut changed = written.binary_search(&(entry - 1)).is_ok();
                if !changed && first & SOLID_FLAG == 0 {
                    let count = record[..16].iter().map(|w| w.count_ones()).sum::<u32>() as usize;
                    let first = first as usize;
                    let next = refs.partition_point(|r| r.end <= first);
                    changed = refs.get(next).is_some_and(|r| r.start < first + count);
                }
                if !changed {
                    continue;
                }
                touched = Some(match touched {
                    None => (key, key),
                    Some((lo, hi)) => (
                        std::array::from_fn(|a| lo[a].min(key[a])),
                        std::array::from_fn(|a| hi[a].max(key[a])),
                    ),
                });
                if any_emissive {
                    let mut boxes = Vec::new();
                    region_boxes(scene, key, entry - 1, &emits, &mut boxes);
                    rescanned.push((key, boxes));
                }
            }
        }
        // Regions the index has and the scene's table no longer does.
        let mut moved: Vec<[i32; 3]> = Vec::new();
        if seen != self.by_region.len() {
            let present = |key: [i32; 3]| {
                scene.levels.first().is_some_and(|level| {
                    let mask = level.table_mask as usize;
                    let mut slot = hash_region(key) as usize & mask;
                    for _ in 0..=mask {
                        let at = (level.table_base as usize + slot) * 4;
                        if scene.table[at + 3] == 0 {
                            return false;
                        }
                        if (0..3).all(|a| scene.table[at + a] as i32 == key[a]) {
                            return true;
                        }
                        slot = (slot + 1) & mask;
                    }
                    false
                })
            };
            moved = self
                .by_region
                .keys()
                .copied()
                .filter(|&k| !present(k))
                .collect();
        }
        let mut dirty: Vec<[i32; 3]> = Vec::new();
        for key in moved {
            self.by_region.remove(&key);
            dirty.push(key);
        }
        for (key, boxes) in rescanned {
            let before = self.by_region.get(&key);
            if before.map(Vec::as_slice).unwrap_or(&[]) == boxes.as_slice() {
                continue;
            }
            if boxes.is_empty() {
                self.by_region.remove(&key);
            } else {
                self.by_region.insert(key, boxes);
            }
            dirty.push(key);
        }
        let mut cells: Vec<[i32; 3]> = dirty.iter().flat_map(|&k| neighbours(k)).collect();
        cells.sort_unstable();
        cells.dedup();
        for cell in cells {
            self.make_list(cell);
        }
        Applied {
            emitters_changed: !dirty.is_empty(),
            touched: touched.map_or(Touched::Nothing, |(lo, hi)| Touched::Regions(lo, hi)),
        }
    }

    /// The buffer of the boxes and lists: at most [`MAX_BOXES`] boxes, the first by region.
    pub fn emitters(&self) -> Emitters {
        let boxes: Vec<EmitterBox> = self
            .by_region
            .values()
            .flatten()
            .copied()
            .take(MAX_BOXES)
            .collect();
        let numbered: HashMap<EmitterBox, usize> =
            boxes.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let lists = self
            .lists
            .iter()
            .filter_map(|(&cell, list)| {
                let indices: Vec<usize> = list
                    .iter()
                    .filter_map(|b| numbered.get(b).copied())
                    .collect();
                (!indices.is_empty()).then_some((cell, indices))
            })
            .collect();
        Emitters::assemble(&boxes, &lists)
    }
}

/// The emitter boxes of the region of level 0 at `key`, whose record is number `record` (from 0).
fn region_boxes(
    scene: &Scene,
    key: [i32; 3],
    record: usize,
    emits: &impl Fn(u32) -> bool,
    boxes: &mut Vec<EmitterBox>,
) {
    let record = &scene.regions[record * REGION_WORDS..][..REGION_WORDS];
    let first_ref = record[REGION_FIRST_REF];
    if first_ref & SOLID_FLAG != 0 {
        // A region that is one Material throughout.
        if emits(first_ref & 0xffff) {
            let min = key.map(|k| k * REGION_VOXELS);
            boxes.push(EmitterBox {
                min,
                max: min.map(|m| m + REGION_VOXELS),
                material: (first_ref & 0xffff) as u16,
            });
        }
        return;
    }
    let mut rank = 0usize;
    for bit in 0..512usize {
        if record[bit / 32] >> (bit % 32) & 1 == 0 {
            continue;
        }
        let entry = scene.brick_refs[first_ref as usize + rank];
        rank += 1;
        let brick: [i32; 3] = [
            key[0] * 8 + (bit & 7) as i32,
            key[1] * 8 + (bit >> 3 & 7) as i32,
            key[2] * 8 + (bit >> 6) as i32,
        ];
        let origin = brick.map(|b| b * 8);
        if entry & SOLID_FLAG != 0 {
            if emits(entry & 0xffff) {
                boxes.push(EmitterBox {
                    min: origin,
                    max: origin.map(|o| o + 8),
                    material: (entry & 0xffff) as u16,
                });
            }
        } else {
            brick_boxes(scene, entry as usize, origin, emits, boxes);
        }
    }
}

/// The emitter boxes of the mixed brick at word `at` of `bricks`, whose first Voxel is `origin`.
fn brick_boxes(
    scene: &Scene,
    at: usize,
    origin: [i32; 3],
    emits: &impl Fn(u32) -> bool,
    out: &mut Vec<EmitterBox>,
) {
    let bricks = &scene.bricks;
    let header = bricks[at + 16];
    let (bits, palette_len) = (header & 0xff, (header >> 8) as usize);
    let palette = at + 17;
    // Most bricks have no emissive Material in their palette: that is all that is read of them.
    let palette_emits =
        (0..palette_len).any(|i| emits((bricks[palette + i / 2] >> ((i % 2) * 16)) & 0xffff));
    if !palette_emits {
        return;
    }
    let indices = palette + palette_len.div_ceil(2);
    // The emitting Voxel's Material, by bit, or 0 where there is none (no Material is 0 and emits
    // by accident: a Voxel's id 0 is air in the occupancy).
    let mut grid = [0u32; 512];
    for (bit, cell) in grid.iter_mut().enumerate() {
        if bricks[at + bit / 32] >> (bit % 32) & 1 == 0 {
            continue;
        }
        // A brick of one palette entry has no indices.
        let position = bit as u32 * bits;
        let index = if bits == 0 {
            0
        } else {
            (bricks[indices + (position / 32) as usize] >> (position % 32)) & ((1 << bits) - 1)
        };
        let material = (bricks[palette + (index / 2) as usize] >> ((index % 2) * 16)) & 0xffff;
        if emits(material) {
            *cell = material + 1;
        }
    }
    // Greedy boxes: from the first Voxel not yet in one (z, then y, then x), as far along x as the
    // Material goes, then as far along y and z as every row of it does.
    for start in 0..512usize {
        if grid[start] == 0 {
            continue;
        }
        let material = grid[start];
        let (x, y, z) = (start & 7, start >> 3 & 7, start >> 6);
        let same = |x: usize, y: usize, z: usize| grid[x | y << 3 | z << 6] == material;
        let mut dx = 1;
        while x + dx < 8 && same(x + dx, y, z) {
            dx += 1;
        }
        let mut dy = 1;
        while y + dy < 8 && (x..x + dx).all(|i| same(i, y + dy, z)) {
            dy += 1;
        }
        let mut dz = 1;
        while z + dz < 8 && (y..y + dy).all(|j| (x..x + dx).all(|i| same(i, j, z + dz))) {
            dz += 1;
        }
        for k in z..z + dz {
            for j in y..y + dy {
                for i in x..x + dx {
                    grid[i | j << 3 | k << 6] = 0;
                }
            }
        }
        let first = [
            origin[0] + x as i32,
            origin[1] + y as i32,
            origin[2] + z as i32,
        ];
        out.push(EmitterBox {
            min: first,
            max: [
                first[0] + dx as i32,
                first[1] + dy as i32,
                first[2] + dz as i32,
            ],
            material: (material - 1) as u16,
        });
    }
}

/// Which Materials of `table` give off light: the ones whose emission is above 0.
pub fn emissive_materials(table: &materials::MaterialTable) -> Vec<bool> {
    let len = table.defs().last().map_or(0, |d| usize::from(d.id) + 1);
    let mut emissive = vec![false; len];
    for d in table.defs() {
        emissive[usize::from(d.id)] =
            d.emissive_intensity > 0.0 && d.emissive.iter().any(|&c| c > 0.0);
    }
    emissive
}

#[cfg(test)]
mod tests {
    use brick_store::{BrickStore, Material, VoxelCoord};

    use super::*;

    const LAMP: Material = Material(7);
    const ROCK: Material = Material(1);

    fn emissive() -> Vec<bool> {
        let mut e = vec![false; 8];
        e[7] = true;
        e
    }

    fn scene_of(voxels: &[([i32; 3], Material)]) -> Scene {
        let mut store = BrickStore::new();
        for &(at, material) in voxels {
            store.set(VoxelCoord(at), Some(material));
        }
        Scene::new(&store, None)
    }

    fn cube(first: [i32; 3], size: i32, material: Material) -> Vec<([i32; 3], Material)> {
        let mut out = Vec::new();
        for x in 0..size {
            for y in 0..size {
                for z in 0..size {
                    out.push(([first[0] + x, first[1] + y, first[2] + z], material));
                }
            }
        }
        out
    }

    #[test]
    fn a_lamp_of_four_voxels_a_side_is_one_box() {
        let scene = scene_of(&cube([3, 3, 3], 4, LAMP));
        let emitters = Emitters::of(&scene, &emissive());
        assert_eq!(
            emitters.boxes(),
            [EmitterBox {
                min: [3, 3, 3],
                max: [7, 7, 7],
                material: 7
            }]
        );
    }

    #[test]
    fn a_lamp_across_two_bricks_is_a_box_in_each_and_rock_makes_none() {
        let mut voxels = cube([6, 2, 2], 4, LAMP);
        voxels.extend(cube([0, 20, 0], 3, ROCK));
        let emitters = Emitters::of(&scene_of(&voxels), &emissive());
        let boxes = emitters.boxes();
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        let volume: i32 = boxes
            .iter()
            .map(|b| (0..3).map(|a| b.max[a] - b.min[a]).product::<i32>())
            .sum();
        assert_eq!(volume, 64);
        assert!(boxes.iter().all(|b| b.material == 7));
    }

    #[test]
    fn an_l_shape_is_a_partition_into_boxes() {
        let mut voxels = cube([0, 0, 0], 3, LAMP);
        voxels.extend(cube([3, 0, 0], 1, LAMP));
        voxels.retain(|&(at, _)| !(at[1] > 0 && at[0] < 2));
        let emitters = Emitters::of(&scene_of(&voxels), &emissive());
        let boxes = emitters.boxes();
        let volume: i32 = boxes
            .iter()
            .map(|b| (0..3).map(|a| b.max[a] - b.min[a]).product::<i32>())
            .sum();
        assert_eq!(volume as usize, voxels.len());
        // None overlap.
        for (i, a) in boxes.iter().enumerate() {
            for b in &boxes[i + 1..] {
                assert!(
                    (0..3).any(|k| a.max[k] <= b.min[k] || b.max[k] <= a.min[k]),
                    "{a:?} overlaps {b:?}"
                );
            }
        }
    }

    #[test]
    fn the_list_of_a_cell_has_the_boxes_in_it_and_the_cells_next_to_it_and_no_others() {
        // A lamp in the cell (0, 0, 0), another two cells over.
        let mut voxels = cube([10, 10, 10], 2, LAMP);
        voxels.extend(cube([2 * 64 + 10, 10, 10], 2, LAMP));
        let emitters = Emitters::of(&scene_of(&voxels), &emissive());
        assert_eq!(emitters.boxes, 2);
        let near = |cell: [i32; 3]| -> Vec<[i32; 3]> {
            emitters.list(cell).iter().map(|b| b.min).collect()
        };
        assert_eq!(near([0, 0, 0]), [[10, 10, 10]]);
        assert_eq!(near([-1, -1, -1]), [[10, 10, 10]]);
        assert_eq!(near([1, 0, 0]).len(), 2, "{:?}", near([1, 0, 0]));
        assert_eq!(near([-2, 0, 0]), Vec::<[i32; 3]>::new());
        assert_eq!(near([3, 0, 0]), [[2 * 64 + 10, 10, 10]]);
    }

    #[test]
    fn a_list_is_cut_to_the_boxes_nearest_the_cell_centre() {
        // Twelve separate lamps in one cell: the list has the eight nearest the centre (32, 32, 32).
        let mut voxels = Vec::new();
        for i in 0..12 {
            voxels.push(([i * 5, 32, 32], LAMP));
        }
        let emitters = Emitters::of(&scene_of(&voxels), &emissive());
        let list = emitters.list([0, 0, 0]);
        assert_eq!(list.len(), MAX_PER_CELL);
        let farthest = list.iter().map(|b| (b.min[0] - 32).abs()).max().unwrap();
        let skipped_nearest = emitters
            .boxes()
            .iter()
            .filter(|b| !list.contains(b))
            .map(|b| (b.min[0] - 32).abs())
            .min()
            .unwrap();
        assert!(farthest <= skipped_nearest);
    }

    #[test]
    fn no_emissive_material_and_no_scene_make_an_empty_buffer_a_shader_can_read() {
        assert_eq!(
            Emitters::of(&Scene::default(), &emissive()),
            Emitters::none()
        );
        let none = Emitters::of(&scene_of(&cube([0, 0, 0], 4, LAMP)), &[false; 8]);
        assert_eq!(none, Emitters::none());
        assert_eq!(none.boxes, 0);
        assert!(none.words.len() >= 8);
        assert!(none.list([0, 0, 0]).is_empty());
    }

    #[test]
    fn the_same_scene_gives_the_same_buffer() {
        let voxels = cube([5, 5, 5], 3, LAMP);
        let a = Emitters::of(&scene_of(&voxels), &emissive());
        let b = Emitters::of(&scene_of(&voxels), &emissive());
        assert_eq!(a, b);
    }

    /// An update through the index and a scan of all of level 0 agree, whatever the edits were.
    fn edit_and_check(
        store: &mut BrickStore,
        scene: &mut Scene,
        index: &mut EmitterIndex,
        edits: &[([i32; 3], Option<Material>)],
    ) -> Applied {
        let mut changed = std::collections::BTreeSet::new();
        for &(at, material) in edits {
            changed.insert(VoxelCoord(at).brick());
            store.set(VoxelCoord(at), material);
        }
        let changed: Vec<_> = changed.into_iter().collect();
        let update = scene.update(store, &changed).unwrap();
        let applied = index.apply(scene, &emissive(), &update);
        assert_eq!(
            index.emitters(),
            Emitters::of(scene, &emissive()),
            "{update:?}"
        );
        applied
    }

    #[test]
    fn an_update_takes_in_the_regions_it_changed_and_gives_what_a_full_scan_does() {
        let mut store = BrickStore::new();
        for (at, m) in cube([0, 0, 0], 4, ROCK)
            .into_iter()
            .chain(cube([300, 0, 0], 4, ROCK))
        {
            store.set(VoxelCoord(at), Some(m));
        }
        let mut scene = Scene::new(&store, None);
        let mut index = EmitterIndex::scan(&scene, &emissive());
        assert_eq!(index.emitters().boxes, 0);
        // A lamp placed, in a region that had only rock.
        let lamp: Vec<_> = cube([10, 10, 10], 2, LAMP)
            .into_iter()
            .map(|(at, m)| (at, Some(m)))
            .collect();
        let applied = edit_and_check(&mut store, &mut scene, &mut index, &lamp);
        assert!(applied.emitters_changed);
        assert_eq!(index.emitters().boxes, 1);
        // Rock dug out elsewhere: the emitters are not touched, and no list is made again.
        let applied = edit_and_check(
            &mut store,
            &mut scene,
            &mut index,
            &[([300, 0, 0], None), ([301, 0, 0], None)],
        );
        assert!(!applied.emitters_changed);
        assert!(
            matches!(applied.touched, Touched::Regions(..)),
            "{applied:?}"
        );
        // A second lamp across a brick boundary, then the first one removed.
        let second: Vec<_> = cube([70, 30, 30], 4, LAMP)
            .into_iter()
            .map(|(at, m)| (at, Some(m)))
            .collect();
        edit_and_check(&mut store, &mut scene, &mut index, &second);
        let gone: Vec<_> = cube([10, 10, 10], 2, LAMP)
            .into_iter()
            .map(|(at, _)| (at, None))
            .collect();
        let applied = edit_and_check(&mut store, &mut scene, &mut index, &gone);
        assert!(applied.emitters_changed);
        assert_eq!(
            index.emitters().boxes,
            8,
            "a lamp across the corner of 8 bricks"
        );
        // A lamp turned to rock: no emitter is left.
        let rock: Vec<_> = cube([70, 30, 30], 4, LAMP)
            .into_iter()
            .map(|(at, _)| (at, Some(ROCK)))
            .collect();
        edit_and_check(&mut store, &mut scene, &mut index, &rock);
        assert_eq!(index.emitters(), Emitters::none());
    }

    #[test]
    fn a_scan_of_an_index_made_by_updates_is_the_same_buffer_as_a_fresh_one() {
        let voxels: Vec<_> = (0..6).flat_map(|i| cube([i * 90, 5, 5], 2, LAMP)).collect();
        let scene = scene_of(&voxels);
        assert_eq!(
            EmitterIndex::scan(&scene, &emissive()).emitters(),
            Emitters::of(&scene, &emissive())
        );
        let rebuilt = EmitterIndex::default()
            .apply(&scene, &emissive(), &SceneUpdate::Rebuilt)
            .emitters_changed;
        assert!(rebuilt);
    }
}
