//! Primary's GI and emissive Voxels (#54, ADR 0005): the radiance cache and the bounce, held to the
//! Reference in a room lit by one emissive block. Needs a Vulkan device (lavapipe in CI).

mod common;

use brick_store::{BrickStore, Material, VoxelCoord};
use materials::MaterialTable;
use render::{Camera, DebugView, Image, Offscreen, Resolution, Scene};

fn material(name: &str) -> Material {
    Material(MaterialTable::built_in().by_name(name).unwrap().id)
}

/// A closed room of rock, 40 by 16 by 40 inside, with a block of warm light on the floor.
fn lit_room() -> Scene {
    let rock = material("rock");
    let lamp = material("warm light");
    let mut store = BrickStore::new();
    for x in -2..42 {
        for y in -2..18 {
            for z in -2..42 {
                let inside = (0..40).contains(&x) && (0..16).contains(&y) && (0..40).contains(&z);
                if !inside {
                    store.set(VoxelCoord([x, y, z]), Some(rock));
                }
            }
        }
    }
    for x in 28..32 {
        for y in 0..3 {
            for z in 8..12 {
                store.set(VoxelCoord([x, y, z]), Some(lamp));
            }
        }
    }
    Scene::new(&store, None)
}

fn room_camera() -> Camera {
    Camera::looking_at([4.0, 6.0, 34.0], [26.0, 3.0, 12.0])
}

const W: u32 = 160;
const H: u32 = 90;

/// An `Offscreen` of `scene` with the light of the old constants (no exposure to adapt, so the
/// brightness of a frame is the light's) and GI.
fn gpu_of(scene: &Scene) -> Offscreen {
    let mut gpu = Offscreen::with_resolution(Resolution::native(W, H), scene).unwrap();
    gpu.use_legacy_light();
    gpu.set_flat_ambient(false);
    gpu
}

/// `frames` frames of primary visibility with nothing carried between them but the radiance cache
/// (no upscaler: the upscaler's history clamps the noise of a sample of GI, and with it takes
/// a little of the light), the last one's image. What the cache holds is what this shows.
fn warmed(gpu: &mut Offscreen, camera: &Camera, frames: u32) -> Image {
    let mut image = gpu.render(camera);
    for _ in 1..frames {
        image = gpu.render(camera);
    }
    image
}

/// The mean luminance of an image.
fn mean(image: &Image) -> f32 {
    let l = luminance(image);
    l.iter().sum::<f32>() / l.len() as f32
}

/// `frames` frames of Primary as the window draws it (traced at the internal resolution and
/// upscaled), the last one's image.
fn settled(gpu: &mut Offscreen, camera: &Camera, frames: u32) -> Image {
    let mut image = gpu.render_upscaled(camera);
    for _ in 1..frames {
        image = gpu.render_upscaled(camera);
    }
    image
}

fn luminance(image: &Image) -> Vec<f32> {
    image
        .rgba
        .chunks(4)
        .map(|p| {
            let l = |c: u8| render::srgb_decode(f32::from(c) / 255.0);
            0.2126 * l(p[0]) + 0.7152 * l(p[1]) + 0.0722 * l(p[2])
        })
        .collect()
}

#[test]
fn a_room_lit_only_by_an_emissive_block_is_lit_as_the_reference_has_it() {
    let scene = lit_room();
    let camera = room_camera();
    let mut gpu = gpu_of(&scene);
    // The mean of several frames, so that one sample's noise is not what is measured.
    warmed(&mut gpu, &camera, 60);
    let frames: Vec<Image> = (0..8).map(|_| gpu.render(&camera)).collect();
    let p = frames.iter().map(mean).sum::<f32>() / 8.0;
    let reference = gpu.render_reference(&camera, 128);
    let r = mean(&reference);
    eprintln!("primary {p} reference {r}");
    assert!(r > 0.03, "the room is lit: {r}");
    assert!(
        (p / r - 1.0).abs() < 0.08,
        "Primary's room is {p} bright, the Reference's {r}"
    );
    // The far wall and the ceiling are lit by what the lamp's light bounced off: not black.
    let corner = luminance(&frames[7])[(5 * W + W / 2) as usize];
    assert!(corner > 0.02, "the ceiling is {corner}");
}

#[test]
fn the_same_room_with_the_lamp_replaced_by_rock_is_dark() {
    let rock = material("rock");
    let mut store = BrickStore::new();
    for x in -2..42 {
        for y in -2..18 {
            for z in -2..42 {
                let inside = (0..40).contains(&x) && (0..16).contains(&y) && (0..40).contains(&z);
                if !inside {
                    store.set(VoxelCoord([x, y, z]), Some(rock));
                }
            }
        }
    }
    let mut gpu = gpu_of(&Scene::new(&store, None));
    let dark = mean(&warmed(&mut gpu, &room_camera(), 40));
    let mut lit = gpu_of(&lit_room());
    let lit = mean(&warmed(&mut lit, &room_camera(), 40));
    assert!(dark < 0.01 && lit > 20.0 * dark, "dark {dark}, lit {lit}");
}

