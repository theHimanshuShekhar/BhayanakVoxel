//! The Planet's bricks packed for the GPU: a shallow sparse tree over bricks.
//!
//! Three levels, each found from the one above:
//!
//! 1. **Regions** are 8×8×8 bricks (64³ Voxels). They are found by hashing the region's integer
//!    coordinate into `table`, an open-addressed hash table, so only regions that hold a brick
//!    cost anything and the Planet's extent never matters.
//! 2. **Bricks** (8³ Voxels). A region is 17 words in `regions`: a 512-bit mask of which of its
//!    bricks exist (bit `x | y << 3 | z << 6`), then the offset of its first entry in
//!    `brick_refs`. A brick's entry is at that offset plus the number of lower mask bits set.
//!    A region that is one Material throughout has `SOLID_FLAG | Material` there instead, with
//!    no entries and an all-ones mask: the Planet's core is such regions.
//! 3. **Voxels.** An entry in `brick_refs` is either `SOLID_FLAG | Material` for a brick that
//!    is one Material throughout, which needs no more storage, or the offset in `bricks` of a
//!    mixed brick:
//!
//!    | words      | content                                                              |
//!    |------------|----------------------------------------------------------------------|
//!    | 0..16      | 512-bit occupancy mask, Voxel `x \| y << 3 \| z << 6` is bit `i % 32` of word `i / 32` |
//!    | 16         | `bits` (bits per palette index) in the low 8 bits, palette length above |
//!    | 17..       | palette: two 16-bit Materials per word, low half first               |
//!    | after that | one `bits`-wide palette index per Voxel, packed from the lowest bit  |
//!
//! `table` holds 4 words per slot: the region's coordinate (3 × i32 as u32), then its index in
//! `regions` plus one, or 0 for an empty slot.
//!
//! Voxel and region coordinates are absolute `i32` Planet-frame integers on both sides, so
//! nothing in the structure loses precision far from the origin. `origin` and `dims` bound the
//! occupied regions, so a ray from far away can jump straight to them.
//!
//! Level of detail, later: each LOD level is another `Scene` of coarser bricks (its own `table`
//! and arrays), traversed by the same code with a larger Voxel size. Nothing here assumes a
//! single level, and nothing is built for it yet.

use std::collections::BTreeMap;

use brick_store::{Brick, BrickCoord, BrickStore, BrickView, Material};

/// Bricks along each edge of a region.
pub const REGION_BRICKS: i32 = 8;
/// Voxels along each edge of a region.
pub const REGION_VOXELS: i32 = 64;
/// Words per region record: 16 mask words and the offset of its first brick entry.
pub const REGION_WORDS: usize = 17;
/// Set in a brick entry for a brick that is one Material throughout.
pub const SOLID_FLAG: u32 = 0x8000_0000;

/// A ball of one Material at the Planet's centre, standing in for the bricks inside it so that
/// they need not be stored. A brick the store holds wins over the ball.
///
/// A brick is in the ball when every Voxel of it is within `radius` (see
/// [`BrickCoord::within_ball`]).
#[derive(Clone, Copy, Debug)]
pub struct Core {
    pub radius: i32,
    pub material: Material,
}

/// Packed bricks and the index over them. Built by [`Scene::new`], uploaded as-is.
#[derive(Debug, Default)]
pub struct Scene {
    /// Voxel coordinate of the scene-space origin: a multiple of [`REGION_VOXELS`].
    pub origin: [i32; 3],
    /// Extent in regions along each axis. Zero when there are no bricks.
    pub dims: [u32; 3],
    pub table: Vec<u32>,
    pub regions: Vec<u32>,
    pub brick_refs: Vec<u32>,
    pub bricks: Vec<u32>,
}

/// Mixes a region coordinate into a table slot. The WGSL `hash_region` is the same function.
pub fn hash_region(key: [i32; 3]) -> u32 {
    let [x, y, z] = key.map(|k| k as u32);
    let mut h =
        x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ z.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^ (h >> 12)
}

