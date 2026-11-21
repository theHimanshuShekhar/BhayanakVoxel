//! The sun-and-sky view of the convergence phase (#52, ADR 0005): how close Primary's shadows are
//! to the Reference's, in a view with nothing else in it.
//!
//! **The view** is the golden view's camera and sun ([`super::views::sun`]) drawn with the sun and
//! the sky alone: no GI (Primary has no ambient term, the Reference follows no path past its first
//! surface) and no emitted light, in both views. What is left to compare is the sun's disc, the
//! shadow rays and the sky behind the hills, which both views draw from the same disc and colour.
//!
//! **The scores.** The Reference's direct-sun image is truth A: [`Settings::truth_spp`] samples
//! (1024 by default, as the convergence views' truths are) at 1440p, with the seed of the
//! convergence phase's truth A. Truth B, with another seed, gives the metric's noise floor in this
//! view, and a line counts only where it is at least [`MIN_LINE_OVER_FLOOR`] times that floor.
//! Primary's image is drawn as the window draws it, traced at half the size and upscaled, with the
//! denoiser on (#55, [`render::Denoise::Full`]); it is scored twice, against truth A, by FLIP (mean
//! and 99th percentile, as the other views are):
//! - **`first_frame`**: the first frame of the view, with no history, which is what the player has
//!   the frame after the sun changes (the shadows have no history, and neither does the upscaler
//!   after a new sun). A few samples of the sun's disc a pixel
//!   ([`Settings::shadow_rays`]). **This is what the target of FLIP 0.05 is held to**
//!   (`meets_target`, reported and printed, no gate), as the ticket's acceptance has it. The 4090
//!   has it at 0.062 with 2 rays (ADR 0005's row), so the target is not met and is not loosened.
//! - **`settled`**: after [`Settings::settle_frames`] frames of a camera that has not moved, the
//!   upscaler's and the denoiser's histories having gathered samples. **This is what #55 gates**:
//!   FLIP 0.05 or under ([`super::gi::PRIMARY_TARGET_FLIP`]), as in every view.
//!
//! **The weights.** Primary is drawn with the shadow weight that `--shadow-weight` chose
//! ([`Settings::shadow_weight`]), which is what `first_frame` and `settled` are of, and again with
//! the other one (`other_weight`), and with the denoiser off (`raw`: #52's Primary), so that one run
//! has the comparison #55 asks for. The Reference at 64 samples (`reference_64`) is reported beside
//! them, not gated.
//!
//! **The shadow-edge error.** Where the sun is half hidden, Primary weighs its rays by the
//! cosine at the disc's centre and the Reference weighs each of its samples by its own cosine, so
//! the two differ most at a shadow's edge, and FLIP's mean over a whole image says little about a
//! few pixels of it. The edge is found from the Reference: a second image of the same view with the
//! shadow rays off ([`render::ReferenceOptions::no_shadow_rays`]) says how much of the sun each
//! pixel has, as the ratio of truth A to it ([`visible_sun`]), and a pixel is on an edge when it has
//! some of the sun and not all of it, or when its neighbours do not agree ([`EDGE_RADIUS`]). The
//! error is the mean and the 99th percentile of the absolute difference of the 8-bit luma that the
//! player sees ([`edge_error`]), in levels of 255, over the edge pixels and, beside it, over the
//! rest.
//!
//! **CI.** `--short` runs the view at a few dozen pixels with no FLIP: the edge error is made
//! without it, and the report has the right shape.

use std::error::Error;
use std::fs;
use std::path::Path;

use render::{Denoise, Image, Offscreen, ReferenceOptions, Resolution, ShadowWeight};
use serde::Serialize;

use super::score::{self, Flip};
use super::views::{self, Description};
use super::{
    Capture, FLIP_VERSION, FlipInfo, MIN_LINE_OVER_FLOOR, RUN_SEED, Seeds, TRUTH_SEEDS, write_png,
};

