//! Majority downsampling: how a coarse brick is made from the eight finer bricks it covers
//! (ADR 0002). Integer-only, so every machine builds the same coarse brick. Also what a brick
//! reads as where none is held.

use std::cmp::Reverse;
use std::fmt::Debug;

use crate::brick::{BRICK_VOXELS, Brick, BrickView, Material};
use crate::coord::BrickCoord;

/// What a brick reads as where the store holds none, at any mip level. Without one, that is
/// air. The store asks it for the level 0 bricks that it does not hold, and for the children
/// that a coarse brick is made from when they are not held either.
///
/// A coarse brick reflects the real Planet where the source is exact for the bricks it is made
/// from, which means one of:
/// - the source returns, at every level, what downsampling the real Planet gives there; or
/// - every coarse brick that the source gets wrong has a level 0 brick under it that is held,
///   so that the store makes it from held bricks and never reads the source for it.
///
/// Whether or not it is true, the pyramid is a function of the bricks held at level 0 and of
/// the source alone, so refreshing it after an Edit gives what building it afresh does.
pub trait BrickSource: Debug + Send + Sync {
    /// The brick at `coord` of mip `level`, where none is held.
    fn brick(&self, level: usize, coord: BrickCoord) -> Brick;
}

/// A ball at the Planet's centre made of one Material, so that none of its bricks need be held.
/// A brick wholly inside `radius` Voxels of the centre reads as `material`, and any other as
/// air, at every level.
///
/// That is exact for bricks wholly inside the ball, and for bricks wholly outside it if the
/// Planet has nothing there that is not held. It is not exact for a brick over the ball's edge,
/// which is part rock and part not. So the Planet must hold every level 0 brick that is not
/// wholly inside the ball and not air: its crust. `planet_gen::generate_planet` does. Then each
/// coarse brick over the ball's edge has held bricks under it, and is made from them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interior {
    pub radius: i32,
    pub material: Material,
}

impl BrickSource for Interior {
    fn brick(&self, level: usize, coord: BrickCoord) -> Brick {
        if coord.within_ball_at(level, self.radius) {
            Brick::solid(self.material)
        } else {
            Brick::AIR
        }
    }
}

/// The coarse brick over eight finer ones, given in [`crate::BrickCoord::children`] order.
///
/// Each coarse Voxel covers 2×2×2 Voxels of the finer bricks. It is solid when at least 4 of
/// those 8 are solid, and then takes the most common Material among the solid ones; if several
/// Materials are equally common, the lowest Material ID wins.
pub(crate) fn downsample(children: [&Brick; 8]) -> Brick {
    // Whole bricks of air, or of one Material, are by far the commonest case up the pyramid.
    if children.iter().all(|c| c.is_air()) {
        return Brick::AIR;
    }
    if let BrickView::Solid(material) = children[0].view()
        && children[1..]
            .iter()
            .all(|c| matches!(c.view(), BrickView::Solid(m) if m == material))
    {
        return Brick::solid(material);
    }

    let mut coarse = Brick::AIR;
    for i in 0..BRICK_VOXELS {
        let at = [i & 7, i >> 3 & 7, i >> 6];
        let child = children[at[0] >> 2 | (at[1] >> 2) << 1 | (at[2] >> 2) << 2];
        let mut tally = Tally::new();
        for j in 0..8 {
            // Voxel `2 * at + offset`, which is `& 7` inside the child that holds it.
            let finer = [0, 1, 2].map(|a| ((2 * at[a] + (j >> a & 1)) & 7) as u8);
            if let Some(material) = child.get(finer) {
                tally.add(material);
            }
        }
        if let Some(material) = tally.majority() {
            coarse.set(at.map(|a| a as u8), Some(material));
        }
    }
    coarse
}

/// How many of one coarse Voxel's eight children are made of each Material.
struct Tally {
    counts: [(Material, u8); 8],
    len: usize,
}

impl Tally {
    fn new() -> Self {
        Self {
            counts: [(Material(0), 0); 8],
            len: 0,
        }
    }

    fn add(&mut self, material: Material) {
        match self.counts[..self.len]
            .iter_mut()
            .find(|(m, _)| *m == material)
        {
            Some((_, count)) => *count += 1,
            None => {
                self.counts[self.len] = (material, 1);
                self.len += 1;
            }
        }
    }