/// What one region holds while the scene is being built.
#[derive(Default)]
struct Region<'a> {
    /// Set when the whole region is one Material and `bricks` is empty.
    uniform: Option<Material>,
    /// (mask bit, brick); a `None` brick is the core's.
    bricks: Vec<(usize, Option<&'a Brick>)>,
    /// Mask bits of bricks the store holds as air, which the core must not fill.
    air: Vec<usize>,
}

fn region_of(brick: BrickCoord) -> [i32; 3] {
    brick.0.map(|c| c.div_euclid(REGION_BRICKS))
}

/// A brick's bit in its region's mask.
fn bit_of(brick: BrickCoord) -> usize {
    let [x, y, z] = brick.0.map(|c| c.rem_euclid(REGION_BRICKS) as usize);
    x | y << 3 | z << 6
}

/// Fills in the bricks of `core` that the store does not hold.
fn add_core<'a>(by_region: &mut BTreeMap<[i32; 3], Region<'a>>, core: Core) {
    // Regions that can touch the ball: its bounding cube, in regions, rounded out.
    let reach = core.radius.div_euclid(REGION_VOXELS) + 1;
    for x in -reach..=reach {
        for y in -reach..=reach {
            for z in -reach..=reach {
                let key = [x, y, z];
                let first = BrickCoord(key.map(|k| k * REGION_BRICKS));
                // The ball is convex, so a region is inside it when its eight corner bricks are.
                let corner = |i: usize| {
                    BrickCoord(std::array::from_fn(|a| {
                        first.0[a] + (i >> a & 1) as i32 * (REGION_BRICKS - 1)
                    }))
                };
                let inside = (0..8).all(|i| corner(i).within_ball(core.radius));
                // A region the ball does not touch has all eight corners beyond it by at least a
                // region's diagonal; skip those without testing every brick.
                if !inside
                    && !(0..8).any(|i| corner(i).within_ball(core.radius + 2 * REGION_VOXELS))
                {
                    continue;
                }
                let region = by_region.entry(key).or_default();
                if inside && region.bricks.is_empty() && region.air.is_empty() {
                    region.uniform = Some(core.material);
                    continue;
                }
                let held: Vec<usize> = region
                    .bricks
                    .iter()
                    .map(|&(bit, _)| bit)
                    .chain(region.air.iter().copied())
                    .collect();
                for bit in (0..512).filter(|bit| !held.contains(bit)) {
                    let brick = BrickCoord([
                        first.0[0] + (bit & 7) as i32,
                        first.0[1] + (bit >> 3 & 7) as i32,
                        first.0[2] + (bit >> 6) as i32,
                    ]);
                    if brick.within_ball(core.radius) {
                        region.bricks.push((bit, None));
                    }
                }
            }
        }
    }
}

impl Scene {
    /// Packs every brick in `store` and, if given, the `core`. The same input always gives the
    /// same buffers.
    pub fn new(store: &BrickStore, core: Option<Core>) -> Scene {
        // Group bricks by region. BTreeMap and sorted coordinates keep the output deterministic.
        // A brick is `None` where the core stands in for it.
        let mut by_region: BTreeMap<[i32; 3], Region<'_>> = BTreeMap::new();
        for coord in store.sorted_coords() {
            let brick = store.brick(coord).expect("sorted_coords lists held bricks");
            let region = by_region.entry(region_of(coord)).or_default();
            if brick.is_air() {
                region.air.push(bit_of(coord));
            } else {
                region.bricks.push((bit_of(coord), Some(brick)));
            }
        }
        if let Some(core) = core {
            add_core(&mut by_region, core);
        }
        // Regions that ended up with nothing in them, such as one of only stored air.
        by_region.retain(|_, region| region.uniform.is_some() || !region.bricks.is_empty());
        let Some(first) = by_region.keys().next() else {
            return Scene::default();
        };
        let (mut min, mut max) = (*first, *first);
        for region in by_region.keys() {
            for axis in 0..3 {
                min[axis] = min[axis].min(region[axis]);
                max[axis] = max[axis].max(region[axis]);
            }
        }

        let mut scene = Scene {
            origin: min.map(|m| m * REGION_VOXELS),
            dims: [0, 1, 2].map(|axis| (max[axis] - min[axis] + 1) as u32),
            ..Scene::default()
        };
        let slots = (by_region.len() * 2).next_power_of_two().max(2);
        scene.table = vec![0; slots * 4];
        for (index, (key, region)) in by_region.into_iter().enumerate() {
            if let Some(material) = region.uniform {
                scene.regions.extend([u32::MAX; 16]);
                scene.regions.push(SOLID_FLAG | u32::from(material.0));
            } else {
                let mut bricks = region.bricks;
                bricks.sort_by_key(|&(bit, _)| bit);
                let mut mask = [0u32; 16];
                let first_ref = scene.brick_refs.len() as u32;
                for (bit, brick) in bricks {
                    mask[bit / 32] |= 1 << (bit % 32);
                    let view = brick.map_or_else(
                        || {
                            BrickView::Solid(
                                core.expect("only the core adds absent bricks").material,
                            )
                        },
                        Brick::view,
                    );
                    let entry = scene.pack_brick(view);
                    scene.brick_refs.push(entry);
                }
                scene.regions.extend(mask);
                scene.regions.push(first_ref);
            }
            scene.insert_region(key, index as u32);
        }
        scene
    }

