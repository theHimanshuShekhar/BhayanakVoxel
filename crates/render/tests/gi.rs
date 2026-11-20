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
fn the_upscaler_does_not_take_light_from_the_noise_of_one_sample_of_gi() {
    // The history is a mean of noisy samples; a mean in a perceptual encoding of noise is darker than
    // the noise's mean (a bright and a dark sample average to less than the light of the two).
    let scene = lit_room();
    let camera = room_camera();
    let mut gpu = gpu_of(&scene);
    warmed(&mut gpu, &camera, 60);
    let frames: Vec<Image> = (0..16).map(|_| gpu.render(&camera)).collect();
    let single = frames.iter().map(mean).sum::<f32>() / 16.0;
    let mut gpu = gpu_of(&scene);
    warmed(&mut gpu, &camera, 60);
    let upscaled = mean(&settled(&mut gpu, &camera, 40));
    let reference = mean(&gpu.render_reference(&camera, 128));
    eprintln!("single {single} upscaled {upscaled} reference {reference}");
    assert!(
        (upscaled / single - 1.0).abs() < 0.04,
        "one sample's frames have {single}, the upscaled has {upscaled}"
    );
}

/// Mean squared error per channel of two images, 0..=255.
fn mse(a: &Image, b: &Image) -> f64 {
    let sum: u64 = a
        .rgba
        .chunks(4)
        .zip(b.rgba.chunks(4))
        .flat_map(|(p, q)| (0..3).map(move |c| u64::from(p[c].abs_diff(q[c])).pow(2)))
        .sum();
    sum as f64 / (3 * a.width * a.height) as f64
}

