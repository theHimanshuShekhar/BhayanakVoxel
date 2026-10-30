//! Applying an Edit to the Planet's Voxels. This is the only code that changes them: the server
//! calls it for each Edit it accepts, and a client calls it for the Edits it predicts and for
//! the ones the server sends. All of them must end with identical bricks (ADR 0003), so
//! everything here is integer maths in a fixed order, and `clippy.toml` bans what is not.

use brick_store::{BRICK_SIZE, BRICK_VOXELS, Brick, BrickCoord, BrickStore, LodRefresh};
use net::{Brush, EditMode, EditOp, RejectReason};

/// Largest sphere a player may dig: 2 m at 10 cm Voxels (issue 15).
pub const MAX_BRUSH_RADIUS: u16 = 20;

/// What the server checks of an Edit before applying it. Reach and rate limits come later.
pub fn validate(op: &EditOp) -> Result<(), RejectReason> {
    let Brush::Sphere { radius } = op.brush;
    if radius > MAX_BRUSH_RADIUS {
        return Err(RejectReason::BrushTooLarge);
    }
    let radius = i32::from(radius);
    let fits = |c: i32| c.checked_sub(radius).is_some() && c.checked_add(radius).is_some();
    if !op.centre.0.into_iter().all(fits) {
        return Err(RejectReason::OutOfRange);
    }
    Ok(())
}

/// What an applied Edit overwrote: the bricks as they were before, so that it can be taken back.
/// It also says which coarse bricks of the store's LOD pyramid the Edit changed.
#[derive(Debug, Default)]
pub struct Undo {
    bricks: Vec<(BrickCoord, Option<Brick>)>,
    lod: LodRefresh,
}

impl Undo {
    /// The bricks the Edit changed, in ascending order.
    pub fn bricks(&self) -> impl Iterator<Item = BrickCoord> + '_ {
        self.bricks.iter().map(|&(coord, _)| coord)
    }

    /// What the Edit did to the store's coarse levels: [`LodRefresh::changed`] is the coarse
    /// bricks that read differently afterwards, and so the ones to upload again.
    pub fn lod(&self) -> &LodRefresh {
        &self.lod
    }

    /// Puts the bricks back as they were, and the store's coarse levels with them, and says
    /// which coarse bricks that changed. The Edit must be the latest change to them.
    pub fn revert(self, store: &mut BrickStore) -> LodRefresh {
        let changed: Vec<_> = self.bricks().collect();
        for (coord, before) in self.bricks {
            match before {
                Some(brick) => store.insert(coord, brick),
                None => {
                    store.remove(coord);
                }
            }
        }
        store.refresh_lod(changed)
    }
}

