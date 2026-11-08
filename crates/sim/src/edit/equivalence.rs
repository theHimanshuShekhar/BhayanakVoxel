//! The Edit brick by brick (#156) against the Edit as it was, Voxel by Voxel (`brush::frozen`):
//! the same list of changes, and after applying them the same bricks, the same coarse levels and
//! the same Undo, over Brushes of every shape and mode, up to the largest, on mixed terrain.

use brick_store::{Brick, BrickCoord, BrickStore, Interior, Material, VoxelCoord};
use net::{Alignment, Brush, EditMode, EditOp, Shape};

use super::*;
use crate::brush::frozen;
use crate::{Reads, plan_bricks, plan_from};

const ROCK: Material = Material(3);

struct Dice(u64);

impl Dice {
    fn below(&mut self, n: u32) -> i32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as i32 % n as i32
    }
}

/// A Planet of rock 160 Voxels in radius, which is not held (the store's source says what it is),
/// with the bricks round its top held as air, as a solid, or with up to six Materials in them
/// at random, so that an Edit meets every kind of brick: those that are not held, those held
/// uniform and those mixed, and their pyramid. The same each time.
fn terrain() -> BrickStore {
    let mut store = BrickStore::new();
    store.set_source(Interior {
        radius: 160,
        material: ROCK,
    });
    let mut dice = Dice(0xC0FF_EE15_BADD_CAFE);
    for x in -10..=10 {
        for y in 18..=24 {
            for z in -10..=10 {
                let coord = BrickCoord([x, y, z]);
                let mut brick = match dice.below(6) {
                    0 => Brick::AIR,
                    1 => Brick::solid(Material(1 + dice.below(5) as u16)),
                    // Mostly rock, with holes and other Materials in it.
                    2 | 3 => Brick::solid(ROCK),
                    _ => Brick::AIR,
                };
                if dice.below(2) == 0 {
                    for _ in 0..dice.below(200) {
                        let local = [0; 3].map(|_| dice.below(8) as u8);
                        let material = match dice.below(7) {
                            0 => None,
                            m => Some(Material(m as u16)),
                        };
                        brick.set(local, material);
                    }
                }
                store.insert(coord, brick);
            }
        }
    }
    store.build_lod(3);
    store
}

/// Everything that is in the two stores, down to each brick and each coarse level.
fn assert_same(now: &BrickStore, then: &BrickStore, what: &str) {
    assert_eq!(now.sorted_coords(), then.sorted_coords(), "{what}");
    for coord in now.sorted_coords() {
        assert_eq!(
            now.brick(coord),
            then.brick(coord),
            "{what}: brick {coord:?}"
        );
    }
    assert_eq!(now.content_hash(), then.content_hash(), "{what}");
    assert_eq!(now.lod_hash(), then.lod_hash(), "{what}");
}

fn op(dice: &mut Dice, mode: EditMode, shape: Shape, radius: u16) -> EditOp {
    EditOp {
        mode,
        brush: Brush { shape, radius },
        centre: VoxelCoord([
            dice.below(81) - 40,
            160 + dice.below(61) - 30,
            dice.below(81) - 40,
        ]),
        material: Material(1 + dice.below(5) as u16),
        alignment: if dice.below(2) == 0 {
            Alignment::World
        } else {
            Alignment::Local
        },
        rotation: dice.below(8) as u8,
    }
}

#[test]
fn per_brick_plan_and_apply_make_what_per_voxel_plan_and_apply_made() {
    let (mut now, mut then) = (terrain(), terrain());
    let start = now.content_hash();
    let mut dice = Dice(0x9E37_79B9_7F4A_7C15);
    let mut ops = Vec::new();
    // The largest Brush of each shape, to dig and to place.
    for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
        for mode in [EditMode::Dig, EditMode::Place] {
            ops.push(op(&mut dice, mode, shape, crate::MAX_BRUSH_RADIUS));
        }
        for mode in [EditMode::Flatten, EditMode::Smooth] {
            ops.push(op(&mut dice, mode, shape, crate::MAX_SMOOTH_RADIUS));
        }
    }
    // And every shape and mode at any radius up to the largest it may have.
    for _ in 0..60 {
        let mode = [
            EditMode::Dig,
            EditMode::Place,
            EditMode::Flatten,
            EditMode::Smooth,
        ][dice.below(4) as usize];
        let shape = [Shape::Sphere, Shape::Cube, Shape::Cylinder][dice.below(3) as usize];
        let largest = if matches!(mode, EditMode::Flatten | EditMode::Smooth) {
            crate::MAX_SMOOTH_RADIUS
        } else {
            crate::MAX_BRUSH_RADIUS
        };
        let radius = dice.below(u32::from(largest) + 1) as u16;
        ops.push(op(&mut dice, mode, shape, radius));
    }

    let mut undos = Vec::new();
    let mut changed = 0;
    for (step, edit) in ops.iter().enumerate() {
        let what = format!("step {step}: {edit:?}");
        let old_list = frozen::plan_from(&mut Reads::new(&then), edit);
        let plan = plan_bricks(&now, edit);
        // The same Voxels, in the same order, and counted the same.
        assert_eq!(plan.changes().collect::<Vec<_>>(), old_list, "{what}");
        assert_eq!(plan.len(), old_list.len(), "{what}");
        assert_eq!(plan_from(&mut Reads::new(&now), edit), old_list, "{what}");
        changed += old_list.len();

        let undo = apply_plan(&mut now, &plan);
        let undone = frozen::apply_changes(&mut then, &old_list);
        assert_same(&now, &then, &what);
        assert!(
            undo.bricks().eq(undone.bricks.iter().map(|&(c, _)| c)),
            "{what}"
        );
        assert_eq!(undo.lod().remade, undone.lod.remade, "{what}");
        assert_eq!(undo.lod().changed, undone.lod.changed, "{what}");
        undos.push((undo, undone));
    }
    assert!(changed > 1_000_000, "{changed}");

    // Taking them back, newest first, puts both back the same, to the start.
    while let Some((undo, undone)) = undos.pop() {
        let (now_lod, then_lod) = (undo.revert(&mut now), undone.revert(&mut then));
        assert_eq!(now_lod.changed, then_lod.changed);
        assert_same(&now, &then, "revert");
    }
    assert_eq!(now.content_hash(), start);
}
