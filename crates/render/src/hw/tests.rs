//! The CPU half of the hardware backend: the tight boxes and the regular grouping (#50). Nothing
//! here needs a GPU; `tests/hw_agreement.rs` has the one that compares the backends' hits.

use brick_store::{BrickCoord, BrickStore, Material, VoxelCoord};

use super::*;
use crate::scene::{Core, SceneUpdate};

const STONE: Material = Material(7);

fn store_with(voxels: &[[i32; 3]]) -> BrickStore {
    let mut store = BrickStore::new();
    for &v in voxels {
        store.set(VoxelCoord(v), Some(STONE));
    }
    store
}

/// The mask of the voxels `voxels` (local coordinates), as a mixed brick holds it.
fn mask_of(voxels: &[[usize; 3]]) -> [u32; 16] {
    let mut mask = [0u32; 16];
    for &[x, y, z] in voxels {
        let i = x | y << 3 | z << 6;
        mask[i / 32] |= 1 << (i % 32);
    }
    mask
}

#[test]
fn a_bricks_box_is_the_min_and_max_of_the_voxels_its_mask_has_set_and_not_the_whole_brick() {
    assert_eq!(
        tight_box(&mask_of(&[[3, 4, 5]])),
        Some(([3, 4, 5], [3, 4, 5]))
    );
    assert_eq!(
        tight_box(&mask_of(&[[2, 3, 4], [5, 3, 4], [3, 6, 1]])),
        Some(([2, 3, 1], [5, 6, 4]))
    );
    // The corners of the brick: the whole box.
    assert_eq!(
        tight_box(&mask_of(&[[0, 0, 0], [7, 7, 7]])),
        Some(([0; 3], [7; 3]))
    );
    // Bits in each word of the mask are found: x is the low three bits, z crosses the words.
    assert_eq!(
        tight_box(&mask_of(&[[7, 0, 0]])),
        Some(([7, 0, 0], [7, 0, 0]))
    );
    assert_eq!(
        tight_box(&mask_of(&[[0, 7, 3], [0, 0, 7]])),
        Some(([0, 0, 3], [0, 7, 7]))
    );
    assert_eq!(tight_box(&[0; 16]), None);
}

#[test]
fn a_mixed_bricks_prim_is_its_tight_box_in_the_groups_frame_and_a_solid_bricks_is_the_whole_brick()
{
    let mut store = store_with(&[[2, 3, 4], [5, 3, 4]]);
    store.insert(
        BrickCoord([1, 0, 0]),
        brick_store::Brick::solid(Material(300)),
    );
    let scene = Scene::new(&store, None);
    let prims = group_prims(&scene, 0, [0, 0, 0]);
    assert_eq!(prims.len(), 2);
    // Region and brick order: brick (0,0,0) then (1,0,0).
    assert_eq!(prims[0].min, [2.0, 3.0, 4.0]);
    assert_eq!(prims[0].max, [6.0, 4.0, 5.0]);
    assert_eq!(prims[1].min, [8.0, 0.0, 0.0]);
    assert_eq!(prims[1].max, [16.0, 8.0, 8.0]);
    // Not the full 8 x 8 x 8 of the mixed brick.
    let volume = |p: &Prim| (0..3).map(|a| p.max[a] - p.min[a]).product::<f32>();
    assert!(volume(&prims[0]) < 8.0 * 8.0 * 8.0);
    // The words say which brick, and the box in it.
    assert_eq!(prims[0].words[0], 0);
    assert_eq!(
        prims[0].words[1],
        2 | 3 << 3 | 4 << 6 | 5 << 9 | 3 << 12 | 4 << 15
    );
    assert_eq!(prims[1].words[0], 1);
}

#[test]
fn a_region_that_is_one_material_is_one_box_and_not_its_512_bricks() {
    let scene = Scene::new(
        &BrickStore::new(),
        Some(Core {
            radius: 600,
            material: STONE,
        }),
    );
    let uniform: Vec<GroupKey> = all_groups(&scene)
        .into_iter()
        .filter(|&(l, g)| {
            group_prims(&scene, l as usize, g)
                .iter()
                .any(|p| p.words[0] & Prim::REGION != 0)
        })
        .collect();
    assert!(
        !uniform.is_empty(),
        "a core of 600 Voxels has whole regions"
    );
    for (level, group) in uniform {
        for p in group_prims(&scene, level as usize, group)
            .iter()
            .filter(|p| p.words[0] & Prim::REGION != 0)
        {
            let side: Vec<f32> = (0..3).map(|a| p.max[a] - p.min[a]).collect();
            assert_eq!(side, [64.0; 3]);
        }
    }
}

