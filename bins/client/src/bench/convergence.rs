//! The convergence phase of `client bench` (#187, ADR 0005): how fast the Reference view's image
//! gets clean, on two still views, in numbers that can be set beside the numbers of another build.
//!
//! **What it measures.** The Reference view traces at the output resolution (2560x1440 for the
//! default `--size`), one sample a dispatch, timed as the flight's reference mode times them, so
//! its `reference` and `gpu_total` stay comparable with that mode's (the report also gives the
//! cost in ms per megapixel per sample). After 1, 4, 16, 64, 256 and 1024 samples the image is
//! read back, and when the last sample is drawn every checkpoint is scored against a **truth**:
//! the same view with another seed, at 1024 samples (`--truth-spp 8192` for ADR 0005's). A second
//! truth with a third seed gives the metric's noise floor in that view.
//!
//! **The scores** are FLIP (mean and 99th percentile, LDR, ppd [`score::FLIP_PPD`], `flip-rs`
//! [`FLIP_VERSION`]) on the 8-bit image the player sees (drawn with the true sRGB curve since #188;
//! it was a gamma of 2.2 before, and `--flip-gamma-2-2` scores that too, from the same samples,
//! so that a change of the curve can be told from a change of the estimator), and the squared error of the `f32`
//! accumulation in linear light. They are made on the CPU after every sample of the view is drawn,
//! outside every timer. The report has each checkpoint's scores, the samples and the GPU seconds
//! that it takes to get to FLIP 0.05 and 0.02, the floor, and whether each of those lines counts
//! in the view (it counts where it is at least [`MIN_LINE_OVER_FLOOR`] times the floor).
//!
//! **The views** are on the patch of the *test* Planet, like the reference golden's, and not on
//! the 320 km Planet that the flight and the stills of this bench draw: a patch takes a fraction
//! of a second to make, and the views are fixed Voxel for Voxel (see [`views`]).
//!
//! **The files.** `convergence.csv` (the curve, a row for each view and checkpoint) is beside
//! `report.json`; the pictures and the truths with their inputs are in `convergence/VIEW/`. A
//! truth is valid for the machine, the driver and the build that made it, which its sidecar
//! records: it is never compared between machines or drivers, and it is never a golden.
//!
//! **CI.** `--short` runs the phase at a few dozen pixels with a short ladder and **no FLIP**, to
//! see that it runs and that the report has the right shape. FLIP is run by `client bench` on a
//! real GPU only, and by one ignored test.

mod gi;
mod score;
mod shadows;
mod still;
mod views;

pub use views::patch_store;
pub use views::room as emissive_room;

use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::Instant;

use render::{Camera, GpuInfo, Image, Offscreen, Resolution};
use serde::Serialize;

use super::stats::Stats;
use super::write_png;
use score::{Flip, How, Reach};
use views::{Description, View};

/// The seed of the run that is timed and scored: the reference's own (`render::tracer::RNG_SEED`).
/// It differs from both truths' seeds, or the first samples would be the truth's own and every
/// error would read low. A test holds that.
pub const RUN_SEED: u32 = render::tracer::RNG_SEED;
/// The seeds of the two truths, A and B. Pinned here, with [`FLIP_VERSION`] and
/// [`score::FLIP_PPD`], because the scores move with each of them.
pub const TRUTH_SEEDS: [u32; 2] = [1, 2];
/// The version of `flip-rs` that scores the images: the exact version `bins/client/Cargo.toml`
/// pins. A test holds the two together.
pub const FLIP_VERSION: &str = "0.1.2";
/// FLIP levels that are reported: acceptable (ADR 0005's target) and clean.
pub const LINES: [f64; 2] = [0.05, 0.02];
/// A line counts in a view only where it is at least this many times the view's floor.
pub const MIN_LINE_OVER_FLOOR: f64 = 4.0;
/// The checkpoints, in samples.
pub const CHECKPOINTS: [u32; 6] = [1, 4, 16, 64, 256, 1024];
/// How many samples a truth has when `--truth-spp` does not say: the last checkpoint. ADR 0005's
/// is 8192, which takes eight times as long.
pub const DEFAULT_TRUTH_SPP: u32 = 1024;
/// How many samples the room's truths have at least when FLIP is run (#54): a 0.05 line counts only
/// where it is four times the floor (0.0125). The room's floor was 0.0211 at 1024 samples and
/// 0.0137 at 4096 on the 4090: it falls slower than one over the square root of the samples (the
/// two fit floor² = 1.0e-4 + 0.35 / samples, the first term being what more samples do not take
/// away), which gives about 0.0120 at 8192.
pub const ROOM_TRUTH_SPP: u32 = 8192;

/// Where the pictures and truths go, in the output folder.
const FOLDER: &str = "convergence";
/// The curve, in the output folder.
pub const CSV_FILE: &str = "convergence.csv";

/// What to run.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The Reference view's size: traced and presented at this size.
    pub size: (u32, u32),
    pub checkpoints: Vec<u32>,
    pub truth_spp: u32,
    /// Whether to score with FLIP. Off for the smoke test, so that CI never runs it.
    pub flip: bool,
    /// Whether to score with FLIP a second time, on the images as the old gamma-2.2 curve would
    /// have drawn them from the same samples (`--flip-gamma-2-2`, #188). Only with `flip`.
    pub flip_gamma22: bool,
    /// The still-view part (#189): the Reference view with the still-frame policy on, from the
    /// stop.
    pub still: still::Settings,
    /// The sun-and-sky view (#52): Primary's shadows against the Reference's.
    pub shadows: shadows::Settings,
    /// Primary's GI against the Reference's truth in each view, and the dig (#54).
    pub gi: gi::Settings,
}

