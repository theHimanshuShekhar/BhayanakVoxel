//! Golden image gate: fixed views of the test Planet, rendered offscreen on lavapipe, must match
//! the approved PNGs in `tests/golden/`: one of primary visibility, one of the reference path
//! tracer after a fixed number of samples, and two of the temporal upscaler (a camera that stays
//! still, and one that pans).
//!
//! On a mismatch the test fails and writes `actual.png`, `expected.png` and `diff.png` to a
//! folder named for the image in `target/golden-failures/` (or `$GOLDEN_FAILURE_DIR`); CI
//! uploads that directory.
//!
//! To approve a deliberate change to an image, rerun with `UPDATE_GOLDEN=1`
//! (`UPDATE_GOLDEN=1 cargo test -p render --test golden`), look at the new PNG, and commit it.
//!
//! The comparison tolerates what differs between Mesa versions and drivers: tiny rounding
//! differences move a few edge pixels to the neighbouring Voxel and nudge shading by a level or
//! two. A real regression (an albedo, the sun, a missed brick, a shifted camera) changes far more.

mod common;

use brick_store::BrickCoord;
use common::{Difference, PRIMARY, REFERENCE, check_golden, compare, diff_image, within_tolerance};
use planet_gen::generate_region;
use render::{Camera, Image, Offscreen, Resolution, Scene, Sun};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 144;
const SEED: u64 = 0xB4A7_A9AC;

/// A patch of the test Planet near the +y pole: Voxels x -440..=447, y 1360..=1575,
/// z -360..=263, the ground out to the horizon from deep in the rock to above the highest peak.
fn planet_patch() -> Scene {
    let store = generate_region(SEED, BrickCoord([-55, 170, -45]), BrickCoord([55, 196, 32]));
    Scene::new(&store, None)
}

/// The view: low over the surface near the +y pole, looking along it towards the horizon.
fn render_view() -> Image {
    let camera = Camera::looking_at([0.0, 1548.0, 250.0], [0.0, 1535.0, -300.0]);
    Offscreen::new(WIDTH, HEIGHT, &planet_patch())
        .unwrap()
        .render(&camera)
}

#[test]
fn the_test_planet_matches_the_approved_golden_image() {
    check_golden("planet_256x144.png", &render_view(), &PRIMARY);
}

/// The upscaler's view: the same as the primary visibility golden's, traced at 128x72 and
/// upscaled to the golden's 256x144.
fn upscaling() -> Offscreen {
    let resolution = Resolution {
        internal: (WIDTH / 2, HEIGHT / 2),
        output: (WIDTH, HEIGHT),
    };
    Offscreen::with_resolution(resolution, &planet_patch()).unwrap()
}

const UPSCALED_FRAMES: u32 = 24;

fn upscaled_view() -> Camera {
    Camera::looking_at([0.0, 1548.0, 250.0], [0.0, 1535.0, -300.0])
}

/// A camera that stays still for [`UPSCALED_FRAMES`] frames: the upscaler has gathered the
/// samples of a whole period of its jitter, and the image is as sharp as it gets.
fn render_still() -> Image {
    let camera = upscaled_view();
    let mut offscreen = upscaling();
    let mut image = offscreen.render_upscaled(&camera);
    for _ in 1..UPSCALED_FRAMES {
        image = offscreen.render_upscaled(&camera);
    }
    image
}

#[test]
fn a_still_camera_matches_the_approved_upscaled_golden_image() {
    check_golden("planet_taau_still_256x144.png", &render_still(), &PRIMARY);
}

/// A camera that turns about a third of a degree a frame (about one output pixel) to the left for
/// [`UPSCALED_FRAMES`] frames: the last frame, where the history has been moved along with the
/// camera 23 times and clamped to what the new samples allow.
fn render_pan() -> Image {
    let mut camera = upscaled_view();
    let mut offscreen = upscaling();
    let mut image = offscreen.render_upscaled(&camera);
    for _ in 1..UPSCALED_FRAMES {
        camera.turn(0.006, 0.0);
        image = offscreen.render_upscaled(&camera);
    }
    image
}

#[test]
fn a_panning_camera_matches_the_approved_upscaled_golden_image() {
    check_golden("planet_taau_pan_256x144.png", &render_pan(), &PRIMARY);
}

const SAMPLES: u32 = 64;