#[test]
fn bricks_are_grouped_by_the_regular_grid_of_their_regions_and_never_by_what_is_in_them() {
    // The group is a function of the region's coordinate, with floor division for negatives.
    assert_eq!(group_of([0, 0, 0]), [0, 0, 0]);
    assert_eq!(group_of([3, 3, 3]), [0, 0, 0]);
    assert_eq!(group_of([4, 0, 0]), [1, 0, 0]);
    assert_eq!(group_of([-1, -4, -5]), [-1, -1, -2]);
    // A scene's groups are those of its regions, whatever the bricks hold.
    let voxels = [
        [10, 0, 0],
        [-900, 35, 70],
        [1000, 3, -500],
        [63, 5, 0],
        [64, 0, 0],
        [3000, 3, 3000],
    ];
    let scene = Scene::new(&store_with(&voxels), None);
    let groups = all_groups(&scene);
    let expected: BTreeSet<GroupKey> = voxels
        .iter()
        .map(|v| (0u8, group_of(v.map(|c| c.div_euclid(REGION_VOXELS)))))
        .collect();
    assert_eq!(groups, expected);
    // Every box of a group is inside its cube: 256 Voxels of the level.
    for &(level, group) in &groups {
        let prims = group_prims(&scene, level as usize, group);
        assert!(!prims.is_empty());
        for p in prims {
            assert!(
                p.min
                    .iter()
                    .chain(&p.max)
                    .all(|c| (0.0..=256.0).contains(c))
            );
        }
    }
    // Adding a brick elsewhere changes no other group, so the grouping is not a function of
    // content, and nothing is split by what is in a region (the TLAS keeps its shape).
    let mut more = voxels.to_vec();
    more.push([-5000, 1, 1]);
    let bigger = all_groups(&Scene::new(&store_with(&more), None));
    assert!(groups.is_subset(&bigger));
    assert_eq!(bigger.len(), groups.len() + 1);
    // Two regions in one cube share a group: (10,0,0) and (64,0,0) are regions 0 and 1.
    assert!(groups.contains(&(0, [0, 0, 0])));
    assert_eq!(groups.iter().filter(|g| g.1 == [0, 0, 0]).count(), 1);
}

#[test]
fn a_group_of_a_ring_scene_has_every_listed_brick_once_and_none_it_does_not_list() {
    let store = store_with(&[[1, 1, 1], [9, 1, 1], [65, 1, 1], [300, 1, 1]]);
    let scene = Scene::new(&store, None);
    let total: usize = all_groups(&scene)
        .into_iter()
        .map(|(l, g)| group_prims(&scene, l as usize, g).len())
        .sum();
    // Four Voxels in four bricks.
    assert_eq!(total, 4);
    assert!(group_prims(&scene, 0, [50, 50, 50]).is_empty());
}

#[test]
fn the_anchor_is_the_snap_multiple_nearest_the_camera() {
    assert_eq!(anchor_for([0.0; 3]), [0; 3]);
    assert_eq!(anchor_for([2047.0, -2049.0, 5000.0]), [0, -4096, 4096]);
    assert_eq!(anchor_for([6_000_000.0, 0.0, 0.0])[0] % ANCHOR_SNAP, 0);
}

/// Rings of a patch of the test Planet around a camera 1565 Voxels up, an Edit that places a block
/// of Stone `size` Voxels each way on the ground at `(x, z)`, and the groups that the scene's patch
/// of it makes stale, with the scene's patch and how many groups there are in all.
fn dirty_after_a_block(
    (x0, z0): (i32, i32),
    size: i32,
) -> (BTreeSet<GroupKey>, crate::scene::RingPatch, usize) {
    use crate::lod::Bands;
    use crate::scene::Bound;
    let mut store =
        planet_gen::generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12]));
    store.build_lod(5);
    let bound = Bound {
        centre: [0; 3],
        radius: 1600,
    };
    let mut scene = Scene::rings(&store, [0.0, 1565.0, 0.0], Bands::with_d0(64.0), bound).unwrap();
    let mut index = RegionIndex::of(&scene);
    let total = all_groups(&scene).len();
    let top = (1400..1650)
        .rev()
        .find(|&y| store.get(VoxelCoord([x0, y, z0])).is_some())
        .unwrap();
    let mut changed = BTreeSet::new();
    for x in x0..x0 + size {
        for y in top + 1..top + 1 + size {
            for z in z0..z0 + size {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
                changed.insert(VoxelCoord([x, y, z]).brick());
            }
        }
    }
    let changed: Vec<BrickCoord> = changed.into_iter().collect();
    store.refresh_lod(changed.iter().copied());
    let SceneUpdate::Rings(patch) = scene.update(&store, &changed).unwrap() else {
        panic!("an Edit of rings is a patch of rings");
    };
    (index.dirty(&scene, &patch), patch, total)
}

#[test]
fn an_edit_makes_stale_only_the_groups_that_hold_the_regions_it_wrote() {
    // An Edit under the camera: level 0, a region's entries written.
    let (dirty, patch, total) = dirty_after_a_block((0, 0), 8);
    assert!(
        !patch.regions.is_empty() || !patch.rewritten.is_empty(),
        "{patch:?}"
    );
    assert!(
        !dirty.is_empty() && dirty.len() < total,
        "{dirty:?} of {total}"
    );
    assert!(dirty.iter().all(|&(level, _)| level == 0), "{dirty:?}");
}

/// A mixed brick of a coarse level whose words are written in place: nothing of `regions`,
/// `table` or `brick_refs` says where, only `rewritten_coarse`, and the box made from the words
/// is stale. Without that list the dirty set is empty and the hardware backend draws the ground
/// that the block stands on (the reviewer's probe of #233).
#[test]
fn a_coarse_brick_written_in_place_makes_its_group_stale() {
    let (dirty, patch, total) = dirty_after_a_block((100, 100), 4);
    assert_eq!(
        (
            patch.regions.len(),
            patch.table.len(),
            patch.brick_refs.len()
        ),
        (0, 0, 0),
        "{patch:?}"
    );
    assert_eq!(patch.rewritten_coarse, vec![(1, [0, 11, 0])]);
    assert_eq!(
        dirty,
        BTreeSet::from([(1, group_of([0, 11, 0]))]),
        "of {total}"
    );
}
