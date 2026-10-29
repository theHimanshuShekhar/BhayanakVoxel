//! Applying an Edit to the Planet's Voxels. This is the only code that changes them: the server
//! calls it for each Edit it accepts, and a client calls it for the Edits it predicts and for
//! the ones the server sends. All of them must end with identical bricks (ADR 0003), so
//! everything here is integer maths in a fixed order, and `clippy.toml` bans what is not.

use brick_store::{BRICK_SIZE, BRICK_VOXELS, Brick, BrickCoord, BrickStore};
use net::{Brush, EditMode, EditOp, RejectReason};
use planet_gen::{CORE_RADIUS, ROCK};

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
#[derive(Debug, Default)]
pub struct Undo(Vec<(BrickCoord, Option<Brick>)>);

impl Undo {
    /// The bricks the Edit changed, in ascending order.
    pub fn bricks(&self) -> impl Iterator<Item = BrickCoord> + '_ {
        self.0.iter().map(|&(coord, _)| coord)
    }

    /// Puts the bricks back as they were. The Edit must be the latest change to them.
    pub fn revert(self, store: &mut BrickStore) {
        for (coord, before) in self.0 {
            match before {
                Some(brick) => store.insert(coord, brick),
                None => {
                    store.remove(coord);
                }
            }
        }
    }
}

/// Applies `op` to `store`. The Edit should have passed [`validate`].
///
/// A dig removes every Voxel whose coordinate is within the radius of the centre, measured by
/// squared integer distance. The Planet's core is not in the store (see
/// [`planet_gen::generate_planet`]), so a brick of it that the dig touches is first stored as
/// the rock it is. Bricks that end up uniform collapse to a flag, and one that ends up all air
/// stays in the store as air, so that the core does not show through where it was dug out.
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
                if before.is_none() && brick.within_ball(CORE_RADIUS) {
                    store.insert(brick, Brick::solid(ROCK));
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
                    undo.0.push((brick, before));
                } else if before.is_none() {
                    // Nothing was dug here, so a core brick stored above goes back to implied.
                    store.remove(brick);
                }
            }
        }
    }
    undo
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::{Material, VoxelCoord};

    const STONE: Material = Material(7);

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
        let untouched = BrickStore::new();
        let mut dug = BrickStore::new();
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
        let mut store = BrickStore::new();
        let undo = dig(&mut store, [0, 0, 0], 3);
        assert!(!store.is_empty(), "the core is dug into");
        undo.revert(&mut store);
        assert!(store.is_empty());
    }

    #[test]
    fn digging_into_the_core_stores_the_rock_that_is_there_and_digs_it() {
        let mut store = BrickStore::new();
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
        let mut store = BrickStore::new();
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
        let mut store = BrickStore::new();
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
}
