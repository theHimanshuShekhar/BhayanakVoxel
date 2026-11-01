//! Downsampling: how a coarse brick is made from the eight finer bricks it covers (ADR 0002),
//! by majority, with the Voxels exposed to air counting first. Integer-only, so every machine
//! builds the same coarse brick. Also what a brick reads as where none is held.

use std::borrow::Cow;
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
/// the source alone, so refreshing it after an Edit gives what building it afresh does. That
/// holds with the source read for the bricks beside the ones a coarse brick is made from, too:
/// whether a Voxel is exposed to air depends on them.
pub trait BrickSource: Debug + Send + Sync {
    /// The brick at `coord` of mip `level`, where none is held.
    fn brick(&self, level: usize, coord: BrickCoord) -> Brick;

    /// Whether every Voxel of level 0 under the brick at `coord` of `level` is solid, which the
    /// source knows without reading them: an answer of `true` must be exact, and `false` means
    /// "not known". A coarse brick that reads as full may still have air under it (a coarse Voxel
    /// is solid when half its children are), so a renderer that leaves out what no ray can reach
    /// asks this before it leaves a brick out for good.
    fn solid_below(&self, _level: usize, _coord: BrickCoord) -> bool {
        false
    }
}

/// The bricks under the brick at `coord` of mip `level` (at least 1) as `source` gives them, in
/// [`BrickCoord::children`] order, except air and the ones `skip` says to leave out (those the
/// store holds already): what [`crate::BrickStore::hold_children`] takes. It reads only the
/// source, so it can run on any thread.
pub fn children_from(
    source: &dyn BrickSource,
    level: usize,
    coord: BrickCoord,
    skip: impl Fn(BrickCoord) -> bool,
) -> Vec<(BrickCoord, Brick)> {
    coord
        .children()
        .into_iter()
        .filter(|&child| !skip(child))
        .map(|child| (child, source.brick(level - 1, child)))
        .filter(|(_, brick)| !brick.is_air())
        .collect()
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

    fn solid_below(&self, level: usize, coord: BrickCoord) -> bool {
        coord.within_ball_at(level, self.radius)
    }
}

/// The coarse brick over eight finer ones, given in [`crate::BrickCoord::children`] order.
/// `outside` reads the bricks around them: the brick at an offset from the first child, with
/// one coordinate in `-1..=2` outside the eight.
///
/// Each coarse Voxel covers 2×2×2 Voxels of the finer bricks. It is solid when at least 4 of
/// those 8 are solid. Then it takes the most common Material among the solid ones that are
/// exposed, if any are, and the most common among all the solid ones if none are. A Voxel is
/// exposed when air is next to one of its six faces, in the same block or not: the air that
/// makes a block of solid Voxels the surface can be in the next block, even in the next brick.
/// If several Materials are equally common, the lowest Material ID wins.
///
/// Which Voxels are solid, and which are exposed, are worked out on the occupancy masks of the
/// bricks, a word at a time, not Voxel by Voxel: only the Materials of the exposed Voxels of a
/// block are read, and those of all its solid Voxels when none is exposed.
pub(crate) fn downsample<'a>(
    children: [&'a Brick; 8],
    outside: impl Fn([i32; 3]) -> Cow<'a, Brick>,
) -> Brick {
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

    // The exposed Voxels of each child, made when the first of its blocks needs them.
    let mut exposed: [Option<[u64; 8]>; 8] = [None; 8];
    let mut coarse = Brick::AIR;
    for i in 0..BRICK_VOXELS {
        let at = [i & 7, i >> 3 & 7, i >> 6];
        // The 8 Voxels are in one child, `& 7` inside it.
        let index = at[0] >> 2 | (at[1] >> 2) << 1 | (at[2] >> 2) << 2;
        let child = children[index];
        let (occupancy, palette) = match child.view() {
            BrickView::Air => continue,
            BrickView::Solid(material) => {
                coarse.set(at.map(|a| a as u8), Some(material));
                continue;
            }
            BrickView::Mixed {
                occupancy, palette, ..
            } => (occupancy, palette),
        };
        // The block's Voxels, bit `j` for the one at `j & 1`, `j >> 1 & 1` and `j >> 2` in it.
        let first = at.map(|a| (2 * a) & 7);
        let solid = block_bits(occupancy, first);
        if solid.count_ones() < 4 {
            continue;
        }
        // The palette has an entry for every Material in the brick (it can also keep ones that were
        // overwritten), so a palette of one is a brick of one Material: nothing to choose, and so
        // nothing to look at the air for.
        let winner = if let [only] = *palette {
            only
        } else {
            let mask = exposed[index].get_or_insert_with(|| {
                exposed_mask(occupancy, |face| {
                    occupancy_beside(&children, &outside, index, face)
                })
            });
            let exposed_here = block_bits(mask, first);
            let mut tally = Tally::new();
            // The exposed Voxels decide if there are any, and else all the solid ones do.
            let decide = if exposed_here != 0 {
                exposed_here
            } else {
                solid
            };
            for j in (0..8).filter(|j| decide >> j & 1 == 1) {
                let found = [0, 1, 2].map(|a| (first[a] + (j >> a & 1)) as u8);
                tally.add(child.get(found).expect("a solid Voxel has a Material"));
            }
            tally.winner()
        };
        coarse.set(at.map(|a| a as u8), Some(winner));
    }
    coarse
}

