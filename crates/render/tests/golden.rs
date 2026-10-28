//! Golden image gate: a fixed view of the test Planet, rendered offscreen on lavapipe, must match
//! the approved PNG in `tests/golden/`.
//!
//! On a mismatch the test fails and writes `actual.png`, `expected.png` and `diff.png` to
//! `target/golden-failures/` (or `$GOLDEN_FAILURE_DIR`); CI uploads that directory.
//!
//! To approve a deliberate change to the image, rerun with `UPDATE_GOLDEN=1`
//! (`UPDATE_GOLDEN=1 cargo test -p render --test golden`), look at the new PNG, and commit it.
//!
//! The comparison tolerates what differs between Mesa versions and drivers: tiny rounding
//! differences move a few edge pixels to the neighbouring Voxel and nudge shading by a level or
//! two. A real regression (an albedo, the sun, a missed brick, a shifted camera) changes far more.

use std::fs;
use std::path::{Path, PathBuf};

use brick_store::BrickCoord;
use planet_gen::generate_region;
use render::{Camera, Image, Offscreen, Scene};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 144;
const SEED: u64 = 0xB4A7_A9AC;

/// Mean squared error per channel (0..=255 scale), over pixels that are not far off, above which
/// the images differ. Rounding noise is far below 1; shifting one channel of a third of the image
/// by 20 levels is over 40.
const MAX_MSE: f64 = 4.0;
/// A pixel is "far off" when any channel is off by more than this many levels.
const FAR_OFF: u8 = 40;
/// Most pixels that may be far off: edges where a ray lands on a neighbouring Voxel.
const MAX_FAR_OFF_FRACTION: f64 = 0.005;

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/planet_256x144.png")
}

fn failure_dir() -> PathBuf {
    std::env::var_os("GOLDEN_FAILURE_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/golden-failures"),
        PathBuf::from,
    )
}

/// The view: low over the surface near the +y pole, looking along it towards the horizon.
fn render_view() -> Image {
    // Voxels x -440..=447, y 1360..=1575, z -360..=263: the ground out to the horizon, from
    // deep in the rock to above the highest peak.
    let store = generate_region(SEED, BrickCoord([-55, 170, -45]), BrickCoord([55, 196, 32]));
    let scene = Scene::new(&store, None);
    let camera = Camera::looking_at([0.0, 1548.0, 250.0], [0.0, 1535.0, -300.0]);
    Offscreen::new(WIDTH, HEIGHT, &scene)
        .unwrap()
        .render(&camera)
}

fn write_png(path: &Path, image: &Image) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&image.rgba)
        .unwrap();
}

fn read_png(path: &Path) -> Image {
    let bytes = fs::read(path).unwrap_or_else(|e| {
        panic!(
            "no golden image at {}: {e}. Approve one with UPDATE_GOLDEN=1.",
            path.display()
        )
    });
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut rgba).unwrap();
    assert_eq!(
        info.color_type,
        png::ColorType::Rgba,
        "golden must be 8-bit RGBA"
    );
    assert_eq!(
        info.bit_depth,
        png::BitDepth::Eight,
        "golden must be 8-bit RGBA"
    );
    Image {
        width: info.width,
        height: info.height,
        rgba,
    }
}

/// How two images differ.
#[derive(Debug, PartialEq)]
struct Difference {
    /// Mean over RGB channels of the squared error, 0..=255 scale, of the pixels that are not far
    /// off. Far-off pixels are counted instead, so that a few flipped edges cannot dominate it.
    mse: f64,
    /// Pixels with a channel off by more than [`FAR_OFF`].
    far_off: usize,
}

fn compare(actual: &Image, expected: &Image) -> Difference {
    assert_eq!(
        (actual.width, actual.height),
        (expected.width, expected.height),
        "image sizes differ"
    );
    let mut squares = 0u64;
    let mut far_off = 0;
    for (a, e) in actual.rgba.chunks(4).zip(expected.rgba.chunks(4)) {
        let errors = [0, 1, 2].map(|c| a[c].abs_diff(e[c]));
        if errors.iter().any(|&d| d > FAR_OFF) {
            far_off += 1;
        } else {
            squares += errors
                .iter()
                .map(|&d| u64::from(d) * u64::from(d))
                .sum::<u64>();
        }
    }
    Difference {
        mse: squares as f64 / (3 * actual.width * actual.height) as f64,
        far_off,
    }
}

fn within_tolerance(difference: &Difference, pixels: usize) -> bool {
    difference.mse <= MAX_MSE && difference.far_off as f64 <= MAX_FAR_OFF_FRACTION * pixels as f64
}

/// Per-channel absolute difference, times 4 so small errors show, on an opaque image.
fn diff_image(actual: &Image, expected: &Image) -> Image {
    let rgba = actual
        .rgba
        .chunks(4)
        .zip(expected.rgba.chunks(4))
        .flat_map(|(a, e)| {
            let d = |c: usize| a[c].abs_diff(e[c]).saturating_mul(4);
            [d(0), d(1), d(2), 255]
        })
        .collect();
    Image {
        width: actual.width,
        height: actual.height,
        rgba,
    }
}

#[test]
fn the_test_planet_matches_the_approved_golden_image() {
    let actual = render_view();
    let path = golden_path();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        write_png(&path, &actual);
        eprintln!("wrote {}", path.display());
        return;
    }
    let expected = read_png(&path);
    let difference = compare(&actual, &expected);
    if !within_tolerance(&difference, (WIDTH * HEIGHT) as usize) {
        let dir = failure_dir();
        write_png(&dir.join("actual.png"), &actual);
        write_png(&dir.join("expected.png"), &expected);
        write_png(&dir.join("diff.png"), &diff_image(&actual, &expected));
        panic!(
            "render differs from {}: {difference:?} (limits: mse {MAX_MSE}, far off {} pixels). \
             Images are in {}. If the change is intended, rerun with UPDATE_GOLDEN=1.",
            path.display(),
            (MAX_FAR_OFF_FRACTION * f64::from(WIDTH * HEIGHT)) as usize,
            dir.display(),
        );
    }
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
    assert!(within_tolerance(&difference, PIXELS), "{difference:?}");
}

#[test]
fn a_shifted_albedo_over_a_third_of_the_image_fails() {
    let expected = flat([90, 120, 60]);
    let mut actual = expected.clone();
    for pixel in actual.rgba.chunks_mut(4).take(PIXELS / 3) {
        pixel[1] += 20;
    }
    let difference = compare(&actual, &expected);
    assert!(!within_tolerance(&difference, PIXELS), "{difference:?}");
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
    assert!(!within_tolerance(&difference, PIXELS), "{difference:?}");
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
    assert!(within_tolerance(&drift(1), PIXELS));
    assert!(!within_tolerance(&drift(3), PIXELS));
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