#[test]
fn the_emissive_room_is_the_same_on_a_second_run() {
    let run = || {
        let mut gpu = gpu_of(&lit_room());
        settled(&mut gpu, &room_camera(), 12)
    };
    assert_eq!(run(), run());
}

#[test]
fn the_cache_debug_view_draws_cells_where_the_cache_has_light_and_black_where_it_has_none() {
    let mut gpu = gpu_of(&lit_room());
    gpu.set_debug_view(DebugView::CacheCells);
    let camera = room_camera();
    // The first frame feeds a sixteenth of the pixels' cells and fills the cells that the bounces
    // landed on that had none: by the third the room's surfaces are all there.
    let image = settled(&mut gpu, &camera, 3);
    let pixels: Vec<[u8; 3]> = image.rgba.chunks(4).map(|p| [p[0], p[1], p[2]]).collect();
    let coloured = pixels.iter().filter(|p| **p != [0; 3]).count();
    assert!(
        coloured * 10 > pixels.len() * 9,
        "{coloured} of {} pixels show a cell",
        pixels.len()
    );
    // Neighbouring cells have colours of their own: many different ones in the picture.
    let distinct: std::collections::BTreeSet<_> = pixels.iter().collect();
    assert!(distinct.len() > 20, "{} colours", distinct.len());
    // Where nothing is hit it is black: a view of the sky.
    let mut sky = gpu_of(&Scene::default());
    sky.set_debug_view(DebugView::CacheCells);
    let image = settled(
        &mut sky,
        &Camera::looking_at([0.0, 0.0, 0.0], [0.0, 1.0, 0.1]),
        2,
    );
    assert!(image.rgba.chunks(4).all(|p| p[..3] == [0; 3]));
    // And the Reference, which has no cache, shows its image.
    gpu.set_debug_view(DebugView::Lit);
    let lit = gpu.render_reference(&camera, 4);
    gpu.set_debug_view(DebugView::CacheCells);
    assert_eq!(gpu.render_reference(&camera, 0), lit);
}

#[test]
fn light_let_into_a_dark_side_of_a_room_shows_within_half_a_second_and_settles_to_the_reference() {
    let mut store = walled_room();
    let mut scene = Scene::new(&store, None);
    let mut gpu = gpu_of(&scene);
    let camera = Camera::looking_at([4.0, 6.0, 34.0], [14.0, 3.0, 10.0]);
    let before = mean(&warmed(&mut gpu, &camera, 60));
    let mut changed = std::collections::BTreeSet::new();
    for x in 18..20 {
        for y in 2..10 {
            for z in 14..24 {
                let v = VoxelCoord([x, y, z]);
                changed.insert(v.brick());
                store.set(v, None);
            }
        }
    }
    let changed: Vec<_> = changed.into_iter().collect();
    let update = scene.update(&store, &changed).unwrap();
    gpu.update_scene(&scene, &update).unwrap();
    let series: Vec<f32> = (0..90).map(|_| mean(&gpu.render(&camera))).collect();
    let settled_at = series[89];
    let reference = mean(&gpu.render_reference(&camera, 128));
    assert!(
        (settled_at / reference - 1.0).abs() < 0.12,
        "settles to {settled_at}, the Reference has {reference}"
    );
    assert!(
        settled_at > before * 1.3,
        "the light shows: {before} to {settled_at}"
    );
    // Half a second at 60 Hz is 30 frames: by then it is within a tenth of the way.
    let at_30 = series[29];
    assert!(
        (at_30 - settled_at).abs() < 0.1 * (settled_at - before),
        "frame 30 is {at_30} of {before} to {settled_at}"
    );
}

#[test]
fn the_emissive_room_is_in_the_approved_golden_images() {
    let scene = lit_room();
    let camera = room_camera();
    let mut gpu = gpu_of(&scene);
    let primary = settled(&mut gpu, &camera, 40);
    common::check_golden("emissive_room_primary.png", &primary, &common::PRIMARY);
    let reference = gpu.render_reference(&camera, 128);
    common::check_golden(
        "emissive_room_reference.png",
        &reference,
        &common::REFERENCE,
    );
}

/// The room with a wall across it that the lamp's light does not get through, at x = 18..20.
fn walled_room() -> BrickStore {
    let rock = material("rock");
    let lamp = material("warm light");
    let mut store = BrickStore::new();
    for x in -2..42 {
        for y in -2..18 {
            for z in -2..42 {
                let inside = (0..40).contains(&x) && (0..16).contains(&y) && (0..40).contains(&z);
                if !inside || (18..20).contains(&x) {
                    store.set(VoxelCoord([x, y, z]), Some(rock));
                }
            }
        }
    }
    for x in 28..32 {
        for y in 0..3 {
            for z in 8..12 {
                store.set(VoxelCoord([x, y, z]), Some(lamp));
            }
        }
    }
    store
}