/// Applies `op` to `store`. The Edit should have passed [`validate`].
///
/// A dig removes every Voxel whose coordinate is within the radius of the centre, measured by
/// squared integer distance. The Planet's core is not in the store (see
/// [`planet_gen::generate_planet`]); it is what the store's [`brick_store::BrickSource`] says a
/// brick that is not held is. So a brick that the dig touches is first stored as that, if it is
/// not air. Bricks that end up uniform collapse to a flag, and one that ends up all air stays in
/// the store as air, so that the core does not show through where it was dug out.
///
/// Every coarse level of the store's LOD pyramid, if it has one, is brought up to date before
/// this returns, remaking only the coarse bricks over the bricks that changed. The returned
/// [`Undo::lod`] lists the coarse bricks whose content changed.
pub fn apply(store: &mut BrickStore, op: &EditOp) -> Undo {
    let EditMode::Dig = op.mode;
    let Brush::Sphere { radius } = op.brush;
    let radius = i64::from(radius);
    let centre = op.centre.0.map(i64::from);
    // The bricks that the sphere's bounding cube touches, clamped to the Voxel range.
    let bound = |sign: i64| {
        [0, 1, 2].map(|a| {
            let voxel = (centre[a] + sign * radius).clamp(i64::from(i32::MIN), i64::from(i32::MAX));
            (voxel as i32).div_euclid(BRICK_SIZE)
        })
    };
    let (first, last) = (bound(-1), bound(1));

    let mut undo = Undo::default();
    for x in first[0]..=last[0] {
        for y in first[1]..=last[1] {
            for z in first[2]..=last[2] {
                let brick = BrickCoord([x, y, z]);
                let in_sphere = |local: [u8; 3]| {
                    let voxel = brick.voxel(local).0;
                    let dist_sq: i64 = [0, 1, 2]
                        .map(|a| (i64::from(voxel[a]) - centre[a]).pow(2))
                        .iter()
                        .sum();
                    dist_sq <= radius * radius
                };
                let before = store.brick(brick).cloned();
                if before.is_none() {
                    let implied = store.brick_at(0, brick).into_owned();
                    if !implied.is_air() {
                        store.insert(brick, implied);
                    }
                }
                let mut changed = false;
                for i in 0..BRICK_VOXELS {
                    let local = [(i & 7) as u8, (i >> 3 & 7) as u8, (i >> 6) as u8];
                    let voxel = brick.voxel(local);
                    if in_sphere(local) && store.get(voxel).is_some() {
                        store.set(voxel, None);
                        changed = true;
                    }
                }
                if changed {
                    undo.bricks.push((brick, before));
                } else if before.is_none() {
                    // Nothing was dug here, so a core brick stored above goes back to implied.
                    store.remove(brick);
                }
            }
        }
    }
    undo.lod = store.refresh_lod(undo.bricks());
    undo
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::{Interior, LodRefresh, Material, VoxelCoord};
    use planet_gen::{CORE, ROCK};

    const STONE: Material = Material(7);

    /// An empty store whose bricks that are not held read as the Planet's core.
    fn core_store() -> BrickStore {
        let mut store = BrickStore::new();
        store.set_source(CORE);
        store
    }

    fn dig(store: &mut BrickStore, centre: [i32; 3], radius: u16) -> Undo {
        apply(store, &EditOp::dig_sphere(VoxelCoord(centre), radius))
    }

    /// A store with a solid cube of STONE from `lo` to `hi` inclusive.
    fn cube(lo: [i32; 3], hi: [i32; 3]) -> BrickStore {
        let mut store = BrickStore::new();
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    store.set(VoxelCoord([x, y, z]), Some(STONE));
                }
            }
        }
        store
    }

    fn dug(store: &BrickStore, lo: [i32; 3], hi: [i32; 3]) -> usize {
        let mut count = 0;
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    count += usize::from(store.get(VoxelCoord([x, y, z])).is_none());
                }
            }
        }
        count
    }

    #[test]
    fn a_sphere_of_radius_zero_is_the_centre_voxel_alone() {
        let mut store = cube([0; 3], [4; 3]);
        dig(&mut store, [2, 2, 2], 0);
        assert_eq!(dug(&store, [0; 3], [4; 3]), 1);
        assert_eq!(store.get(VoxelCoord([2, 2, 2])), None);
    }

    #[test]
    fn a_sphere_covers_exactly_the_voxels_within_the_radius() {
        // Counts of integer points with x^2 + y^2 + z^2 <= r^2, worked out independently.
        for (radius, count) in [(1, 7), (2, 33), (3, 123), (10, 4169)] {
            let r = i32::from(radius);
            let mut store = cube([-r - 2; 3], [r + 2; 3]);
            dig(&mut store, [0; 3], radius);
            assert_eq!(
                dug(&store, [-r - 2; 3], [r + 2; 3]),
                count,
                "radius {radius}"
            );
        }
    }

    #[test]
    fn the_rim_is_decided_by_exact_squared_distance() {
        let mut store = cube([-8; 3], [8; 3]);
        dig(&mut store, [0; 3], 5);
        // 3^2 + 4^2 + 0 = 25 is on the sphere; 3^2 + 4^2 + 1 = 26 is outside it.
        assert_eq!(store.get(VoxelCoord([3, 4, 0])), None);
        assert_eq!(store.get(VoxelCoord([-3, 0, -4])), None);
        assert_eq!(store.get(VoxelCoord([3, 4, 1])), Some(STONE));
        assert_eq!(store.get(VoxelCoord([5, 1, 0])), Some(STONE));
        assert_eq!(store.get(VoxelCoord([5, 0, 0])), None);
    }

    #[test]
    fn a_sphere_straddling_brick_boundaries_and_zero_is_the_same_everywhere() {
        // Centred on a corner where eight bricks meet, with negative coordinates on one side.
        let mut store = cube([-12; 3], [11; 3]);
        dig(&mut store, [-1, -1, -1], 6);
        let mut want = 0;
        for x in -12..12i32 {
            for y in -12..12i32 {
                for z in -12..12i32 {
                    let inside = (x + 1).pow(2) + (y + 1).pow(2) + (z + 1).pow(2) <= 36;
                    want += usize::from(inside);
                    assert_eq!(
                        store.get(VoxelCoord([x, y, z])).is_none(),
                        inside,
                        "{x} {y} {z}"
                    );
                }
            }
        }
        assert_eq!(want, 925);
    }

    #[test]
    fn digging_air_changes_nothing_and_allocates_nothing() {
        let mut store = BrickStore::new();
        let undo = dig(&mut store, [4000, -50, 7], 10);
        assert!(store.is_empty());
        assert_eq!(undo.bricks().count(), 0);
    }

    #[test]
    fn digging_out_a_whole_brick_collapses_it_to_air() {
        let mut store = BrickStore::new();
        store.insert(BrickCoord([0, 0, 0]), Brick::solid(STONE));
        // Radius 7 about the brick's middle reaches every corner of it: 3.5^2 * 3 < 7^2.
        dig(&mut store, [3, 3, 3], 7);
        let brick = store.brick(BrickCoord([0, 0, 0])).unwrap();
        assert!(brick.is_air() && brick.is_uniform());
    }

    #[test]
    fn a_core_brick_dug_out_completely_hashes_differently_from_an_untouched_one() {
        // In the core a brick that is not stored is rock, so air held there is a real change.
        let untouched = core_store();
        let mut dug = core_store();
        dig(&mut dug, [3, 3, 3], 7);
        let brick = dug.brick(BrickCoord([0, 0, 0])).unwrap();
        assert!(brick.is_air(), "the brick is held, as air");
        assert_ne!(dug.content_hash(), untouched.content_hash());
        // Taking the brick out of the store, as a faulty Edit might, is seen as well.
        let mut deleted = dug.clone();
        deleted.remove(BrickCoord([0, 0, 0]));
        assert_ne!(dug.content_hash(), deleted.content_hash());
    }

    #[test]
    fn digging_part_of_a_brick_leaves_it_mixed_and_the_rest_intact() {
        let mut store = BrickStore::new();
        store.insert(BrickCoord([-1, -1, -1]), Brick::solid(STONE));
        dig(&mut store, [-1, -1, -1], 1);
        let brick = store.brick(BrickCoord([-1, -1, -1])).unwrap();
        assert!(!brick.is_uniform());
        assert_eq!(store.get(VoxelCoord([-1, -1, -1])), None);
        assert_eq!(store.get(VoxelCoord([-8, -8, -8])), Some(STONE));
    }

    #[test]
    fn undo_lists_only_the_bricks_that_changed_and_reverting_restores_them() {
        let mut store = cube([0; 3], [15; 3]);
        let before = store.content_hash();
        // Reaches the bricks around (8, 8, 8) but only digs where there is something.
        let undo = dig(&mut store, [7, 7, 7], 3);
        assert_eq!(undo.bricks().count(), 8);
        assert!(undo.bricks().is_sorted());
        assert_ne!(store.content_hash(), before);
        undo.revert(&mut store);
        assert_eq!(store.content_hash(), before);
    }

    #[test]
    fn reverting_removes_bricks_that_the_edit_had_to_create() {
        let mut store = core_store();
        let undo = dig(&mut store, [0, 0, 0], 3);
        assert!(!store.is_empty(), "the core is dug into");
        undo.revert(&mut store);
        assert!(store.is_empty());
    }

    #[test]
    fn a_dig_reads_the_core_from_the_stores_own_source_and_not_from_the_planet_generator() {
        let mut store = BrickStore::new();
        store.set_source(Interior {
            radius: 50,
            material: Material(9),
        });
        dig(&mut store, [0, 0, 0], 3);
        assert_eq!(store.get(VoxelCoord([4, 0, 0])), Some(Material(9)));
        // 200 Voxels out is inside the Planet's core, but not inside this store's.
        let undo = dig(&mut store, [200, 0, 0], 3);
        assert_eq!(undo.bricks().count(), 0);
        assert_eq!(store.get(VoxelCoord([200, 1, 0])), None);
    }

    #[test]
    fn without_a_source_a_brick_that_is_not_held_is_air_and_a_dig_leaves_it_so() {
        let mut store = BrickStore::new();
        let undo = dig(&mut store, [0, 0, 0], 3);
        assert_eq!(undo.bricks().count(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn digging_into_the_core_stores_the_rock_that_is_there_and_digs_it() {
        let mut store = core_store();
        dig(&mut store, [0, 0, 0], 3);
        assert_eq!(store.get(VoxelCoord([0, 0, 0])), None);
        assert_eq!(store.get(VoxelCoord([3, 0, 0])), None);
        assert_eq!(store.get(VoxelCoord([4, 0, 0])), Some(ROCK));
        assert_eq!(store.get(VoxelCoord([-1, -1, -1])), None);
        assert_eq!(store.get(VoxelCoord([-2, -2, -2])), Some(ROCK));
        // The eight bricks around the centre, and no more.
        assert_eq!(store.len(), 8);
    }

    #[test]
    fn a_dig_that_only_grazes_a_core_brick_stores_no_extra_bricks() {
        let mut store = core_store();
        // The sphere's bounding cube reaches brick (1, 1, 1) at (8, 8, 8), which is outside it.
        dig(&mut store, [7, 7, 7], 1);
        assert_eq!(
            store.sorted_coords(),
            [
                BrickCoord([0, 0, 0]),
                BrickCoord([0, 0, 1]),
                BrickCoord([0, 1, 0]),
                BrickCoord([1, 0, 0]),
            ]
        );
    }

    #[test]
    fn an_edit_at_the_edge_of_the_coordinate_range_does_not_overflow() {
        let mut store = core_store();
        for centre in [[i32::MAX; 3], [i32::MIN; 3]] {
            dig(&mut store, centre, 20);
        }
        assert!(store.is_empty());
    }

    #[test]
    fn the_same_edits_in_the_same_order_give_the_same_bricks() {
        let run = || {
            let mut store = cube([-20; 3], [20; 3]);
            for centre in [[0, 0, 0], [5, -3, 9], [-8, -8, 8], [0, 0, 1]] {
                dig(&mut store, centre, 7);
            }
            store.content_hash()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn validate_accepts_up_to_the_largest_brush_and_names_the_reason_otherwise() {
        let op = |centre, radius| EditOp::dig_sphere(VoxelCoord(centre), radius);
        assert_eq!(validate(&op([0; 3], MAX_BRUSH_RADIUS)), Ok(()));
        assert_eq!(
            validate(&op([0; 3], MAX_BRUSH_RADIUS + 1)),
            Err(RejectReason::BrushTooLarge)
        );
        assert_eq!(validate(&op([i32::MAX - 20, 0, 0], 20)), Ok(()));
        assert_eq!(
            validate(&op([i32::MAX - 19, 0, 0], 20)),
            Err(RejectReason::OutOfRange)
        );
        assert_eq!(
            validate(&op([0, i32::MIN + 19, 0], 20)),
            Err(RejectReason::OutOfRange)
        );
    }

    /// Solid STONE bricks from brick `lo` to brick `hi` inclusive, with `levels` coarse levels.
    fn bricks_with_pyramid(lo: i32, hi: i32, levels: usize) -> BrickStore {
        let mut store = BrickStore::new();
        for x in lo..=hi {
            for y in lo..=hi {
                for z in lo..=hi {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(STONE));
                }
            }
        }
        store.build_lod(levels);
        store
    }

    /// The pyramid that `build_lod` makes of `store`'s level 0 bricks and source.
    fn rebuilt(store: &BrickStore) -> BrickStore {
        let mut again = store.clone();
        again.build_lod(store.level_count() - 1);
        again
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

    /// Xorshift64*: the same "random" inputs on every machine.
    struct Dice(u64);

    impl Dice {
        fn below(&mut self, n: u32) -> i32 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as i32 % n as i32
        }
    }

    #[test]
    fn a_crater_shows_at_the_coarsest_level_that_can_resolve_it() {
        // Stone from Voxel -16 to 47 on each axis, around level 5's Voxel 0.
        let mut store = bricks_with_pyramid(-2, 5, 6);
        // A sphere of radius 14 about (8, 8, 8) holds the 16³ cube of Voxels from 0 to 15
        // (its far corner is 8^2 * 3 = 192 <= 196 away), and no larger aligned cube.
        dig(&mut store, [8, 8, 8], 14);
        // Levels 1 to 4 have a coarse Voxel that is all dug out: the cube, at 8, 4, 2 and 1 wide.
        for (n, width) in [(1, 8), (2, 4), (3, 2), (4, 1)] {
            assert_eq!(
                store.voxel_at(n, VoxelCoord([width - 1; 3])),
                None,
                "level {n}"
            );
            assert_eq!(store.voxel_at(n, VoxelCoord([0; 3])), None, "level {n}");
        }
        // Level 5's Voxel is 32 wide, and only one of its eight children is dug out: it is
        // still stone.
        assert_eq!(store.voxel_at(5, VoxelCoord([0; 3])), Some(STONE));
        // The levels see the stone around the crater, too.
        assert_eq!(store.voxel_at(4, VoxelCoord([-1, 0, 0])), Some(STONE));
        assert_eq!(store.voxel_at(4, VoxelCoord([1, 1, 1])), Some(STONE));
    }

    #[test]
    fn every_level_after_an_edit_is_the_one_a_rebuild_from_level_0_would_make() {
        let mut store = bricks_with_pyramid(-2, 2, 5);
        for centre in [[0, 0, 0], [7, -3, 9], [-8, -8, 8], [0, 0, 1], [19, 19, 19]] {
            dig(&mut store, centre, 17);
            assert_eq!(
                store.lod_hash(),
                rebuilt(&store).lod_hash(),
                "after {centre:?}"
            );
        }
    }

    #[test]
    fn an_edit_remakes_only_the_ancestors_of_the_bricks_it_changed() {
        let mut store = bricks_with_pyramid(-3, 3, 4);
        // Stone far from the Edits, whose coarse bricks must be left alone.
        for far in [[30, 0, 0], [-40, 25, -9]] {
            store.insert(BrickCoord(far), Brick::solid(STONE));
        }
        store.build_lod(4);

        // Inside one brick: one ancestor on each of the four levels.
        let before = store.clone();
        let undo = dig(&mut store, [4, 4, 4], 3);
        assert_eq!(undo.bricks().count(), 1);
        assert_eq!(undo.lod().remade, 4);
        assert_eq!(undo.lod().changed, differing(&before, &store));
        assert!(undo.lod().changed.contains(&(1, BrickCoord([0; 3]))));
        assert!(
            undo.lod()
                .changed
                .iter()
                .all(|&(_, c)| c == BrickCoord([0; 3]))
        );

        // On the corner where eight bricks meet: bricks -1 and 0 never share a parent, so each
        // level has eight ancestors, 32 in all.
        let before = store.clone();
        let undo = dig(&mut store, [0, 0, 0], 2);
        assert_eq!(undo.bricks().count(), 8);
        assert_eq!(undo.lod().remade, 8 * 4);
        let changed = differing(&before, &store);
        assert!(!changed.is_empty());
        assert_eq!(undo.lod().changed, changed);
        let over = |&(_, c): &(usize, BrickCoord)| c.0.iter().all(|&a| a == 0 || a == -1);
        assert!(changed.iter().all(over), "{changed:?}");
    }

    #[test]
    fn reverting_reports_the_coarse_bricks_it_put_back() {
        let mut store = bricks_with_pyramid(-2, 2, 4);
        let before = store.clone();
        let undo = dig(&mut store, [3, 3, 3], 6);
        let dug = undo.lod().clone();
        assert!(!dug.changed.is_empty());
        let reverted = undo.revert(&mut store);
        assert_eq!(reverted.remade, dug.remade);
        assert_eq!(reverted.changed, dug.changed);
        assert!(differing(&before, &store).is_empty());
    }

    /// A rock planet of 72 Voxels' radius, with a core of rock to 40 that is not held unless
    /// `hold_core`. Every brick not wholly inside the core is held, so its crust is.
    fn rock_planet(hold_core: bool) -> BrickStore {
        let core = Interior {
            radius: 40,
            material: ROCK,
        };
        let mut store = BrickStore::new();
        for i in 0..18 * 18 * 18 {
            let coord = BrickCoord([i % 18 - 9, i / 18 % 18 - 9, i / 324 - 9]);
            if coord.within_ball(72) && (hold_core || !coord.within_ball(core.radius)) {
                store.insert(coord, Brick::solid(ROCK));
            }
        }
        if !hold_core {
            store.set_source(core);
        }
        store.build_lod(8);
        store
    }

    #[test]
    fn digging_into_an_implied_core_makes_the_same_levels_as_digging_a_planet_that_holds_it() {
        let (mut implied, mut whole) = (rock_planet(false), rock_planet(true));
        assert!(whole.len() > implied.len() + 50);
        let digs = [
            ([0, 0, 0], 20),
            ([38, 3, -2], 15),
            ([-41, 41, 0], 12),
            ([0, 0, 57], 20),
            ([0, 0, 0], 1),
            ([60, 0, 0], 20),
        ];
        for (centre, radius) in digs {
            dig(&mut implied, centre, radius);
            dig(&mut whole, centre, radius);
            for level in 0..whole.level_count() {
                for coord in whole.level(level).unwrap().sorted_coords() {
                    assert_eq!(
                        implied.brick_at(level, coord),
                        whole.brick_at(level, coord),
                        "after {centre:?} level {level} {coord:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_edit_in_the_core_is_carried_up_through_the_rock_it_implies() {
        let mut store = core_store();
        store.build_lod(3);
        dig(&mut store, [0, 0, 0], 12);
        // The crater is 25 wide, and so shows at level 2, whose Voxels are 4 wide.
        assert_eq!(store.voxel_at(2, VoxelCoord([0, 0, 0])), None);
        assert_eq!(store.voxel_at(2, VoxelCoord([-1, -1, -1])), None);
        assert_eq!(store.voxel_at(2, VoxelCoord([7, 0, 0])), Some(ROCK));
        assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash());
    }

    #[test]
    fn reverting_an_edit_puts_every_level_back() {
        let mut store = bricks_with_pyramid(-2, 2, 5);
        let hash = store.lod_hash();
        let first = dig(&mut store, [0, 0, 0], 17);
        let second = dig(&mut store, [9, 9, 9], 12);
        assert_ne!(store.lod_hash(), hash);
        second.revert(&mut store);
        first.revert(&mut store);
        assert_eq!(store.lod_hash(), hash);
    }

    #[test]
    fn random_edits_and_reverts_over_an_interior_always_leave_the_levels_a_rebuild_would() {
        // A ball of radius 90, so every coarse brick from level 4 up is over its edge.
        let mut store = BrickStore::new();
        store.set_source(Interior {
            radius: 90,
            material: ROCK,
        });
        store.build_lod(9);
        let bare = (store.content_hash(), store.lod_hash());

        let mut dice = Dice(0x9E37_79B9_7F4A_7C15);
        // Each Edit with the hashes from before it was applied.
        let mut done: Vec<(Undo, (u64, u64))> = Vec::new();
        let (mut digs, mut reverts) = (0, 0);
        for step in 0..120 {
            if !done.is_empty() && dice.below(3) == 0 {
                let (undo, before) = done.pop().unwrap();
                undo.revert(&mut store);
                assert_eq!(
                    (store.content_hash(), store.lod_hash()),
                    before,
                    "revert at step {step}"
                );
                reverts += 1;
            } else {
                let before = (store.content_hash(), store.lod_hash());
                let centre = [0; 3].map(|_| dice.below(221) - 110);
                let undo = dig(&mut store, centre, dice.below(21) as u16);
                done.push((undo, before));
                digs += 1;
            }
            assert_eq!(store.lod_hash(), rebuilt(&store).lod_hash(), "step {step}");
        }
        assert!(digs > 60 && reverts > 20, "{digs} digs, {reverts} reverts");

        // Taking back what is left gives the bare ball again.
        while let Some((undo, before)) = done.pop() {
            undo.revert(&mut store);
            assert_eq!((store.content_hash(), store.lod_hash()), before);
        }
        assert_eq!((store.content_hash(), store.lod_hash()), bare);
    }

    #[test]
    fn an_edit_on_a_store_without_a_pyramid_reports_no_coarse_bricks() {
        let mut store = cube([0; 3], [8; 3]);
        let undo = dig(&mut store, [4, 4, 4], 2);
        assert!(undo.bricks().count() > 0);
        assert_eq!(*undo.lod(), LodRefresh::default());
        assert_eq!(store.level_count(), 1);
        assert_eq!(undo.revert(&mut store), LodRefresh::default());
    }
}