/// The FLIP level that Primary's shadows are held to (the ticket's, ADR 0005's "acceptable").
pub const TARGET_FLIP: f64 = 0.05;
/// A pixel has all of the sun above this share of the unshadowed light, and none of it below
/// [`DARK`]: between the two is a penumbra.
const LIT: f32 = 0.95;
const DARK: f32 = 0.05;
/// Where the sun is the other side of the middle.
const HALF: f32 = 0.5;
/// How far, in pixels, a pixel is from a crossing of [`HALF`] for it to be on an edge: the
/// pixels in a window this far from it in each direction are looked at.
pub const EDGE_RADIUS: usize = 2;
/// A pixel of the unshadowed image under this (linear luminance) is not lit by the sun at all,
/// whatever is in front of it: it is in the shade of its own face or is the sky, and is on no
/// shadow's edge.
const SUNLESS: f32 = 0.002;
/// The samples of the unshadowed image: it is only a divisor and has no noise to speak of past this
/// many, with the cosine of the sun's disc the only thing that varies.
const UNSHADOWED_SPP: u32 = 256;

/// What to run.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The Reference view's size, and Primary's output size. Primary traces at half of it.
    pub size: (u32, u32),
    /// Samples of truth A and truth B.
    pub truth_spp: u32,
    /// Frames of a still camera that Primary has for `settled`.
    pub settle_frames: u32,
    /// Whether to score with FLIP (off for `--short`).
    pub flip: bool,
    /// The shadow rays a pixel of Primary casts (`--shadow-rays`; the renderer's default if not
    /// given).
    pub shadow_rays: u32,
    /// How Primary weighs the shadow rays (`--shadow-weight`): `first_frame` and `settled` are of
    /// it, and the gate is on it.
    pub shadow_weight: ShadowWeight,
}

impl Settings {
    /// 1440p for the default `--size`, truths of the convergence views' size, 32 frames (twice the
    /// upscaler's 16-frame history).
    pub fn full(size: (u32, u32), truth_spp: u32) -> Self {
        Self {
            size,
            truth_spp,
            settle_frames: 32,
            flip: true,
            shadow_rays: render::DEFAULT_SHADOW_RAYS,
            shadow_weight: ShadowWeight::default(),
        }
    }

    /// A smoke test: a few pixels, a few samples, no FLIP.
    pub fn short(size: (u32, u32), truth_spp: u32) -> Self {
        Self {
            size,
            truth_spp,
            settle_frames: 4,
            flip: false,
            shadow_rays: render::DEFAULT_SHADOW_RAYS,
            shadow_weight: ShadowWeight::default(),
        }
    }
}

/// The report's `convergence.shadows` block.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ShadowsReport {
    pub name: &'static str,
    #[serde(flatten)]
    pub description: Description,
    /// The Reference view's size and Primary's output size, and what Primary traces.
    pub width: u32,
    pub height: u32,
    pub traced_width: u32,
    pub traced_height: u32,
    /// Both views have no GI and no emitters (see the module's account of the view).
    pub sun_and_sky_only: bool,
    /// The shadow rays a pixel of Primary cast towards the sun's disc.
    pub shadow_rays: u32,
    /// Samples of each truth, the unshadowed image's, and the seeds.
    pub truth_spp: u32,
    pub unshadowed_spp: u32,
    pub seeds: Seeds,
    /// How the images were scored; null where FLIP was not run (`--short`).
    pub flip: Option<FlipInfo>,
    /// FLIP of truth B against truth A: the metric's noise floor in this view.
    pub floor: Option<Scores>,
    /// The level Primary is held to, and whether it counts here: at least
    /// [`MIN_LINE_OVER_FLOOR`] times the floor's mean.
    pub target_flip: f64,
    pub min_line_over_floor: f64,
    pub target_counts: Option<bool>,
    /// Primary's first frame, with no history: what the next frame after a sun change shows. The
    /// denoiser is on and the shadow rays are weighed as `shadow_weight` says.
    pub first_frame: PrimaryScore,
    /// Primary after `settle_frames` frames of a still camera. **What #55 gates**
    /// (`convergence.gi.gates`, `primary-flip:sun`).
    pub settled: PrimaryScore,
    pub settle_frames: u32,
    /// The weight of the sun's shadow rays of `first_frame` and `settled`, and the other one's, drawn
    /// the same way in `other_weight`.
    pub shadow_weight: &'static str,
    pub other_shadow_weight: &'static str,
    pub other_weight: PrimaryFrames,
    /// Primary with the denoiser off (as #52 shipped it), the same weight as `shadow_weight`.
    pub raw: PrimaryFrames,
    /// The Reference at 64 samples against truth A: reported, not gated. Null without FLIP.
    pub reference_64: Option<Scores>,
    /// Whether the first frame's FLIP mean is at or under the target; null without FLIP. The
    /// settled image is reported beside it and is not what is held to the target.
    pub meets_target: Option<bool>,
    /// What share of the image is on a shadow's edge, which truth A's pixels decide.
    pub edge_share: f64,
    /// The files of this view in the output folder's `convergence/sun/`, sorted.
    pub files: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Scores {
    pub flip_mean: f64,
    pub flip_p99: f64,
}

