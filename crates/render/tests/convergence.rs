//! How the reference path tracer converges, on a scene with a known answer (#187, ADR 0005): the
//! squared error of the `f32` accumulation against that answer falls as 1/N in the number of
//! samples, and two independent seeds differ by twice what each differs from the answer.
//!
//! This is the check that the bench's convergence phase rests on, at a small size, on lavapipe in
//! CI. It runs no FLIP: that is the bench's, on a real GPU. Needs a Vulkan device.

use brick_store::{BrickStore, Material, VoxelCoord};
use render::{Camera, Offscreen, Scene, Sun};

/// Rock in `materials.toml`.
const STONE: Material = Material(1);
const STONE_ALBEDO: [f64; 3] = [0.45, 0.43, 0.40];
/// `SUN_RADIANCE` in `pathtrace.wgsl`.
const SUN_RADIANCE: [f64; 3] = [1.6, 1.5, 1.35];
/// The sky's gradient in the ray's y, as `common.wgsl` has it.
const HORIZON: [f64; 3] = [0.75, 0.82, 0.92];
const ZENITH: [f64; 3] = [0.25, 0.45, 0.85];

const SIZE: u32 = 32;

/// A flat floor of rock: Voxels y -8..=-1 (its top face is at y = 0), x and z -64..=63.
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

/// 20 Voxels above the floor, looking straight down: every pixel sees the floor.
fn overhead() -> Camera {
    Camera::looking_at([0.5, 20.0, 0.5], [0.5, 0.0, 0.5])
}

/// The sun at 53 degrees from straight up, so that nothing clips.
fn sun() -> Sun {
    Sun::new([0.8, 0.6, 0.0], 0.02).unwrap()
}

fn offscreen() -> Offscreen {
    let mut offscreen = Offscreen::new(SIZE, SIZE, &floor()).unwrap();
    offscreen.set_sun(sun());
    offscreen
}

/// The light the floor sends up, in linear light, which every pixel of the image converges to. A
/// flat floor is never in the way of its own bounce, so it sees the sun and the sky only:
/// `albedo * (sun * cos(theta) + the sky averaged with the cosine weight)`, and the sky is a
/// gradient in y, whose cosine-weighted mean over the hemisphere about +y is 2/3.
fn answer() -> [f64; 3] {
    let cos_theta = f64::from(sun().direction()[1]);
    [0, 1, 2].map(|c| {
        let sky = HORIZON[c] + (ZENITH[c] - HORIZON[c]) * 2.0 / 3.0;
        STONE_ALBEDO[c] * (SUN_RADIANCE[c] * cos_theta + sky)
    })
}

/// The variance of one sample of one channel at one pixel, worked out. A sample's light is the
/// sun's (a constant here, up to the disc's tiny spread) plus the albedo times the sky in the
/// bounce direction, a gradient in the direction's y: `albedo * (horizon + (zenith - horizon) *
/// y)`. For a cosine-weighted direction `y` has the mean 2/3 and the variance 1/2 - 4/9 = 1/18.
fn variance_of_one_sample() -> [f64; 3] {
    [0, 1, 2].map(|c| (STONE_ALBEDO[c] * (ZENITH[c] - HORIZON[c])).powi(2) / 18.0)
}

/// Mean over the image's pixels and channels of the squared difference between the accumulation
/// and `want`, and the per-channel mean variance the answer predicts for one sample.
fn mse_against(accumulation: &[f32], want: [f64; 3]) -> f64 {
    let squares: f64 = accumulation
        .chunks_exact(3)
        .map(|p| (0..3).map(|c| (f64::from(p[c]) - want[c]).powi(2)).sum::<f64>())
        .sum();
    squares / accumulation.len() as f64
}

/// The accumulation of `samples` samples from `seed`.
fn accumulate(offscreen: &mut Offscreen, seed: u32, samples: u32) -> Vec<f32> {
    offscreen.set_reference_seed(seed);
    for _ in 0..samples {
        offscreen.reference_sample(&overhead());
    }
    assert_eq!(offscreen.reference_samples(), samples);
    let accumulation = offscreen.read_accumulation();
    assert_eq!(accumulation.len(), (SIZE * SIZE * 3) as usize);
    accumulation
}