    /// Appends `view`'s data and returns its entry for `brick_refs`.
    fn pack_brick(&mut self, view: BrickView<'_>) -> u32 {
        match view {
            BrickView::Air => unreachable!("air bricks are skipped"),
            BrickView::Solid(material) => SOLID_FLAG | u32::from(material.0),
            BrickView::Mixed {
                occupancy,
                palette,
                bits,
                indices,
            } => {
                let offset = self.bricks.len() as u32;
                for word in occupancy {
                    self.bricks.extend([*word as u32, (*word >> 32) as u32]);
                }
                self.bricks.push(bits | (palette.len() as u32) << 8);
                for pair in palette.chunks(2) {
                    let low = u32::from(pair[0].0);
                    let high = pair.get(1).map_or(0, |m| u32::from(m.0));
                    self.bricks.push(low | high << 16);
                }
                // The CPU packs indices into bytes from the lowest bit; little-endian words
                // keep that order.
                self.bricks.extend(
                    indices.chunks(4).map(|c| {
                        u32::from_le_bytes(c.try_into().expect("indices are whole words"))
                    }),
                );
                offset
            }
        }
    }

    fn insert_region(&mut self, region: [i32; 3], index: u32) {
        let mask = self.table_mask();
        let mut slot = hash_region(region) & mask;
        while self.table[slot as usize * 4 + 3] != 0 {
            slot = (slot + 1) & mask;
        }
        let at = slot as usize * 4;
        self.table[at..at + 3].copy_from_slice(&region.map(|r| r as u32));
        self.table[at + 3] = index + 1;
    }

    /// Slots in the table, minus one. Slots are a power of two.
    pub fn table_mask(&self) -> u32 {
        (self.table.len() / 4).saturating_sub(1) as u32
    }