/// The 2×2×2 Voxels of `words`, an occupancy mask, from `first` in Voxels: bit `j` of the result
/// is the Voxel at `j & 1`, `j >> 1 & 1` and `j >> 2` beyond it. `first` is even along each axis.
#[inline]
fn block_bits(words: &[u64; 8], first: [usize; 3]) -> u8 {
    let rows = |z: usize| {
        let pair = words[z] >> (first[0] + 8 * first[1]) & 0x0303;
        (pair & 3 | pair >> 6 & 0xc) as u8
    };
    rows(first[2]) | rows(first[2] + 1) << 4
}

/// The occupancy mask of a brick, as `BrickView::Mixed` has it.
fn occupancy_of(brick: &Brick) -> [u64; 8] {
    match brick.view() {
        BrickView::Air => [0; 8],
        BrickView::Solid(_) => [u64::MAX; 8],
        BrickView::Mixed { occupancy, .. } => *occupancy,
    }
}

/// The occupancy of the brick that touches `face` of child `index` of the eight (see
/// [`exposed_mask`] for the faces): one of the eight, or else the brick around them.
fn occupancy_beside<'a>(
    children: &[&Brick; 8],
    outside: &impl Fn([i32; 3]) -> Cow<'a, Brick>,
    index: usize,
    face: usize,
) -> [u64; 8] {
    let mut at = [0, 1, 2].map(|a| (index >> a & 1) as i32);
    at[face / 2] += if face.is_multiple_of(2) { -1 } else { 1 };
    if at.iter().all(|a| (0..2).contains(a)) {
        return occupancy_of(children[(at[0] | at[1] << 1 | at[2] << 2) as usize]);
    }
    occupancy_of(&outside(at))
}

/// The Voxels at `x = 0`, and at `x = 7`, of a word of an occupancy mask.
const X_FIRST: u64 = 0x0101_0101_0101_0101;
const X_LAST: u64 = 0x8080_8080_8080_8080;