impl Settings {
    /// The measurement: 1440p for the default `--size` (twice the traced size, the size that the
    /// player sees), the ladder of ADR 0005, FLIP.
    pub fn full(traced: (u32, u32), truth_spp: Option<u32>, flip_gamma22: bool) -> Self {
        Self {
            size: (traced.0 * 2, traced.1 * 2),
            checkpoints: CHECKPOINTS.to_vec(),
            truth_spp: truth_spp.unwrap_or(DEFAULT_TRUTH_SPP),
            flip: true,
            flip_gamma22,
            still: still::Settings::full(),
            shadows: shadows::Settings::full(
                (traced.0 * 2, traced.1 * 2),
                truth_spp.unwrap_or(DEFAULT_TRUTH_SPP),
            ),
            gi: gi::Settings::full((traced.0 * 2, traced.1 * 2)),
        }
    }

    /// A smoke test that takes a second on a software driver: half the traced size, a ladder of
    /// three, no FLIP. Its numbers mean nothing.
    pub fn short(traced: (u32, u32), truth_spp: Option<u32>) -> Self {
        Self {
            size: ((traced.0 / 2).max(1), (traced.1 / 2).max(1)),
            checkpoints: vec![1, 4, 16],
            truth_spp: truth_spp.unwrap_or(16),
            flip: false,
            flip_gamma22: false,
            still: still::Settings::short(),
            shadows: shadows::Settings::short(
                ((traced.0 / 2).max(1), (traced.1 / 2).max(1)),
                truth_spp.unwrap_or(16),
            ),
            gi: gi::Settings::short(((traced.0 / 2).max(1), (traced.1 / 2).max(1))),
        }
    }

    /// How many samples each of a view's truths has: the room's has more where FLIP is run, for
    /// a floor that the 0.05 line is four times (the room is dim, so its truth is noisier).
    pub fn truth_spp_for(&self, view: &str) -> u32 {
        if self.flip && view == "room" {
            self.truth_spp.max(ROOM_TRUTH_SPP)
        } else {
            self.truth_spp
        }
    }

    /// Samples drawn for each view: two truths and the run.
    fn samples_per_view(&self) -> u64 {
        2 * u64::from(self.truth_spp) + u64::from(self.checkpoints.last().copied().unwrap_or(0))
    }
}

/// The report's `convergence` block.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConvergenceReport {
    /// The Reference view's size, traced and presented.
    pub width: u32,
    pub height: u32,
    /// The scene the views are on: the patch of the test Planet, not the 320 km Planet (the `room`
    /// view's own scene is a box of rock; see its `what`).
    pub scene: &'static str,
    pub checkpoints: Vec<u32>,
    /// The samples of each truth by default; a view may have more (`views[].truth_spp`).
    pub truth_spp: u32,
    pub seeds: Seeds,
    /// How the images were scored; null where FLIP was not run (`--short`).
    pub flip: Option<FlipInfo>,
    /// Whether the `*_gamma22` scores are there (`--flip-gamma-2-2`).
    pub flip_gamma22: bool,
    /// The FLIP levels that `views[].lines` are for.
    pub lines: Vec<f64>,
    /// A line counts in a view where it is at least this many times the view's floor.
    pub min_line_over_floor: f64,
    /// Where the pictures and the truths are, in the output folder, and the curve's file.
    pub folder: &'static str,
    pub csv: &'static str,
    /// Wall-clock seconds the whole phase took: making the scenes and the GPU contexts, the
    /// truths, the run, scoring and the files.
    pub wall_s: f64,
    pub views: Vec<ViewReport>,
    /// The sun-and-sky view (#52): Primary's shadows against the Reference's direct-sun image.
    pub shadows: shadows::ShadowsReport,
    /// Primary's GI against truth in each of `views` (their `gi`), the dig, and the gates of #54.
    pub gi: gi::GiReport,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Seeds {
    pub run: u32,
    pub truth_a: u32,
    pub truth_b: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FlipInfo {
    pub crate_version: &'static str,
    pub mode: &'static str,
    pub ppd: f32,
}

/// One view's convergence.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ViewReport {
    pub name: &'static str,
    #[serde(flatten)]
    pub description: Description,
    /// The samples of each of this view's truths: the report's `truth_spp`, or more where the view
    /// asks for it (the room's, [`ROOM_TRUTH_SPP`], so that its floor is low enough for the 0.05
    /// line to count).
    pub truth_spp: u32,
    /// The GPU time of one sample (the `reference` pass) and of the whole frame of it
    /// (`gpu_total`), in the run that is scored: as the flight's reference mode reports them.
    pub reference: Stats,
    pub gpu_total: Stats,
    /// The mean time of the `reference` pass, per megapixel of the image, for one sample.
    pub ms_per_mpix_per_sample: Option<f64>,
    /// How bright the converged image is: truth A, the same view at `truth_spp` samples.
    pub converged: Converged,
    pub floor: Floor,
    pub curve: Vec<Point>,
    /// For each of [`LINES`]; empty where FLIP was not run.
    pub lines: Vec<Line>,
    /// The files of this view in the output folder's `convergence/`, sorted.
    pub files: Vec<String>,
    /// The Reference view with the still-frame policy on, from the stop (#189).
    pub still: still::StillReport,
    /// Primary against this view's truth A, with the flat ambient of before #54 and with the
    /// radiance cache (#54).
    pub gi: gi::ViewGi,
}

/// How bright a view's converged image is, in linear light before the display encoding, over all
/// its pixels (#188): what a change of the estimator moves in a view where light bounces, which
/// FLIP only shows indirectly. Both truths are of the same view and agree to the noise.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Converged {
    /// The mean of truth A's pixels, per channel.
    pub mean_linear: [f64; 3],
    /// The mean of the pixels' luminance (Rec. 709 weights).
    pub mean_luminance: f64,
    /// The same of truth B, which shows how well the two agree.
    pub mean_luminance_b: f64,
}

