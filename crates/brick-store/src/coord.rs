//! Integer Planet-frame coordinates for Voxels and bricks.

/// Edge length of a brick in Voxels (ADR 0002).
pub const BRICK_SIZE: i32 = 8;

/// Position of a Voxel within its brick, each axis in `0..BRICK_SIZE`.
pub type LocalCoord = [u8; 3];

/// Integer Voxel coordinate in the Planet frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VoxelCoord(pub [i32; 3]);

/// Integer brick coordinate in the Planet frame. Ordered lexicographically by x, then y, then z,
/// which is the order to iterate in wherever order matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BrickCoord(pub [i32; 3]);

impl VoxelCoord {
    /// The brick containing this Voxel. Floors, so negative coordinates land in the brick below.
    pub fn brick(self) -> BrickCoord {
        BrickCoord(self.0.map(|v| v.div_euclid(BRICK_SIZE)))
    }

    /// This Voxel's position inside [`Self::brick`].
    pub fn local(self) -> LocalCoord {
        self.0.map(|v| v.rem_euclid(BRICK_SIZE) as u8)
    }
}

impl BrickCoord {
    /// The Voxel at this brick's minimum corner. Panics, in every build profile, if it overflows `i32`.
    pub fn origin(self) -> VoxelCoord {
        VoxelCoord(self.0.map(|b| {
            b.checked_mul(BRICK_SIZE)
                .expect("brick coordinate out of Voxel range")
        }))
    }

    /// The brick one level coarser that contains this one: each coarse brick covers 2×2×2 of
    /// these. Floors, so negative coordinates land in the brick below.
    pub fn parent(self) -> BrickCoord {
        BrickCoord(self.0.map(|b| b.div_euclid(2)))
    }

    /// The eight bricks one level finer that this brick covers, in octant order: x fastest, then
    /// y, then z.
    pub fn children(self) -> [BrickCoord; 8] {
        std::array::from_fn(|i| {
            let octant = [i & 1, i >> 1 & 1, i >> 2].map(|bit| bit as i32);
            BrickCoord([0, 1, 2].map(|a| 2 * self.0[a] + octant[a]))
        })
    }

    /// True when every Voxel of this brick lies within `radius` Voxels of the Planet's centre.
    /// A Voxel is measured at its minimum corner, the point its coordinate names.
    pub fn within_ball(self, radius: i32) -> bool {
        self.within_ball_at(0, radius)
    }

    /// [`Self::within_ball`] for a brick of mip `level`, whose Voxels are `2^level` times the
    /// size of level 0's. The ball's radius and the test are still in level 0 Voxels, so the
    /// answer for a coarse brick is the answer for every level 0 brick under it.
    pub fn within_ball_at(self, level: usize, radius: i32) -> bool {
        // Level 0 Voxels along one edge of this brick.
        let size = i64::from(BRICK_SIZE) << level;
        // Unsigned, since the squares of three coordinates near `i32::MAX` overflow an `i64`.
        let far_sq: u128 = self
            .0
            .map(|b| {
                let o = i64::from(b) * size;
                let far = o.unsigned_abs().max((o + size - 1).unsigned_abs());
                u128::from(far) * u128::from(far)
            })
            .iter()
            .sum();
        far_sq <= u128::from(i64::from(radius).unsigned_abs().pow(2))
    }