    /// Index in `regions` of the region at absolute region coordinate `key`.
    pub fn find_region(&self, key: [i32; 3]) -> Option<usize> {
        let mask = self.table_mask();
        let mut slot = hash_region(key) & mask;
        for _ in 0..=mask {
            let at = slot as usize * 4;
            let index = *self.table.get(at + 3)?;
            if index == 0 {
                return None;
            }
            if self.table[at..at + 3] == key.map(|k| k as u32) {
                return Some(index as usize - 1);
            }
            slot = (slot + 1) & mask;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use brick_store::{Brick, BrickCoord, Material, VoxelCoord};

    use super::*;

    const ROCK: Material = Material(1);
    const SOIL: Material = Material(2);

    #[test]
    fn no_bricks_make_an_empty_scene() {
        let scene = Scene::new(&BrickStore::new(), None);
        assert_eq!(scene.dims, [0, 0, 0]);
        assert_eq!(scene.find_region([0, 0, 0]), None);
    }

    #[test]
    fn origin_and_extent_cover_every_region_including_negative_ones() {
        let mut store = BrickStore::new();
        store.insert(BrickCoord([-1, 0, 0]), Brick::solid(ROCK));
        store.insert(BrickCoord([17, 9, -30]), Brick::solid(ROCK));
        let scene = Scene::new(&store, None);
        // Bricks -1 and 17 are in regions -1 and 2; 0 and 9 in 0 and 1; -30 in -4 (floor).
        assert_eq!(scene.origin, [-64, 0, -256]);
        assert_eq!(scene.dims, [4, 2, 5]);
    }

    #[test]
    fn every_region_is_found_and_empty_places_are_not() {
        let mut store = BrickStore::new();
        let mut regions = Vec::new();
        for i in 0..200 {
            let coord = BrickCoord([i * 8, (i * 5) % 40 * 8, -i * 8 - 1]);
            store.insert(coord, Brick::solid(ROCK));
            regions.push(coord.0.map(|c| c.div_euclid(8)));
        }
        let scene = Scene::new(&store, None);
        let mut found: Vec<_> = regions
            .iter()
            .map(|&r| scene.find_region(r).unwrap())
            .collect();
        found.sort_unstable();
        found.dedup();
        assert_eq!(found.len(), regions.len(), "each region has its own record");
        assert_eq!(scene.find_region([1000, 0, 0]), None);
        assert_eq!(scene.find_region([0, 0, 0]), None);
    }

    #[test]
    fn a_core_is_uniform_regions_inside_and_solid_bricks_where_it_crosses_a_region() {
        let core = Core {
            radius: 120,
            material: ROCK,
        };
        let scene = Scene::new(&BrickStore::new(), Some(core));
        // Region (0, 0, 0) spans 0..64 on each axis: its far corner is 109 from the centre.
        let inner = scene.find_region([0, 0, 0]).unwrap() * REGION_WORDS;
        assert_eq!(scene.regions[inner + 16], SOLID_FLAG | 1);
        assert_eq!(scene.regions[inner], u32::MAX);
        // Region (1, 0, 0) is cut by the ball: some of its bricks are the core's, others absent.
        let edge = scene.find_region([1, 0, 0]).unwrap() * REGION_WORDS;
        assert_ne!(scene.regions[edge + 16] & SOLID_FLAG, SOLID_FLAG);
        let present: u32 = scene.regions[edge..edge + 16]
            .iter()
            .map(|w| w.count_ones())
            .sum();
        assert!(present > 0 && present < 512, "{present}");
        // A region the ball cannot reach is not there at all.
        assert_eq!(scene.find_region([2, 2, 2]), None);
        assert_eq!(scene.find_region([5, 0, 0]), None);
    }

    #[test]
    fn stored_bricks_win_over_the_core() {
        let core = Core {
            radius: 120,
            material: ROCK,
        };
        let mut store = BrickStore::new();
        store.insert(BrickCoord([1, 1, 1]), Brick::solid(SOIL));
        let scene = Scene::new(&store, Some(core));
        // The stored brick sits in a region that would otherwise be uniform: now it is a
        // normal region with the SOIL brick among core bricks.
        let region = scene.find_region([0, 0, 0]).unwrap() * REGION_WORDS;
        assert_eq!(scene.regions[region + 16] & SOLID_FLAG, 0);
        let bit = 1 | 1 << 3 | 1 << 6;
        let (word, shift) = (bit / 32, bit % 32);
        let before: u32 = scene.regions[region..region + word]
            .iter()
            .map(|w| w.count_ones())
            .sum::<u32>()
            + (scene.regions[region + word] & ((1 << shift) - 1)).count_ones();
        let entry = scene.brick_refs[(scene.regions[region + 16] + before) as usize];
        assert_eq!(entry, SOLID_FLAG | 2);
    }

    #[test]
    fn a_stored_air_brick_inside_the_core_stays_air() {
        let core = Core {
            radius: 120,
            material: ROCK,
        };
        let mut store = BrickStore::new();
        store.insert(BrickCoord([1, 1, 1]), Brick::AIR);
        let scene = Scene::new(&store, Some(core));
        let region = scene.find_region([0, 0, 0]).unwrap() * REGION_WORDS;
        assert_eq!(scene.regions[region + 16] & SOLID_FLAG, 0);
        let bit = 1 | 1 << 3 | 1 << 6;
        let mask = &scene.regions[region..region + 16];
        assert_eq!(
            mask[bit / 32] >> (bit % 32) & 1,
            0,
            "the air brick is not listed"
        );
        let present: u32 = mask.iter().map(|w| w.count_ones()).sum();
        assert_eq!(
            present, 511,
            "every other brick of the region is the core's"
        );
        // Stored air outside the core makes no region of its own.
        let mut store = BrickStore::new();
        store.insert(BrickCoord([500, 0, 0]), Brick::AIR);
        assert_eq!(Scene::new(&store, None).dims, [0, 0, 0]);
    }

    #[test]
    fn a_solid_brick_is_a_flagged_entry_with_no_voxel_data() {
        let mut store = BrickStore::new();
        store.insert(BrickCoord([9, 0, 0]), Brick::solid(SOIL));
        let scene = Scene::new(&store, None);
        assert_eq!(scene.brick_refs, [SOLID_FLAG | 2]);
        assert!(scene.bricks.is_empty());
        // Region (1, 0, 0), brick (1, 0, 0) inside it is mask bit 1.
        let region = scene.find_region([1, 0, 0]).unwrap() * REGION_WORDS;
        assert_eq!(scene.regions[region], 0b10);
        assert_eq!(scene.regions[region + 16], 0);
    }

    #[test]
    fn a_mixed_brick_packs_occupancy_palette_and_indices() {
        let mut brick = Brick::AIR;
        brick.set([1, 0, 0], Some(ROCK));
        brick.set([2, 0, 0], Some(SOIL));
        brick.set([0, 0, 4], Some(SOIL)); // Voxel 256: word 8, bit 0.
        let mut store = BrickStore::new();
        store.insert(BrickCoord([0, 0, 0]), brick);
        let scene = Scene::new(&store, None);
        assert_eq!(scene.brick_refs, [0]);
        let b = &scene.bricks;
        assert_eq!(b[0], 0b110);
        assert_eq!(b[8], 1);
        assert_eq!(b[16], 1 | 2 << 8, "1 bit per index, two palette entries");
        assert_eq!(b[17], 1 | 2 << 16, "ROCK then SOIL in one word");
        // 512 one-bit indices are 16 words; voxel 2 is index 1, voxel 256 is index 1.
        assert_eq!(b.len(), 17 + 1 + 16);
        assert_eq!(b[18], 0b100);
        assert_eq!(b[18 + 8], 1);
    }

    #[test]
    fn a_region_lists_its_bricks_in_mask_bit_order() {
        let mut store = BrickStore::new();
        // Inserted out of order: bits 3, 8 (y = 1) and 64 (z = 1) of region (0, 0, 0).
        for (c, m) in [([0, 0, 1], 30), ([3, 0, 0], 10), ([0, 1, 0], 20)] {
            store.insert(BrickCoord(c), Brick::solid(Material(m)));
        }
        let scene = Scene::new(&store, None);
        assert_eq!(
            scene.brick_refs,
            [SOLID_FLAG | 10, SOLID_FLAG | 20, SOLID_FLAG | 30]
        );
        let mask = &scene.regions[..16];
        assert_eq!((mask[0], mask[2]), (1 << 3 | 1 << 8, 1));
    }

    #[test]
    fn building_twice_gives_identical_buffers() {
        let mut store = BrickStore::new();
        for i in 0..50 {
            store.set(VoxelCoord([i * 13, -i * 7, i * 3]), Some(ROCK));
        }
        let (a, b) = (Scene::new(&store, None), Scene::new(&store, None));
        assert_eq!(
            (a.table, a.regions, a.brick_refs, a.bricks),
            (b.table, b.regions, b.brick_refs, b.bricks)
        );
    }
}
