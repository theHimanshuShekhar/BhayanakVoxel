//! The CPU half of the hardware backend: the tight boxes and the regular grouping (#50). Nothing
//! here needs a GPU; `tests/hw_agreement.rs` has the one that compares the backends' hits.

use brick_store::{BrickCoord, BrickStore, Material, VoxelCoord};

use std::collections::BTreeSet;

use super::*;
use crate::scene::{Core, RingPatch, SceneUpdate};

/// Every group the scene has a region in.
fn all_groups(scene: &Scene) -> BTreeSet<GroupKey> {
    group_counts(scene).into_keys().collect()
}

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

/// Rings of a patch of the test Planet around a camera 1565 Voxels up, and an Edit that places a
/// block of Stone `size` Voxels each way on the ground at `(x, z)`: the scene before and after
/// it, and the scene's patch of it.
fn a_block_placed(at: (i32, i32), size: i32) -> (Scene, Scene, RingPatch) {
    an_edit(at, size, false)
}

/// As [`a_block_placed`], or with `dig` the same `size` of the ground dug out under where it
/// would stand.
fn an_edit((x0, z0): (i32, i32), size: i32, dig: bool) -> (Scene, Scene, RingPatch) {
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
    let before = scene.mirror();
    let top = (1400..1650)
        .rev()
        .find(|&y| store.get(VoxelCoord([x0, y, z0])).is_some())
        .unwrap();
    let mut changed = BTreeSet::new();
    let (low, fill) = if dig {
        (top + 1 - size, None)
    } else {
        (top + 1, Some(STONE))
    };
    for x in x0..x0 + size {
        for y in low..low + size {
            for z in z0..z0 + size {
                store.set(VoxelCoord([x, y, z]), fill);
                changed.insert(VoxelCoord([x, y, z]).brick());
            }
        }
    }
    let changed: Vec<BrickCoord> = changed.into_iter().collect();
    store.refresh_lod(changed.iter().copied());
    let SceneUpdate::Rings(patch) = scene.update(&store, &changed).unwrap() else {
        panic!("an Edit of rings is a patch of rings");
    };
    (before, scene, patch)
}

/// The groups whose boxes differ between two scenes (the boxes, not the words they were made
/// from): what a patch's groups must hold every one of.
fn groups_that_differ(before: &Scene, after: &Scene) -> BTreeSet<GroupKey> {
    all_groups(before)
        .union(&all_groups(after))
        .copied()
        .filter(|&(level, group)| {
            group_prims(before, level as usize, group) != group_prims(after, level as usize, group)
        })
        .collect()
}

/// What a group that is not in `patch.grown` must be: every box it has in `after` lies inside
/// a box it had in `before`, so that the BLAS it had still covers all of it. The groups that
/// are new are not asked for it (they are always built).
fn assert_only_lost(before: &Scene, after: &Scene, patch: &RingPatch) -> usize {
    let mut waiting = 0;
    for &((level, group), _) in &patch.groups {
        if patch.grown.contains(&(level, group)) {
            continue;
        }
        let old = group_prims(before, level as usize, group);
        for new in group_prims(after, level as usize, group) {
            assert!(
                old.iter()
                    .any(|o| (0..3).all(|a| o.min[a] <= new.min[a] && o.max[a] >= new.max[a])),
                "level {level} group {group:?}: the box {new:?} is in no box it had"
            );
        }
        waiting += 1;
    }
    waiting
}

/// A patch's groups with the count each has in the scene it was made for.
fn listed(scene: &Scene, patch: &RingPatch) -> BTreeSet<GroupKey> {
    for &((level, group), count) in &patch.groups {
        assert_eq!(
            count,
            group_prims(scene, level as usize, group).len() as u32,
            "the count of {level} {group:?}"
        );
        assert_eq!(count, group_box_count(scene, level as usize, group));
    }
    patch.groups.iter().map(|&(key, _)| key).collect()
}