    /// The Material the coarse Voxel takes, or `None` when fewer than 4 children are solid.
    fn majority(&self) -> Option<Material> {
        let counts = &self.counts[..self.len];
        let solid: u8 = counts.iter().map(|&(_, n)| n).sum();
        if solid < 4 {
            return None;
        }
        counts
            .iter()
            .max_by_key(|&&(m, n)| (n, Reverse(m)))
            .map(|&(m, _)| m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coord::LocalCoord;

    const ROCK: Material = Material(7);
    const SAND: Material = Material(3);

    fn local(i: usize) -> LocalCoord {
        [(i % 8) as u8, (i / 8 % 8) as u8, (i / 64) as u8]
    }

    /// Downsamples eight copies of `brick`.
    fn of_eight(brick: &Brick) -> Brick {
        downsample([brick; 8])
    }

    /// The 2×2×2 Voxels that make coarse Voxel (0, 0, 0).
    fn cell() -> [LocalCoord; 8] {
        std::array::from_fn(|i| [(i & 1) as u8, (i >> 1 & 1) as u8, (i >> 2) as u8])
    }

    /// Coarse Voxel (0, 0, 0) when its cell holds `voxels` and the other children are air.
    fn coarse_origin(voxels: &[Option<Material>; 8]) -> Option<Material> {
        let mut brick = Brick::AIR;
        for (at, &voxel) in cell().into_iter().zip(voxels) {
            brick.set(at, voxel);
        }
        let air = Brick::AIR;
        downsample([&brick, &air, &air, &air, &air, &air, &air, &air]).get([0, 0, 0])
    }

    #[test]
    fn eight_air_bricks_make_an_air_brick() {
        assert!(of_eight(&Brick::AIR).is_air());
    }

    #[test]
    fn eight_bricks_of_one_material_make_a_uniform_brick_of_it() {
        let coarse = of_eight(&Brick::solid(ROCK));
        assert!(coarse.is_uniform());
        assert!((0..BRICK_VOXELS).all(|i| coarse.get(local(i)) == Some(ROCK)));
    }

    #[test]
    fn eight_solid_bricks_of_different_materials_make_a_mixed_brick() {
        let (rock, sand) = (Brick::solid(ROCK), Brick::solid(SAND));
        let coarse = downsample([&rock, &sand, &rock, &rock, &rock, &rock, &rock, &rock]);
        assert!(!coarse.is_uniform());
        // The second child is the one at x + 1, which fills x in 4..8.
        assert_eq!(coarse.get([3, 0, 0]), Some(ROCK));
        assert_eq!(coarse.get([4, 0, 0]), Some(SAND));
        assert_eq!(coarse.get([7, 3, 3]), Some(SAND));
        assert_eq!(coarse.get([4, 4, 0]), Some(ROCK));
    }

    #[test]
    fn a_child_decides_the_octant_of_the_coarse_brick_it_fills() {
        let (rock, air) = (Brick::solid(ROCK), Brick::AIR);
        // Child 5 is the one at x + 1 and z + 1.
        let coarse = downsample([&air, &air, &air, &air, &air, &rock, &air, &air]);
        for i in 0..BRICK_VOXELS {
            let [x, y, z] = local(i);
            let in_octant = x >= 4 && y < 4 && z >= 4;
            assert_eq!(coarse.get([x, y, z]).is_some(), in_octant, "{x} {y} {z}");
        }
    }

    #[test]
    fn a_coarse_voxel_reads_the_cell_at_double_its_position() {
        // Coarse Voxel (1, 2, 3) of the first child reads Voxels 2..=3, 4..=5 and 6..=7.
        let mut block = Brick::AIR;
        for at in [[2, 4, 6], [3, 4, 6], [2, 5, 6], [3, 5, 6]] {
            block.set(at, Some(ROCK));
        }
        let air = Brick::AIR;
        let coarse = downsample([&block, &air, &air, &air, &air, &air, &air, &air]);
        assert_eq!(coarse.get([1, 2, 3]), Some(ROCK));
        assert_eq!(coarse.get([0, 2, 3]), None);
        assert_eq!(coarse.get([1, 1, 3]), None);
        assert_eq!(coarse.get([1, 2, 2]), None);
    }

    #[test]
    fn four_solid_voxels_are_enough_and_three_are_not() {
        let mut voxels = [None; 8];
        for voxel in &mut voxels[..3] {
            *voxel = Some(ROCK);
        }
        assert_eq!(coarse_origin(&voxels), None, "3 of 8");
        voxels[3] = Some(ROCK);
        assert_eq!(coarse_origin(&voxels), Some(ROCK), "4 of 8");
        voxels[4] = Some(ROCK);
        assert_eq!(coarse_origin(&voxels), Some(ROCK), "5 of 8");
    }

    #[test]
    fn the_most_common_solid_material_wins_and_air_does_not_count_against_it() {
        let (r, s, n) = (Some(ROCK), Some(SAND), None);
        // Seven solid: four sand, three rock.
        assert_eq!(coarse_origin(&[s, s, s, s, r, r, r, n]), Some(SAND));
        // Four solid: three sand, one rock, though air is as common as all of them together.
        assert_eq!(coarse_origin(&[s, s, s, r, n, n, n, n]), Some(SAND));
        // Five solid: rock outnumbers sand, so the lower ID of sand does not help it.
        assert_eq!(coarse_origin(&[r, r, r, s, s, n, n, n]), Some(ROCK));
        // Three solid is a minority however they are split.
        assert_eq!(coarse_origin(&[s, s, r, n, n, n, n, n]), None);
    }

    #[test]
    fn a_tie_between_materials_goes_to_the_lowest_material_id() {
        let (r, s, n) = (Some(ROCK), Some(SAND), None);
        assert!(SAND < ROCK);
        assert_eq!(coarse_origin(&[r, r, r, r, s, s, s, s]), Some(SAND));
        assert_eq!(coarse_origin(&[s, s, s, s, r, r, r, r]), Some(SAND));
        // A tie among exactly the four solid Voxels needed.
        assert_eq!(coarse_origin(&[r, r, s, s, n, n, n, n]), Some(SAND));
    }

    #[test]
    fn the_result_does_not_depend_on_the_order_the_materials_were_written_in() {
        let (mut forwards, mut backwards) = (Brick::AIR, Brick::AIR);
        for (i, &at) in cell().iter().enumerate() {
            forwards.set(at, Some(Material(i as u16 % 3)));
        }
        for (i, &at) in cell().iter().enumerate().rev() {
            backwards.set(at, Some(Material(i as u16 % 3)));
        }
        assert_eq!(
            of_eight(&forwards).content_hash(),
            of_eight(&backwards).content_hash()
        );
    }
}
