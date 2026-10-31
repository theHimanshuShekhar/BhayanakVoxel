//! Updating the GPU copy of the Planet after Edits must show what drawing it afresh would, and
//! must start the reference image over. Needs a Vulkan device (lavapipe in CI).

use brick_store::{Brick, BrickCoord, BrickStore, Material, VoxelCoord};
use render::scene::{Core, SceneUpdate};
use render::{Camera, Offscreen, Scene};

const STONE: Material = Material(1);
const SOIL: Material = Material(2);
const CORE: Core = Core {
    radius: 96,
    material: STONE,
};

/// Looking down at the top of a ball of stone with a soil crust, from outside.
fn camera() -> Camera {
    Camera::looking_at([0.0, 150.0, 0.0], [0.0, 80.0, 0.0])
}

/// A crust of soil over the core's top, in bricks of their own.
fn store() -> BrickStore {
    let mut store = BrickStore::new();
    for x in -4..4 {
        for z in -4..4 {
            store.insert(BrickCoord([x, 12, z]), Brick::solid(SOIL));
        }
    }
    store
}

/// Digs a ball like an Edit does and returns the bricks it changed.
fn dig(store: &mut BrickStore, centre: [i32; 3], radius: i32) -> Vec<BrickCoord> {
    let mut changed = std::collections::BTreeSet::new();
    for x in -radius..=radius {
        for y in -radius..=radius {
            for z in -radius..=radius {
                if x * x + y * y + z * z <= radius * radius {
                    let voxel = VoxelCoord([centre[0] + x, centre[1] + y, centre[2] + z]);
                    let brick = voxel.brick();
                    if store.brick(brick).is_none() && brick.within_ball(CORE.radius) {
                        store.insert(brick, Brick::solid(STONE));
                    }
                    if store.get(voxel).is_some() {
                        changed.insert(brick);
                        store.set(voxel, None);
                    }
                }
            }
        }
    }
    changed.into_iter().collect()
}

#[test]
fn an_updated_scene_draws_what_a_new_one_would() {
    let mut store = store();
    let mut scene = Scene::new(&store, Some(CORE));
    let mut gpu = Offscreen::new(48, 48, &scene).unwrap();
    let before = gpu.render(&camera());

    // A crater through the crust and into the core below it.
    let changed = dig(&mut store, [0, 98, 0], 14);
    let update = scene.update(&store, &changed).unwrap();
    assert!(matches!(update, SceneUpdate::Patched { .. }));
    gpu.update_scene(&scene, &update).unwrap();

    let fresh = Offscreen::new(48, 48, &Scene::new(&store, Some(CORE)))
        .unwrap()
        .render(&camera());
    let after = gpu.render(&camera());
    assert_ne!(after, before, "the crater shows");
    assert_eq!(after, fresh);
}

#[test]
fn an_update_starts_the_reference_image_over() {
    let mut store = store();
    let mut scene = Scene::new(&store, Some(CORE));
    let mut gpu = Offscreen::new(24, 24, &scene).unwrap();
    gpu.render_reference(&camera(), 3);

    let changed = dig(&mut store, [0, 98, 0], 14);
    let update = scene.update(&store, &changed).unwrap();
    gpu.update_scene(&scene, &update).unwrap();

    // Samples are seeded by their index, so an image that started over after the update equals
    // one that never saw the old scene.
    let mut fresh = Offscreen::new(24, 24, &Scene::new(&store, Some(CORE))).unwrap();
    assert_eq!(
        gpu.render_reference(&camera(), 2),
        fresh.render_reference(&camera(), 2)
    );
}

#[test]
fn a_rebuilt_scene_is_uploaded_whole() {
    let mut store = store();
    let mut scene = Scene::new(&store, Some(CORE));
    let mut gpu = Offscreen::new(32, 32, &scene).unwrap();
    let camera = Camera::looking_at([0.0, 400.0, 0.0], [0.0, 320.0, 0.0]);
    let before = gpu.render(&camera);
    // A brick far from every region: the scene has to be built again.
    store.insert(BrickCoord([0, 40, 0]), Brick::solid(SOIL));
    let update = scene.update(&store, &[BrickCoord([0, 40, 0])]).unwrap();
    assert_eq!(update, SceneUpdate::Rebuilt);
    gpu.update_scene(&scene, &update).unwrap();

    let mut fresh = Offscreen::new(32, 32, &Scene::new(&store, Some(CORE))).unwrap();
    let after = gpu.render(&camera);
    assert_ne!(after, before, "the brick shows");
    assert_eq!(after, fresh.render(&camera));
}

#[test]
fn many_updates_in_a_row_pack_the_scene_again_and_still_draw_right() {
    let mut store = store();
    let mut scene = Scene::new(&store, Some(CORE));
    let mut gpu = Offscreen::new(48, 48, &scene).unwrap();
    let mut rebuilt = 0;
    // Each dig replaces the records of the bricks it touches, so garbage piles up until the
    // scene is packed again (see `scene.rs` for the bound), which happens twice over these.
    for i in 0..120 {
        let changed = dig(&mut store, [i % 7 * 5 - 15, 98, i % 5 * 6 - 12], 12);
        let update = scene.update(&store, &changed).unwrap();
        rebuilt += usize::from(update == SceneUpdate::Rebuilt);
        gpu.update_scene(&scene, &update).unwrap();
    }
    assert!(rebuilt > 1, "the scene was packed again {rebuilt} times");

    let fresh = Offscreen::new(48, 48, &Scene::new(&store, Some(CORE)))
        .unwrap()
        .render(&camera());
    assert_eq!(gpu.render(&camera()), fresh);
}