impl Converged {
    fn of(a: &[f32], b: &[f32]) -> Self {
        let means = |linear: &[f32]| {
            let pixels = (linear.len() / 3) as f64;
            let mut sum = [0.0f64; 3];
            for p in linear.as_chunks::<3>().0 {
                for c in 0..3 {
                    sum[c] += f64::from(p[c]);
                }
            }
            sum.map(|s| s / pixels)
        };
        let luminance = |m: [f64; 3]| 0.2126 * m[0] + 0.7152 * m[1] + 0.0722 * m[2];
        let (ma, mb) = (means(a), means(b));
        Self {
            mean_linear: ma,
            mean_luminance: luminance(ma),
            mean_luminance_b: luminance(mb),
        }
    }
}

/// The metric's noise floor in a view: truth A against truth B.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Floor {
    pub flip_mean: Option<f64>,
    pub flip_p99: Option<f64>,
    /// The same on images drawn with the old gamma-2.2 curve (`--flip-gamma-2-2`).
    pub flip_mean_gamma22: Option<f64>,
    pub mse_linear: f64,
}

/// A checkpoint's scores against truth A.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Point {
    pub samples: u32,
    /// The GPU time of the samples so far, in seconds, as timed; null without timers.
    pub gpu_s: Option<f64>,
    pub flip_mean: Option<f64>,
    pub flip_p99: Option<f64>,
    /// FLIP's mean of the same samples drawn with the old gamma-2.2 curve, both this image and
    /// truth A (`--flip-gamma-2-2`): the curve's effect on the score, apart from the estimator's.
    pub flip_mean_gamma22: Option<f64>,
    /// The squared error of the `f32` accumulation against truth A, in linear light, per channel.
    pub mse_linear: f64,
    /// `mse_linear` less the truth's own share of it, half the floor's: what remains is the
    /// run's noise alone, which falls as 1/N.
    pub mse_linear_run_only: f64,
}

/// Where the curve gets to a FLIP line.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Line {
    pub flip: f64,
    /// Samples, null where the curve never gets there.
    pub samples: Option<f64>,
    pub how: How,
    /// GPU seconds for those samples at the mean and at the 95th percentile of one sample's time;
    /// null without timers.
    pub gpu_s: Option<f64>,
    pub gpu_s_p95: Option<f64>,
    /// Whether the line counts in this view: it is at least [`MIN_LINE_OVER_FLOOR`] times the
    /// floor's FLIP mean. Where it does not, the view cannot tell this line from noise.
    pub counts: bool,
}

/// An image the run drew: what the player sees, and what the accumulation holds.
struct Capture {
    samples: u32,
    image: Image,
    linear: Vec<f32>,
}

impl Capture {
    fn take(gpu: &mut Offscreen, camera: &Camera) -> Self {
        Self {
            samples: gpu.reference_samples(),
            // Zero more samples: the image as it is.
            image: gpu.render_reference(camera, 0),
            linear: gpu.read_accumulation(),
        }
    }
}

/// Runs the phase on both views and writes the pictures and the curve to `out`.
pub fn run(settings: &Settings, out: &Path) -> Result<ConvergenceReport, Box<dyn Error>> {
    let started = Instant::now();
    let (width, height) = settings.size;
    eprintln!(
        "convergence: {width}x{height}, {} samples a view ({} for each of two truths, {} for the \
         run), FLIP {}...",
        settings.samples_per_view(),
        settings.truth_spp,
        settings.checkpoints.last().copied().unwrap_or(0),
        if settings.flip { "on" } else { "off" }
    );
    let mut views = Vec::new();
    for view in [views::golden(), views::pit(), views::room()] {
        views.push(run_view(view, settings, out)?);
    }
    let shadows = shadows::run(&settings.shadows, out)?;
    eprintln!("convergence: the dig...");
    let edit = gi::dig(&settings.gi, out)?;
    let gi = {
        let scored: Vec<_> = views
            .iter()
            .map(|v| {
                // The 0.05 line counts in a view where it is four times the floor.
                let counts = v
                    .lines
                    .iter()
                    .find(|l| l.flip == still::FLIP_TARGET)
                    .is_none_or(|l| l.counts);
                (v.name, &v.gi, v.still.flip_within_target, counts)
            })
            .collect();
        gi::GiReport {
            gates: gi::gates(&settings.gi, &scored, &edit),
            edit,
        }
    };
    let mut report = ConvergenceReport {
        width,
        height,
        scene: "patch",
        checkpoints: settings.checkpoints.clone(),
        truth_spp: settings.truth_spp,
        seeds: Seeds {
            run: RUN_SEED,
            truth_a: TRUTH_SEEDS[0],
            truth_b: TRUTH_SEEDS[1],
        },
        flip_gamma22: settings.flip && settings.flip_gamma22,
        flip: settings.flip.then_some(FlipInfo {
            crate_version: FLIP_VERSION,
            mode: "ldr",
            ppd: score::FLIP_PPD,
        }),
        lines: LINES.to_vec(),
        min_line_over_floor: MIN_LINE_OVER_FLOOR,
        folder: FOLDER,
        csv: CSV_FILE,
        wall_s: 0.0,
        views,
        shadows,
        gi,
    };
    let csv = out.join(CSV_FILE);
    fs::write(&csv, csv_of(&report)).map_err(|e| format!("cannot write {}: {e}", csv.display()))?;
    report.wall_s = started.elapsed().as_secs_f64();
    Ok(report)
}

/// The exit status of the phase's gates (#54): 2 if one is over, unless `no_fail`.
pub fn gi_exit_code(report: &ConvergenceReport, no_fail: bool) -> u8 {
    gi::exit_code(&report.gi, no_fail)
}

/// Draws `samples` more samples of the reference image, timing each as the flight's reference mode
/// does (the dispatch is submitted and waited for), and stops at each of `stops` to take the image.
fn draw(
    gpu: &mut Offscreen,
    camera: &Camera,
    seed: u32,
    stops: &[u32],
    samples: u32,
) -> Vec<Capture> {
    gpu.set_reference_seed(seed);
    let mut captures = Vec::new();
    for n in 1..=samples {
        gpu.reference_sample(camera);
        if stops.contains(&n) {
            captures.push(Capture::take(gpu, camera));
        }
    }
    captures
}

