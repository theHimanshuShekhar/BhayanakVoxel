//! What the render tests share: for the golden image tests, reading and writing the PNGs, comparing
//! two images, and the rule for approving a new one (see `golden.rs`); and for the tests of the
//! GPU `trace()`, a ray generator and the check that it agrees with the CPU reference.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use render::reference::{Ray, trace};
use render::{Image, Offscreen, Scene, camera};

/// A pixel is "far off" when any channel is off by more than this many levels.
pub const FAR_OFF: u8 = 40;

/// How much two images may differ and still match.
pub struct Tolerance {
    /// Mean squared error per channel (0..=255 scale), over pixels that are not far off.
    pub max_mse: f64,
    /// Most pixels, as a fraction of the image, that may be far off.
    pub max_far_off_fraction: f64,
}

/// Rounding noise is far below an MSE of 1; shifting one channel of a third of the image by 20
/// levels is over 40. Far-off pixels are edges where a ray lands on a neighbouring Voxel.
pub const PRIMARY: Tolerance = Tolerance {
    max_mse: 4.0,
    max_far_off_fraction: 0.005,
};

/// A path-traced image has no hard edges to flip, only noise that is the same every run, so it
/// can be held much closer.
pub const REFERENCE: Tolerance = Tolerance {
    max_mse: 1.0,
    max_far_off_fraction: 0.002,
};

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn failure_dir() -> PathBuf {
    std::env::var_os("GOLDEN_FAILURE_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/golden-failures"),
        PathBuf::from,
    )
}

pub fn write_png(path: &Path, image: &Image) {
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

pub fn read_png(path: &Path) -> Image {
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
pub struct Difference {
    /// Mean over RGB channels of the squared error, 0..=255 scale, of the pixels that are not far
    /// off. Far-off pixels are counted instead, so that a few flipped edges cannot dominate it.
    pub mse: f64,
    /// Pixels with a channel off by more than [`FAR_OFF`].
    pub far_off: usize,
}

pub fn compare(actual: &Image, expected: &Image) -> Difference {
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

pub fn within_tolerance(difference: &Difference, pixels: usize, tolerance: &Tolerance) -> bool {
    difference.mse <= tolerance.max_mse
        && difference.far_off as f64 <= tolerance.max_far_off_fraction * pixels as f64
}

/// Per-channel absolute difference, times 4 so small errors show, on an opaque image.
pub fn diff_image(actual: &Image, expected: &Image) -> Image {
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

/// Checks `actual` against the approved image `name`, or approves it under `UPDATE_GOLDEN`.
pub fn check_golden(name: &str, actual: &Image, tolerance: &Tolerance) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        write_png(&path, actual);
        eprintln!("wrote {}", path.display());
        return;
    }
    let expected = read_png(&path);
    let difference = compare(actual, &expected);
    let pixels = (actual.width * actual.height) as usize;
    if !within_tolerance(&difference, pixels, tolerance) {
        let dir = failure_dir().join(name.trim_end_matches(".png"));
        write_png(&dir.join("actual.png"), actual);
        write_png(&dir.join("expected.png"), &expected);
        write_png(&dir.join("diff.png"), &diff_image(actual, &expected));
        panic!(
            "render differs from {}: {difference:?} (limits: mse {}, far off {} pixels). \
             Images are in {}. If the change is intended, rerun with UPDATE_GOLDEN=1.",
            path.display(),
            tolerance.max_mse,
            (tolerance.max_far_off_fraction * pixels as f64) as usize,
            dir.display(),
        );
    }
}

/// A small deterministic generator, so the ray sets are fixed.
pub struct Lcg(pub u64);

impl Lcg {
    pub fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / ((1u64 << 24) as f32)
    }
}

/// A ray from `position` (f64, Planet frame) towards `towards`.
pub fn ray(position: [f64; 3], towards: [f64; 3]) -> Ray {
    let d = [0, 1, 2].map(|a| towards[a] - position[a]);
    let len = d.iter().map(|c| c * c).sum::<f64>().sqrt();
    let (cell, origin) = camera::split(position);
    Ray {
        cell,
        origin,
        dir: d.map(|c| (c / len) as f32),
    }
}

pub fn assert_agree(label: &str, rays: &[Ray], scene: &Scene) -> usize {
    let gpu = Offscreen::new(8, 8, scene).unwrap().trace_rays(rays);
    assert_eq!(gpu.len(), rays.len());
    let mut hits = 0;
    for (i, (r, got)) in rays.iter().zip(&gpu).enumerate() {
        let want = trace(scene, r);
        let context = format!("{label} ray {i}: {r:?}\n gpu {got:?}\n cpu {want:?}");
        match (got, &want) {
            (None, None) => {}
            (Some(g), Some(w)) => {
                hits += 1;
                assert_eq!(
                    (g.voxel, g.normal, g.material),
                    (w.voxel, w.normal, w.material),
                    "{context}"
                );
                assert!((g.t - w.t).abs() <= 1e-3 * (1.0 + w.t.abs()), "{context}");
                for a in 0..3 {
                    assert!(
                        (g.position[a] - w.position[a]).abs() <= 1e-3 * (1.0 + w.t.abs()),
                        "{context}"
                    );
                }
            }
            _ => panic!("hit and miss disagree\n{context}"),
        }
    }
    hits
}
