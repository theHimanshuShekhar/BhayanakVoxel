//! The debug views on a hand-made scene, where each can be worked out. Needs a Vulkan device
//! (lavapipe in CI). That the default view is unchanged is the job of the golden images.

use brick_store::{BrickStore, Material, VoxelCoord};
use render::{Camera, DebugView, Image, Offscreen, Scene};

const STONE: Material = Material(1);
/// `albedo(1)` in `common.wgsl`.
const STONE_ALBEDO: [f64; 3] = [0.45, 0.43, 0.40];
const SIZE: u32 = 32;

/// A floor of stone, Voxels y -8..=-1 (its top face is at y = 0), x and z -64..=63.
fn floor() -> Scene {
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    Scene::new(&store, None)
}

/// 20 Voxels above the floor, looking straight down.
fn overhead() -> Camera {
    Camera::looking_at([0.5, 20.0, 0.5], [0.5, 0.0, 0.5])
}

fn skyward() -> Camera {
    Camera::looking_at([0.5, 20.0, 0.5], [0.5, 40.0, 0.5])
}

fn gpu(view: DebugView) -> Offscreen {
    let mut gpu = Offscreen::new(SIZE, SIZE, &floor()).unwrap();
    gpu.set_debug_view(view);
    gpu
}

fn pixel(image: &Image, x: u32, y: u32) -> [u8; 3] {
    let at = ((y * image.width + x) * 4) as usize;
    [image.rgba[at], image.rgba[at + 1], image.rgba[at + 2]]
}

fn near(actual: [u8; 3], expected: [f64; 3]) {
    for c in 0..3 {
        assert!(
            (f64::from(actual[c]) - expected[c]).abs() <= 2.0,
            "{actual:?} is not {expected:?}"
        );
    }
}

fn all_pixels(image: &Image) -> impl Iterator<Item = [u8; 3]> + '_ {
    (0..SIZE * SIZE).map(|i| pixel(image, i % SIZE, i / SIZE))
}

#[test]
fn normals_show_the_face_the_floor_presents_and_the_sky_is_black() {
    // The floor faces +y: (0, 1, 0) is (0.5, 1, 0.5) once scaled to 0..1.
    let image = gpu(DebugView::Normals).render(&overhead());
    assert!(all_pixels(&image).all(|p| {
        let d = |a: u8, b: f64| (f64::from(a) - b).abs() <= 2.0;
        d(p[0], 127.5) && d(p[1], 255.0) && d(p[2], 127.5)
    }));
    let sky = gpu(DebugView::Normals).render(&skyward());
    assert!(all_pixels(&sky).all(|p| p == [0; 3]));
}

#[test]
fn albedo_shows_the_material_without_light_or_shade() {
    let image = gpu(DebugView::Albedo).render(&overhead());
    let encoded = STONE_ALBEDO.map(|c| c.powf(1.0 / 2.2) * 255.0);
    assert!(
        all_pixels(&image).all(|p| (0..3).all(|c| (f64::from(p[c]) - encoded[c]).abs() <= 2.0))
    );
}

#[test]
fn depth_is_brighter_the_nearer_the_hit() {
    let image = gpu(DebugView::Depth).render(&overhead());
    // The middle looks straight down from 20 Voxels: 64 / (64 + 20) of full brightness.
    near(pixel(&image, 15, 15), [64.0 / 84.0 * 255.0; 3]);
    // A corner is further along its ray.
    assert!(pixel(&image, 0, 0)[0] < pixel(&image, 15, 15)[0]);
    let sky = gpu(DebugView::Depth).render(&skyward());
    assert!(all_pixels(&sky).all(|p| p == [0; 3]));
}

#[test]
fn the_sample_count_view_brightens_as_samples_accumulate() {
    let mut gpu = gpu(DebugView::SampleCount);
    // One sample: the dark end of the scale, (0.10, 0, 0.30).
    let one = gpu.render_reference(&overhead(), 1);
    near(pixel(&one, 3, 3), [25.5, 0.0, 76.5]);
    // Sixteen in all: 4 of the 10 doublings to 1024, so 0.4 of the way to (1, 0.9, 0.2).
    let sixteen = gpu.render_reference(&overhead(), 15);
    near(
        pixel(&sixteen, 3, 3),
        [255.0 * 0.46, 255.0 * 0.36, 255.0 * 0.26],
    );
    // The whole image shows one count, sky or not.
    assert!(all_pixels(&sixteen).all(|p| p == pixel(&sixteen, 3, 3)));
}

#[test]
fn a_debug_view_changes_the_picture_and_switching_back_restores_the_lit_image() {
    let mut gpu = gpu(DebugView::Lit);
    let lit = gpu.render(&overhead());
    gpu.set_debug_view(DebugView::Normals);
    assert_ne!(gpu.render(&overhead()), lit);
    gpu.set_debug_view(DebugView::Lit);
    assert_eq!(gpu.render(&overhead()), lit);
}

#[test]
fn the_reference_image_keeps_accumulating_while_a_debug_view_is_shown() {
    let mut a = gpu(DebugView::Lit);
    a.render_reference(&overhead(), 4);
    a.set_debug_view(DebugView::Depth);
    a.render_reference(&overhead(), 4);
    a.set_debug_view(DebugView::Lit);
    let switched = a.render_reference(&overhead(), 1);

    let straight = gpu(DebugView::Lit).render_reference(&overhead(), 9);
    assert_eq!(switched, straight);
}