fn run_view(view: View, settings: &Settings, out: &Path) -> Result<ViewReport, Box<dyn Error>> {
    let name = view.name;
    eprintln!("convergence: {name}...");
    let resolution = Resolution::native(settings.size.0, settings.size.1);
    let mut gpu = Offscreen::with_resolution(resolution, &view.scene)?;
    // The light and display of before #202 (the old sun and sky, no exposure, bloom or tone
    // curve): the phase's numbers are compared from report to report, and a change of look would
    // move every one of them with no change in the estimator. The art direction's cost is in the
    // flight's passes; its look is the goldens'. This sets the old sun, too: the one these views
    // were made with is not drawn, and the report names the sun that is.
    gpu.use_legacy_light();
    let mut view = view;
    view.description = views::under_the_legacy_sun(view.description);
    gpu.set_keep_history(true);
    let info = gpu.gpu_info().clone();
    let folder = out.join(FOLDER).join(name);
    fs::create_dir_all(&folder).map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
    let last = settings.checkpoints.last().copied().unwrap_or(0);
    let truth_spp = settings.truth_spp_for(name);

    // The truths first: they warm the GPU's clocks and caches, and they are not timed.
    let truth = |gpu: &mut Offscreen, seed| {
        draw(gpu, &view.camera, seed, &[truth_spp], truth_spp)
            .pop()
            .expect("a truth has samples")
    };
    let truth_a = truth(&mut gpu, TRUTH_SEEDS[0]);
    let truth_b = truth(&mut gpu, TRUTH_SEEDS[1]);

    // The run, timed. What the scores take is done after it, with the GPU idle.
    let passes = gpu.timings();
    let reference_from = passes
        .pass("reference")
        .map_or(0, |p| p.times.history().len());
    let total_from = passes.total().history().len();
    let checkpoints = draw(
        &mut gpu,
        &view.camera,
        RUN_SEED,
        &settings.checkpoints,
        last,
    );
    let timings = gpu.timings();
    let reference_times: Vec<f64> = timings
        .pass("reference")
        .map_or(&[][..], |p| &p.times.history()[reference_from..])
        .to_vec();
    let reference = Stats::of(&reference_times);
    let gpu_total = Stats::of(&timings.total().history()[total_from..]);
    // The GPU seconds of the first n samples, as timed. Without timers there are none.
    let gpu_s = |samples: u32| {
        (reference_times.len() as u32 >= samples)
            .then(|| reference_times[..samples as usize].iter().sum::<f64>() / 1e3)
    };
    // The still-view run (#189): the policy on, batched, at 60 Hz, from the stop. After everything
    // that is timed above, which it cannot move, on the same GPU and the same views.
    eprintln!(
        "convergence: {name}: the still view, {} s...",
        settings.still.seconds
    );
    let (from, to) = (view.description.camera_from, view.description.camera_to);
    let elsewhere = Camera::looking_at([from[0] + 1.0, from[1], from[2]], to);
    let batching_exact = still::batching_is_exact(&mut gpu, &view.camera);
    let max_batch_samples = gpu.max_batch_samples();
    let still_run = still::run(&mut gpu, &view.camera, &elsewhere, &settings.still);
    drop(gpu);

    // Scoring, on the CPU.
    let pixels = (settings.size.0 as usize, settings.size.1 as usize);
    let rgb = |capture: &Capture| score::rgb_of_rgba(&capture.image.rgba);
    let floor_mse = score::mse(&truth_a.linear, &truth_b.linear);
    let truth_a_rgb = rgb(&truth_a);
    let flip = |what: &str, test: &Capture, against: &[u8]| -> Result<Option<Flip>, String> {
        if !settings.flip {
            return Ok(None);
        }
        eprintln!("convergence: {name}: FLIP of {what}...");
        score::flip(against, &rgb(test), pixels).map(Some)
    };
    let floor_flip = flip(
        "truth B against truth A (the floor)",
        &truth_b,
        &truth_a_rgb,
    )?;
    // The same samples as the old gamma-2.2 curve would have drawn them (`--flip-gamma-2-2`).
    let truth_a_gamma = score::gamma22_rgb(&truth_a.linear);
    let flip_gamma = |what: &str, test: &Capture| -> Result<Option<f64>, String> {
        if !settings.flip || !settings.flip_gamma22 {
            return Ok(None);
        }
        eprintln!("convergence: {name}: FLIP of {what}, old curve...");
        score::flip(&truth_a_gamma, &score::gamma22_rgb(&test.linear), pixels).map(|f| Some(f.mean))
    };
    let floor = Floor {
        flip_mean: floor_flip.map(|f| f.mean),
        flip_p99: floor_flip.map(|f| f.p99),
        flip_mean_gamma22: flip_gamma("truth B against truth A (the floor)", &truth_b)?,
        mse_linear: floor_mse,
    };
    let mut curve = Vec::new();
    for capture in &checkpoints {
        let scores = flip(
            &format!("{} samples", capture.samples),
            capture,
            &truth_a_rgb,
        )?;
        let mse_linear = score::mse(&capture.linear, &truth_a.linear);
        curve.push(Point {
            samples: capture.samples,
            gpu_s: gpu_s(capture.samples),
            flip_mean: scores.map(|f| f.mean),
            flip_p99: scores.map(|f| f.p99),
            flip_mean_gamma22: flip_gamma(&format!("{} samples", capture.samples), capture)?,
            mse_linear,
            mse_linear_run_only: mse_linear - floor_mse / 2.0,
        });
    }

    // Where the curve gets to each line, and whether the view can tell it from its own noise.
    let mut lines = Vec::new();
    if let Some(floor_mean) = floor.flip_mean {
        let points: Vec<(f64, f64)> = curve
            .iter()
            .filter_map(|p| Some((f64::from(p.samples), p.flip_mean?)))
            .collect();
        for line in LINES {
            let reach = score::samples_to_reach(&points, line).unwrap_or(Reach {
                samples: None,
                how: How::NotReached,
            });
            lines.push(Line {
                flip: line,
                samples: reach.samples,
                how: reach.how,
                gpu_s: reach
                    .samples
                    .zip(reference.mean_ms)
                    .map(|(n, ms)| n * ms / 1e3),
                gpu_s_p95: reach
                    .samples
                    .zip(reference.p95_ms)
                    .map(|(n, ms)| n * ms / 1e3),
                counts: line >= MIN_LINE_OVER_FLOOR * floor_mean,
            });
        }
    }

    // The still run's images against the same truth A, as the curve's.
    let mut still_scores = Vec::new();
    let mut still_mse = Vec::new();
    for capture in &still_run.captures {
        still_scores.push(flip(
            &format!("the still run's {} samples", capture.samples),
            capture,
            &truth_a_rgb,
        )?);
        still_mse.push(score::mse(&capture.linear, &truth_a.linear));
    }
    let gi = gi::score_view(&view, &settings.gi, &truth_a.image, &truth_a_rgb, &folder)?;
    let still = still::report(
        batching_exact,
        max_batch_samples,
        still::within_s_for(name),
        &settings.still,
        &still_run,
        &still_scores,
        &still_mse,
        &lines,
    );

    let mut files = write_files(
        &folder,
        &view,
        &info,
        settings,
        &truth_a,
        &truth_b,
        &checkpoints,
        &still_run,
    )?;
    files.extend(gi.files.iter().cloned());
    files.sort();
    let megapixels = f64::from(settings.size.0) * f64::from(settings.size.1) / 1e6;
    Ok(ViewReport {
        name,
        description: view.description,
        truth_spp,
        ms_per_mpix_per_sample: reference.mean_ms.map(|ms| ms / megapixels),
        converged: Converged::of(&truth_a.linear, &truth_b.linear),
        reference,
        gpu_total,
        floor,
        curve,
        lines,
        files,
        still,
        gi,
    })
}

