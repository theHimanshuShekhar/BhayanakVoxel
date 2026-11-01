//! THROWAWAY (#99): the temporary shader variants and read-backs of the measurement tooling agree
//! with the production reference kernel. Needs a Vulkan device (lavapipe in CI).

use brick_store::{BrickStore, Material, VoxelCoord};
use render::{Camera, Offscreen, Resolution, Scene, Sun, Variant};

fn scene() -> Scene {
    let mut store = BrickStore::new();
    for x in -32..32 {
        for z in -32..32 {
            for y in -8..0 {
                store.set(VoxelCoord([x, y, z]), Some(Material(1)));
            }
        }
    }
    // A wall, so that there is something to bounce off.
    for x in -32..32 {
        for z in 4..6 {
            for y in 0..12 {
                store.set(VoxelCoord([x, y, z]), Some(Material(1)));
            }
        }
    }
    Scene::new(&store, None)
}

fn camera() -> Camera {
    Camera::looking_at([0.5, 10.0, -14.0], [0.5, 2.0, 2.0])
}

fn gpu() -> Offscreen {
    let mut gpu = Offscreen::new(48, 32, &scene()).unwrap();
    gpu.set_sun(Sun::from_angles(0.5, 0.7, 0.04));
    gpu
}

fn max_difference(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max)
}

#[test]
fn the_production_variant_with_k_samples_a_dispatch_matches_production_samples() {
    let camera = camera();
    let mut production = gpu();
    for _ in 0..8 {
        production.reference_sample(&camera);
    }
    let want = production.read_accumulation();
    for k in [1, 2, 4] {
        let mut variant = gpu();
        for dispatch in 0..8 / k {
            variant.variant_dispatch(
                &camera,
                Variant {
                    k,
                    ..Variant::PRODUCTION
                },
                dispatch * k,
            );
        }
        let got = variant.read_accumulation();
        // The same random streams; only the order of the float additions differs.
        assert!(max_difference(&got, &want) < 2e-3, "k = {k}");
    }
}

#[test]
fn fewer_bounces_and_no_shadow_rays_change_the_image() {
    let camera = camera();
    let mut full = gpu();
    full.variant_dispatch(&camera, Variant::PRODUCTION, 0);
    let full = full.read_accumulation();
    for variant in [
        Variant {
            bounces: 0,
            ..Variant::PRODUCTION
        },
        Variant {
            shadow: false,
            ..Variant::PRODUCTION
        },
    ] {
        let mut other = gpu();
        other.variant_dispatch(&camera, variant, 0);
        assert!(max_difference(&other.read_accumulation(), &full) > 0.01);
    }
}

#[test]
fn another_seed_is_another_image_and_the_same_seed_the_same() {
    let camera = camera();
    let image = |seed| {
        let mut gpu = gpu();
        gpu.set_seed(seed);
        for _ in 0..4 {
            gpu.reference_sample(&camera);
        }
        gpu.read_accumulation()
    };
    assert_eq!(image(7), image(7));
    assert!(max_difference(&image(7), &image(8)) > 0.01);
}

#[test]
fn the_blit_is_the_traced_image_at_the_traced_size_and_stretched_to_the_output_size() {
    let camera = camera();
    let mut gpu = gpu();
    let traced = gpu.render_reference(&camera, 2);
    assert_eq!(gpu.blit_reference(), traced);

    let mut half = Offscreen::with_resolution(Resolution::scaled((96, 64), 0.5), &scene()).unwrap();
    half.set_sun(Sun::from_angles(0.5, 0.7, 0.04));
    half.reference_sample(&camera);
    let shown = half.blit_reference();
    assert_eq!((shown.width, shown.height), (96, 64));
    assert_eq!(half.read_accumulation().len(), 48 * 32 * 3);
}
