//! The reference path tracer on a hand-made scene, where the right answer can be worked out.
//! Needs a Vulkan device (lavapipe in CI).

use brick_store::{BrickStore, Material, VoxelCoord};
use render::{Camera, Offscreen, Scene, Sun};

const STONE: Material = Material(1);
/// Rock in `materials.toml`.
const STONE_ALBEDO: [f32; 3] = [0.45, 0.43, 0.40];
/// `SUN_RADIANCE` in `pathtrace.wgsl`.
const SUN_RADIANCE: [f32; 3] = [1.6, 1.5, 1.35];

/// A flat floor of `material`: Voxels y -8..=-1 (the top face is at y = 0), x and z -64..=63.
fn floor_of(material: Material) -> Scene {
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(material));
            }
        }
    }
    Scene::new(&store, None)
}

fn floor() -> Scene {
    floor_of(STONE)
}

/// 20 Voxels above the floor, looking straight down.
fn overhead() -> Camera {
    Camera::looking_at([0.5, 20.0, 0.5], [0.5, 0.0, 0.5])
}

fn on_floor(sun: Sun) -> Offscreen {
    let mut offscreen = Offscreen::new(32, 32, &floor()).unwrap();
    offscreen.set_sun(sun);
    offscreen
}

fn mean_linear(image: &render::Image) -> [f64; 3] {
    let mut sum = [0.0; 3];
    for pixel in image.rgba.chunks(4) {
        for c in 0..3 {
            // Undo the display encoding (a gamma of 2.2).
            sum[c] += (f64::from(pixel[c]) / 255.0).powf(2.2);
        }
    }
    let n = f64::from(image.width * image.height);
    sum.map(|s| s / n)
}

#[test]
fn a_sunlit_floor_converges_to_the_light_it_should_reflect() {
    // The sun is at 53 degrees from straight up, so nothing clips. The floor sees only the sun and the
    // sky: a flat floor is never in the way of its own bounce, so the radiance leaving it is
    //   albedo * (sun * cos(theta) + the sky averaged with the cosine weight).
    // The sky is a gradient in the ray's y: horizon + (zenith - horizon) * y, and the cosine
    // weighted mean of y over the hemisphere about +y is 2/3.
    let sun = Sun::new([0.8, 0.6, 0.0], 0.02).unwrap();
    let cos_theta = f64::from(sun.direction()[1]);
    let (horizon, zenith) = ([0.75, 0.82, 0.92], [0.25, 0.45, 0.85]);
    let image = on_floor(sun).render_reference(&overhead(), 64);
    let got = mean_linear(&image);
    for c in 0..3 {
        let sky = horizon[c] + (zenith[c] - horizon[c]) * 2.0 / 3.0;
        let want = f64::from(STONE_ALBEDO[c]) * (f64::from(SUN_RADIANCE[c]) * cos_theta + sky);
        assert!(
            (got[c] - want).abs() < 0.02,
            "channel {c}: got {} want {want}",
            got[c]
        );
    }
}

#[test]
fn a_floor_in_the_shade_of_a_slab_gets_no_sun() {
    // The same floor with a slab 6 Voxels above it, under the sun: what is below gets only the
    // sky, which the slab also hides, so the floor under it is dark.
    let mut store = BrickStore::new();
    for x in -64..64 {
        for z in -64..64 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(STONE));
            }
        }
    }
    for x in -40..40 {
        for z in -40..40 {
            store.set(VoxelCoord([x, 6, z]), Some(STONE));
        }
    }
    let scene = Scene::new(&store, None);
    let mut offscreen = Offscreen::new(32, 32, &scene).unwrap();
    // Straight above: any ray from the floor under the middle of the slab that gets to the sun
    // passes through the slab.
    offscreen.set_sun(Sun::new([0.0, 1.0, 0.0], 0.02).unwrap());
    // Camera in the gap between floor and slab, looking down at the floor.
    let camera = Camera::looking_at([0.5, 3.0, 0.5], [0.5, 0.0, 0.5]);
    let shaded = mean_linear(&offscreen.render_reference(&camera, 64));
    let lit = mean_linear(
        &on_floor(Sun::new([0.0, 1.0, 0.0], 0.02).unwrap()).render_reference(&overhead(), 64),
    );
    for c in 0..3 {
        assert!(
            shaded[c] < 0.5 * lit[c],
            "channel {c}: {} vs {}",
            shaded[c],
            lit[c]
        );
    }
}

#[test]
fn the_same_seed_and_sample_count_give_the_same_image() {
    let sun = Sun::default();
    let a = on_floor(sun).render_reference(&overhead(), 8);
    let b = on_floor(sun).render_reference(&overhead(), 8);
    assert_eq!(a, b);
}

#[test]
fn a_still_camera_keeps_accumulating_where_it_left_off() {
    let sun = Sun::default();
    let straight = on_floor(sun).render_reference(&overhead(), 6);
    let mut in_parts = on_floor(sun);
    in_parts.render_reference(&overhead(), 2);
    in_parts.render_reference(&overhead(), 3);
    let parts = in_parts.render_reference(&overhead(), 1);
    assert_eq!(straight, parts);
}

#[test]
fn moving_the_camera_or_the_sun_starts_the_image_over() {
    let sun = Sun::default();
    let moved = Camera::looking_at([3.5, 20.0, 0.5], [0.5, 0.0, 0.5]);
    let fresh = on_floor(sun).render_reference(&moved, 3);

    let mut offscreen = on_floor(sun);
    offscreen.render_reference(&overhead(), 5);
    assert_eq!(offscreen.render_reference(&moved, 3), fresh);

    // The same with the sun: sample 0 of the new sun replaces the old mean.
    let other_sun = Sun::from_angles(2.0, 0.5, 0.02);
    let fresh = on_floor(other_sun).render_reference(&moved, 3);
    offscreen.set_sun(other_sun);
    assert_eq!(offscreen.render_reference(&moved, 3), fresh);
}

#[test]
fn more_samples_make_a_smoother_image() {
    // Noise is what is left of the difference between one run and another.
    let sun = Sun::default();
    let spread = |samples: u32| {
        let image = on_floor(sun).render_reference(&overhead(), samples);
        let mean = mean_linear(&image);
        let mut squares = 0.0;
        for pixel in image.rgba.chunks(4) {
            let v = (f64::from(pixel[1]) / 255.0).powf(2.2);
            squares += (v - mean[1]).powi(2);
        }
        squares / f64::from(image.width * image.height)
    };
    assert!(spread(64) < 0.25 * spread(2));
}

#[test]
fn a_light_source_adds_its_emission_to_what_it_reflects() {
    // The sun is below the horizon, so only the sky lights the floor: the light leaving it is
    // what it emits plus its albedo times the sky it sees (at most 0.92 here, the sky's brightest
    // channel, which the floor reflects only a part of).
    let table = materials::MaterialTable::built_in();
    let moss = table.by_name("glowing moss").unwrap();
    let emitted = f64::from(moss.emissive[0] * moss.emissive_intensity);
    let reflected = f64::from(moss.colour[0]) * 0.92;
    let mut offscreen = Offscreen::new(32, 32, &floor_of(Material(moss.id))).unwrap();
    offscreen.set_sun(Sun::from_angles(0.0, -0.5, 0.02));
    let got = mean_linear(&offscreen.render_reference(&overhead(), 32))[0];
    assert!(
        got >= emitted - 0.02 && got <= emitted + reflected,
        "{got} is not {emitted} plus up to {reflected}"
    );
}