/// What a truth was made from, written beside it. A truth is for the machine, driver and build
/// that made it: nothing reads one back, and it is never compared with another machine's.
#[derive(Serialize)]
struct TruthInputs<'a> {
    view: &'static str,
    description: &'a Description,
    seed: u32,
    samples: u32,
    width: u32,
    height: u32,
    /// `f32` little-endian, interleaved RGB, rows from the top: the running mean in linear light,
    /// before the display encoding.
    f32_format: &'static str,
    scene: &'static str,
    generator_version: u32,
    gpu: &'a str,
    device_type: &'a str,
    driver: &'a str,
    driver_info: &'a str,
    build_version: &'static str,
    build_commit: Option<&'static str>,
    note: &'static str,
}

/// Writes one file at the path it is given.
type Writer<'a> = dyn Fn(&Path) -> Result<(), Box<dyn Error>> + 'a;

/// Writes the pictures, and the truths with their inputs, to `folder`; returns the names.
#[allow(clippy::too_many_arguments)]
fn write_files(
    folder: &Path,
    view: &View,
    info: &GpuInfo,
    settings: &Settings,
    truth_a: &Capture,
    truth_b: &Capture,
    checkpoints: &[Capture],
    still_run: &still::Run,
) -> Result<Vec<String>, Box<dyn Error>> {
    let mut names = Vec::new();
    let mut put = |name: String, write: &Writer<'_>| {
        let path = folder.join(&name);
        write(&path)?;
        names.push(name);
        Ok::<(), Box<dyn Error>>(())
    };
    for (label, seed, truth) in [
        ("truth-a", TRUTH_SEEDS[0], truth_a),
        ("truth-b", TRUTH_SEEDS[1], truth_b),
    ] {
        put(format!("{label}.png"), &|path| {
            write_png(path, &truth.image)
        })?;
        put(format!("{label}.f32"), &|path| {
            let bytes: Vec<u8> = truth.linear.iter().flat_map(|v| v.to_le_bytes()).collect();
            fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            Ok(())
        })?;
        put(format!("{label}.json"), &|path| {
            let inputs = TruthInputs {
                view: view.name,
                description: &view.description,
                seed,
                samples: truth.samples,
                width: settings.size.0,
                height: settings.size.1,
                f32_format: "f32 little-endian, interleaved RGB, rows from the top, linear \
                             radiance (the running mean, before the display encoding)",
                scene: view.scene_name,
                generator_version: planet_gen::GENERATOR_VERSION,
                gpu: &info.name,
                device_type: &info.device_type,
                driver: &info.driver,
                driver_info: &info.driver_info,
                build_version: env!("CARGO_PKG_VERSION"),
                build_commit: option_env!("GITHUB_SHA"),
                note: "valid for this machine, driver and build only: never compare a truth \
                       between machines or drivers, and never use one as a golden",
            };
            fs::write(path, serde_json::to_string_pretty(&inputs)?)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            Ok(())
        })?;
    }
    for capture in checkpoints {
        put(format!("spp{:04}.png", capture.samples), &|path| {
            write_png(path, &capture.image)
        })?;
    }
    // The still run's image at each mark (#189): what the Reference view had by then.
    for &(seconds, at) in &still_run.marks {
        let capture = &still_run.captures[at];
        put(
            format!("still-{:04}ms.png", (seconds * 1e3).round() as u32),
            &|path| write_png(path, &capture.image),
        )?;
    }
    Ok(names)
}