/// The reference path tracer's view: close to the terraced hillside, with a low sun to the side,
/// so that steps are lit on one face and shade the ground below them on the other.
fn render_reference() -> Image {
    let camera = Camera::looking_at([0.0, 1535.0, 120.0], [0.0, 1508.0, 20.0]);
    let mut offscreen = Offscreen::new(WIDTH, HEIGHT, &planet_patch()).unwrap();
    offscreen.set_sun(Sun::from_angles(0.9, 0.25, 0.04));
    offscreen.render_reference(&camera, SAMPLES)
}

/// The fixed seed makes the noise the same every run, so the image is compared like any other.
/// All the random numbers are integer maths; what can differ between Mesa versions is float
/// rounding, which sends a few paths to a neighbouring Voxel and so changes one sample of 64 at
/// a few pixels.
///
/// Mean squared error of changed renders against this image, measured (the limit is 1): a
/// different seed, which is all new noise, 4.06; no shadow rays 62; the sun 0.1 rad round the
/// sky 5.9; the sun's strength up 10% 5.1; one albedo channel up 0.05 about 38; the sun's
/// strength up 3% 0.6; hard shadows 0.9; one bounce fewer 0.02. A change too small to fail here
/// is for the primary-visibility golden and the analytic checks in `path_tracer.rs`.
#[test]
fn the_reference_path_tracer_matches_the_approved_golden_image() {
    check_golden(
        "planet_reference_256x144_64spp.png",
        &render_reference(),
        &REFERENCE,
    );
}

fn flat(rgb: [u8; 3]) -> Image {
    Image {
        width: WIDTH,
        height: HEIGHT,
        rgba: [rgb[0], rgb[1], rgb[2], 255].repeat((WIDTH * HEIGHT) as usize),
    }
}

const PIXELS: usize = (WIDTH * HEIGHT) as usize;

#[test]
fn identical_images_pass_with_no_error() {
    let a = flat([90, 120, 60]);
    assert_eq!(
        compare(&a, &a),
        Difference {
            mse: 0.0,
            far_off: 0
        }
    );
}

#[test]
fn rounding_noise_and_a_few_flipped_edge_pixels_pass() {
    let expected = flat([90, 120, 60]);
    let mut actual = expected.clone();
    for (i, pixel) in actual.rgba.chunks_mut(4).enumerate() {
        // Every pixel off by one level in one channel, and 40 far-off pixels (0.1%).
        pixel[i % 3] += 1;
        if i % 900 == 0 {
            pixel[0] = 255;
        }
    }
    let difference = compare(&actual, &expected);
    assert!(
        within_tolerance(&difference, PIXELS, &PRIMARY),
        "{difference:?}"
    );
}

#[test]
fn a_shifted_albedo_over_a_third_of_the_image_fails() {
    let expected = flat([90, 120, 60]);
    let mut actual = expected.clone();
    for pixel in actual.rgba.chunks_mut(4).take(PIXELS / 3) {
        pixel[1] += 20;
    }
    let difference = compare(&actual, &expected);
    assert!(
        !within_tolerance(&difference, PIXELS, &PRIMARY),
        "{difference:?}"
    );
}

#[test]
fn a_missing_patch_of_terrain_fails() {
    let expected = flat([90, 120, 60]);
    let mut actual = expected.clone();
    // 2% of the pixels turn to sky.
    for pixel in actual.rgba.chunks_mut(4).take(PIXELS / 50) {
        pixel[..3].copy_from_slice(&[190, 210, 235]);
    }
    let difference = compare(&actual, &expected);
    assert!(
        !within_tolerance(&difference, PIXELS, &PRIMARY),
        "{difference:?}"
    );
}

#[test]
fn a_small_global_brightness_drift_fails_only_when_it_is_large() {
    let expected = flat([90, 120, 60]);
    let drift = |d: u8| {
        let mut actual = expected.clone();
        actual
            .rgba
            .chunks_mut(4)
            .for_each(|p| p[..3].iter_mut().for_each(|c| *c += d));
        compare(&actual, &expected)
    };
    assert!(within_tolerance(&drift(1), PIXELS, &PRIMARY));
    assert!(!within_tolerance(&drift(3), PIXELS, &PRIMARY));
}

#[test]
fn the_diff_image_marks_where_the_images_differ() {
    let expected = flat([10, 10, 10]);
    let mut actual = expected.clone();
    actual.rgba[0] = 30;
    let diff = diff_image(&actual, &expected);
    assert_eq!(diff.rgba[..4], [80, 0, 0, 255]);
    assert_eq!(diff.rgba[4..8], [0, 0, 0, 255]);
}