/// The Voxels of a brick that are solid with air next to one of their six faces: for each
/// direction, the occupancy mask shifted one Voxel along that axis, with the layer it shifts in
/// taken from the touching brick. `neighbour(face)` is the occupancy of that brick, for `face`
/// 0 to 5 as 2 × axis, plus 1 on the positive side; it is not asked for when the layer of the
/// brick's own that touches it has no solid Voxel, which is then all it could change.
fn exposed_mask(occupancy: &[u64; 8], mut neighbour: impl FnMut(usize) -> [u64; 8]) -> [u64; 8] {
    // A word is one layer of constant z: Voxel `x | y << 3` of it.
    let touching = [
        occupancy.iter().any(|w| w & X_FIRST != 0),
        occupancy.iter().any(|w| w & X_LAST != 0),
        occupancy.iter().any(|w| w & 0xff != 0),
        occupancy.iter().any(|w| w >> 56 != 0),
        occupancy[0] != 0,
        occupancy[7] != 0,
    ];
    let beside: [[u64; 8]; 6] = std::array::from_fn(|face| {
        if touching[face] {
            neighbour(face)
        } else {
            [0; 8]
        }
    });
    std::array::from_fn(|z| {
        let w = occupancy[z];
        let x_down = (w << 1) & !X_FIRST | (beside[0][z] & X_LAST) >> 7;
        let x_up = (w >> 1) & !X_LAST | (beside[1][z] & X_FIRST) << 7;
        let y_down = w << 8 | beside[2][z] >> 56;
        let y_up = w >> 8 | beside[3][z] << 56;
        let z_down = if z > 0 {
            occupancy[z - 1]
        } else {
            beside[4][7]
        };
        let z_up = if z < 7 {
            occupancy[z + 1]
        } else {
            beside[5][0]
        };
        w & !(x_down & x_up & y_down & y_up & z_down & z_up)
    })
}

/// How many of one coarse Voxel's children are made of each Material.
struct Tally {
    counts: [(Material, u8); 8],
    /// How many Materials are tallied.
    len: usize,
    /// How many children are tallied, which is how many are solid. Only the per-Voxel rule in
    /// the tests reads it now.
    count: u8,
}

impl Tally {
    fn new() -> Self {
        Self {
            counts: [(Material(0), 0); 8],
            len: 0,
            count: 0,
        }
    }