impl From<Flip> for Scores {
    fn from(flip: Flip) -> Self {
        Self {
            flip_mean: flip.mean,
            flip_p99: flip.p99,
        }
    }
}

/// Primary's first frame and settled frame in one of the ways of drawing it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PrimaryFrames {
    pub first_frame: PrimaryScore,
    pub settled: PrimaryScore,
}

/// An image of Primary's, scored against truth A.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PrimaryScore {
    /// Null where FLIP was not run.
    pub flip: Option<Scores>,
    /// The error at the shadows' edges and away from them.
    pub edge: EdgeError,
}

/// The absolute difference of two images' 8-bit luma, in levels of 255.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct EdgeError {
    /// Pixels on a shadow's edge, and the mean and 99th percentile of the difference over them.
    pub edge_pixels: usize,
    pub edge_mean: Option<f64>,
    pub edge_p99: Option<f64>,
    /// The mean over every other pixel: what the error is where there is no edge.
    pub other_mean: Option<f64>,
}

/// How much of the sun each pixel has: truth A's linear luminance over the unshadowed image's, in
/// 0..=1 (both RGB, linear light), and `None` where the unshadowed image has no sun to lose
/// ([`SUNLESS`]): the sky, and faces turned from the sun.
pub fn visible_sun(truth: &[f32], unshadowed: &[f32]) -> Vec<Option<f32>> {
    assert_eq!(truth.len(), unshadowed.len());
    let luminance = |p: &[f32]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    truth
        .as_chunks::<3>()
        .0
        .iter()
        .zip(unshadowed.as_chunks::<3>().0)
        .map(|(t, u)| {
            let u = luminance(u);
            (u > SUNLESS).then(|| (luminance(t) / u).clamp(0.0, 1.0))
        })
        .collect()
}

/// The pixels on a shadow's edge, `width` by `height`, rows from the top, from [`visible_sun`]: a
/// pixel with some of the sun and not all of it (a penumbra), or with a pixel within
/// [`EDGE_RADIUS`] on each side of [`HALF`] (a hard edge, which a pixel's width of penumbra
/// is).
pub fn edge_pixels(sun: &[Option<f32>], (width, height): (usize, usize)) -> Vec<bool> {
    assert_eq!(sun.len(), width * height);
    let mut edge = vec![false; width * height];
    for y in 0..height {
        for x in 0..width {
            let Some(here) = sun[y * width + x] else {
                continue;
            };
            if here > DARK && here < LIT {
                edge[y * width + x] = true;
                continue;
            }
            let (mut lit, mut shaded) = (false, false);
            for ny in y.saturating_sub(EDGE_RADIUS)..=(y + EDGE_RADIUS).min(height - 1) {
                for nx in x.saturating_sub(EDGE_RADIUS)..=(x + EDGE_RADIUS).min(width - 1) {
                    if let Some(there) = sun[ny * width + nx] {
                        lit |= there >= HALF;
                        shaded |= there < HALF;
                    }
                }
            }
            edge[y * width + x] = lit && shaded;
        }
    }
    edge
}