    /// The Voxel at `local` inside this brick.
    pub fn voxel(self, local: LocalCoord) -> VoxelCoord {
        let o = self.origin().0;
        VoxelCoord([
            o[0] + i32::from(local[0]),
            o[1] + i32::from(local[1]),
            o[2] + i32::from(local[2]),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_voxels_map_to_bricks_by_division() {
        assert_eq!(VoxelCoord([0, 7, 8]).brick(), BrickCoord([0, 0, 1]));
        assert_eq!(VoxelCoord([17, 16, 23]).brick(), BrickCoord([2, 2, 2]));
    }

    #[test]
    fn negative_voxels_floor_into_the_brick_below() {
        assert_eq!(VoxelCoord([-1, -8, -9]).brick(), BrickCoord([-1, -1, -2]));
        assert_eq!(VoxelCoord([-1, -8, -9]).local(), [7, 0, 7]);
    }

    #[test]
    fn local_is_always_in_range_and_round_trips() {
        for x in -20..20 {
            let v = VoxelCoord([x, -x, x * 3]);
            let local = v.local();
            assert!(local.iter().all(|&l| i32::from(l) < BRICK_SIZE));
            assert_eq!(v.brick().voxel(local), v);
        }
    }

    #[test]
    fn brick_origin_is_its_minimum_voxel() {
        assert_eq!(BrickCoord([-2, 0, 3]).origin(), VoxelCoord([-16, 0, 24]));
    }

    #[test]
    fn extreme_coordinates_do_not_overflow() {
        assert_eq!(
            VoxelCoord([i32::MIN, i32::MAX, 0]).brick(),
            BrickCoord([i32::MIN / 8, i32::MAX / 8, 0])
        );
    }

    #[test]
    #[should_panic(expected = "out of Voxel range")]
    fn an_origin_beyond_the_voxel_range_panics_in_every_profile() {
        BrickCoord([i32::MAX / 8 + 1, 0, 0]).origin();
    }

    #[test]
    #[should_panic(expected = "out of Voxel range")]
    fn a_very_negative_origin_panics_in_every_profile() {
        BrickCoord([0, i32::MIN / 8 - 1, 0]).origin();
    }

    #[test]
    fn a_brick_is_within_a_ball_only_when_its_farthest_voxel_is() {
        // Farthest Voxel of brick 0 is (7, 7, 7): 147 squared. Of brick -1 it is (-8, -8, -8): 192.
        assert!(!BrickCoord([0, 0, 0]).within_ball(12));
        assert!(BrickCoord([0, 0, 0]).within_ball(13));
        assert!(!BrickCoord([-1, -1, -1]).within_ball(13));
        assert!(BrickCoord([-1, -1, -1]).within_ball(14));
        assert!(!BrickCoord([1000, 0, 0]).within_ball(1500));
    }

    #[test]
    fn within_ball_does_not_overflow_at_the_edge_of_the_voxel_range() {
        assert!(!BrickCoord([i32::MAX / 8; 3]).within_ball(i32::MAX));
        assert!(BrickCoord([0; 3]).within_ball(i32::MAX));
    }

    #[test]
    fn a_parent_floors_so_negative_bricks_pair_up_across_zero_correctly() {
        assert_eq!(BrickCoord([0, 1, 2]).parent(), BrickCoord([0, 0, 1]));
        assert_eq!(BrickCoord([-1, -2, -3]).parent(), BrickCoord([-1, -1, -2]));
        assert_eq!(
            BrickCoord([-1, 0, 0]).parent().parent(),
            BrickCoord([-1, 0, 0])
        );
    }

    #[test]
    fn children_are_the_eight_bricks_that_share_the_parent_in_octant_order() {
        let children = BrickCoord([-1, 0, 2]).children();
        assert_eq!(children[0], BrickCoord([-2, 0, 4]));
        assert_eq!(children[1], BrickCoord([-1, 0, 4]));
        assert_eq!(children[2], BrickCoord([-2, 1, 4]));
        assert_eq!(children[4], BrickCoord([-2, 0, 5]));
        assert_eq!(children[7], BrickCoord([-1, 1, 5]));
        assert!(
            children
                .iter()
                .all(|c| c.parent() == BrickCoord([-1, 0, 2]))
        );
    }

    #[test]
    fn a_coarse_brick_is_within_a_ball_only_when_every_level_0_brick_under_it_is() {
        // Level 1 brick 0 covers Voxels 0..=15, so its farthest Voxel is (15, 15, 15): 675 squared.
        assert!(!BrickCoord([0; 3]).within_ball_at(1, 25));
        assert!(BrickCoord([0; 3]).within_ball_at(1, 26));
        // Level 2 brick -1 covers -32..=-1, so its farthest Voxel is (-32, -32, -32): 3072 squared.
        assert!(!BrickCoord([-1; 3]).within_ball_at(2, 55));
        assert!(BrickCoord([-1; 3]).within_ball_at(2, 56));
        for level in 0..4 {
            let coarse = BrickCoord([1, -1, 0]);
            let all_under = descendants(coarse, level).all(|b| b.within_ball(60));
            assert_eq!(coarse.within_ball_at(level, 60), all_under, "level {level}");
        }
    }

    /// Every level 0 brick under `brick`, which is of mip `level`.
    fn descendants(brick: BrickCoord, level: usize) -> Box<dyn Iterator<Item = BrickCoord>> {
        if level == 0 {
            return Box::new(std::iter::once(brick));
        }
        Box::new(
            brick
                .children()
                .into_iter()
                .flat_map(move |c| descendants(c, level - 1)),
        )
    }

    #[test]
    fn bricks_sort_lexicographically() {
        let mut v = vec![
            BrickCoord([1, 0, 0]),
            BrickCoord([0, 5, 0]),
            BrickCoord([0, 0, 9]),
        ];
        v.sort();
        assert_eq!(
            v,
            [
                BrickCoord([0, 0, 9]),
                BrickCoord([0, 5, 0]),
                BrickCoord([1, 0, 0])
            ]
        );
    }
}