#[test]
fn a_camera_moving_sideways_through_noisy_gi_is_not_smeared_by_the_history_the_clamp_lets_through()
{
    // The upscaler's clamp lets history stray beyond the range of the traced samples, because the
    // mean of noisy GI is often outside the range of one frame's samples. That slack must not let
    // a frame carry the light of where the camera was.
    let scene = lit_room();
    let resolution = Resolution {
        internal: (80, 45),
        output: (W, H),
    };
    let fresh = || {
        let mut gpu = Offscreen::with_resolution(resolution, &scene).unwrap();
        gpu.use_legacy_light();
        gpu.set_flat_ambient(false);
        gpu
    };
    let at = |frames: u32| {
        let mut camera = room_camera();
        let right = camera.right();
        camera.position =
            std::array::from_fn(|a| camera.position[a] + right[a] * 0.4 * f64::from(frames));
        camera
    };
    let frames = 24;
    // The cache is warm, and the history is of the first view.
    let mut moving = fresh();
    for _ in 0..60 {
        moving.render_upscaled(&at(0));
    }
    let mut image = moving.render_upscaled(&at(0));
    for frame in 1..=frames {
        image = moving.render_upscaled(&at(frame));
    }
    // What the camera sees now, settled; and what it saw 8 frames before, settled.
    let settled_at = |frame: u32| {
        let mut gpu = fresh();
        settled(&mut gpu, &at(frame), 100)
    };
    let now = mse(&image, &settled_at(frames));
    let unmoved = mse(&image, &settled_at(frames - 8));
    eprintln!("strafe with GI: {now:.2} against now, {unmoved:.2} against 8 frames back");
    assert!(
        now * 2.0 < unmoved,
        "the frame is no closer to the view it is of ({now}) than to an old one ({unmoved})"
    );
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
fn light_let_in_by_an_edit_shows_through_the_upscaler_within_half_a_second() {
    // As the window draws it: upscaled, with a history that would carry the dark side's old light
    // for a long time (16 frames of it is a time constant of 16 frames), and the Edit told to the
    // renderer, which shortens that history while the light settles.
    let mut store = walled_room();
    let mut scene = Scene::new(&store, None);
    let mut gpu = gpu_of(&scene);
    let camera = Camera::looking_at([4.0, 6.0, 34.0], [14.0, 3.0, 10.0]);
    let before = mean(&settled(&mut gpu, &camera, 90));
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
    gpu.edit_made(&changed);
    let series: Vec<f32> = (0..120)
        .map(|_| mean(&gpu.render_upscaled(&camera)))
        .collect();
    let end = series[119];
    assert!(end > before * 1.3, "the light shows: {before} to {end}");
    let at_30 = series[29];
    assert!(
        (at_30 - end).abs() < 0.1 * (end - before),
        "frame 30 is {at_30} of {before} to {end}"
    );
}

#[test]
fn a_wall_thinner_than_a_cell_keeps_the_light_of_the_lit_side_off_the_dark_one() {
    // The wall at x = 18..20 is inside one 8-voxel cell along x (16..24): the floors either side of
    // it must not be one cell, whatever order the two sides are looked at in.
    let scene = Scene::new(&walled_room(), None);
    let lit_side = Camera::looking_at([34.0, 6.0, 34.0], [26.0, 3.0, 12.0]);
    let dark_side = Camera::looking_at([4.0, 6.0, 34.0], [14.0, 3.0, 10.0]);
    for lit_first in [false, true] {
        let mut gpu = gpu_of(&scene);
        if lit_first {
            warmed(&mut gpu, &lit_side, 40);
        }
        let from_the_start = mean(&warmed(&mut gpu, &dark_side, 1));
        let after_a_while = mean(&warmed(&mut gpu, &dark_side, 60));
        if !lit_first {
            warmed(&mut gpu, &lit_side, 40);
        }
        let later = mean(&warmed(&mut gpu, &dark_side, 60));
        let reference = mean(&gpu.render_reference(&dark_side, 32));
        // The Reference has the dark side dark (the wall does not let light by).
        for (when, light) in [
            ("at first", from_the_start),
            ("after a while", after_a_while),
            ("after the lit side was seen", later),
        ] {
            assert!(
                light <= reference + 0.0002,
                "lit side first: {lit_first}: the dark side is {light} {when}, the Reference has {reference}"
            );
        }
    }
    let mut gpu = gpu_of(&scene);
    let lit = mean(&warmed(&mut gpu, &lit_side, 60));
    assert!(lit > 0.01, "the lit side is lit: {lit}");
}

/// The slots of the radiance cache (12 words each, `cache.wgsl`): the cells that hold light
/// (check, odd, and a weight), and the tombstones of the cells that were dropped (check 2).
fn cache_census(gpu: &mut Offscreen) -> (usize, usize, usize) {
    const TOMBSTONE: u32 = 2;
    let words = gpu.read_radiance_cache();
    let slots = words.chunks(12);
    let live: Vec<u32> = slots
        .clone()
        .filter(|s| s[0] & 1 == 1 && f32::from_bits(s[8]) > 0.0)
        .map(|s| s[0])
        .collect();
    let mut distinct = live.clone();
    distinct.sort_unstable();
    distinct.dedup();
    let dropped = slots.filter(|s| s[0] == TOMBSTONE).count();
    (live.len(), live.len() - distinct.len(), dropped)
}

#[test]
fn an_expired_cell_leaves_a_tombstone_so_the_cells_after_it_are_still_found() {
    // A free slot ends a probe, so a dropped cell's slot must not be free: the cells put beyond it
    // would not be found, and would be made again (twice in the table, until the first expired).
    // The two sides of the wall: no ray from one gets to the other, so the cells of the side that
    // is no longer looked at are fed by no one.
    let scene = Scene::new(&walled_room(), None);
    let mut gpu = gpu_of(&scene);
    let west = Camera::looking_at([4.0, 6.0, 34.0], [14.0, 3.0, 10.0]);
    let east = Camera::looking_at([34.0, 6.0, 34.0], [26.0, 3.0, 12.0]);
    warmed(&mut gpu, &west, 20);
    warmed(&mut gpu, &east, 20);
    let (both, _, none_yet) = cache_census(&mut gpu);
    assert_eq!(none_yet, 0, "nothing has expired yet");
    // 180 frames of looking only east, and the west's cells go: those that nothing east of it
    // feeds or reads.
    warmed(&mut gpu, &east, 200);
    let (live, duplicates, dropped) = cache_census(&mut gpu);
    assert!(
        dropped > 0,
        "cells expired ({both} then {live}, none dropped)"
    );
    assert!(live < both, "{both} cells, then {live}");
    assert_eq!(duplicates, 0, "a cell is in the table once");
    // Looking west again takes the tombstones' slots back, and makes each cell once.
    warmed(&mut gpu, &west, 20);
    let (_, duplicates, after) = cache_census(&mut gpu);
    assert_eq!(duplicates, 0, "a cell is in the table once");
    assert!(
        after < dropped,
        "the slots are used again: {dropped} then {after}"
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