/// The curve as CSV: a row for each view and checkpoint. Empty where there is no number (no
/// timers, no FLIP). The floor's two numbers are repeated on every row of a view, so that a plot
/// can draw it from the file alone.
pub fn csv_of(report: &ConvergenceReport) -> String {
    fn cell(v: Option<f64>) -> String {
        v.map_or(String::new(), |v| format!("{v}"))
    }
    let mut csv = String::from(
        "view,samples,gpu_s,flip_mean,flip_p99,mse_linear,mse_linear_run_only,\
         floor_flip_mean,floor_mse_linear,flip_mean_gamma22,floor_flip_mean_gamma22\n",
    );
    for view in &report.views {
        for p in &view.curve {
            csv += &format!(
                "{},{},{},{},{},{},{},{},{},{},{}\n",
                view.name,
                p.samples,
                cell(p.gpu_s),
                cell(p.flip_mean),
                cell(p.flip_p99),
                p.mse_linear,
                p.mse_linear_run_only,
                cell(view.floor.flip_mean),
                view.floor.mse_linear,
                cell(p.flip_mean_gamma22),
                cell(view.floor.flip_mean_gamma22),
            );
        }
    }
    csv
}

/// The phase in brief, for whoever is watching the run.
pub fn print_summary(report: &ConvergenceReport) {
    eprintln!("convergence: {:.1} s in all", report.wall_s);
    let num =
        |v: Option<f64>, digits: usize| v.map_or("n/a".to_string(), |v| format!("{v:.digits$}"));
    for view in &report.views {
        eprintln!(
            "convergence, {} ({}x{}, truths of {} samples): one sample {} ms (p95 {}), {} ms per megapixel; floor FLIP {} (mse {:.3e})",
            view.name,
            report.width,
            report.height,
            view.truth_spp,
            num(view.reference.mean_ms, 3),
            num(view.reference.p95_ms, 3),
            num(view.ms_per_mpix_per_sample, 3),
            num(view.floor.flip_mean, 4),
            view.floor.mse_linear
        );
        eprintln!(
            "  converged brightness: luminance {:.5} (truth B {:.5}), RGB {:.5} {:.5} {:.5}",
            view.converged.mean_luminance,
            view.converged.mean_luminance_b,
            view.converged.mean_linear[0],
            view.converged.mean_linear[1],
            view.converged.mean_linear[2]
        );
        eprintln!(
            "  {:>8} {:>9} {:>10} {:>10} {:>12} {:>14}",
            "samples", "gpu s", "FLIP mean", "FLIP p99", "mse linear", "FLIP, gamma 2.2"
        );
        for p in &view.curve {
            eprintln!(
                "  {:>8} {:>9} {:>10} {:>10} {:>12.3e} {:>14}",
                p.samples,
                num(p.gpu_s, 3),
                num(p.flip_mean, 4),
                num(p.flip_p99, 4),
                p.mse_linear,
                num(p.flip_mean_gamma22, 4)
            );
        }
        still::print(view.name, &view.still);
        for l in &view.lines {
            let note = if l.counts {
                String::new()
            } else {
                format!(" (does not count here: under {MIN_LINE_OVER_FLOOR} times the floor)")
            };
            eprintln!(
                "  FLIP <= {}: {} samples, {} GPU s ({:?}){note}",
                l.flip,
                num(l.samples, 1),
                num(l.gpu_s, 3),
                l.how,
            );
        }
    }
    shadows::print_summary(&report.shadows);
    let scored: Vec<_> = report.views.iter().map(|v| (v.name, &v.gi)).collect();
    gi::print_summary(&scored, &report.gi);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(samples: u32, flip_mean: Option<f64>) -> Point {
        Point {
            samples,
            gpu_s: Some(f64::from(samples) * 0.003),
            flip_mean,
            flip_p99: flip_mean.map(|m| m * 4.0),
            flip_mean_gamma22: None,
            mse_linear: 1.0 / f64::from(samples),
            mse_linear_run_only: 0.5 / f64::from(samples),
        }
    }

    fn still_report() -> still::StillReport {
        let tail = still::Tail::of(&[8.0, 9.0, 10.0]);
        still::StillReport {
            batching_bit_exact: true,
            hz: 60,
            budget_ms: 12.0,
            max_batch_samples: 16,
            still_frames: 2,
            fade_ms: 150,
            seconds: 0.3,
            frames: 18,
            accumulating_frames: 16,
            frame: tail.clone(),
            frames_over: 0,
            gpu_batch: tail,
            batch_first: 1,
            batch_last: 3,
            batch_max: 3,
            samples_per_s: Some(100.0),
            marks: Vec::new(),
            flip_within_target: None,
            flip_within_s: still::WITHIN_S,
            frame_p99_within_target: Some(true),
            to_line: Vec::new(),
            cuts: still::CutsReport::default(),
        }
    }

    fn report(flip: bool) -> ConvergenceReport {
        let view = |name| {
            let mut view = views_description();
            view.pit = None;
            ViewReport {
                name,
                description: view,
                truth_spp: 16,
                reference: Stats::of(&[2.0, 3.0]),
                gpu_total: Stats::of(&[2.5, 3.5]),
                ms_per_mpix_per_sample: Some(0.7),
                converged: Converged {
                    mean_linear: [0.3, 0.3, 0.3],
                    mean_luminance: 0.3,
                    mean_luminance_b: 0.3,
                },
                floor: Floor {
                    flip_mean: flip.then_some(0.003),
                    flip_p99: flip.then_some(0.02),
                    flip_mean_gamma22: None,
                    mse_linear: 4e-7,
                },
                curve: [1, 4, 16]
                    .map(|n| point(n, flip.then_some(0.1 / f64::from(n))))
                    .to_vec(),
                lines: Vec::new(),
                files: vec!["spp0001.png".into()],
                still: still_report(),
                gi: gi_view(flip),
            }
        };
        ConvergenceReport {
            width: 64,
            height: 36,
            scene: "patch",
            checkpoints: vec![1, 4, 16],
            truth_spp: 16,
            seeds: Seeds {
                run: RUN_SEED,
                truth_a: TRUTH_SEEDS[0],
                truth_b: TRUTH_SEEDS[1],
            },
            flip: None,
            flip_gamma22: false,
            lines: LINES.to_vec(),
            min_line_over_floor: MIN_LINE_OVER_FLOOR,
            folder: FOLDER,
            wall_s: 0.0,
            csv: CSV_FILE,
            views: vec![view("golden"), view("pit")],
            shadows: shadows_report(flip),
            gi: gi::GiReport {
                edit: gi::EditSettle {
                    width: 64,
                    height: 36,
                    voxels_dug: 160,
                    update_ms: 1.0,
                    before: 0.1,
                    after: 0.2,
                    frame_ms: vec![16.7, 33.3],
                    luminance: vec![0.15, 0.2],
                    settle_ms: flip.then_some(33.3),
                    slow_frames: 0,
                    within_target: flip.then_some(true),
                    game_path: flip,
                },
                gates: gi::Gates {
                    room_target_flip: gi::ROOM_TARGET_FLIP,
                    settle_within_ms: gi::SETTLE_WITHIN_MS,
                    reference_flip: still::FLIP_TARGET,
                    reference_within_s: still::WITHIN_S,
                    reference_room_within_s: still::ROOM_WITHIN_S,
                    held: flip,
                    over: Vec::new(),
                    not_counted: Vec::new(),
                },
            },
        }
    }

    fn gi_view(flip: bool) -> gi::ViewGi {
        let score = |mean: f64| gi::Score {
            flip: flip.then_some(shadows::Scores {
                flip_mean: mean,
                flip_p99: mean * 4.0,
            }),
            luminance_ratio: 1.0,
        };
        let variant = |first: f64, settled: f64| gi::Variant {
            first_frame: score(first),
            settled: score(settled),
        };
        gi::ViewGi {
            flat: variant(0.2, 0.2),
            gi: variant(0.2, 0.1),
            settle_frames: 4,
            gi_not_further: flip.then_some(true),
            files: vec!["primary-gi-settled.png".into()],
        }
    }

    fn shadows_report(flip: bool) -> shadows::ShadowsReport {
        let score = |mean: f64| shadows::PrimaryScore {
            flip: flip.then_some(shadows::Scores {
                flip_mean: mean,
                flip_p99: mean * 4.0,
            }),
            edge: shadows::EdgeError {
                edge_pixels: 12,
                edge_mean: Some(6.0),
                edge_p99: Some(30.0),
                other_mean: Some(1.0),
            },
        };
        shadows::ShadowsReport {
            name: "sun",
            description: views_description(),
            width: 64,
            height: 36,
            traced_width: 32,
            traced_height: 18,
            sun_and_sky_only: true,
            shadow_rays: 2,
            truth_spp: 16,
            unshadowed_spp: 16,
            seeds: Seeds {
                run: RUN_SEED,
                truth_a: TRUTH_SEEDS[0],
                truth_b: TRUTH_SEEDS[1],
            },
            flip: None,
            floor: flip.then_some(shadows::Scores {
                flip_mean: 0.002,
                flip_p99: 0.01,
            }),
            target_flip: shadows::TARGET_FLIP,
            min_line_over_floor: MIN_LINE_OVER_FLOOR,
            target_counts: flip.then_some(true),
            first_frame: score(0.04),
            settled: score(0.02),
            settle_frames: 4,
            meets_target: flip.then_some(true),
            edge_share: 0.05,
            files: vec!["truth-a.png".into()],
        }
    }

    fn views_description() -> Description {
        Description {
            what: "a view",
            camera_from: [0.0; 3],
            camera_to: [1.0; 3],
            fov_y_deg: 70.0,
            sun_azimuth: 0.0,
            sun_elevation: 0.0,
            sun_angular_radius: 0.0,
            pit: None,
        }
    }

    #[test]
    fn the_run_and_the_truths_have_three_different_seeds() {
        // The run's first samples would be a truth's own if they shared a seed, and every error
        // would read low; the floor is not a floor if the truths share one.
        let seeds = [RUN_SEED, TRUTH_SEEDS[0], TRUTH_SEEDS[1]];
        assert!(seeds[0] != seeds[1] && seeds[0] != seeds[2] && seeds[1] != seeds[2]);
    }

    #[test]
    fn the_flip_version_is_the_one_cargo_pins() {
        let manifest = include_str!("../../Cargo.toml");
        assert!(
            manifest.contains(&format!("flip-rs = \"={FLIP_VERSION}\"")),
            "bins/client/Cargo.toml must pin flip-rs to exactly {FLIP_VERSION}"
        );
    }

    #[test]
    fn the_csv_has_a_row_for_each_view_and_checkpoint_and_empty_cells_for_what_was_not_made() {
        let csv = csv_of(&report(false));
        let rows: Vec<&str> = csv.lines().collect();
        assert_eq!(rows.len(), 1 + 2 * 3);
        assert_eq!(
            rows[0],
            "view,samples,gpu_s,flip_mean,flip_p99,mse_linear,mse_linear_run_only,floor_flip_mean,floor_mse_linear,flip_mean_gamma22,floor_flip_mean_gamma22"
        );
        // No FLIP: its three cells are empty, and the squared error is there.
        let cells: Vec<&str> = rows[1].split(',').collect();
        assert_eq!(cells.len(), 11);
        assert_eq!(&cells[..2], ["golden", "1"]);
        assert!(cells[3].is_empty() && cells[4].is_empty() && cells[7].is_empty());
        assert_eq!(cells[5], "1");
        assert_eq!(cells[8], "0.0000004");
        assert_eq!(rows[4].split(',').next(), Some("pit"));

        let with_flip = csv_of(&report(true));
        let cells: Vec<&str> = with_flip.lines().nth(2).unwrap().split(',').collect();
        assert_eq!((cells[1], cells[3], cells[7]), ("4", "0.025", "0.003"));
    }

    #[test]
    fn the_short_settings_run_no_flip_and_the_full_ones_do() {
        let short = Settings::short((160, 90), None);
        assert!(!short.flip);
        assert_eq!(short.size, (80, 45));
        let full = Settings::full((1280, 720), None, false);
        assert!(full.flip);
        assert_eq!(full.size, (2560, 1440));
        assert_eq!(full.checkpoints, CHECKPOINTS);
        assert_eq!(full.truth_spp, 1024);
        assert_eq!(
            Settings::full((1280, 720), Some(8192), false).truth_spp,
            8192
        );
        // Two truths and the run: 2 * 8192 + 1024 samples a view.
        assert_eq!(
            Settings::full((1280, 720), Some(8192), false).samples_per_view(),
            17_408
        );
    }

    /// The phase with FLIP on, at a few dozen pixels on whatever Vulkan device there is (lavapipe
    /// locally): CI never runs FLIP, so this is run by hand, `cargo test -p client --bin client --
    /// --ignored the_phase_with_flip`. `CONVERGENCE_TEST_SIZE=2560x1440` runs it at the real size;
    /// `CONVERGENCE_TEST_SHADOW_RAYS` and `CONVERGENCE_TEST_SUN_TRUTH_SPP` set the sun-and-sky
    /// view's shadow rays (#52) and the samples of its truths.
    #[test]
    #[ignore = "runs FLIP, which CI does not"]
    fn the_phase_with_flip_scores_both_views_and_writes_the_files() {
        let size = std::env::var("CONVERGENCE_TEST_SIZE")
            .ok()
            .and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or((96, 54));
        let env = |name: &str, default: u32| {
            std::env::var(name)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default)
        };
        let mut sun = shadows::Settings::full(size, env("CONVERGENCE_TEST_SUN_TRUTH_SPP", 64));
        sun.shadow_rays = env("CONVERGENCE_TEST_SHADOW_RAYS", sun.shadow_rays);
        let settings = Settings {
            size,
            checkpoints: vec![1, 4, 16, 64],
            truth_spp: 64,
            flip: true,
            flip_gamma22: true,
            still: still::Settings {
                marks_s: vec![0.1, 0.25],
                seconds: 0.3,
            },
            shadows: sun,
            gi: gi::Settings::full(size),
        };
        let out = std::env::temp_dir().join(format!("bhayanak-convergence-{}", std::process::id()));
        let _ = fs::remove_dir_all(&out);
        fs::create_dir_all(&out).unwrap();
        let began = std::time::Instant::now();
        let report = run(&settings, &out).unwrap();
        eprintln!("the phase took {:.1?}", began.elapsed());
        print_summary(&report);
        assert_eq!(report.flip.as_ref().map(|f| f.ppd), Some(67.02));
        assert_eq!(report.views.len(), 3);
        for view in &report.views {
            // The curve falls: more samples are closer to the truth, by every score.
            let flips: Vec<f64> = view.curve.iter().map(|p| p.flip_mean.unwrap()).collect();
            assert!(
                flips.windows(2).all(|w| w[1] < w[0]),
                "{} {flips:?}",
                view.name
            );
            let mses: Vec<f64> = view.curve.iter().map(|p| p.mse_linear).collect();
            assert!(
                mses.windows(2).all(|w| w[1] < w[0]),
                "{} {mses:?}",
                view.name
            );
            // The floor is under what the first sample scores.
            assert!(view.floor.flip_mean.unwrap() < flips[0]);
            assert!(view.floor.flip_p99.unwrap() >= view.floor.flip_mean.unwrap());
            // The still run (#189): its images are scored against the same truth, so more samples
            // are closer to it, and the time to each line is the run's.
            let still = &view.still;
            assert!(still.batching_bit_exact);
            assert!(!still.marks.is_empty(), "{}", view.name);
            let marked: Vec<(u32, f64)> = still
                .marks
                .iter()
                .map(|m| (m.samples, m.flip_mean.unwrap()))
                .collect();
            assert!(
                marked.windows(2).all(|w| w[0].0 <= w[1].0),
                "{} {marked:?}",
                view.name
            );
            assert!(
                marked
                    .windows(2)
                    .all(|w| w[1].0 == w[0].0 || w[1].1 < w[0].1),
                "{} {marked:?}",
                view.name
            );
            assert_eq!(still.to_line.len(), 2);
            assert_eq!(view.lines.len(), 2);
            for line in &view.lines {
                // A line that counts is at least four times the floor.
                assert_eq!(
                    line.counts,
                    line.flip >= 4.0 * view.floor.flip_mean.unwrap(),
                    "{line:?}"
                );
            }
        }
        // The sun-and-sky view (#52): both of Primary's images are scored, and the edge is found.
        let sun = &report.shadows;
        let floor = sun.floor.expect("the floor");
        for (what, p) in [("first frame", &sun.first_frame), ("settled", &sun.settled)] {
            let flip = p.flip.unwrap_or_else(|| panic!("{what} has no FLIP"));
            assert!(flip.flip_p99 >= flip.flip_mean, "{what}: {flip:?}");
            assert!(p.edge.edge_pixels > 0, "{what}: no edge pixels");
            assert!(p.edge.edge_mean.unwrap() >= 0.0);
        }
        assert!(
            sun.edge_share > 0.0 && sun.edge_share < 0.5,
            "{}",
            sun.edge_share
        );
        assert!(floor.flip_mean < sun.first_frame.flip.unwrap().flip_mean);
        assert_eq!(
            sun.meets_target,
            Some(sun.first_frame.flip.unwrap().flip_mean <= 0.05)
        );
        let csv = fs::read_to_string(out.join(CSV_FILE)).unwrap();
        assert!(
            csv.lines()
                .skip(1)
                .all(|r| r.split(',').nth(3).is_some_and(|c| !c.is_empty()))
        );
        let _ = fs::remove_dir_all(&out);
    }
}