/// The 8-bit luma of a pixel of an RGBA image (Rec. 709 weights on the encoded values, which is
/// what the eye is shown), in levels.
fn luma(rgba: &[u8]) -> f64 {
    0.2126 * f64::from(rgba[0]) + 0.7152 * f64::from(rgba[1]) + 0.0722 * f64::from(rgba[2])
}

/// The absolute difference of the luma of `test` and `truth` (RGBA, the same size), over the pixels
/// of `edge` and over the others. Only pixels the sun reaches at all count (`sun` is not `None`):
/// the sky and the faces turned from the sun are the same in both views and say nothing of shadows.
pub fn edge_error(test: &[u8], truth: &[u8], sun: &[Option<f32>], edge: &[bool]) -> EdgeError {
    assert_eq!(test.len(), truth.len());
    assert_eq!(test.len(), sun.len() * 4);
    let (mut on, mut off) = (Vec::new(), Vec::new());
    for (i, (t, r)) in test
        .as_chunks::<4>()
        .0
        .iter()
        .zip(truth.as_chunks::<4>().0)
        .enumerate()
    {
        if sun[i].is_none() {
            continue;
        }
        let difference = (luma(t) - luma(r)).abs();
        if edge[i] {
            on.push(difference);
        } else {
            off.push(difference);
        }
    }
    let mean = |v: &[f64]| (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64);
    on.sort_by(f64::total_cmp);
    // The nearest rank, as `Stats` has it.
    let p99 = (!on.is_empty()).then(|| on[((on.len() as f64 * 0.99).ceil() as usize).max(1) - 1]);
    EdgeError {
        edge_pixels: on.len(),
        edge_mean: mean(&on),
        edge_p99: p99,
        other_mean: mean(&off),
    }
}

/// An image of the edge mask, white on an edge: to see what was measured.
fn edge_image(edge: &[bool], (width, height): (u32, u32)) -> Image {
    Image {
        width,
        height,
        rgba: edge
            .iter()
            .flat_map(|&e| {
                let v = if e { 255 } else { 0 };
                [v, v, v, 255]
            })
            .collect(),
    }
}

/// Draws `samples` samples of the Reference view with `seed` and reads the image.
fn truth(gpu: &mut Offscreen, camera: &render::Camera, seed: u32, samples: u32) -> Capture {
    gpu.set_reference_seed(seed);
    for _ in 0..samples {
        gpu.reference_sample(camera);
    }
    Capture::take(gpu, camera)
}