#[test]
fn an_edit_lists_only_the_groups_that_hold_the_regions_it_wrote_and_every_one_whose_boxes_moved() {
    // An Edit under the camera: level 0, a region's entries written.
    let (before, after, patch) = a_block_placed((0, 0), 8);
    let total = all_groups(&after).len();
    let groups = listed(&after, &patch);
    assert!(
        !patch.regions.is_empty() || !patch.rewritten.is_empty(),
        "{patch:?}"
    );
    assert!(
        !groups.is_empty() && groups.len() < total,
        "{groups:?} of {total}"
    );
    assert!(groups.iter().all(|&(level, _)| level == 0), "{groups:?}");
    // The patch says it all: a group whose boxes are not the same is in it.
    let moved = groups_that_differ(&before, &after);
    assert!(!moved.is_empty());
    assert!(
        moved.is_subset(&groups),
        "moved {moved:?}, listed {groups:?}"
    );
    // A block placed grows boxes: the group it is in is built at once (not allowed to wait), and
    // the groups that do not grow only lost.
    assert!(!patch.grown.is_empty(), "{patch:?}");
    assert!(patch.grown.iter().all(|g| groups.contains(g)));
    assert_only_lost(&before, &after, &patch);
}

#[test]
fn digging_the_surface_only_loses_boxes_so_its_groups_may_wait_and_placing_makes_them_grow() {
    for at in [(0, 0), (100, 100)] {
        // A pit of two Voxels at the surface: the bricks that were listed read with fewer Voxels.
        let (before, after, dug) = an_edit(at, 2, true);
        let groups = listed(&after, &dug);
        assert!(!groups.is_empty(), "nothing was dug at {at:?}");
        assert!(
            dug.grown.is_empty(),
            "a dig lists a group as grown at {at:?}: {:?}",
            dug.grown
        );
        assert_eq!(assert_only_lost(&before, &after, &dug), groups.len());
        let (before, after, placed) = a_block_placed(at, 2);
        assert!(!placed.grown.is_empty(), "no group grows at {at:?}");
        assert_only_lost(&before, &after, &placed);
    }
}

/// A dig that goes deep exposes bricks that the scene did not list (the interior is not): they
/// are new boxes, and the group may not wait for them.
#[test]
fn a_dig_that_exposes_the_interior_lists_its_group_as_grown() {
    let (before, after, dug) = an_edit((0, 0), 6, true);
    assert!(!dug.grown.is_empty(), "{dug:?}");
    assert_only_lost(&before, &after, &dug);
}

/// A mixed brick of a coarse level whose words are written in place: nothing of `regions`,
/// `table` or `brick_refs` says where, and the box made from the words is stale. The patch's
/// groups are made from the regions `write_region` was given, so it lists the group (the reviewer's
/// probe of #233: without it the hardware backend draws the ground that the block stands on).
#[test]
fn a_coarse_brick_written_in_place_lists_its_group() {
    let (before, after, patch) = a_block_placed((100, 100), 4);
    assert_eq!(
        (
            patch.regions.len(),
            patch.table.len(),
            patch.brick_refs.len()
        ),
        (0, 0, 0),
        "{patch:?}"
    );
    let groups = listed(&after, &patch);
    assert_eq!(groups, BTreeSet::from([(1, group_of([0, 11, 0]))]));
    assert!(groups_that_differ(&before, &after).is_subset(&groups));
}

#[test]
fn a_move_of_the_rings_lists_every_group_whose_boxes_moved_and_the_groups_it_emptied_with_a_count_of_0()
 {
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
    let mut merged: Option<RingPatch> = None;
    // Walk along the ground in steps a ring follows (not a jump), the patches kept as the
    // outbox keeps them: merged.
    for step in 1..=8 {
        let before = scene.mirror();
        let camera = [f64::from(step) * 40.0, 1565.0, 0.0];
        let Some(SceneUpdate::Rings(patch)) = scene.recentre(&store, camera).unwrap() else {
            continue;
        };
        let groups = listed(&scene, &patch);
        assert_only_lost(&before, &scene, &patch);
        let moved = groups_that_differ(&before, &scene);
        assert!(
            moved.is_subset(&groups),
            "step {step}: {moved:?} not in {groups:?}"
        );
        for &(key, count) in &patch.groups {
            if count == 0 {
                assert!(!all_groups(&scene).contains(&key));
            }
        }
        match &mut merged {
            Some(all) => all.merge(patch),
            None => merged = Some(patch),
        }
    }
    let merged = merged.expect("the rings moved");
    // Merged, each group has its count as the scene is now.
    assert_eq!(merged.groups.len(), listed(&scene, &merged).len());
}

