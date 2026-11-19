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

use std::collections::BTreeMap;

use crate::scene::{REGION_FIRST_REF, REGION_VOXELS, REGION_WORDS, SOLID_FLAG, Scene, hash_region};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        Self::pack(&find_boxes(scene, emissive))
    }

    /// The buffer for `boxes`.
    pub fn pack(boxes: &[EmitterBox]) -> Self {
        // Each box's cell, and the cells it can light: its own and the 26 about it.
        let mut cells: BTreeMap<[i32; 3], Vec<(i64, usize)>> = BTreeMap::new();
        for (index, b) in boxes.iter().enumerate() {
            let centre2 = b.centre2();
            let home: [i32; 3] =
                std::array::from_fn(|a| (centre2[a] / 2).div_euclid(i64::from(CELL_VOXELS)) as i32);
            for d in 0..27 {
                let cell: [i32; 3] =
                    std::array::from_fn(|a| home[a] + (d / 3i32.pow(a as u32)) % 3 - 1);
                // Twice the squared distance from the cell's centre, in half Voxels: enough to order
                // the boxes, and whole numbers.
                let from_centre: i64 = (0..3)
                    .map(|a| {
                        let centre = i64::from(cell[a]) * i64::from(CELL_VOXELS) * 2
                            + i64::from(CELL_VOXELS);
                        (centre2[a] - centre).pow(2)
                    })
                    .sum();
                cells.entry(cell).or_default().push((from_centre, index));
            }
        }
        for list in cells.values_mut() {
            list.sort_unstable();
            list.truncate(MAX_PER_CELL);
        }
        let slots = (cells.len() * 2).next_power_of_two().max(2);
        let lists_base = HEADER_WORDS + slots * 4;
        let list_words: usize = cells.values().map(Vec::len).sum();
        let boxes_base = lists_base + list_words;
        let mut words = vec![0u32; boxes_base + boxes.len() * BOX_WORDS];
        words[0] = slots as u32 - 1;
        words[1] = boxes.len() as u32;
        words[2] = lists_base as u32;
        words[3] = boxes_base as u32;
        let mut next = 0usize;
        for (cell, list) in &cells {
            let mut slot = hash_region(*cell) as usize & (slots - 1);
            while words[HEADER_WORDS + slot * 4 + 3] != 0 {
                slot = (slot + 1) & (slots - 1);
            }
            let at = HEADER_WORDS + slot * 4;
            for a in 0..3 {
                words[at + a] = cell[a] as u32;
            }
            words[at + 3] = (list.len() as u32) << 24 | next as u32;
            for &(_, index) in list {
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

/// The emitter boxes of level 0 of `scene`, in the order of the table's slots (so the same scene
/// gives the same boxes), at most [`MAX_BOXES`].
fn find_boxes(scene: &Scene, emissive: &[bool]) -> Vec<EmitterBox> {
    let mut boxes = Vec::new();
    let Some(level) = scene.levels.first() else {
        return boxes;
    };
    if !emissive.iter().any(|&e| e) {
        return boxes;
    }
    let emits = |material: u32| emissive.get(material as usize).copied().unwrap_or(false);
    let slots = level.table_mask as usize + 1;
    for slot in 0..slots {
        let at = (level.table_base as usize + slot) * 4;
        let index = scene.table[at + 3] as usize;
        if index == 0 {
            continue;
        }
        let key: [i32; 3] = std::array::from_fn(|a| scene.table[at + a] as i32);
        let record = &scene.regions[(index - 1) * REGION_WORDS..][..REGION_WORDS];
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
            continue;
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
                brick_boxes(scene, entry as usize, origin, &emits, &mut boxes);
            }
            if boxes.len() >= MAX_BOXES {
                boxes.truncate(MAX_BOXES);
                return boxes;
            }
        }
    }
    boxes
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
}
