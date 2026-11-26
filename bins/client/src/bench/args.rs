//! The command line of `client bench`.

use std::path::PathBuf;

use super::convergence::CHECKPOINTS;
use super::daycycle::SHORTEST_DAY_SECONDS;
use super::rt::RtMode;

pub const USAGE: &str = "\
Usage: client bench [OPTIONS]

Flies a fixed camera path over the Planet in primary and then reference mode, offscreen on the
GPU, and writes report.json and screenshots to the output folder. Exits with status 2 if a pass
is over its budget.

Options:
  --out DIR        Output folder, created if need be [default: bench-out]
  --budgets FILE   Budgets (TOML) to use instead of the ones built into the executable
  --size WxH       Resolution of the traced image [default: 1280x720]; the upscaled image
                   is twice as big in each direction
  --short          A tiny run on a patch of the Planet at low resolution: for a smoke test,
                   not for timings
  --bias F         LOD bias: output pixels one Voxel may cover [default: 1]. Every switch
                   distance between levels of detail scales with it; 2 does a quarter of the
                   work for up to a pixel more of error
  --diag LIST      Switches for finding out where time goes, separated by commas; a report made
                   with any of them is not a measurement (and does not go with --real-time). no-follow: the Planet does not follow
                   the camera. no-upload: it does, on the CPU, but the GPU is not told.
                   no-flush: writes to the GPU go with the next frame's commands. gap-ms=F:
                   sleep F milliseconds before each frame, as the CPU work of following would
  --streaming      Also run the streaming mode: the loader, the rings' recentring, packing and
                   the copy to the GPU, on the test Planet, stepped on a fixed frame schedule,
                   so that its counts (residency, soft misses, holes, upload bytes) are the same
                   on every run. Exits with status 2 if a frame has a hole
  --real-time      With --streaming, also run the real-time mode: the full-size Planet with the
                   real loader workers and scene thread, on the machine's clock, along paths of
                   50 m/s flights, a take-off, a turn, a stop and a cold start. Takes tens of
                   minutes and is not deterministic: for the 4090. With --short it runs on the
                   test Planet, as a smoke test. Exits with status 2 if a gate of the budgets'
                   [streaming] table is over (docs/agents/benchmarking.md)
  --runs N         With --real-time, how many times to run the paths in this process
                   [default: 3]. The report gives each count's min and max over the runs, and
                   the gates use the worst
  --flat-ambient   With --real-time, draw Primary with the flat ambient of before #54 and not the
                   radiance cache's GI: a baseline on the same build, to read GI's cost against
                   (the report's planet says `flat ambient`; the gates are held as ever)
  --stream-slice-ms N
                   With --real-time, how long the scene thread takes in what the loader made
                   for, in a round, while the loader's workers are as far ahead as they may be
                   [default: 32]. For finding what a slice does to the flights' soft misses, the
                   settle and the frame times; the report's config says what it was
  --no-convergence Leave out the convergence phase (ADR 0005), which otherwise runs last of the
                   timed phases: the Reference view on two still views (the reference golden's,
                   and a pit lit by bounce light) at the output resolution, scored with FLIP and
                   squared error against a truth (the same view, another seed) after 1, 4, 16,
                   64, 256 and 1024 samples. It adds a `convergence` block to report.json,
                   convergence.csv and a convergence/ folder of pictures and truths. It takes
                   about 45 s at the default 1024 on the 4090, and says so in the block as
                   `wall_s`. With --short it is a tiny run with no FLIP
  --flip-gamma-2-2 With the convergence phase's FLIP, also score every image as the old gamma-2.2
                   curve would have drawn it (#188), from the same samples: the report's
                   `flip_mean_gamma22` columns. They separate what the true sRGB curve moved from
                   what the estimator moved, in one run. Adds about as much time as the FLIP
                   it repeats
  --truth-spp N    Samples of each of the convergence phase's two truths [default: 1024]; ADR
                   0005's 8192 makes each truth eight times as long (the phase takes about 2.5
                   minutes on the 4090 since #188). At least 1024, the last checkpoint
  --shadow-rays N  Shadow rays a pixel of Primary casts towards the sun's disc in the convergence
                   phase's sun-and-sky view (#52), 1 to 16 [default: the renderer's, 2]. The
                   report's `convergence.shadows.shadow_rays` says which; run it with 1 and with 2
                   to compare them
  --rt-backend B   How rays find the Planet (#50): compute (the default: the compute-shader DDA),
                   hw (ray queries over the bricks' tight boxes, with shadow rays that end at the
                   first hit, for the flight, the sweep and the Edit stress and for the runs of
                   `--streaming --real-time`, #236; the compute path, with
                   a warning and the reason in the report's `rt.fallback`, if the adapter has no
                   ray queries), hw-nearest (hw with shadow rays that look for the nearest hit) or
                   both (the bench as for compute, then the same flight and Edit stress on hw, and
                   the flight on hw-nearest, each on a context of its own, and the passes that cast
                   rays compared: `rt.comparison`; with `--streaming --real-time`, the real-time
                   runs on compute and then on hw: `rt.real_time_hw`). The report's `rt` says which ran and what the
                   acceleration structures cost to update
  --hw-group-regions G
                   How the hardware backend (--rt-backend hw|both) makes its BLASes (#236): G
                   regions along an edge of each, 4, 8, 16 or 32, or NEAR:FAR@LEVEL (levels below
                   LEVEL have BLASes of NEAR regions an edge, the others FAR, e.g. 4:16@6), or
                   NEAR:MID@LEVEL:FAR@LEVEL (a third size from the second LEVEL up). The default
                   is 16:32@2. Fewer, larger BLASes cost the frame thread less and the GPU more; the
                   report's `rt.hw_group_regions` says what ran
  --hw-build-blas N
                   The most BLASes a frame builds of the groups that may wait (#236), default no
                   bound; the groups that may not wait (they gained boxes, or have no BLAS) are built
                   at once whatever it is. The report counts them as `blas_urgent`
  --hw-wait        A diagnosis (#236): every update of the hardware backend waits for the GPU, so that
                   the paths' `hardware.wait` (and the slowest frames' `wait_ms`) is the GPU's time of
                   the boxes' pass, the BLAS builds and the TLAS of the frame. It is not a player's
                   frame: the gates mean nothing with it. On a device with timestamps in encoders it
                   also times the BLAS builds and the TLAS build apart (`hardware.gpu_blas`,
                   `hardware.gpu_tlas`)
  --time-of-day H  Light the flight (primary and reference, the sweep and the edits) by the sky of the
                   world clock at H hours (0 to 24: 0 midnight, 6 sunrise, 12 noon, 18 sunset) in
                   place of the fixed hero sun, with the clock stopped there (#203)
  --day-cycle      Also run the day cycle (#203), after the flight: a day of the world clock run
                   fast over the flight's path, on its GPU. It reports the frames' times (exit
                   status 2 if the p99 is over 16.7 ms or any frame is), the sun's step a frame
                   and its jumps, the exposure's stability (how far, how fast and how often it
                   turned), and the radiance cache's lag behind a drifting sun at the game's pace
                   and the sped-up one, against a jump. It adds a `day_cycle` block
  --day-seconds N  With --day-cycle, how long the sped-up day is, in seconds of the clock
                   [default: 60; 6 with --short]. At least 4: a faster sun would jump
  --shadow-weight W How Primary weighs the shadow rays of the sun in the convergence phase (#55):
                   `per-sample` (each ray's own cosine, the Reference's) [default] or
                   `centre-cosine` (the cosine to the centre of the disc, what shipped with
                   #52). It is the one the gates are held on and the one the report's `gi` and
                   `settled` are of; the other is reported beside it (`other_shadow_weight`), so
                   one run compares both
  --converged-frames N  Frames of a still view that Primary with no denoiser is averaged over in the
                   convergence phase, for `Primary converged` (#55): the error of each view is split
                   into its bias (FLIP of that against truth) and the denoiser's residual (FLIP of
                   the denoised image against it) [default: 1024; 0 leaves it out]
  --no-fail        Exit with status 0 even if a pass is over its budget, there is a hole or a
                   real-time gate is over
  -h, --help       Print this";

/// What `--diag` switches on. Everything off is a measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Diag {
    pub no_follow: bool,
    pub no_upload: bool,
    pub no_flush: bool,
    pub gap_ms: f64,
}

impl Diag {
    /// The switches that are on, as `--diag` spells them: empty for a measurement.
    pub fn switches(&self) -> Vec<String> {
        [
            (self.no_follow, "no-follow".to_string()),
            (self.no_upload, "no-upload".to_string()),
            (self.no_flush, "no-flush".to_string()),
            (self.gap_ms > 0.0, format!("gap-ms={}", self.gap_ms)),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
    }
}

#[derive(Debug, PartialEq)]
pub struct Args {
    pub out: PathBuf,
    pub budgets: Option<PathBuf>,
    pub short: bool,
    pub streaming: bool,
    /// The real-time mode of the streaming bench (#162), and how many runs of it.
    pub real_time: bool,
    pub runs: u32,
    /// `--flat-ambient`: the real-time mode draws Primary with the flat ambient of before #54, as a
    /// baseline on the same build to read GI's cost against.
    pub flat_ambient: bool,
    /// `--stream-slice-ms`: the scene thread's slice for taking in while the loader is backed up,
    /// if it was said (see [`crate::scene_worker::BACKED_UP_SLICE`]).
    pub stream_slice_ms: Option<u32>,
    pub no_fail: bool,
    /// The convergence phase (#187), which runs unless `--no-convergence`, and its truths' samples
    /// if `--truth-spp` said.
    pub convergence: bool,
    pub truth_spp: Option<u32>,
    /// `--flip-gamma-2-2`: the convergence phase also scores the old curve's images (#188).
    pub flip_gamma22: bool,
    /// `--shadow-rays N`: the shadow rays a pixel of Primary casts in the convergence phase's
    /// sun-and-sky view (#52), where the renderer's default is not wanted.
    pub shadow_rays: Option<u32>,
    /// `--rt-backend` (#50): how rays find the Planet.
    pub rt: RtMode,
    /// `--hw-group-regions` (#236): how the hardware backend's BLASes are made of the cells; None
    /// is the default, `PRODUCTION_GROUPING`.
    pub hw_group: Option<render::hw::Grouping>,
    /// `--hw-build-blas` (#236): the most BLASes a frame builds of the groups that may wait.
    pub hw_blas_budget: Option<u32>,
    /// `--hw-wait` (#236): every update of the hardware backend waits for the GPU, a diagnosis of
    /// what the GPU's time of the boxes, builds and TLAS is (not a player's frame).
    pub hw_wait: bool,
    /// `--time-of-day`: the hours at which the flight's sky is put (#203).
    pub time_of_day: Option<f64>,
    /// `--day-cycle` and `--day-seconds`: the day cycle phase, and the length of its day if said.
    pub day_cycle: bool,
    pub day_seconds: Option<f64>,
    /// `--shadow-weight W`: which weight of the sun's shadow rays the convergence phase's gates are
    /// held on (#55); the other is reported beside it.
    pub shadow_weight: Option<render::ShadowWeight>,
    /// `--converged-frames N`: the frames `Primary converged` is averaged over (the phase's default
    /// if not given).
    pub converged_frames: Option<u32>,
    pub size: Option<(u32, u32)>,
    pub bias: f64,
    pub diag: Diag,
}

/// How many times `--real-time` runs its paths when `--runs` does not say: the gates use the
/// worst of them, and real-time counts vary from run to run.
pub const DEFAULT_RUNS: u32 = 3;

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Help,
    Run(Box<Args>),
}

/// Parses what follows `bench` on the command line.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut parsed = Args {
        out: "bench-out".into(),
        budgets: None,
        short: false,
        streaming: false,
        real_time: false,
        runs: DEFAULT_RUNS,
        flat_ambient: false,
        stream_slice_ms: None,
        no_fail: false,
        convergence: true,
        truth_spp: None,
        flip_gamma22: false,
        shadow_rays: None,
        rt: RtMode::default(),
        hw_group: None,
        hw_blas_budget: None,
        hw_wait: false,
        time_of_day: None,
        day_cycle: false,
        day_seconds: None,
        shadow_weight: None,
        converged_frames: None,
        size: None,
        bias: 1.0,
        diag: Diag::default(),
    };
    let mut runs_given = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |option: &str| args.next().ok_or(format!("{option} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--short" => parsed.short = true,
            "--streaming" => parsed.streaming = true,
            "--real-time" => parsed.real_time = true,
            "--runs" => {
                parsed.runs = runs(&value("--runs")?)?;
                runs_given = true;
            }
            "--flat-ambient" => parsed.flat_ambient = true,
            "--stream-slice-ms" => {
                parsed.stream_slice_ms = Some(stream_slice_ms(&value("--stream-slice-ms")?)?);
            }
            "--no-fail" => parsed.no_fail = true,
            "--no-convergence" => parsed.convergence = false,
            "--flip-gamma-2-2" => parsed.flip_gamma22 = true,
            "--truth-spp" => parsed.truth_spp = Some(truth_spp(&value("--truth-spp")?)?),
            "--shadow-rays" => parsed.shadow_rays = Some(shadow_rays(&value("--shadow-rays")?)?),
            "--rt-backend" => parsed.rt = RtMode::parse(&value("--rt-backend")?)?,
            "--hw-group-regions" => {
                parsed.hw_group = Some(render::hw::Grouping::parse(&value("--hw-group-regions")?)?);
            }
            "--hw-build-blas" => {
                parsed.hw_blas_budget = Some(
                    value("--hw-build-blas")?
                        .parse::<u32>()
                        .ok()
                        .filter(|&n| n >= 1)
                        .ok_or("--hw-build-blas takes a whole number of at least 1")?,
                );
            }
            "--hw-wait" => parsed.hw_wait = true,
            "--time-of-day" => parsed.time_of_day = Some(time_of_day(&value("--time-of-day")?)?),
            "--day-cycle" => parsed.day_cycle = true,
            "--day-seconds" => parsed.day_seconds = Some(day_seconds(&value("--day-seconds")?)?),
            "--converged-frames" => {
                parsed.converged_frames = Some(converged_frames(&value("--converged-frames")?)?);
            }
            "--shadow-weight" => {
                parsed.shadow_weight = Some(shadow_weight(&value("--shadow-weight")?)?);
            }
            "--out" => parsed.out = value("--out")?.into(),
            "--budgets" => parsed.budgets = Some(value("--budgets")?.into()),
            "--size" => parsed.size = Some(size(&value("--size")?)?),
            "--bias" => parsed.bias = bias(&value("--bias")?)?,
            "--diag" => parsed.diag = diag(&value("--diag")?)?,
            _ => return Err(format!("unexpected argument `{arg}`")),
        }
    }
    if parsed.real_time && !parsed.streaming {
        return Err("--real-time is a mode of the streaming bench: it needs --streaming".into());
    }
    if runs_given && !parsed.real_time {
        return Err("--runs is for --real-time".into());
    }
    if parsed.flat_ambient && !parsed.real_time {
        return Err("--flat-ambient is for --real-time".into());
    }
    if parsed.stream_slice_ms.is_some() && !parsed.real_time {
        return Err("--stream-slice-ms is for --real-time".into());
    }
    if parsed.day_seconds.is_some() && !parsed.day_cycle {
        return Err("--day-seconds is for --day-cycle".into());
    }
    if parsed.truth_spp.is_some() && !parsed.convergence {
        return Err(
            "--truth-spp is for the convergence phase, which --no-convergence leaves out".into(),
        );
    }
    if parsed.shadow_rays.is_some() && !parsed.convergence {
        return Err(
            "--shadow-rays is for the convergence phase's sun-and-sky view, which \
             --no-convergence leaves out"
                .into(),
        );
    }
    if parsed.converged_frames.is_some() && !parsed.convergence {
        return Err(
            "--converged-frames is for the convergence phase, which --no-convergence leaves out"
                .into(),
        );
    }
    if parsed.shadow_weight.is_some() && !parsed.convergence {
        return Err(
            "--shadow-weight is for the convergence phase, which --no-convergence leaves out"
                .into(),
        );
    }
    if parsed.flip_gamma22 && (!parsed.convergence || parsed.short) {
        return Err(
            "--flip-gamma-2-2 is for the convergence phase's FLIP, which --no-convergence leaves \
             out and --short does not run"
                .into(),
        );
    }
    // `--diag` says that the budgets are not checked, and the real-time mode's gates are checked
    // whatever it says (they are a failure of their own): the two do not go together.
    if parsed.real_time && !parsed.diag.switches().is_empty() {
        return Err(
            "--diag does not go with --real-time: a diagnostic run is not held to the budgets, \
             and the real-time gates always are"
                .into(),
        );
    }
    Ok(Parsed::Run(Box::new(parsed)))
}

/// `--runs`: at least 1.
fn runs(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("`{text}` is not a number of runs like 3"))
}

/// `--stream-slice-ms`: from 1 ms (under the 2 ms of an ordinary slice it would be no slice) to a
/// second.
fn stream_slice_ms(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .ok()
        .filter(|ms| (1..=1000).contains(ms))
        .ok_or_else(|| format!("`{text}` is not a slice in milliseconds like 32 (1 to 1000)"))
}

/// `--truth-spp`: at least the last checkpoint, or the truth would be rougher than what it judges.
fn truth_spp(text: &str) -> Result<u32, String> {
    let least = CHECKPOINTS[CHECKPOINTS.len() - 1];
    text.parse::<u32>()
        .ok()
        .filter(|&n| n >= least)
        .ok_or_else(|| format!("`{text}` is not a number of samples of at least {least}"))
}

/// `--shadow-rays`: from 1 to `render::MAX_SHADOW_RAYS`, the cap of `Tracer::set_shadow_rays`
/// (the shader's loop has none of its own).
fn shadow_rays(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .ok()
        .filter(|n| (1..=render::MAX_SHADOW_RAYS).contains(n))
        .ok_or_else(|| {
            format!(
                "`{text}` is not a number of shadow rays from 1 to {}",
                render::MAX_SHADOW_RAYS
            )
        })
}

/// `--time-of-day`: hours, from 0 up to 24 (24 is midnight again).
fn time_of_day(text: &str) -> Result<f64, String> {
    text.parse::<f64>()
        .ok()
        .filter(|h| h.is_finite() && (0.0..=24.0).contains(h))
        .ok_or_else(|| format!("`{text}` is not a time of day in hours like 18.5 (0 to 24)"))
}

/// `--day-seconds`: at least [`SHORTEST_DAY_SECONDS`], or the sun would jump between frames.
fn day_seconds(text: &str) -> Result<f64, String> {
    text.parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s >= SHORTEST_DAY_SECONDS)
        .ok_or_else(|| {
            format!("`{text}` is not a day of at least {SHORTEST_DAY_SECONDS} seconds like 60")
        })
}

/// `--converged-frames`: a count of frames, 0 for none.
fn converged_frames(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .ok()
        .filter(|&n| n <= 1 << 16)
        .ok_or_else(|| format!("`{text}` is not a count of frames, 0 to 65536"))
}

/// `--shadow-weight`: the name of one of `render::ShadowWeight::ALL`.
fn shadow_weight(text: &str) -> Result<render::ShadowWeight, String> {
    render::ShadowWeight::ALL
        .into_iter()
        .find(|w| w.name() == text)
        .ok_or_else(|| {
            let names: Vec<_> = render::ShadowWeight::ALL.iter().map(|w| w.name()).collect();
            format!("`{text}` is not a shadow weight: {}", names.join(" or "))
        })
}

/// `--diag`'s comma-separated switches.
fn diag(text: &str) -> Result<Diag, String> {
    let mut diag = Diag::default();
    for item in text.split(',') {
        match item.split_once('=') {
            None if item == "no-follow" => diag.no_follow = true,
            None if item == "no-upload" => diag.no_upload = true,
            None if item == "no-flush" => diag.no_flush = true,
            Some(("gap-ms", ms)) => {
                diag.gap_ms = ms
                    .parse::<f64>()
                    .ok()
                    .filter(|m| m.is_finite() && *m >= 0.0)
                    .ok_or_else(|| format!("`{ms}` is not a number of milliseconds"))?;
            }
            _ => return Err(format!("`{item}` is not a --diag switch")),
        }
    }
    Ok(diag)
}

/// A LOD bias: a positive, finite number.
fn bias(text: &str) -> Result<f64, String> {
    text.parse::<f64>()
        .ok()
        .filter(|b| b.is_finite() && *b > 0.0)
        .ok_or_else(|| format!("`{text}` is not a LOD bias like 1 or 2.5"))
}

/// `WIDTHxHEIGHT`, both at least 1.
fn size(text: &str) -> Result<(u32, u32), String> {
    let wrong = || format!("`{text}` is not a size like 1280x720");
    let (width, height) = text.split_once('x').ok_or_else(wrong)?;
    let dimension = |d: &str| d.parse::<u32>().ok().filter(|&d| d > 0);
    dimension(width).zip(dimension(height)).ok_or_else(wrong)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::convergence::DEFAULT_TRUTH_SPP;

    fn parse_all(args: &[&str]) -> Result<Parsed, String> {
        parse(args.iter().map(|a| a.to_string()))
    }

    fn run(args: &[&str]) -> Args {
        match parse_all(args) {
            Ok(Parsed::Run(args)) => *args,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn with_no_options_the_defaults_apply() {
        assert_eq!(
            run(&[]),
            Args {
                out: "bench-out".into(),
                budgets: None,
                short: false,
                streaming: false,
                real_time: false,
                runs: 3,
                flat_ambient: false,
                stream_slice_ms: None,
                no_fail: false,
                convergence: true,
                truth_spp: None,
                flip_gamma22: false,
                shadow_rays: None,
                rt: RtMode::Compute,
                hw_group: None,
                hw_blas_budget: None,
                hw_wait: false,
                time_of_day: None,
                day_cycle: false,
                day_seconds: None,
                shadow_weight: None,
                converged_frames: None,
                size: None,
                bias: 1.0,
                diag: Diag::default()
            }
        );
    }

    #[test]
    fn the_ray_backend_is_compute_unless_asked_and_a_bad_one_is_refused() {
        assert_eq!(run(&[]).rt, RtMode::Compute);
        assert_eq!(run(&["--rt-backend", "compute"]).rt, RtMode::Compute);
        assert_eq!(run(&["--rt-backend", "hw"]).rt, RtMode::Hardware);
        assert_eq!(
            run(&["--rt-backend", "hw-nearest"]).rt,
            RtMode::HardwareNearest
        );
        assert_eq!(run(&["--rt-backend", "both"]).rt, RtMode::Both);
        for bad in [&["--rt-backend"][..], &["--rt-backend", "rtx"]] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn real_time_needs_streaming_and_runs_need_real_time() {
        let args = run(&["--streaming", "--real-time"]);
        assert!(args.real_time && args.streaming);
        assert_eq!(args.runs, DEFAULT_RUNS);
        assert_eq!(run(&["--streaming", "--real-time", "--runs", "1"]).runs, 1);
        assert!(run(&["--streaming", "--real-time", "--flat-ambient"]).flat_ambient);
        for bad in [
            &["--real-time"][..],
            &["--streaming", "--runs", "3"],
            &["--streaming", "--real-time", "--runs"],
            &["--streaming", "--real-time", "--runs", "0"],
            &["--streaming", "--real-time", "--runs", "-1"],
            &["--streaming", "--real-time", "--runs", "many"],
            &["--streaming", "--flat-ambient"],
            // The diagnostic switches say the budgets are not checked, which the gates always are.
            &["--streaming", "--real-time", "--diag", "no-flush"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_hardware_grouping_is_read_as_a_size_or_a_split_and_bad_ones_are_refused() {
        use render::hw::Grouping;
        assert_eq!(run(&[]).hw_group, None);
        assert_eq!(
            run(&["--hw-group-regions", "8"]).hw_group,
            Some(Grouping::uniform(8).unwrap())
        );
        assert_eq!(
            run(&["--hw-group-regions", "4:16@6"]).hw_group,
            Some(Grouping::split(4, 16, 6).unwrap())
        );
        assert_eq!(
            run(&["--hw-group-regions", "16:32@2"]).hw_group,
            Some(Grouping::split(16, 32, 2).unwrap())
        );
        assert_eq!(run(&[]).hw_blas_budget, None);
        assert_eq!(run(&["--hw-build-blas", "12"]).hw_blas_budget, Some(12));
        assert!(!run(&[]).hw_wait);
        assert!(run(&["--hw-wait"]).hw_wait);
        for bad in [
            &["--hw-group-regions"][..],
            &["--hw-group-regions", "5"],
            &["--hw-group-regions", "64"],
            &["--hw-group-regions", "4:16"],
            &["--hw-build-blas"],
            &["--hw-build-blas", "0"],
            &["--hw-build-blas", "x"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_stream_slice_is_read_for_the_real_time_mode_and_bounded() {
        assert_eq!(run(&["--streaming", "--real-time"]).stream_slice_ms, None);
        let args = run(&["--streaming", "--real-time", "--stream-slice-ms", "64"]);
        assert_eq!(args.stream_slice_ms, Some(64));
        for bad in [
            &["--streaming", "--stream-slice-ms", "64"][..],
            &["--streaming", "--real-time", "--stream-slice-ms"],
            &["--streaming", "--real-time", "--stream-slice-ms", "0"],
            &["--streaming", "--real-time", "--stream-slice-ms", "1001"],
            &["--streaming", "--real-time", "--stream-slice-ms", "fast"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn every_option_is_read() {
        let args = run(&[
            "--short",
            "--out",
            "o",
            "--no-fail",
            "--budgets",
            "b.toml",
            "--size",
            "640x360",
            "--bias",
            "2",
            "--streaming",
        ]);
        assert_eq!(args.out, PathBuf::from("o"));
        assert_eq!(args.budgets, Some(PathBuf::from("b.toml")));
        assert!(args.short && args.no_fail && args.streaming);
        assert_eq!(args.size, Some((640, 360)));
        assert_eq!(args.bias, 2.0);
    }

    #[test]
    fn the_convergence_phase_is_on_unless_left_out_and_its_truth_is_read() {
        let args = run(&[]);
        assert!(args.convergence);
        assert_eq!(args.truth_spp, None);
        assert!(!run(&["--no-convergence"]).convergence);
        assert_eq!(run(&["--truth-spp", "8192"]).truth_spp, Some(8192));
        assert_eq!(run(&["--truth-spp", "1024"]).truth_spp, Some(1024));
        assert_eq!(DEFAULT_TRUTH_SPP, 1024);
        for bad in [
            // A truth is of the convergence phase, which this leaves out.
            &["--no-convergence", "--truth-spp", "8192"][..],
            &["--truth-spp"],
            &["--truth-spp", "1023"],
            &["--truth-spp", "0"],
            &["--truth-spp", "lots"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_shadow_rays_are_read_from_one_to_the_most() {
        assert_eq!(run(&[]).shadow_rays, None);
        assert_eq!(run(&["--shadow-rays", "1"]).shadow_rays, Some(1));
        assert_eq!(run(&["--shadow-rays", "2"]).shadow_rays, Some(2));
        let most = render::MAX_SHADOW_RAYS.to_string();
        assert_eq!(
            run(&["--shadow-rays", &most]).shadow_rays,
            Some(render::MAX_SHADOW_RAYS)
        );
        let over = (render::MAX_SHADOW_RAYS + 1).to_string();
        for bad in [
            &["--shadow-rays", "0"][..],
            &["--shadow-rays", &over],
            &["--shadow-rays"],
            &["--shadow-rays", "two"],
            // The view is of the convergence phase, which this leaves out.
            &["--no-convergence", "--shadow-rays", "2"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_converged_frames_are_a_count_for_the_convergence_phase() {
        assert_eq!(run(&[]).converged_frames, None);
        assert_eq!(run(&["--converged-frames", "0"]).converged_frames, Some(0));
        assert_eq!(
            run(&["--converged-frames", "256"]).converged_frames,
            Some(256)
        );
        for bad in [
            &["--converged-frames", "many"][..],
            &["--converged-frames", "-1"],
            &["--converged-frames", "1000000"],
            &["--converged-frames"],
            &["--no-convergence", "--converged-frames", "8"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_shadow_weight_is_one_of_the_two_the_renderer_has() {
        assert_eq!(run(&[]).shadow_weight, None);
        assert_eq!(
            run(&["--shadow-weight", "per-sample"]).shadow_weight,
            Some(render::ShadowWeight::PerSample)
        );
        assert_eq!(
            run(&["--shadow-weight", "centre-cosine"]).shadow_weight,
            Some(render::ShadowWeight::CentreCosine)
        );
        for bad in [
            &["--shadow-weight", "cosine"][..],
            &["--shadow-weight"],
            &["--no-convergence", "--shadow-weight", "per-sample"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_old_curve_is_scored_only_when_asked_and_only_with_the_phases_flip() {
        assert!(!run(&[]).flip_gamma22);
        assert!(run(&["--flip-gamma-2-2"]).flip_gamma22);
        assert!(run(&["--flip-gamma-2-2", "--truth-spp", "8192"]).flip_gamma22);
        for bad in [
            &["--flip-gamma-2-2", "--no-convergence"][..],
            &["--flip-gamma-2-2", "--short"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn diag_switches_are_read_and_a_wrong_one_is_an_error() {
        let args = run(&["--diag", "no-follow,gap-ms=2.5,no-flush"]);
        assert_eq!(
            args.diag,
            Diag {
                no_follow: true,
                no_flush: true,
                gap_ms: 2.5,
                ..Diag::default()
            }
        );
        assert!(run(&["--diag", "no-upload"]).diag.no_upload);
        for bad in ["fast", "gap-ms=x", "gap-ms=-1", "no-follow,"] {
            assert!(parse_all(&["--diag", bad]).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_switches_that_are_on_are_listed_as_they_are_spelled() {
        assert!(Diag::default().switches().is_empty());
        let diag = run(&["--diag", "gap-ms=2.5,no-flush,no-follow,no-upload"]).diag;
        assert_eq!(
            diag.switches(),
            ["no-follow", "no-upload", "no-flush", "gap-ms=2.5"]
        );
    }

    #[test]
    fn help_is_not_a_run() {
        assert_eq!(parse_all(&["--no-fail", "--help"]), Ok(Parsed::Help));
        assert_eq!(parse_all(&["-h"]), Ok(Parsed::Help));
    }

    #[test]
    fn mistakes_are_errors_that_say_what_was_wrong() {
        for bad in [
            &["--nope"][..],
            &["--out"],
            &["--budgets"],
            &["--size", "640"],
            &["--size", "0x360"],
            &["--size", "axb"],
            &["--bias"],
            &["--bias", "0"],
            &["--bias", "-1"],
            &["--bias", "fast"],
            &["--bias", "inf"],
            &["stray"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
        assert!(parse_all(&["--nope"]).unwrap_err().contains("--nope"));
    }

    #[test]
    fn the_day_cycle_and_its_time_of_day_are_read_and_bounded() {
        let args = run(&[
            "--day-cycle",
            "--day-seconds",
            "30",
            "--time-of-day",
            "18.5",
        ]);
        assert!(args.day_cycle);
        assert_eq!(
            (args.day_seconds, args.time_of_day),
            (Some(30.0), Some(18.5))
        );
        assert_eq!(run(&["--time-of-day", "24"]).time_of_day, Some(24.0));
        for bad in [
            &["--day-seconds", "30"][..],
            &["--day-cycle", "--day-seconds", "3.9"],
            &["--day-cycle", "--day-seconds", "fast"],
            &["--day-cycle", "--day-seconds"],
            &["--time-of-day", "24.5"],
            &["--time-of-day", "-1"],
            &["--time-of-day", "noon"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
    }
}