#[test]
fn merged_patches_list_a_group_once_with_the_later_count() {
    let key = (0u8, [1, 2, 3]);
    let mut a = RingPatch {
        groups: vec![(key, 5), ((1, [0; 3]), 9)],
        ..RingPatch::default()
    };
    a.grown = vec![(1, [0; 3])];
    let b = RingPatch {
        groups: vec![(key, 7), ((0, [9, 9, 9]), 1)],
        grown: vec![key, (1, [0; 3])],
        ..RingPatch::default()
    };
    a.merge(b);
    // A group that grew in either is one that grew.
    assert_eq!(a.grown, vec![key, (1, [0; 3])]);
    assert_eq!(
        a.groups,
        vec![(key, 7), ((0, [9, 9, 9]), 1), ((1, [0; 3]), 9)]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_regions_box_count_is_the_boxes_the_cpu_makes_of_it_and_a_groups_the_sum() {
    let mut store = store_with(&[[1, 1, 1], [9, 1, 1], [9, 9, 9], [300, 1, 1]]);
    store.insert(
        BrickCoord([5, 5, 5]),
        brick_store::Brick::solid(Material(300)),
    );
    for scene in [
        Scene::new(&store, None),
        Scene::new(
            &BrickStore::new(),
            Some(Core {
                radius: 500,
                material: STONE,
            }),
        ),
    ] {
        let counts = group_counts(&scene);
        assert!(!counts.is_empty());
        for (&(level, group), &count) in &counts {
            assert!(count > 0);
            assert_eq!(
                count as usize,
                group_prims(&scene, level as usize, group).len()
            );
            assert_eq!(count, group_box_count(&scene, level as usize, group));
        }
        assert_eq!(
            counts.keys().copied().collect::<BTreeSet<_>>(),
            all_groups(&scene)
        );
    }
}

#[test]
fn a_mask_that_sets_no_voxel_is_the_whole_brick_here_as_on_the_gpu() {
    // No scene lists one, but a word that is not there is read as a zero on the GPU, and the CPU's
    // twin does the same, so that the boxes can be compared box for box.
    let mut scene = Scene::new(&store_with(&[[1, 1, 1]]), None);
    let at = scene.brick_refs[0] as usize;
    scene.bricks[at..at + 16].fill(0);
    let prims = group_prims(&scene, 0, [0, 0, 0]);
    assert_eq!(prims.len(), 1);
    assert_eq!((prims[0].min, prims[0].max), ([0.0; 3], [8.0; 3]));
}

#[test]
fn the_pools_room_is_given_out_first_fit_given_back_whole_and_grown_only_at_the_top() {
    let mut pool = Pool {
        capacity: 100,
        ..Pool::default()
    };
    assert_eq!(pool.alloc(30), Some(0));
    assert_eq!(pool.alloc(30), Some(30));
    assert_eq!(pool.alloc(30), Some(60));
    assert_eq!(pool.alloc(30), None, "10 are left above the top");
    assert_eq!(pool.used(), 90);
    // The middle one is given back and the next that fits goes there.
    pool.release(30..60);
    assert_eq!(pool.alloc(40), None);
    assert_eq!(pool.alloc(20), Some(30));
    assert_eq!(pool.alloc(10), Some(50));
    // Neighbours that are free join, and the top comes down with the last.
    pool.release(0..30);
    pool.release(30..50);
    assert_eq!(pool.free, vec![0..50]);
    pool.release(50..60);
    pool.release(60..90);
    assert_eq!((pool.top, pool.free.len(), pool.used()), (0, 0, 0));
}

#[test]
fn a_blas_is_a_cube_of_cells_that_is_a_function_of_the_cells_coordinate_and_the_levels_size() {
    let g = Grouping::split(4, 16, 2).unwrap();
    // Levels 0 and 1 have one cell a BLAS, the others 4 x 4 x 4.
    assert_eq!(g.regions(1), 4);
    assert_eq!(g.regions(2), 16);
    assert_eq!(g.blas_of(1, [3, -1, 0]), (1, [3, -1, 0]));
    assert_eq!(g.blas_of(2, [3, 0, 4]), (2, [0, 0, 1]));
    assert_eq!(g.blas_of(2, [-1, -4, -5]), (2, [-1, -1, -2]));
    // The cells of a BLAS are 64 of them, each in the BLAS that holds it, at the place the order
    // of x, y, z says.
    let cells: Vec<_> = g.cells(2, [-1, 0, 2]).collect();
    assert_eq!(cells.len(), 64);
    for (i, (cell, at)) in cells.iter().enumerate() {
        assert_eq!(g.blas_of(2, *cell), (2, [-1, 0, 2]));
        assert_eq!(*at, [i as u32 % 4, i as u32 / 4 % 4, i as u32 / 16]);
    }
    assert_eq!(
        g.cells(0, [5, 6, 7]).collect::<Vec<_>>(),
        [([5, 6, 7], [0; 3])]
    );
    // A BLAS's corner is its first cell's.
    assert_eq!(g.origin(0, [1, 0, 0]), group_origin(0, [1, 0, 0]));
    assert_eq!(g.origin(3, [1, 0, 0]), group_origin(3, [4, 0, 0]));
}

#[test]
fn the_programs_grouping_is_16_regions_for_levels_0_and_1_and_32_above() {
    let g = Grouping::parse(PRODUCTION_GROUPING).unwrap();
    assert_eq!([0, 1, 2, 3, 17].map(|l| g.regions(l)), [16, 16, 32, 32, 32]);
    assert_eq!(g.describe(), PRODUCTION_GROUPING);
}

#[test]
fn the_grouping_flag_takes_a_size_or_a_size_below_a_level_and_another_from_it() {
    assert_eq!(Grouping::parse("4").unwrap(), Grouping::default());
    assert_eq!(Grouping::parse("8").unwrap(), Grouping::uniform(8).unwrap());
    let split = Grouping::parse("8:16@3").unwrap();
    assert_eq!((split.regions(2), split.regions(3)), (8, 16));
    assert_eq!(split.describe(), "8:16@3");
    assert_eq!(Grouping::uniform(16).unwrap().describe(), "16");
    assert_eq!(
        Grouping::parse("16:32@3").unwrap(),
        Grouping::split(16, 32, 3).unwrap()
    );
    assert_eq!(Grouping::parse("32").unwrap().regions(0), 32);
    assert_eq!(Grouping::uniform(32).unwrap().cells(1, [0; 3]).count(), 512);
    // A third size from a second level up.
    let three = Grouping::parse("8:16@1:32@3").unwrap();
    assert_eq!(
        [0, 1, 2, 3, 4].map(|l| three.regions(l)),
        [8, 16, 16, 32, 32]
    );
    assert_eq!(three.describe(), "8:16@1:32@3");
    assert_eq!(Grouping::parse(&three.describe()).unwrap(), three);
    assert_eq!(Grouping::parse("16:32@2").unwrap().describe(), "16:32@2");
    for bad in [
        "",
        "3",
        "64",
        "8:16",
        "8:16@x",
        "a:b@1",
        "8:5@1",
        "8:16@3:32@2",
        "8:16@1:64@3",
        "8:16@1:32",
    ] {
        assert!(Grouping::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_biggest_allocation_that_is_new_is_told_from_the_sizes_before() {
    let at = |sizes: &[u64]| Reserve {
        sizes: sizes.to_vec(),
        ..Reserve::default()
    };
    // Nothing new, and a scratch buffer that is.
    assert_eq!(at(&[4, 8, 8]).largest_new(&at(&[4, 8, 8])), 0);
    assert_eq!(at(&[4, 8, 8, 4096]).largest_new(&at(&[4, 8, 8])), 4096);
    // A size that was there once and is there twice now is new.
    assert_eq!(at(&[4, 8, 8]).largest_new(&at(&[4, 8])), 8);
    // Allocations that are gone are not new, and the new one may be the smallest.
    assert_eq!(at(&[2, 4]).largest_new(&at(&[1, 4, 9])), 2);
    assert_eq!(at(&[]).largest_new(&at(&[5])), 0);
}
