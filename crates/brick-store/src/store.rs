//! Sparse hash index of bricks keyed by integer brick coordinate.

use std::collections::HashMap;

use crate::brick::{Brick, Material};
use crate::coord::{BrickCoord, VoxelCoord};

/// The Planet's bricks. A brick that is absent reads as air.
///
/// The index is a `HashMap`, whose iteration order is random per process, so it is never
/// exposed: use [`Self::sorted_coords`] wherever order matters.
#[derive(Clone, Debug, Default)]
pub struct BrickStore {
    bricks: HashMap<BrickCoord, Brick>,
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

    /// Stores `brick` at `coord`, replacing any brick there.
    pub fn insert(&mut self, coord: BrickCoord, brick: Brick) {
        self.bricks.insert(coord, brick);
    }

    /// Removes the brick at `coord`, which then reads as air.
    pub fn remove(&mut self, coord: BrickCoord) -> Option<Brick> {
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
        self.bricks
            .entry(brick)
            .or_insert(Brick::AIR)
            .set(coord.local(), voxel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROCK: Material = Material(3);

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
}