    fn add(&mut self, material: Material) {
        self.count += 1;
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

    /// The most common Material, the lowest ID of those equally common. Panics if there is none.
    fn winner(&self) -> Material {
        self.counts[..self.len]
            .iter()
            .max_by_key(|&&(m, n)| (n, Reverse(m)))
            .map(|&(m, _)| m)
            .expect("a Material was tallied")
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

    /// Downsamples `children` with air all around them.
    fn downsample_in_air(children: [&Brick; 8]) -> Brick {
        downsample(children, |_| Cow::Owned(Brick::AIR))
    }

    /// Downsamples eight copies of `brick`.
    fn of_eight(brick: &Brick) -> Brick {
        downsample_in_air([brick; 8])
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
        downsample_in_air([&brick, &air, &air, &air, &air, &air, &air, &air]).get([0, 0, 0])
    }

    /// The Materials the exposed-surface tests use. BEDROCK has the lowest ID, so it wins every
    /// tie that is decided by ID, and no majority rule can favour SOIL by accident.
    const BEDROCK: Material = Material(2);
    const SOIL: Material = Material(9);

    /// The coarse brick over the eight bricks at (0..2)³ of a world given Voxel by Voxel, made
    /// the way the store makes it: the bricks around them are read as the world has them.
    fn coarse_of(world: impl Fn([i32; 3]) -> Option<Material>) -> Brick {
        let brick_at = |at: [i32; 3]| {
            let mut brick = Brick::AIR;
            for i in 0..BRICK_VOXELS {
                let local = local(i);
                let voxel = BrickCoord(at).voxel(local).0;
                brick.set(local, world(voxel));
            }
            Cow::Owned(brick)
        };
        let children = BrickCoord([0; 3]).children().map(|c| brick_at(c.0));
        downsample(children.each_ref().map(|c| &**c), brick_at)
    }

    /// Ground that is `top` Voxels up: SOIL for the `thin` Voxels under its surface, BEDROCK
    /// below that, and air above, whatever x and y are.
    fn layered(top: i32, thin: i32) -> impl Fn([i32; 3]) -> Option<Material> {
        move |[_, _, z]| match z {
            z if z >= top => None,
            z if z >= top - thin => Some(SOIL),
            _ => Some(BEDROCK),
        }
    }

    #[test]
    fn a_solid_voxel_exposed_to_air_decides_the_material_before_the_buried_ones() {
        // Voxels 2 and 3 in z are the top of the ground in coarse Voxel (0, 0, 1): its upper
        // layer is soil with air above it, its lower layer bedrock. Four of each, and bedrock
        // has the lower ID, so counting alone would say bedrock.
        let coarse = coarse_of(layered(4, 1));
        assert_eq!(coarse.get([0, 0, 1]), Some(SOIL));
        // The Voxel under it is all bedrock, and so are the others below.
        assert_eq!(coarse.get([0, 0, 0]), Some(BEDROCK));
    }

    #[test]
    fn a_voxel_with_no_air_next_to_it_keeps_the_most_common_material() {
        // Five Voxels of soil and three of bedrock, all of them buried: a solid block with
        // solid Voxels all around it.
        let coarse = coarse_of(|[x, y, z]| {
            let inside = (0..2).contains(&x) && (0..2).contains(&y) && (0..2).contains(&z);
            Some(match (inside, x + 2 * y + 4 * z) {
                (true, i) if i < 3 => BEDROCK,
                _ => SOIL,
            })
        });
        assert_eq!(coarse.get([0, 0, 0]), Some(SOIL));
        // And with the Materials the other way round the lowest ID does not matter: the
        // majority is bedrock.
        let coarse = coarse_of(|[x, y, z]| {
            let inside = (0..2).contains(&x) && (0..2).contains(&y) && (0..2).contains(&z);
            Some(match (inside, x + 2 * y + 4 * z) {
                (true, i) if i < 3 => SOIL,
                _ => BEDROCK,
            })
        });
        assert_eq!(coarse.get([0, 0, 0]), Some(BEDROCK));
        // Buried ties still go to the lowest ID.
        let coarse = coarse_of(|[x, y, z]| {
            let inside = (0..2).contains(&x) && (0..2).contains(&y) && (0..2).contains(&z);
            Some(if inside && z == 0 { SOIL } else { BEDROCK })
        });
        assert_eq!(coarse.get([0, 0, 0]), Some(BEDROCK));
    }

    #[test]
    fn a_block_that_is_all_solid_is_surface_when_the_air_is_in_the_coarse_voxel_next_to_it() {
        // Ground up to z = 6, so coarse Voxel (0, 0, 2) is all solid: soil in its top layer
        // and bedrock under it. The air is in (0, 0, 3), one coarse Voxel up.
        assert_eq!(coarse_of(layered(6, 1)).get([0, 0, 2]), Some(SOIL));
        assert_eq!(coarse_of(layered(6, 1)).get([0, 0, 3]), None);
    }

    #[test]
    fn the_air_can_be_in_the_next_brick_over() {
        // Ground up to z = 16, the top of the eight bricks. Coarse Voxel (0, 0, 7) is the last
        // of its brick, and its air is the finer brick above them, which only the surroundings
        // know. Its x and y neighbours are in the next brick over too.
        assert_eq!(coarse_of(layered(16, 1)).get([0, 0, 7]), Some(SOIL));
        assert_eq!(coarse_of(layered(16, 1)).get([7, 7, 7]), Some(SOIL));
        // Without that air it is buried, and the tie goes to bedrock.
        assert_eq!(coarse_of(layered(17, 1)).get([0, 0, 7]), Some(BEDROCK));
    }

    #[test]
    fn a_wall_is_exposed_on_its_side_whatever_its_depth() {
        // Ground for x < 16, in layers of x: a skin of soil at x = 15 over bedrock, and air
        // beyond x = 16 which is only in the next brick over.
        let coarse = coarse_of(|[x, _, _]| match x {
            x if x >= 16 => None,
            15 => Some(SOIL),
            _ => Some(BEDROCK),
        });
        assert_eq!(coarse.get([7, 3, 3]), Some(SOIL));
        assert_eq!(coarse.get([6, 3, 3]), Some(BEDROCK));
    }

    #[test]
    fn a_voxel_is_solid_by_the_same_count_whether_or_not_it_is_exposed() {
        // Three solid Voxels of a block are still too few, and 4 enough, with the surface rule.
        let world = |solid: i32| {
            coarse_of(move |[x, y, z]| {
                let at = x + 2 * y + 4 * z;
                (x < 2 && y < 2 && z < 2 && at < solid).then_some(SOIL)
            })
            .get([0, 0, 0])
        };
        assert_eq!(world(3), None);
        assert_eq!(world(4), Some(SOIL));
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
        let coarse = downsample_in_air([&rock, &sand, &rock, &rock, &rock, &rock, &rock, &rock]);
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
        let coarse = downsample_in_air([&air, &air, &air, &air, &air, &rock, &air, &air]);
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
        let coarse = downsample_in_air([&block, &air, &air, &air, &air, &air, &air, &air]);
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

    /// The rule as it was before exposure was read from the occupancy masks: each Voxel of a block
    /// read through `Brick::get`, and the air beside it looked for Voxel by Voxel. Kept so that
    /// tests can show that the masks give the same coarse bricks, not only the same goldens. Its
    /// `downsample` is the old one; its `Around` is simpler than the old one (no cache of the
    /// bricks it has read), which gives the same answers because `outside` has no side effects.
    mod per_voxel {
        use super::*;

        pub(super) fn downsample<'a>(
            children: [&'a Brick; 8],
            outside: impl Fn([i32; 3]) -> Cow<'a, Brick>,
        ) -> Brick {
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
            let mut around = None;
            let mut coarse = Brick::AIR;
            for i in 0..BRICK_VOXELS {
                let at = [i & 7, i >> 3 & 7, i >> 6];
                let child = children[at[0] >> 2 | (at[1] >> 2) << 1 | (at[2] >> 2) << 2];
                match child.view() {
                    BrickView::Air => continue,
                    BrickView::Solid(material) => {
                        coarse.set(at.map(|a| a as u8), Some(material));
                        continue;
                    }
                    BrickView::Mixed { .. } => {}
                }
                let voxels: [Option<Material>; 8] = std::array::from_fn(|j| {
                    child.get([0, 1, 2].map(|a| ((2 * at[a] + (j >> a & 1)) & 7) as u8))
                });
                let mut all = Tally::new();
                for material in voxels.into_iter().flatten() {
                    all.add(material);
                }
                if all.count < 4 {
                    continue;
                }
                let mut winner = all.winner();
                if all.len > 1 {
                    let mut exposed = Tally::new();
                    for (j, voxel) in voxels.into_iter().enumerate() {
                        let found = [0, 1, 2].map(|a| (2 * at[a] + (j >> a & 1)) as i32);
                        if let Some(material) = voxel
                            && around
                                .get_or_insert_with(|| Around::new(children, &outside))
                                .exposed(found)
                        {
                            exposed.add(material);
                        }
                    }
                    if exposed.count > 0 {
                        winner = exposed.winner();
                    }
                }
                coarse.set(at.map(|a| a as u8), Some(winner));
            }
            coarse
        }

        /// The Voxels of eight bricks and those beside them, read one at a time.
        pub(super) struct Around<'a, F> {
            children: [&'a Brick; 8],
            outside: F,
        }

        impl<'a, F: Fn([i32; 3]) -> Cow<'a, Brick>> Around<'a, F> {
            pub(super) fn new(children: [&'a Brick; 8], outside: F) -> Self {
                Self { children, outside }
            }

            fn voxel(&self, at: [i32; 3]) -> Option<Material> {
                let brick = at.map(|v| v.div_euclid(8));
                let local = at.map(|v| v.rem_euclid(8) as u8);
                if brick.iter().all(|&b| (0..2).contains(&b)) {
                    return self.children[(brick[0] | brick[1] << 1 | brick[2] << 2) as usize]
                        .get(local);
                }
                (self.outside)(brick).get(local)
            }

            /// Whether there is air next to a face of the Voxel at `at`.
            pub(super) fn exposed(&self, at: [i32; 3]) -> bool {
                (0..6).any(|face| {
                    let mut next = at;
                    next[face / 2] += if face % 2 == 0 { -1 } else { 1 };
                    self.voxel(next).is_none()
                })
            }
        }
    }

    /// A small deterministic generator (SplitMix64), so that the random cases are the same on
    /// every run and machine.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ z >> 30).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ z >> 27).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ z >> 31
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    const PALETTE: [Material; 4] = [Material(5), Material(2), Material(9), Material(3)];

    /// A brick of a shape that the exposed-surface rule is sensitive to: air, a flag, a brick
    /// with a few holes or a few Voxels (often on its edges and corners), random at every
    /// density, a slab or a staircase of ground at any height, only its faces, all but its
    /// faces, a checkerboard. Its Materials are one to four of [`PALETTE`], by layer or at random.
    fn any_brick(rng: &mut Rng) -> Brick {
        let materials = &PALETTE[..1 + rng.below(4)];
        match rng.below(20) {
            0 => return Brick::AIR,
            1 => return Brick::solid(materials[0]),
            _ => {}
        }
        let kind = rng.below(10);
        let density = [1, 4, 8, 12, 15][rng.below(5)];
        let height = rng.below(9);
        let by_layer = rng.below(2) == 0;
        // A few Voxels, with a lean towards the corners and edges.
        let few: Vec<LocalCoord> = (0..1 + rng.below(6))
            .map(|_| {
                [0, 1, 2].map(|_| match rng.below(3) {
                    0 => 0,
                    1 => 7,
                    _ => rng.below(8) as u8,
                })
            })
            .collect();
        let mut brick = Brick::AIR;
        for i in 0..BRICK_VOXELS {
            let at = local(i);
            let [x, y, z] = at.map(usize::from);
            let faces = at.iter().filter(|&&c| c == 0 || c == 7).count();
            let solid = match kind {
                0 => true,
                1 => !few.contains(&at),
                2 => few.contains(&at),
                3 => rng.below(16) < density,
                4 => z < height,
                5 => z < height + (x ^ y) % 3,
                6 => faces > 0,
                7 => faces == 0,
                8 => (x + y + z) % 2 == 0,
                _ => x < height || y >= 8 - height,
            };
            if solid {
                let layer = if by_layer { z } else { rng.below(8) };
                brick.set(at, Some(materials[layer * materials.len() / 8]));
            }
        }
        brick
    }

    /// The occupancy of `brick`, read one Voxel at a time.
    fn occupancy_by_get(brick: &Brick) -> [u64; 8] {
        let mut mask = [0u64; 8];
        for i in 0..BRICK_VOXELS {
            if brick.get(local(i)).is_some() {
                mask[i / 64] |= 1 << (i % 64);
            }
        }
        mask
    }

    #[test]
    fn the_exposed_voxels_of_a_brick_are_those_with_air_beside_a_face_in_it_or_in_a_neighbour() {
        let mut rng = Rng(1);
        for case in 0..2000 {
            let brick = any_brick(&mut rng);
            let neighbours: [Brick; 6] = std::array::from_fn(|_| any_brick(&mut rng));
            let mask = exposed_mask(&occupancy_by_get(&brick), |face| {
                occupancy_by_get(&neighbours[face])
            });
            for i in 0..BRICK_VOXELS {
                let at = local(i);
                let beside_air = (0..6).any(|face| {
                    let (axis, up) = (face / 2, face % 2 == 1);
                    let mut next = at.map(i32::from);
                    next[axis] += if up { 1 } else { -1 };
                    if (0..8).contains(&next[axis]) {
                        return brick.get(next.map(|v| v as u8)).is_none();
                    }
                    let wrapped = next.map(|v| v.rem_euclid(8) as u8);
                    neighbours[face].get(wrapped).is_none()
                });
                let expected = brick.get(at).is_some() && beside_air;
                assert_eq!(
                    mask[i / 64] >> (i % 64) & 1 == 1,
                    expected,
                    "case {case}, Voxel {at:?}"
                );
            }
        }
    }

    #[test]
    fn a_face_with_no_solid_voxel_on_it_does_not_ask_for_the_brick_beside_it() {
        // Only the layer at x = 3 is solid: neither face in x touches it.
        let mut brick = Brick::AIR;
        for v in 0..64 {
            brick.set([3, v % 8, v / 8], Some(ROCK));
        }
        let mask = exposed_mask(&occupancy_by_get(&brick), |face| {
            assert!(face >= 2, "asked for the brick beside face {face}");
            [0; 8]
        });
        // Air on both sides in x, so every solid Voxel is exposed.
        assert_eq!(mask, occupancy_by_get(&brick));
    }

    /// What the bricks beside the eight read as, when the store holds none of them or all.
    #[derive(Clone, Copy, Debug)]
    enum Beside {
        /// As the world has them.
        World,
        Air,
        Solid(Material),
    }

    /// 4×4×4 bricks: the eight to downsample at (1..3)³ and the ring around them, indexed by
    /// offset from the first of the eight plus one on each axis.
    type World = Vec<Brick>;

    fn slot(offset: [i32; 3]) -> usize {
        (offset[0] + 1 + 4 * (offset[1] + 1) + 16 * (offset[2] + 1)) as usize
    }

    /// The bricks of a world that has ground rolling through the brick boundaries: a height
    /// field of stratified ground with caves, so that its surface crosses the faces between
    /// bricks as the Planet's does.
    fn terrain(rng: &mut Rng) -> World {
        let (a, b, c) = (rng.below(7) + 1, rng.below(7) + 1, rng.below(5));
        let thin: [usize; 3] = std::array::from_fn(|_| rng.below(3) + 1);
        let hash = |x: usize, y: usize| (x * 7 + y * 13 + (x ^ y) * a + b) % 11;
        let voxel = |x: usize, y: usize, z: usize| {
            let top = 8 + hash(x / 2, y / 2) + c;
            let cave = (x * 5 + y * 3 + z * 11).is_multiple_of(23) && z + 3 < top;
            match z {
                _ if z >= top || cave => None,
                z if z + thin[0] >= top => Some(PALETTE[2]),
                z if z + thin[0] + thin[1] >= top => Some(PALETTE[0]),
                _ => Some(PALETTE[1]),
            }
        };
        (0..64)
            .map(|n| {
                let origin = [n % 4, n / 4 % 4, n / 16].map(|b| b * 8);
                let mut brick = Brick::AIR;
                for i in 0..BRICK_VOXELS {
                    let at = local(i);
                    let [x, y, z] = [0, 1, 2].map(|k| origin[k] + usize::from(at[k]));
                    brick.set(at, voxel(x, y, z));
                }
                brick
            })
            .collect()
    }

    /// The coarse brick over the eight bricks of `world` as the masks make it, and as the
    /// per-Voxel rule does.
    fn coarse_both(world: &World, beside: Beside) -> (Brick, Brick) {
        let children: [&Brick; 8] = std::array::from_fn(|j| {
            &world[slot([(j & 1) as i32, (j >> 1 & 1) as i32, (j >> 2) as i32])]
        });
        let outside = |at: [i32; 3]| match beside {
            Beside::World => Cow::Borrowed(&world[slot(at)]),
            Beside::Air => Cow::Owned(Brick::AIR),
            Beside::Solid(material) => Cow::Owned(Brick::solid(material)),
        };
        (
            downsample(children, outside),
            per_voxel::downsample(children, outside),
        )
    }

    fn assert_same_as_per_voxel(world: &World, beside: Beside, what: &str) {
        let (new, old) = coarse_both(world, beside);
        assert_eq!(new, old, "{what} {beside:?}");
        assert_eq!(new.content_hash(), old.content_hash(), "{what} {beside:?}");
    }

    #[test]
    fn a_coarse_brick_is_what_the_per_voxel_rule_makes_of_random_bricks() {
        let mut rng = Rng(2);
        for case in 0..600 {
            let world: World = (0..64).map(|_| any_brick(&mut rng)).collect();
            for beside in [Beside::World, Beside::Air, Beside::Solid(PALETTE[1])] {
                assert_same_as_per_voxel(&world, beside, &format!("random world {case}"));
            }
        }
    }

    #[test]
    fn a_coarse_brick_is_what_the_per_voxel_rule_makes_of_ground_across_brick_boundaries() {
        let mut rng = Rng(3);
        for case in 0..300 {
            let world = terrain(&mut rng);
            for beside in [Beside::World, Beside::Air, Beside::Solid(PALETTE[1])] {
                assert_same_as_per_voxel(&world, beside, &format!("terrain {case}"));
            }
        }
    }

    #[test]
    fn a_coarse_brick_is_what_the_per_voxel_rule_makes_of_bricks_beside_uniform_ones() {
        // The eight are mixed, and the ring around them is air or one Material, whole bricks, as
        // it is where the store holds none; and the eight bricks themselves are sometimes flags.
        let mut rng = Rng(4);
        for case in 0..300 {
            let mut world: World = (0..64)
                .map(|n| match (n % 4, n / 4 % 4, n / 16) {
                    (1 | 2, 1 | 2, 1 | 2) if rng.below(6) > 0 => any_brick(&mut rng),
                    _ => match rng.below(3) {
                        0 => Brick::AIR,
                        1 => Brick::solid(PALETTE[rng.below(4)]),
                        _ => any_brick(&mut rng),
                    },
                })
                .collect();
            for beside in [Beside::World, Beside::Air, Beside::Solid(PALETTE[0])] {
                assert_same_as_per_voxel(&world, beside, &format!("uniform ring {case}"));
            }
            // Then every brick that is not one of the eight is the same flag.
            let flag = [Brick::AIR, Brick::solid(PALETTE[3])][case % 2].clone();
            for (n, brick) in world.iter_mut().enumerate() {
                if !matches!((n % 4, n / 4 % 4, n / 16), (1 | 2, 1 | 2, 1 | 2)) {
                    *brick = flag.clone();
                }
            }
            assert_same_as_per_voxel(&world, Beside::World, &format!("flag ring {case}"));
        }
    }

    #[test]
    fn a_coarse_brick_is_what_the_per_voxel_rule_makes_of_a_hole_at_any_edge_or_corner() {
        // Eight solid bricks of two Materials with one Voxel taken out, at every Voxel on the
        // boundary of the 16×16×16 in turn: the layers that the shifts bring in from outside,
        // and the corners where two or three of them meet. The bricks around are air, or rock.
        for n in 0..16 * 16 * 16usize {
            let at = [n & 15, n >> 4 & 15, n >> 8];
            if at.iter().all(|&c| (1..15).contains(&c)) {
                continue;
            }
            let mut world: World = vec![Brick::AIR; 64];
            for j in 0..8 {
                let mut brick = Brick::solid(PALETTE[1]);
                for v in (0..BRICK_VOXELS).step_by(3) {
                    brick.set(local(v), Some(PALETTE[2]));
                }
                world[slot([j & 1, j >> 1 & 1, j >> 2])] = brick;
            }
            let brick = [0, 1, 2].map(|k| (at[k] / 8) as i32);
            world[slot(brick)].set([0, 1, 2].map(|k| (at[k] % 8) as u8), None);
            for beside in [Beside::World, Beside::Solid(PALETTE[0])] {
                assert_same_as_per_voxel(&world, beside, &format!("hole at {at:?}"));
            }
        }
    }
}