/// Runs the view and writes its pictures to `out/convergence/sun/`.
pub fn run(settings: &Settings, out: &Path) -> Result<ShadowsReport, Box<dyn Error>> {
    let view = views::sun();
    let name = view.name;
    let (width, height) = settings.size;
    eprintln!(
        "convergence: {name} ({width}x{height}, sun and sky only): truths of {} samples, FLIP {}...",
        settings.truth_spp,
        if settings.flip { "on" } else { "off" }
    );
    let resolution = Resolution::scaled(settings.size, render::upscale::DEFAULT_SCALE);
    // The Reference at the output size, as a window has it, and Primary at the traced size.
    let mut gpu = Offscreen::with_full_reference(resolution, &view.scene)?;
    // The light and display of before #202, as the convergence views have, so that the numbers
    // compare from report to report: the shadows' look is the goldens'. `use_legacy_light` sets
    // the old sun too, so the view's own (the hero sun's geometry) is set after it, or the legacy
    // sun would be drawn and the report would name another.
    gpu.use_legacy_light();
    gpu.set_sun(view.sun);
    gpu.set_sun_and_sky_only(true);
    gpu.set_shadow_rays(settings.shadow_rays);
    gpu.set_keep_history(true);
    // Primary as the window draws it: the denoiser on (an `Offscreen` starts with it off).
    gpu.set_denoise(Denoise::Full);
    gpu.set_shadow_weight(settings.shadow_weight);

    // Truth A and B, then the unshadowed image: the Reference, with the same samples.
    let truth_a = truth(&mut gpu, &view.camera, TRUTH_SEEDS[0], settings.truth_spp);
    let truth_b = truth(&mut gpu, &view.camera, TRUTH_SEEDS[1], settings.truth_spp);
    gpu.set_reference_options(ReferenceOptions {
        no_shadow_rays: true,
        ..ReferenceOptions::default()
    });
    let unshadowed = truth(
        &mut gpu,
        &view.camera,
        TRUTH_SEEDS[0],
        UNSHADOWED_SPP.min(settings.truth_spp),
    );
    gpu.set_reference_options(ReferenceOptions::default());
    // The Reference at 64 samples, a frame's worth for a path tracer: reported beside Primary.
    let reference_64 = truth(
        &mut gpu,
        &view.camera,
        TRUTH_SEEDS[0],
        64.min(settings.truth_spp),
    );

    // Primary: the first frame of the view, and the frame after `settle_frames` of it, in each of the
    // ways: the chosen weight (denoised), the other weight (denoised), the chosen weight with no
    // denoiser. A change of either starts the history over.
    let other = match settings.shadow_weight {
        ShadowWeight::CentreCosine => ShadowWeight::PerSample,
        ShadowWeight::PerSample => ShadowWeight::CentreCosine,
    };
    let mut frames = |denoise: Denoise, weight: ShadowWeight| {
        gpu.set_denoise(denoise);
        gpu.set_shadow_weight(weight);
        let first = gpu.render_upscaled(&view.camera);
        let mut settled = first.clone();
        for _ in 1..settings.settle_frames.max(2) {
            settled = gpu.render_upscaled(&view.camera);
        }
        (first, settled)
    };
    let (first, settled) = frames(Denoise::Full, settings.shadow_weight);
    let (other_first, other_settled) = frames(Denoise::Full, other);
    let (raw_first, raw_settled) = frames(Denoise::Off, settings.shadow_weight);
    drop(gpu);

    let sun = visible_sun(&truth_a.linear, &unshadowed.linear);
    let size = (width as usize, height as usize);
    let edge = edge_pixels(&sun, size);
    let edge_share = edge.iter().filter(|&&e| e).count() as f64 / edge.len() as f64;

    let truth_rgb = score::rgb_of_rgba(&truth_a.image.rgba);
    let flip = |what: &str, test: &Image| -> Result<Option<Scores>, String> {
        if !settings.flip {
            return Ok(None);
        }
        eprintln!("convergence: {name}: FLIP of {what}...");
        score::flip(&truth_rgb, &score::rgb_of_rgba(&test.rgba), size).map(|f| Some(f.into()))
    };
    let floor = flip("truth B against truth A (the floor)", &truth_b.image)?;
    let scored = |what: &str, image: &Image| -> Result<PrimaryScore, String> {
        Ok(PrimaryScore {
            flip: flip(what, image)?,
            edge: edge_error(&image.rgba, &truth_a.image.rgba, &sun, &edge),
        })
    };
    let first_frame = scored("Primary's first frame", &first)?;
    let settled_score = scored("Primary, settled", &settled)?;
    let other_weight = PrimaryFrames {
        first_frame: scored("Primary's first frame, the other weight", &other_first)?,
        settled: scored("Primary, settled, the other weight", &other_settled)?,
    };
    let raw = PrimaryFrames {
        first_frame: scored("Primary's first frame, no denoiser", &raw_first)?,
        settled: scored("Primary, settled, no denoiser", &raw_settled)?,
    };
    let reference_64_score = flip("the Reference at 64 samples", &reference_64.image)?;

    let target_counts = floor.map(|f| TARGET_FLIP >= MIN_LINE_OVER_FLOOR * f.flip_mean);
    let meets_target = first_frame.flip.map(|f| f.flip_mean <= TARGET_FLIP);

    let folder = out.join(super::FOLDER).join(name);
    fs::create_dir_all(&folder).map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
    let mut files = Vec::new();
    for (file, image) in [
        ("truth-a.png", &truth_a.image),
        ("unshadowed.png", &unshadowed.image),
        ("primary-first.png", &first),
        ("primary-settled.png", &settled),
        ("primary-first-other-weight.png", &other_first),
        ("primary-settled-other-weight.png", &other_settled),
        ("primary-first-no-denoiser.png", &raw_first),
        ("primary-settled-no-denoiser.png", &raw_settled),
        ("reference-64.png", &reference_64.image),
        ("edges.png", &edge_image(&edge, (width, height))),
    ] {
        write_png(&folder.join(file), image)?;
        files.push(file.to_string());
    }
    files.sort();
    Ok(ShadowsReport {
        name,
        description: view.description,
        width,
        height,
        traced_width: resolution.internal.0,
        traced_height: resolution.internal.1,
        sun_and_sky_only: true,
        shadow_rays: settings.shadow_rays,
        truth_spp: settings.truth_spp,
        unshadowed_spp: UNSHADOWED_SPP.min(settings.truth_spp),
        seeds: Seeds {
            run: RUN_SEED,
            truth_a: TRUTH_SEEDS[0],
            truth_b: TRUTH_SEEDS[1],
        },
        flip: settings.flip.then_some(FlipInfo {
            crate_version: FLIP_VERSION,
            mode: "ldr",
            ppd: score::FLIP_PPD,
        }),
        floor,
        target_flip: TARGET_FLIP,
        min_line_over_floor: MIN_LINE_OVER_FLOOR,
        target_counts,
        first_frame,
        settled: settled_score,
        settle_frames: settings.settle_frames.max(2),
        shadow_weight: settings.shadow_weight.name(),
        other_shadow_weight: other.name(),
        other_weight,
        raw,
        reference_64: reference_64_score,
        meets_target,
        edge_share,
        files,
    })
}