const SEED: u32 = 0xB4A7_A9AC;
const COUNTS: [u32; 5] = [1, 4, 16, 64, 256];

#[test]
fn the_squared_error_against_a_known_answer_falls_as_one_over_n() {
    let mut offscreen = offscreen();
    let want = answer();
    let errors: Vec<f64> = COUNTS
        .iter()
        .map(|&n| mse_against(&accumulate(&mut offscreen, SEED, n), want))
        .collect();

    // The expected value is one over N times the variance of a sample: N * MSE is that variance
    // at every N. The estimate of it from 1024 pixels is good to a few per cent; a quarter either
    // way is far wider than that and far narrower than a bias or a wrong weight would be.
    let predicted = variance_of_one_sample().iter().sum::<f64>() / 3.0;
    for (&n, &error) in COUNTS.iter().zip(&errors) {
        let per_sample = error * f64::from(n);
        eprintln!("N = {n:>3}: MSE {error:.3e}, N * MSE {per_sample:.3e} (predicted {predicted:.3e})");
        assert!(
            (per_sample / predicted - 1.0).abs() < 0.25,
            "N = {n}: N * MSE = {per_sample:.3e}, the answer predicts {predicted:.3e}"
        );
    }

    // And the line through (ln N, ln MSE) has the slope -1: the error halves with each doubling
    // of the samples' count, to within what a few hundred samples of noise allow.
    let (xs, ys): (Vec<f64>, Vec<f64>) = COUNTS
        .iter()
        .zip(&errors)
        .map(|(&n, &e)| (f64::from(n).ln(), e.ln()))
        .unzip();
    let (mx, my) = (
        xs.iter().sum::<f64>() / xs.len() as f64,
        ys.iter().sum::<f64>() / ys.len() as f64,
    );
    let slope = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum::<f64>()
        / xs.iter().map(|x| (x - mx).powi(2)).sum::<f64>();
    assert!(
        (slope + 1.0).abs() < 0.1,
        "ln MSE against ln N has the slope {slope}, errors {errors:?}"
    );
}

#[test]
fn two_seeds_differ_by_twice_what_each_differs_from_the_answer() {
    // The floor of the convergence phase: two truths with different seeds are two independent
    // estimates of the answer, so their mean squared difference is the sum of their errors.
    let mut offscreen = offscreen();
    let want = answer();
    let n = 64;
    let a = accumulate(&mut offscreen, 1, n);
    let b = accumulate(&mut offscreen, 2, n);
    assert_ne!(a, b, "a seed changes the samples");
    let between: f64 = a
        .iter()
        .zip(&b)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    let each = mse_against(&a, want) + mse_against(&b, want);
    assert!(
        (between / each - 1.0).abs() < 0.2,
        "between the seeds {between:.3e}, each from the answer {each:.3e}"
    );
}

#[test]
fn the_same_seed_gives_the_same_accumulation_and_a_new_seed_starts_over() {
    let mut offscreen = offscreen();
    let first = accumulate(&mut offscreen, SEED, 8);
    // Another seed in between, then back: the same seed and count are the same image.
    accumulate(&mut offscreen, 7, 3);
    assert_eq!(accumulate(&mut offscreen, SEED, 8), first);
    // And the default seed is the one the tracer has always had.
    let mut fresh = self::offscreen();
    for _ in 0..8 {
        fresh.reference_sample(&overhead());
    }
    assert_eq!(fresh.read_accumulation(), first);
}

#[test]
fn the_image_that_is_read_back_is_the_accumulation_encoded() {
    // What the player sees is the mean, encoded with a gamma of 2.2 and clamped; the bench scores
    // the two and they must be of the same samples.
    let mut offscreen = offscreen();
    let accumulation = accumulate(&mut offscreen, SEED, 16);
    let image = offscreen.render_reference(&overhead(), 0);
    for (pixel, linear) in image.rgba.chunks_exact(4).zip(accumulation.chunks_exact(3)) {
        for c in 0..3 {
            let shown = f64::from(linear[c]).clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0;
            assert!(
                (f64::from(pixel[c]) - shown).abs() <= 1.0,
                "{} against {shown}",
                pixel[c]
            );
        }
    }
}