/// The view in brief, for whoever is watching the run.
pub fn print_summary(report: &ShadowsReport) {
    let num =
        |v: Option<f64>, digits: usize| v.map_or("n/a".to_string(), |v| format!("{v:.digits$}"));
    eprintln!(
        "convergence, {} ({}x{}, sun and sky only, {} shadow rays): floor FLIP {}; {:.1} % of the \
         image is on a shadow's edge",
        report.name,
        report.width,
        report.height,
        report.shadow_rays,
        num(report.floor.map(|f| f.flip_mean), 4),
        report.edge_share * 100.0
    );
    let settled = format!("settled ({} frames)", report.settle_frames);
    for (what, p) in [
        (
            format!("denoised, {}, first frame", report.shadow_weight),
            &report.first_frame,
        ),
        (
            format!("denoised, {}, {settled}", report.shadow_weight),
            &report.settled,
        ),
        (
            format!("denoised, {}, first frame", report.other_shadow_weight),
            &report.other_weight.first_frame,
        ),
        (
            format!("denoised, {}, {settled}", report.other_shadow_weight),
            &report.other_weight.settled,
        ),
        (
            format!("no denoiser, {}, first frame", report.shadow_weight),
            &report.raw.first_frame,
        ),
        (
            format!("no denoiser, {}, {settled}", report.shadow_weight),
            &report.raw.settled,
        ),
    ] {
        eprintln!(
            "  Primary, {what}: FLIP mean {} p99 {}; edge error {} levels (p99 {}), elsewhere {}",
            num(p.flip.map(|f| f.flip_mean), 4),
            num(p.flip.map(|f| f.flip_p99), 4),
            num(p.edge.edge_mean, 2),
            num(p.edge.edge_p99, 1),
            num(p.edge.other_mean, 2),
        );
    }
    eprintln!(
        "  the Reference at 64 samples: FLIP mean {} (reported, not gated)",
        num(report.reference_64.map(|f| f.flip_mean), 4)
    );
    let note = match (report.meets_target, report.target_counts) {
        (Some(meets), Some(counts)) => format!(
            "{}{}",
            if meets { "met" } else { "NOT met" },
            if counts {
                ""
            } else {
                " (does not count here: under 4 times the floor)"
            }
        ),
        _ => "not scored".to_string(),
    };
    eprintln!(
        "  target FLIP <= {} (first frame): {note}",
        report.target_flip
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row of 8 pixels, lit up to x = 3, shaded from x = 4, with a single sky pixel.
    fn row() -> Vec<Option<f32>> {
        vec![
            Some(1.0),
            Some(1.0),
            Some(1.0),
            Some(1.0),
            Some(0.0),
            Some(0.0),
            Some(0.0),
            None,
        ]
    }

    #[test]
    fn the_sun_a_pixel_has_is_truth_over_the_light_with_no_shadow_rays() {
        // Three pixels RGB: lit, half lit, in shadow; and a pixel the sun does not reach.
        let unshadowed = [0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0];
        let truth = [
            0.5, 0.5, 0.5, 0.25, 0.25, 0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        let sun = visible_sun(&truth, &unshadowed);
        assert_eq!(sun, [Some(1.0), Some(0.5), Some(0.0), None]);
    }

    #[test]
    fn a_hard_edge_is_where_the_neighbours_do_not_agree_and_nowhere_else() {
        let sun = row();
        let edge = edge_pixels(&sun, (8, 1));
        // The window is 2 pixels each way: pixels 0 and 1 see only light, 2 to 5 see the step,
        // and 6 sees only shade (and the sky).
        assert_eq!(edge, [false, false, true, true, true, true, false, false]);
    }

    #[test]
    fn a_penumbra_is_an_edge_with_no_neighbour_across_it() {
        // Every pixel has a third of the sun: an edge by itself, a wide penumbra being all of it.
        let sun = vec![Some(0.33); 25];
        assert!(edge_pixels(&sun, (5, 5)).iter().all(|&e| e));
        // A flat lit patch and a flat shadow have no edge, and the sky is none.
        let flat = vec![Some(1.0); 25];
        assert!(edge_pixels(&flat, (5, 5)).iter().all(|&e| !e));
        let dark = vec![Some(0.0); 25];
        assert!(edge_pixels(&dark, (5, 5)).iter().all(|&e| !e));
        let sky = vec![None; 25];
        assert!(edge_pixels(&sky, (5, 5)).iter().all(|&e| !e));
    }

    /// An RGBA image of grey pixels with these levels.
    fn grey(levels: &[u8]) -> Vec<u8> {
        levels.iter().flat_map(|&v| [v, v, v, 255]).collect()
    }

    #[test]
    fn the_edge_error_is_the_luma_difference_on_the_edge_and_off_it_and_skips_pixels_without_sun() {
        let sun = row();
        let edge = [false, true, true, true, true, false, false, false];
        let truth = grey(&[100, 100, 100, 100, 20, 20, 20, 250]);
        // Off by 10 levels at the edge pixels 1 and 4, by 2 at the pixel 0 off the edge, and by 200
        // at the sky pixel, which does not count.
        let test = grey(&[102, 110, 100, 100, 30, 20, 20, 50]);
        let error = edge_error(&test, &truth, &sun, &edge);
        assert_eq!(error.edge_pixels, 4);
        // The weights of the channels add up to 1, to a rounding.
        assert!((error.edge_mean.unwrap() - 5.0).abs() < 1e-9, "{error:?}");
        assert!((error.edge_p99.unwrap() - 10.0).abs() < 1e-9, "{error:?}");
        // Pixels 0, 5 and 6: 2, 0 and 0.
        assert!((error.other_mean.unwrap() - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn an_image_with_no_edge_pixels_has_no_edge_error() {
        let sun = vec![Some(1.0); 4];
        let edge = vec![false; 4];
        let image = grey(&[1, 2, 3, 4]);
        let error = edge_error(&image, &image, &sun, &edge);
        assert_eq!(error.edge_pixels, 0);
        assert_eq!((error.edge_mean, error.edge_p99), (None, None));
        assert_eq!(error.other_mean, Some(0.0));
    }

    #[test]
    fn the_target_is_the_tickets_and_adr_0005s() {
        assert_eq!(TARGET_FLIP, 0.05);
        assert_eq!(MIN_LINE_OVER_FLOOR, 4.0);
    }
}
