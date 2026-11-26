//! `client bench`: a scripted flight over the Planet that times every GPU pass and writes a
//! report. See `docs/agents/benchmarking.md`.
//!
//! It draws offscreen, through [`Offscreen`], not in a window. That is the same tracer and the
//! same pass timers as the window's, minus the window's own blit and overlay passes, and it
//! brings three things a window would not: nothing waits on the display (no vsync, no
//! compositor), a frame's GPU times are in by the time it returns (no 1 to 2 frames of lag, no
//! frames still in flight when the run ends), and it needs no display at all. `Offscreen` asks
//! Vulkan for the high-performance adapter like the window does, so on the dev's machine it is the
//! RTX 4090, and the report says which GPU it got.

mod args;
mod budget;
mod caves;
mod convergence;
#[cfg(test)]
mod crystals;
mod daycycle;
mod edits;
#[cfg(test)]
mod landscape;
pub(crate) mod lookdev;
mod path;
mod process;
mod realtime;
mod report;
mod rt;
mod stats;
mod stream;

use std::error::Error;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use brick_store::BrickStore;
use net::EditMode;
use render::{Camera, HwUpdate, Image, Mode, Offscreen, Resolution, RtBackend, Scene};

use args::{Args, Diag, Parsed, USAGE};
use budget::{Budgets, Status};
use path::{CameraPath, Keyframe};
use report::{
    Altitude, Build, CaveStill, Config, KeyframeLod, Machine, Measured, ModeReport, Modes, Report,
    SCHEMA_VERSION,
};
use rt::{AccelReport, AccelSummary, AccelUpdate, BackendTimes, Comparison, RtMode, RtReport};
use stats::Stats;

const REPORT_FILE: &str = "report.json";

/// How much of the path to draw, how, and on what.
struct Plan {
    /// The traced (internal) resolution. The image presented is twice as big in each direction:
    /// 2560x1440 for the default 1280x720.
    width: u32,
    height: u32,
    /// The test Planet, or `--short`'s patch of it.
    patch: bool,
    path_name: &'static str,
    path: CameraPath,
    primary: ModePlan,
    reference: ModePlan,
}

/// One mode's share of the run.
#[derive(Clone, Copy)]
struct ModePlan {
    /// Frames of the path that are measured.
    frames: u32,
    /// Frames drawn first, at the end of the path, and left out. They let the GPU's clocks, caches
    /// and the driver settle, and they keep the mode switch from leaking into the numbers.
    warmup: u32,
    /// Samples per frame (reference mode).
    spp: u32,
}

impl Plan {
    /// The measurement: the whole Planet at the resolution that the frame budgets are for.
    fn full() -> Self {
        Self {
            width: 1280,
            height: 720,
            patch: false,
            path_name: "planet-flyover-v1",
            path: path::planet_flyover(),
            primary: ModePlan {
                frames: 240,
                warmup: 10,
                spp: 1,
            },
            reference: ModePlan {
                frames: 60,
                warmup: 3,
                spp: 4,
            },
        }
    }

    /// A smoke test that takes seconds even on a software driver. Its times mean nothing.
    ///
    /// Generating the whole Planet takes about 22 s (release), so this makes the patch of it that
    /// the golden image test uses instead, which takes a fraction of a second.
    fn short() -> Self {
        Self {
            width: 160,
            height: 90,
            patch: true,
            path_name: "patch-flyover-v1",
            path: path::patch_flyover(),
            primary: ModePlan {
                frames: 6,
                warmup: 1,
                spp: 1,
            },
            reference: ModePlan {
                frames: 3,
                warmup: 1,
                spp: 2,
            },
        }
    }
}

/// Runs `client bench` with the arguments after `bench`. The exit status is 0 if all is well,
/// [`budget::EXIT_OVER_BUDGET`] if a pass is over budget and 1 if the bench could not run.
pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    match args::parse(args) {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(args)) => match run(&args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("bench failed: {err}");
                ExitCode::FAILURE
            }
        },
        Err(err) => {
            eprintln!("{err}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<u8, Box<dyn Error>> {
    // Every hardware backend of the run is made with it (#236).
    if let Some(grouping) = args.hw_group {
        render::hw::set_process_grouping(grouping);
    }
    if let Some(blas) = args.hw_blas_budget {
        render::hw::set_process_blas_budget(blas);
    }
    render::hw::set_process_wait(args.hw_wait);
    let (budgets, source) = match &args.budgets {
        Some(file) => {
            let text = fs::read_to_string(file)
                .map_err(|e| format!("cannot read budgets {}: {e}", file.display()))?;
            let budgets =
                Budgets::parse(&text).map_err(|e| format!("budgets {}: {e}", file.display()))?;
            (budgets, file.display().to_string())
        }
        None => (
            Budgets::parse(budget::EMBEDDED).expect("the built-in budgets parse"),
            "embedded".to_string(),
        ),
    };
    // The real-time mode takes tens of minutes: a budgets file that cannot gate it is found out now.
    let real_time_budgets = match (args.real_time, budgets.streaming) {
        (true, None) => {
            return Err(format!(
                "budgets {source}: --real-time needs the [streaming] table (frame_time_p99_ms, \
                 frame_cpu_p99_ms, holes, scene_mib)"
            )
            .into());
        }
        (_, budgets) => budgets,
    };
    let mut plan = if args.short {
        Plan::short()
    } else {
        Plan::full()
    };
    if let Some((width, height)) = args.size {
        (plan.width, plan.height) = (width, height);
    }
    fs::create_dir_all(&args.out)
        .map_err(|e| format!("cannot create {}: {e}", args.out.display()))?;

    let started = Instant::now();
    let mut flight = new_flight(&plan, args)?;
    eprintln!("scene ready after {:.1?}", started.elapsed());
    let resolution = Resolution {
        internal: (plan.width, plan.height),
        output: (plan.width * 2, plan.height * 2),
    };
    let mut gpu = Offscreen::with_backend(resolution, &flight.scene, args.rt.main_backend())?;
    let choice = gpu.rt().clone();
    if let Some(why) = &choice.fallback {
        eprintln!(
            "warning: --rt-backend {}: {why}; this is a compute run",
            args.rt.name()
        );
    }
    let initial_accel = gpu.take_hardware_updates().into_iter().next();
    prepare(&mut gpu, &plan, args.time_of_day);
    eprintln!(
        "the Planet takes {:.1} MiB of GPU memory",
        gpu.scene_bytes() as f64 / (1024.0 * 1024.0)
    );
    let info = gpu.gpu_info().clone();
    eprintln!(
        "GPU: {} ({}, {}, {})",
        info.name, info.device_type, info.backend, info.driver
    );
    if info.device_type == "Cpu" {
        eprintln!("warning: this is a software renderer; the timings are not those of a GPU");
    }
    if !gpu.timers_supported() {
        eprintln!("warning: the GPU has no timestamp queries; passes cannot be timed");
    }
    gpu.set_flush_scene_updates(!args.diag.no_flush);
    let mut screenshots = Vec::new();
    let primary = run_mode(
        &mut gpu,
        &mut flight,
        &plan.path,
        plan.primary,
        Mode::Primary,
        &budgets,
        Some((&args.out, &mut screenshots)),
    )?;
    let reference = run_mode(
        &mut gpu,
        &mut flight,
        &plan.path,
        plan.reference,
        Mode::Reference,
        &budgets,
        Some((&args.out, &mut screenshots)),
    )?;
    let modes = Modes { primary, reference };
    let altitudes = if plan.patch {
        Vec::new()
    } else {
        sweep(&mut gpu, &mut flight, &budgets)?
    };
    let edits = if plan.patch {
        None
    } else {
        eprintln!("edits: the largest Brush at the metric rate...");
        settle_for_edits(&mut gpu, &mut flight, EDIT_HEIGHTS[1])?;
        edits::run(
            &mut gpu,
            &mut flight,
            &budgets,
            EditMode::Dig,
            edits::FRAMES,
        )?
    };
    // The same stress by the ground, where its Edits are in level 0 (see `ground_edits`): on the
    // run's own backend, so that `--rt-backend both` has compute's to put by hw's.
    let edits_ground = match &edits {
        Some(first) => {
            eprintln!("edits: by the ground...");
            ground_edits(&mut gpu, &mut flight, &budgets, first)?
        }
        None => None,
    };
    // The day cycle (#203) is on the flight's GPU and path, after everything that is timed against
    // the budgets and before the GPU is let go; its own gates are its own.
    let day_cycle = if args.day_cycle {
        let seconds = args.day_seconds.unwrap_or(if plan.patch {
            daycycle::DEFAULT_DAY_SECONDS / 10.0
        } else {
            daycycle::DEFAULT_DAY_SECONDS
        });
        let follow_camera = plan.path.camera_at(0, 2);
        match daycycle::run(
            &mut gpu,
            &mut flight,
            &plan.path,
            seconds,
            // The cycle starts where `--time-of-day` lit the flight, or at midnight.
            args.time_of_day.map_or(0.0, |hours| hours / 24.0),
            &follow_camera,
        ) {
            Ok(report) => Some(report),
            Err(e) => {
                eprintln!("warning: the day cycle failed, and is left out of the report: {e}");
                None
            }
        }
    } else {
        None
    };
    let timers_supported = gpu.timers_supported();
    let main_accel = choice
        .active
        .is_hardware()
        .then(|| accel_report(initial_accel.as_ref(), &flight, edits.as_ref(), &gpu));
    // The cave stills are of another scene, and another GPU context: this one's is done with.
    drop(gpu);
    drop(flight);
    let (caves, cave_shots) = if plan.patch {
        (Vec::new(), Vec::new())
    } else {
        cave_stills(&plan, &budgets, &args.out)?
    };
    screenshots.extend(cave_shots);
    // `--rt-backend both`: the hardware backend's flight, after what is timed against the budgets.
    let (comparison, compare_fallback) = if args.rt == RtMode::Both {
        match realtime::catching(|| {
            compare_hardware(&plan, args, resolution, &budgets, &modes.primary)
        }) {
            Ok(done) => done,
            Err(e) => {
                eprintln!("warning: the hardware comparison failed, and is left out: {e}");
                (None, Some(format!("the hardware run failed: {e}")))
            }
        }
    } else {
        (None, None)
    };
    let mut rt = RtReport {
        requested: args.rt.name().to_string(),
        active: choice.active.name().to_string(),
        fallback: choice.fallback.clone().or(compare_fallback),
        accel: main_accel,
        comparison,
        real_time_hw: None,
        hw_group_regions: render::hw::process_grouping().describe(),
    };
    // The convergence phase (#187) has GPU contexts of its own, one for each of its views, and
    // runs after everything that is timed against the budgets, so that it cannot
    // move any of it. It is not timed against a budget itself.
    let convergence = if args.convergence {
        let traced = (plan.width, plan.height);
        let mut settings = if plan.patch {
            convergence::Settings::short(traced, args.truth_spp)
        } else {
            convergence::Settings::full(traced, args.truth_spp, args.flip_gamma22)
        };
        if let Some(rays) = args.shadow_rays {
            settings.shadows.shadow_rays = rays;
        }
        if let Some(frames) = args.converged_frames {
            settings.shadows.converged_frames = frames;
            settings.gi.converged_frames = frames;
        }
        if let Some(weight) = args.shadow_weight {
            settings.shadows.shadow_weight = weight;
            settings.gi.shadow_weight = weight;
        }
        // It is not budgeted, so a phase that fails must not take the timed results with it: not
        // by an error, and not by a panic either (the readback's `expect`s, wgpu's uncaptured-error
        // handler, a worker of FLIP's thread pool), which would end the process with status 101
        // and no report.
        match realtime::catching(|| convergence::run(&settings, &args.out)) {
            Ok(report) => Some(report),
            Err(e) => {
                eprintln!(
                    "warning: the convergence phase failed, and is left out of the report: {e}"
                );
                None
            }
        }
    } else {
        None
    };
    // The streaming mode runs last, so that `--streaming` cannot move the timings of anything else:
    // it has a GPU context of its own, and what it leaves behind (memory, a warm driver) comes after
    // every other measurement.
    let streaming = if args.streaming {
        Some(run_streaming(&plan, resolution)?)
    } else {
        None
    };
    // The real-time mode runs after the stepped one, for the same reason: its own world and GPU. If
    // a run of it stops on an error, the report still has everything that was done before, and the
    // error: tens of minutes of the 4090's time are not thrown away for the last run's.
    let stream_slice = args
        .stream_slice_ms
        .map(|ms| std::time::Duration::from_millis(u64::from(ms)));
    let streaming_real_time = match real_time_budgets {
        Some(budgets) if args.real_time => Some(realtime::run(
            !plan.patch,
            args.runs,
            resolution,
            args.flat_ambient,
            &budgets,
            stream_slice,
            args.rt.main_backend(),
        )),
        _ => None,
    };
    // `--rt-backend both` with the real-time mode (#236): the same runs again, on the hardware
    // backend, to put its frame thread's update and its frame time beside the compute backend's.
    rt.real_time_hw = match real_time_budgets {
        Some(budgets) if args.real_time && args.rt == RtMode::Both => Some(realtime::run(
            !plan.patch,
            args.runs,
            resolution,
            args.flat_ambient,
            &budgets,
            stream_slice,
            RtBackend::Hardware,
        )),
        _ => None,
    };

    let report = Report {
        schema_version: SCHEMA_VERSION,
        build: Build {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: option_env!("GITHUB_SHA").map(str::to_string),
        },
        machine: Machine {
            gpu: info.name,
            device_type: info.device_type,
            driver: info.driver,
            driver_info: info.driver_info,
            backend: info.backend,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            timers_supported,
        },
        config: Config {
            width: plan.width,
            height: plan.height,
            output_width: resolution.output.0,
            output_height: resolution.output.1,
            scene: if plan.patch { "patch" } else { "planet" }.to_string(),
            path: plan.path_name.to_string(),
            keyframes: plan.path.keyframes(),
            bias: args.bias,
            time_of_day: args.time_of_day,
            stream_slice_ms: stream_slice
                .unwrap_or(crate::scene_worker::BACKED_UP_SLICE)
                .as_secs_f64()
                * 1e3,
            diag: args.diag.switches(),
        },
        altitudes,
        caves,
        screenshots,
        budgets: report::summarize(
            &source,
            &budgets,
            &modes,
            &edits.as_ref().map_or(Vec::new(), |e| e.entries().to_vec()),
            &args.diag.switches(),
        ),
        edits,
        edits_ground,
        convergence,
        modes,
        streaming,
        streaming_real_time,
        rt,
        day_cycle,
    };
    let json = serde_json::to_string_pretty(&report)?;
    let file = args.out.join(REPORT_FILE);
    fs::write(&file, &json).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    print_summary(&report);
    eprintln!("report and screenshots are in {}", args.out.display());
    // The report is also the only thing on stdout, to paste or to pipe.
    println!("{json}");

    // A diagnostic run is not held to the budgets: it exits 0 whatever they say.
    let code = if report.config.diag.is_empty() {
        let mut statuses = report.modes.statuses();
        statuses.extend(report.edits.iter().flat_map(edits::EditsReport::statuses));
        budget::exit_code(&statuses, args.no_fail)
    } else {
        0
    };
    // A hole is a failure of the streaming mode whatever `--diag` said of the floating one.
    let streaming_code = report
        .streaming
        .as_ref()
        .map_or(0, |s| stream::exit_code(s, args.no_fail));
    if streaming_code != 0
        && let Some(s) = &report.streaming
    {
        eprintln!(
            "streaming: {} bricks were split with the bricks under them not resident, in {} frames",
            s.holes.total, s.holes.frames_with
        );
    }
    // A gate of the real-time mode is a failure whatever `--diag` said of the floating one.
    let real_time_code = report
        .streaming_real_time
        .as_ref()
        .map_or(0, |r| realtime::exit_code(r, args.no_fail));
    if real_time_code != 0
        && let Some(r) = &report.streaming_real_time
    {
        eprintln!(
            "streaming (real time): over its gates: {}",
            r.gates.over.join(", ")
        );
    }
    // The hardware run of `--rt-backend both` is held to the same gates (#236): it is the one
    // that must pass them with the acceleration structures on.
    let real_time_hw_code = report
        .rt
        .real_time_hw
        .as_ref()
        .map_or(0, |r| realtime::exit_code(r, args.no_fail));
    if real_time_hw_code != 0
        && let Some(r) = &report.rt.real_time_hw
    {
        eprintln!(
            "streaming (real time, hardware rays): over its gates: {}",
            r.gates.over.join(", ")
        );
    }
    // A gate of the GI (#54) is a failure whatever `--diag` said: it holds Primary to the Reference.
    let gi_code = report
        .convergence
        .as_ref()
        .map_or(0, |c| convergence::gi_exit_code(c, args.no_fail));
    if gi_code != 0
        && let Some(c) = &report.convergence
    {
        eprintln!(
            "convergence: over its gates: {}",
            c.gi.gates.over.join(", ")
        );
    }
    // The day cycle's gates (#203): a frame over the limit, a p99 over it.
    let day_code = match &report.day_cycle {
        Some(d) if !d.gates.over.is_empty() => {
            eprintln!("day cycle: over its gates: {}", d.gates.over.join(", "));
            if args.no_fail { 0 } else { 2 }
        }
        _ => 0,
    };
    let code = code
        .max(streaming_code)
        .max(real_time_code)
        .max(real_time_hw_code)
        .max(gi_code)
        .max(day_code);
    // An error of the real-time mode is exit status 1, not 2 (a gate that is over): nothing says
    // whether its gates would have held.
    // The hardware half of `--rt-backend both` is the same (an adapter with no ray queries stops it
    // with its reason, #236: its gates would have been compute's).
    let hw_error = report
        .rt
        .real_time_hw
        .as_ref()
        .and_then(|r| r.error.as_ref());
    if let Some(e) = hw_error {
        eprintln!(
            "streaming (real time, hardware rays) stopped at `{}`: {}",
            e.step, e.message
        );
    }
    let code = match &report.streaming_real_time {
        Some(r) if r.error.is_some() => 1,
        _ if hw_error.is_some() => 1,
        _ => code,
    };
    if code != 0 {
        let (no_data, over) = failures(&report.modes);
        if !over.is_empty() {
            eprintln!("over budget: {}", over.join(", "));
        }
        if !no_data.is_empty() {
            eprintln!("could not be timed: {}", no_data.join(", "));
        }
    }
    Ok(code)
}

/// The scene of a flight, made afresh: the patch, or the Planet generated (which takes a while).
fn new_flight(plan: &Plan, args: &Args) -> Result<Flight, Box<dyn Error>> {
    Ok(if plan.patch {
        Flight {
            scene: patch_scene(),
            store: None,
            diag: args.diag,
            accel: Default::default(),
        }
    } else {
        eprintln!("generating the Planet (this takes a while)...");
        let mut store = planet_gen::generate_planet(crate::SEED);
        store.build_lod(crate::RING_LEVELS - 1);
        // The bands are for the image that is presented: twice the traced height.
        let bands = render::lod::Bands::new(plan.height * 2, render::lod::VFOV, args.bias);
        let scene = crate::planet_scene(
            &store,
            plan.path.camera_at(0, 2).position,
            bands,
            planet_gen::MAX_RADIUS,
        )?;
        Flight {
            scene,
            store: Some(store),
            diag: args.diag,
            accel: Default::default(),
        }
    })
}

/// What the flight needs of a context before it is timed.
fn prepare(gpu: &mut Offscreen, plan: &Plan, time_of_day: Option<f64>) {
    // The flight times what the player's frame costs, and the player's Primary has the radiance
    // cache's GI (#54) and the denoiser (#55): an `Offscreen` starts with the flat ambient of before
    // them and no denoiser.
    gpu.set_flat_ambient(false);
    gpu.set_denoise(render::Denoise::Full);
    if !plan.patch {
        // The atmosphere measures the camera's height from the Planet's sea level.
        gpu.set_ground_level(f64::from(planet_gen::world_map::WORLD_RADIUS));
    }
    gpu.set_keep_history(true);
    // `--time-of-day`: the flight is lit by the world clock's sky at that time, stopped there.
    if let Some(hours) = time_of_day {
        let mut daylight = render::Daylight::new();
        let drive = daylight.step(hours / 24.0, 0);
        gpu.set_day_sky(drive.sky, drive.jumped);
    }
}

/// The hardware backend's acceleration structures over a run: its first build, the updates of the
/// flight, and those of the Edit stress.
fn accel_report(
    initial: Option<&HwUpdate>,
    flight: &Flight,
    edits: Option<&edits::EditsReport>,
    gpu: &Offscreen,
) -> AccelReport {
    AccelReport {
        group_regions: gpu.hardware_grouping().describe(),
        initial: initial.map(AccelUpdate::from).unwrap_or_default(),
        flight: AccelSummary::of(&flight.accel.measured),
        flight_frames: flight.accel.measured_frames,
        warmup: AccelSummary::of(&flight.accel.warmup),
        first_frame: AccelSummary::of(&flight.accel.first_frame),
        sweep: AccelSummary::of(&flight.accel.sweep),
        settle: AccelSummary::of(&flight.accel.settle),
        edits: edits.and_then(|e| e.accel.clone()),
    }
}

/// One hardware flight of `--rt-backend both` (#50): its primary mode's times and what its
/// acceleration structures cost, and with `with_edits` its Edit stress too.
struct HardwareFlight {
    times: BackendTimes,
    accel: AccelReport,
    edits: Option<edits::EditsReport>,
    /// The Edit stress again with the camera by the ground, where the Edits are in level 0.
    ground: Option<edits::EditsReport>,
}

/// The flight (and with `with_edits` the Edit stress) on the hardware backend `backend`, on a
/// context of its own. `Ok(Err(why))` is an adapter that cannot do it; `Err` a failure to run.
fn fly_hardware(
    plan: &Plan,
    args: &Args,
    resolution: Resolution,
    budgets: &Budgets,
    backend: RtBackend,
    with_edits: bool,
) -> Result<Result<HardwareFlight, Option<String>>, Box<dyn Error>> {
    eprintln!("rt: the same flight on the {} backend...", backend.name());
    let mut flight = new_flight(plan, args)?;
    let mut gpu = Offscreen::with_backend(resolution, &flight.scene, backend)?;
    if gpu.rt().active != backend {
        return Ok(Err(gpu.rt().fallback.clone()));
    }
    let initial = gpu.take_hardware_updates().into_iter().next();
    prepare(&mut gpu, plan, args.time_of_day);
    gpu.set_flush_scene_updates(!args.diag.no_flush);
    let primary = run_mode(
        &mut gpu,
        &mut flight,
        &plan.path,
        plan.primary,
        Mode::Primary,
        budgets,
        None,
    )?;
    let edits = if plan.patch || !with_edits {
        None
    } else {
        eprintln!("rt: edits on the {} backend...", backend.name());
        settle_for_edits(&mut gpu, &mut flight, EDIT_HEIGHTS[1])?;
        edits::run(&mut gpu, &mut flight, budgets, EditMode::Dig, edits::FRAMES)?
    };
    // The Edits of the compute run's stress are in the coarse levels (the camera is a kilometre
    // up); a BLAS of level 0 is made again by an Edit by the ground, which this stress has: the
    // Edit+BVH of the BLAS sizes (#236). It goes on from where the first left the Planet.
    let ground = match &edits {
        Some(first) if with_edits => {
            eprintln!(
                "rt: edits by the ground on the {} backend...",
                backend.name()
            );
            ground_edits(&mut gpu, &mut flight, budgets, first)?
        }
        _ => None,
    };
    Ok(Ok(HardwareFlight {
        times: BackendTimes::of(backend.name(), &primary),
        accel: accel_report(initial.as_ref(), &flight, edits.as_ref(), &gpu),
        edits,
        ground,
    }))
}

/// `--rt-backend both` (#50): the flight and the Edit stress again on the hardware backend, and the
/// flight on the one with nearest-hit shadows, each on a context of its own, put by the compute
/// run's. `Err` is a failure to run; a missing feature is `Ok((None, Some(why)))`.
fn compare_hardware(
    plan: &Plan,
    args: &Args,
    resolution: Resolution,
    budgets: &Budgets,
    compute: &ModeReport,
) -> Result<(Option<Comparison>, Option<String>), Box<dyn Error>> {
    let run =
        |backend, with_edits| fly_hardware(plan, args, resolution, budgets, backend, with_edits);
    let hw = match run(RtBackend::Hardware, true)? {
        Ok(hw) => hw,
        Err(why) => {
            eprintln!(
                "warning: no hardware comparison: {}",
                why.as_deref()
                    .unwrap_or("the hardware backend did not start")
            );
            return Ok((None, why));
        }
    };
    // The nearest-hit shadows are a side question: if it cannot run, the comparison is without it.
    let nearest = match run(RtBackend::HardwareNearest, false) {
        Ok(Ok(n)) => Some(n.times),
        Ok(Err(why)) => {
            eprintln!("warning: no hw-nearest run: {why:?}");
            None
        }
        Err(e) => {
            eprintln!("warning: the hw-nearest run failed, and is left out: {e}");
            None
        }
    };
    let mut comparison = Comparison::new(
        BackendTimes::of("compute", compute),
        hw.times,
        nearest,
        hw.accel,
        hw.edits,
    );
    comparison.hw_edits_ground = hw.ground;
    Ok((Some(comparison), None))
}

/// The streaming mode (see [`stream`]): the test Planet streamed in, stepped, and drawn on a GPU of
/// its own, which is the one thing in the report that is timed.
fn run_streaming(
    plan: &Plan,
    resolution: Resolution,
) -> Result<stream::StreamReport, Box<dyn Error>> {
    eprintln!("streaming: making the test Planet's world (this takes a while)...");
    let streamer = stream::Streamer::new();
    let mut gpu = Offscreen::with_resolution(resolution, streamer.scene())?;
    gpu.set_flat_ambient(false);
    gpu.set_denoise(render::Denoise::Full);
    gpu.set_ground_level(f64::from(planet_gen::world_map::TEST_RADIUS));
    streamer.set_buffer_limit(gpu.buffer_limit_words());
    // A frame is drawn every frame in a measurement, and every tenth in a smoke test, where a
    // software driver takes long to draw one.
    let mut sink = stream::GpuSink::new(&mut gpu, if plan.patch { 10 } else { 1 });
    let mut report = streamer.run(&mut sink)?;
    report.timing = Some(sink.timing());
    Ok(report)
}

/// `mode/name` of the entries with no times to check their budget, and of those over budget.
fn failures(modes: &Modes) -> (Vec<String>, Vec<String>) {
    let named = |status| modes.names_with(status);
    (named(Status::NoData), named(Status::Over))
}

/// The patch of the test Planet near the +y pole that the golden image test also renders:
/// Voxels x -440..=447, y 1360..=1575, z -360..=263.
fn patch_scene() -> Scene {
    Scene::new(&convergence::patch_store(), None)
}

/// The Planet being flown over: its packed bricks and, for rings, the bricks they are made from.
pub(crate) struct Flight {
    scene: Scene,
    store: Option<BrickStore>,
    /// What `--diag` asked for.
    diag: Diag,
    /// The updates of the hardware backend's acceleration structures that following the camera
    /// made, from every mode flown so far (empty on the compute backend).
    accel: AccelLog,
}

/// The updates of the hardware backend's acceleration structures (#50), by where in a run they
/// were made: the jumps of the camera, which rebuild all of them, are not the flight's.
#[derive(Default)]
struct AccelLog {
    /// Moving to the end of the path, where each mode warms up, and the warm-up frames.
    warmup: Vec<HwUpdate>,
    /// The first measured frame, which follows the camera back from the end of the path to its start:
    /// another jump.
    first_frame: Vec<HwUpdate>,
    /// The rest of the measured frames: the flight.
    measured: Vec<HwUpdate>,
    /// How many frames `measured` is of (the measured frames of every mode flown, less the first of
    /// each).
    measured_frames: u32,
    /// Everything the sweep made (its still frames and its jumps to each height and back):
    /// `sweep` calls `run_mode` eight times, and without this its frames would be counted as the
    /// flight's and its jumps as the warm-up's.
    sweep: Vec<HwUpdate>,
    /// Moving to where the Edit stress starts (`settle_for_edits`).
    settle: Vec<HwUpdate>,
}

impl Flight {
    /// The pause `--diag gap-ms=F` puts before a frame, standing in for the CPU's time spent on
    /// following the camera. It is not part of `scene_update_ms`.
    fn pause(&self) {
        if self.diag.gap_ms > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(self.diag.gap_ms / 1e3));
        }
    }

    /// Moves the rings to the camera, if the scene has rings, and copies what that changed to
    /// the GPU. A window does the same before it draws a frame; the time it takes is not part
    /// of any pass.
    fn follow(&mut self, gpu: &mut Offscreen, camera: &Camera) -> Result<(), Box<dyn Error>> {
        if self.diag.no_follow {
            return Ok(());
        }
        if let Some(store) = &self.store
            && let Some(update) = self.scene.recentre(store, camera.position)?
            && !self.diag.no_upload
        {
            gpu.update_scene(&self.scene, &update)?;
        }
        Ok(())
    }
}

/// Draws a frame, and returns its image if `read_back`. Primary mode only reads the image back
/// for a screenshot: at 1440p it is 15 MB a frame, which would be most of the CPU time of the
/// frame and leave the GPU idle between frames (and so at lower clocks than a real frame has).
pub(crate) fn draw(
    gpu: &mut Offscreen,
    mode: Mode,
    camera: &Camera,
    spp: u32,
    read_back: bool,
) -> Option<Image> {
    match mode {
        Mode::Primary if read_back => Some(gpu.render_upscaled(camera)),
        Mode::Primary => {
            gpu.draw_upscaled(camera);
            None
        }
        Mode::Reference => Some(gpu.render_reference(camera, spp)),
    }
}

/// Flies `path` in one mode: warm-up frames, then the measured ones. With `shots`, a screenshot of
/// each keyframe goes to the folder, and its name to the list, and the bricks drawn at each
/// keyframe are noted; without, there are none of either.
fn run_mode(
    gpu: &mut Offscreen,
    flight: &mut Flight,
    path: &CameraPath,
    share: ModePlan,
    mode: Mode,
    budgets: &Budgets,
    mut shots: Option<(&Path, &mut Vec<String>)>,
) -> Result<ModeReport, Box<dyn Error>> {
    let name = match mode {
        Mode::Primary => "primary",
        Mode::Reference => "reference",
    };
    eprintln!(
        "{name}: {} warm-up and {} measured frames...",
        share.warmup, share.frames
    );
    let passes_before = gpu.timings().passes().len();
    // Warm up at the end of the path, away from where the measured frames start, so that frame 0
    // of the reference mode starts a new image.
    let end = path.camera_at(share.frames.saturating_sub(1), share.frames);
    flight.follow(gpu, &end)?;
    for _ in 0..share.warmup {
        draw(gpu, mode, &end, share.spp, false);
    }
    flight.accel.warmup.extend(gpu.take_hardware_updates());
    // The timers' histories hold everything since the start of the run, warm-up and the other
    // mode's frames included. From here on is this mode's.
    let timings = gpu.timings();
    let starts: Vec<usize> = timings
        .passes()
        .iter()
        .map(|p| p.times.history().len())
        .collect();
    let total_start = timings.total().history().len();

    let keyframes = path.keyframe_frames(share.frames);
    let mut wall = Vec::new();
    let mut scene_update = Vec::new();
    let mut lod_at_keyframes = Vec::new();
    for frame in 0..share.frames {
        let camera = path.camera_at(frame, share.frames);
        flight.pause();
        let following = Instant::now();
        flight.follow(gpu, &camera)?;
        scene_update.push(following.elapsed().as_secs_f64() * 1e3);
        if shots.is_some() && keyframes.contains(&frame) && flight.scene.is_rings() {
            let radius = f64::from(planet_gen::PLANET_RADIUS);
            let distance = camera.position.iter().map(|c| c * c).sum::<f64>().sqrt();
            lod_at_keyframes.push(KeyframeLod {
                frame,
                height_m: (distance - radius) / brick_store::VOXELS_PER_METRE,
                bricks: (0..render::lod::LEVELS)
                    .map(|level| flight.scene.level_bricks(level))
                    .collect(),
            });
        }
        let began = Instant::now();
        let shot = shots.is_some() && keyframes.contains(&frame);
        let image = draw(gpu, mode, &camera, share.spp, shot);
        wall.push(began.elapsed().as_secs_f64() * 1e3);
        if frame == 0 {
            flight.accel.first_frame.extend(gpu.take_hardware_updates());
        }
        if let (true, Some(image), Some((out, names))) = (shot, image, shots.as_mut()) {
            let file = format!("{name}-frame{frame:04}.png");
            write_png(&out.join(&file), &image)?;
            names.push(file);
        }
    }

    flight.accel.measured.extend(gpu.take_hardware_updates());
    flight.accel.measured_frames += share.frames.saturating_sub(1);
    let timings = gpu.timings();
    let passes: Vec<(String, &[f64])> = timings
        .passes()
        .iter()
        .enumerate()
        .filter_map(|(i, pass)| {
            let new = &pass.times.history()[starts.get(i).copied().unwrap_or(0)..];
            // A pass belongs to the mode that ran it: it has times from it, or it was first used
            // in it (when the GPU cannot time passes, there are no times to tell by).
            (!new.is_empty() || i >= passes_before).then(|| (pass.name.clone(), new))
        })
        .collect();
    Ok(report::mode_report(
        name,
        &Measured {
            frames: share.frames,
            warmup_frames: share.warmup,
            spp: share.spp,
            passes,
            gpu_total: &timings.total().history()[total_start..],
            wall: &wall,
            scene_update: &scene_update,
            lod_at_keyframes,
        },
        budgets,
    ))
}

/// Heights above the Planet's mean radius that the sweep looks from, in Voxels, straight above
/// its pole: 2.4 m (the ground), 50 m, 250 m and 1 km. The test Planet is a fixed number of
/// Voxels (see [`planet_gen`]), so these are too, whatever a Voxel measures.
const SWEEP_HEIGHTS: [f64; 4] = [48.0, 1000.0, 5000.0, 20000.0];

/// The heights the Edit stress is run from: by the ground (2.4 m), where its Edits are in level 0 and
/// the finest BLASes, and the sweep's last (1 km), where they are in the coarse levels only.
const EDIT_HEIGHTS: [f64; 2] = [SWEEP_HEIGHTS[0], SWEEP_HEIGHTS[3]];

/// The camera of the sweep at `height` Voxels over the pole.
fn sweep_keyframe(height: f64) -> Keyframe {
    let radius = f64::from(planet_gen::PLANET_RADIUS);
    Keyframe {
        position: [0.0, radius + height, 0.0],
        target: [0.0, radius, -1500.0],
    }
}

/// The Edit stress again with the camera by the ground (2.4 m up), where its Edits are in level 0
/// and in the finest BLASes (the altitude stress's are in the coarse levels: #236). It goes on from
/// where `first`, the altitude stress, left the Planet, with the Edit that undoes its last.
fn ground_edits(
    gpu: &mut Offscreen,
    flight: &mut Flight,
    budgets: &Budgets,
    first: &edits::EditsReport,
) -> Result<Option<edits::EditsReport>, Box<dyn Error>> {
    settle_for_edits(gpu, flight, EDIT_HEIGHTS[0])?;
    let start = if first.edits.is_multiple_of(2) {
        EditMode::Dig
    } else {
        EditMode::Place
    };
    edits::run(gpu, flight, budgets, start, edits::GROUND_FRAMES)
}

/// Puts the rings where the Edit stress starts from, on either backend: the camera of the sweep's
/// last height. The compute run reaches the stress from the sweep, which ends there, so this does
/// nothing to it; the hardware run of `--rt-backend both` has no sweep, and without this it would
/// start from the low end of the flight, with its Edits in level 0 and not in the coarse levels the
/// compute run's are in (#50), and `hw_edits` and `edits` would be of different Edits.
fn settle_for_edits(
    gpu: &mut Offscreen,
    flight: &mut Flight,
    height: f64,
) -> Result<(), Box<dyn Error>> {
    let at = sweep_keyframe(height);
    let camera = CameraPath::new(vec![at, at]).camera_at(0, 2);
    // Anything still logged is the last part's tail, and a jump's: with the warm-up's.
    flight.accel.warmup.extend(gpu.take_hardware_updates());
    flight.follow(gpu, &camera)?;
    flight.accel.settle.extend(gpu.take_hardware_updates());
    // A dig is never in the frame of the jump: the game draws frames in between, and each waits
    // for the GPU, so what the jump left behind (its upload, the old buffers it freed, the
    // hardware's builds) is paid for in those frames and not in the first Edit's (#236: that Edit's
    // submit was 5.8 ms on compute and 7.0 on hardware, against 0.06 on the Edits after it, with
    // the stress going on at once). What they cost is printed.
    let frame_ms: Vec<f64> = (0..SETTLE_FRAMES)
        .map(|_| {
            let began = Instant::now();
            gpu.draw_upscaled(&camera);
            began.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    eprintln!(
        "settle: {SETTLE_FRAMES} frames drawn before the Edits, in ms: {}",
        frame_ms
            .iter()
            .map(|ms| format!("{ms:.1}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    flight.accel.settle.extend(gpu.take_hardware_updates());
    Ok(())
}

/// The frames drawn between the jump to where the Edit stress starts and its first Edit (see
/// `settle_for_edits`).
const SETTLE_FRAMES: u32 = 6;

/// The test Planet from a still camera at each of [`SWEEP_HEIGHTS`], looking at the ground that
/// slopes away towards the horizon: both modes' passes timed there, over a few dozen frames, and
/// the bricks the bands select. It follows the flight and does not change it.
fn sweep(
    gpu: &mut Offscreen,
    flight: &mut Flight,
    budgets: &Budgets,
) -> Result<Vec<Altitude>, Box<dyn Error>> {
    let primary = ModePlan {
        frames: 24,
        warmup: 4,
        spp: 1,
    };
    let reference = ModePlan {
        frames: 6,
        warmup: 2,
        spp: 2,
    };
    // The flight's log is set aside, and the sweep's is its own.
    let outside = std::mem::take(&mut flight.accel);
    let mut altitudes = Vec::new();
    for height in SWEEP_HEIGHTS {
        let at = sweep_keyframe(height);
        let path = CameraPath::new(vec![at, at]);
        eprintln!("sweep: {:.0} m", height / brick_store::VOXELS_PER_METRE);
        let by_mode = |gpu: &mut Offscreen, flight: &mut Flight, share, mode| {
            run_mode(gpu, flight, &path, share, mode, budgets, None)
        };
        let primary = by_mode(gpu, flight, primary, Mode::Primary)?;
        let bricks = (0..render::lod::LEVELS)
            .map(|level| flight.scene.level_bricks(level))
            .collect();
        let reference = by_mode(gpu, flight, reference, Mode::Reference)?;
        altitudes.push(Altitude {
            height_m: height / brick_store::VOXELS_PER_METRE,
            bricks,
            primary: pass_stats(&primary, "primary"),
            upscale: pass_stats(&primary, "upscale"),
            reference: pass_stats(&reference, "reference"),
        });
    }
    let swept = std::mem::replace(&mut flight.accel, outside);
    flight.accel.sweep.extend(
        swept
            .warmup
            .into_iter()
            .chain(swept.first_frame)
            .chain(swept.measured)
            .chain(swept.settle)
            .chain(swept.sweep),
    );
    Ok(altitudes)
}

/// The times of the pass `name` in `report`: none if no such pass ran.
fn pass_stats(report: &ModeReport, name: &str) -> Stats {
    report
        .passes
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.stats.clone())
        .unwrap_or_default()
}

/// The cave stills (see [`caves`]): a camera at a cave mouth on a slope and one inside a
/// tunnel, each timed over a few dozen frames like the sweep's, and photographed. Returns their
/// times and the names of the screenshots.
fn cave_stills(
    plan: &Plan,
    budgets: &Budgets,
    out: &Path,
) -> Result<(Vec<CaveStill>, Vec<String>), Box<dyn Error>> {
    eprintln!("cave stills: making a cave of the Planet...");
    let (planet, site) = caves::site();
    let shares = caves::Shares {
        primary: ModePlan {
            frames: 24,
            warmup: 4,
            spp: 1,
        },
        reference: ModePlan {
            frames: 6,
            warmup: 2,
            spp: 2,
        },
    };
    caves::run(
        &planet,
        &site,
        (plan.width, plan.height),
        &shares,
        budgets,
        out,
    )
}

fn write_png(path: &Path, image: &Image) -> Result<(), Box<dyn Error>> {
    let file = BufWriter::new(
        File::create(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?,
    );
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&image.rgba)?;
    Ok(())
}

/// The day cycle's lines (#203).
fn print_day_cycle(d: &daycycle::DayCycleReport) {
    let (e, fixed) = (&d.exposure, &d.exposure_fixed_camera);
    eprintln!(
        "\nday cycle: a day of {} s, {} frames: frame p99 {:.2} ms, max {:.2} ms, {} over {} ms; the \
         sun moved at most {:.2} deg a frame, {} jumps; exposure {:.2} to {:.2} stops, at most {:.2} \
         stops/s, travel {:.2} of its range, {} reversals; from a fixed camera {:.2} to {:.2} stops, \
         at most {:.2} stops/s, travel {:.2} of its range, {} reversals",
        d.day_seconds,
        d.frames,
        d.frame_p99_ms,
        d.frame_ms.max_ms.unwrap_or(0.0),
        d.frames_over_limit,
        daycycle::FRAME_LIMIT_MS,
        d.sun_step_max_degrees,
        d.sun_jumps,
        e.min_stops,
        e.max_stops,
        e.max_stops_per_s,
        e.travel_over_range,
        e.reversals,
        fixed.min_stops,
        fixed.max_stops,
        fixed.max_stops_per_s,
        fixed.travel_over_range,
        fixed.reversals
    );
    for f in &d.cache_follow {
        eprintln!(
            "  cache follow, {} s day from {:.2} (sun {:+.0} deg, {:.1} deg drift): {:.2} levels from a jump's, the floor {:.2}",
            f.day_seconds,
            f.time_of_day,
            f.sun_elevation,
            f.drifted_degrees,
            f.drifted_levels,
            f.floor_levels
        );
    }
}

/// The report in brief, for whoever is watching the run.
fn print_summary(report: &Report) {
    let ms = |v: Option<f64>| v.map_or("n/a".to_string(), |v| format!("{v:.3}"));
    if !report.config.diag.is_empty() {
        eprintln!(
            "\nWARNING: a diagnostic run (--diag {}), not a measurement: the budgets were not \
             checked, and these times are for finding out where time goes",
            report.config.diag.join(",")
        );
    }
    if let Some(d) = &report.day_cycle {
        print_day_cycle(d);
    }
    if let Some(s) = &report.streaming {
        print_streaming(s);
    }
    if let Some(r) = &report.streaming_real_time {
        print_real_time(r);
    }
    if let Some(r) = &report.rt.real_time_hw {
        eprintln!("the same real-time runs on the hardware backend (--rt-backend both):");
        print_real_time(r);
    }
    if let Some(c) = &report.convergence {
        convergence::print_summary(c);
    }
    for a in &report.altitudes {
        eprintln!(
            "at {:>7.1} m: primary {} (p95 {}), upscale {}, reference {} (p95 {}) ms; bricks {:?}",
            a.height_m,
            ms(a.primary.mean_ms),
            ms(a.primary.p95_ms),
            ms(a.upscale.mean_ms),
            ms(a.reference.mean_ms),
            ms(a.reference.p95_ms),
            a.bricks
        );
    }
    if let Some(e) = &report.edits {
        eprintln!(
            "edits: {} Edits of a {} in {} frames, {:.1} m3/s: per frame {} ms (budget {:?}), per Edit {} (p95 {}, max {}) ms; frame thread {} (max {}, budget {:?}), scene thread {} (max {}) ms",
            e.edits,
            e.brush,
            e.frames,
            e.m3_per_s,
            ms(e.per_frame.stats.mean_ms),
            e.per_frame.budget_ms,
            ms(e.per_edit.mean_ms),
            ms(e.per_edit.p95_ms),
            ms(e.per_edit.max_ms),
            ms(e.frame_thread.stats.mean_ms),
            ms(e.frame_thread.stats.max_ms),
            e.frame_thread.budget_ms,
            ms(e.scene_thread.mean_ms),
            ms(e.scene_thread.max_ms)
        );
    }
    for c in &report.caves {
        eprintln!(
            "cave {:<6}: primary {} (p95 {}), upscale {}, reference {} (p95 {}) ms; bricks {:?}",
            c.name,
            ms(c.primary.mean_ms),
            ms(c.primary.p95_ms),
            ms(c.upscale.mean_ms),
            ms(c.reference.mean_ms),
            ms(c.reference.p95_ms),
            c.bricks
        );
    }
    for (mode, run) in [
        ("primary", &report.modes.primary),
        ("reference", &report.modes.reference),
    ] {
        eprintln!("\n{mode} ({} frames)", run.frames);
        eprintln!(
            "  {:<12} {:>9} {:>9} {:>9} {:>9}  status",
            "ms", "mean", "p95", "max", "budget"
        );
        for e in run.passes.iter().chain([&run.gpu_total]) {
            eprintln!(
                "  {:<12} {:>9} {:>9} {:>9} {:>9}  {}",
                e.name,
                ms(e.stats.mean_ms),
                ms(e.stats.p95_ms),
                ms(e.stats.max_ms),
                ms(e.budget_ms),
                serde_json::to_value(e.status)
                    .ok()
                    .and_then(|s| s.as_str().map(str::to_string))
                    .unwrap_or_default()
            );
        }
        eprintln!(
            "  {:<12} {:>9} {:>9} {:>9}",
            "frame (cpu)",
            ms(run.frame_time.mean_ms),
            ms(run.frame_time.p95_ms),
            ms(run.frame_time.max_ms)
        );
        eprintln!(
            "  {:<12} {:>9} {:>9} {:>9}",
            "scene (cpu)",
            ms(run.scene_update_ms.mean_ms),
            ms(run.scene_update_ms.p95_ms),
            ms(run.scene_update_ms.max_ms)
        );
        for k in &run.lod_at_keyframes {
            eprintln!(
                "  frame {:>4} at {:>8.1} m: bricks per level {:?}",
                k.frame, k.height_m, k.bricks
            );
        }
    }
    eprintln!();
}

/// The real-time mode's paths and gates, in brief: the worst of the runs.
fn print_real_time(r: &realtime::RealTimeReport) {
    eprintln!(
        "streaming, real time ({} Planet, d0 {}; {} runs; not deterministic):",
        r.planet,
        r.d0,
        r.runs.len()
    );
    for (n, run) in r.runs.iter().enumerate() {
        let c = &run.cold_start;
        eprintln!(
            "  run {}: control {:.2} s after the world map (load {:.1} s: spawn ring {:.2} s, {} of {} cells of the Guard {:.1} s), level 0 under the camera {:.1} s after the load began, Guard complete after {:.1} s, resident set complete after {:.1} s",
            n + 1,
            c.time_to_control_s,
            c.load_s,
            c.spawn_ring_s,
            c.guard_cells_at_control,
            c.guard_cells,
            c.guard_s,
            c.time_to_level_0_under_camera_s,
            c.guard_complete_s,
            c.fill_s
        );
        for p in run.paths.iter().chain([&run.unpaced_control]) {
            eprintln!(
                "    {:<20} {:>5} frames: frame p99 {:>6.2} worst {:>7.2} ms, cpu p99 {:>6.2} worst {:>7.2} ms, soft misses {:.2} a frame, holes {} in {} looks ({:.0} ms, {:.0}% of the scene thread's busy time; the end look {:.0} ms), {} frames over the limit{}",
                p.name,
                p.frames,
                p.frame_time.p99_ms,
                p.frame_time.worst_ms,
                p.frame_cpu.p99_ms,
                p.frame_cpu.worst_ms,
                p.soft_misses_per_frame,
                p.holes,
                p.hole_looks,
                p.scene_thread.hole_scan_ms,
                p.scene_thread.hole_scan_share * 100.0,
                p.end_look_ms,
                p.frames_over_limit,
                p.settle.map_or(String::new(), |s| format!(
                    ", settled after {:.1} s{}",
                    s.seconds,
                    if s.settled {
                        ""
                    } else {
                        " (not settled: the cap)"
                    }
                ))
            );
            // What the mean frame is made of, and the GPU's own time of each pass (means).
            let m = &p.frame_parts;
            eprintln!(
                "        mean frame {:.2} ms = recentre {:.2} + update {:.2} + encode {:.2} + submit {:.2} + wait {:.2} (GPU passes {:.2}, the rest of the wait {:.2})",
                p.frame_time.mean_ms,
                m.recentre.mean_ms,
                m.update.mean_ms,
                m.encode.mean_ms,
                m.submit.mean_ms,
                m.wait.mean_ms,
                m.gpu_total.mean_ms,
                m.wait_not_passes.mean_ms,
            );
            eprintln!(
                "        frame thread's scene update: mean {:.2} p95 {:.2} max {:.2} ms",
                m.update.mean_ms, m.update.p95_ms, m.update.worst_ms,
            );
            if let Some(h) = &p.hardware {
                eprintln!(
                    "        hardware per frame, mean (p95) ms: total {:.2} ({:.2}) = before {:.2} + plan {:.2} + instances {:.2} + boxes pass {:.2} + builds {:.2} + finish {:.2} + submit {:.2} ({:.2}); update less hardware {:.2}; BLAS {:.1} (p95 {:.0}, max {}; urgent {:.1}), {} us each, {} frames with builds; draw submit {:.2} ({:.2}) with {:.0} TLAS instances",
                    h.total.mean_ms,
                    h.total.p95_ms,
                    h.before.mean_ms,
                    h.plan.mean_ms,
                    h.instances.mean_ms,
                    h.boxes_pass.mean_ms,
                    h.builds.mean_ms,
                    h.finish.mean_ms,
                    h.submit.mean_ms,
                    h.submit.p95_ms,
                    h.update_rest.mean_ms,
                    h.blas.mean,
                    h.blas.p95,
                    h.blas.max,
                    h.blas_urgent.mean,
                    h.per_blas_us.total.round(),
                    h.frames_with_builds,
                    h.draw_submit.mean_ms,
                    h.draw_submit.p95_ms,
                    h.tlas_instances.mean,
                );
            }
            if !p.gpu_passes.is_empty() {
                let passes: Vec<String> = p
                    .gpu_passes
                    .iter()
                    .map(|(name, s)| format!("{name} {:.3}", s.mean_ms))
                    .collect();
                eprintln!("        GPU passes, mean ms: {}", passes.join(", "));
            }
            eprintln!(
                "        loader: {} bricks generated, {} arrived, queue max {} mean {:.0}; scene thread reband {:.0} ms{}",
                p.generated_bricks,
                p.arrived_bricks,
                p.loader_queue_max,
                p.loader_queue_mean,
                p.scene_thread.reband_ms,
                p.settle.map_or(String::new(), |s| format!(
                    "; settle: walk {:.1} s, reband {:.1} s, fill {:.1} s",
                    s.walk_s, s.reband_s, s.fill_s
                ))
            );
            // Where the time of the slowest frames went, when there were frames over the limit.
            if p.frames_over_limit > 0 {
                for f in p.slowest.iter().take(3) {
                    eprintln!(
                        "        frame {:>5} at {:>8.0} ms: {:>6.2} ms = recentre {:.2} + update {:.2} ({} writes, {} {} B) + encode {:.2} + submit {:.2} + GPU wait {:.2} + other {:.2}; camera before {:.2}, memory after {:.2}{}",
                        f.index,
                        f.at_ms,
                        f.time_ms,
                        f.recentre_ms,
                        f.update_ms,
                        f.writes,
                        f.update,
                        f.upload_bytes,
                        f.encode_ms,
                        f.submit_ms,
                        f.wait_ms,
                        f.other_ms,
                        f.camera_ms,
                        f.after_ms,
                        if f.hole_look {
                            ", hole look running"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        let t = &run.scene_thread;
        eprintln!(
            "    scene thread: follow {:.0}, reband {:.0}, pack {:.0}, upload {:.0}, eviction {:.0}, coarse {:.0}, hole looks {:.0} ms; scene used peak {:.0} MiB (reserved {:.0} MiB), store peak {} of {} bricks, resident peak {} MiB (whole process {} MiB); {} looks, {} holes in the run; {}",
            t.follow_ms,
            t.reband_ms,
            t.pack_ms,
            t.upload_ms,
            t.evict_ms,
            t.coarse_ms,
            t.hole_scan_ms,
            run.memory.scene_used_peak_mib,
            run.memory.scene_reserved_mib,
            run.memory.store_peak_bricks,
            run.memory.store_limit_bricks,
            run.memory
                .resident_peak_mib
                .map_or("n/a".to_string(), |m| format!("{m:.0}")),
            run.memory
                .whole_process_peak_mib
                .map_or("n/a".to_string(), |m| format!("{m:.0}")),
            run.hole_looks,
            run.holes,
            run.edit_latency.map_or_else(
                || format!("no Edit made ({})", run.edit_not_made.unwrap_or("?")),
                |e| format!(
                    "Edit taken in {} frames, {:.1} ms (its call {:.1} ms)",
                    e.frames, e.ms, e.call_ms
                )
            )
        );
        for c in &run.sizing.checks {
            eprintln!(
                "    {:<24} constant {:>7.2} (limit {:>7.2}), measured {:>7.2}{}",
                c.constant,
                c.value,
                c.limit,
                c.measured,
                if c.over { "  OVER" } else { "" }
            );
        }
    }
    let g = &r.gates;
    for (name, limit) in [
        ("frame time p99 (ms)", g.frame_time_p99_ms),
        ("frame cpu p99 (ms)", g.frame_cpu_p99_ms),
        ("holes", g.holes),
        ("scene (MiB)", g.scene_mib),
        ("store (bricks)", g.store_bricks),
        ("soft-miss frames, flights", g.camera_soft_miss_frames),
        ("scene lag, flights (d0)", g.scene_lag_d0_max),
        ("spawn ring (s)", g.spawn_ring_s),
        ("time to control (s)", g.time_to_control_s),
        ("settle (s)", g.settle_s),
        ("gi pass mean (ms)", g.gi_mean_ms),
        ("gi pass p99 (ms)", g.gi_p99_ms),
    ] {
        eprintln!(
            "  gate {name:<20} worst {:>12.2} limit {:>12.2}{}",
            limit.worst,
            limit.limit,
            if limit.worst > limit.limit {
                "  OVER"
            } else {
                ""
            }
        );
    }
    if let Some(e) = &r.error {
        eprintln!(
            "  STOPPED in run {} at `{}`: {} (the runs above are those that finished)",
            e.run, e.step, e.message
        );
    }
}

/// The streaming mode's counts, in brief.
fn print_streaming(s: &stream::StreamReport) {
    eprintln!(
        "streaming ({}, d0 {}; {} fill, {} steady, {} moving frames; counts {}):",
        s.planet,
        s.d0,
        s.schedule.fill_frames,
        s.soft_misses.steady.bricks.frames,
        s.soft_misses.moving.bricks.frames,
        s.counts_hash
    );
    eprintln!(
        "  upload {} bytes in {} updates ({} patch bytes, {} rebuilds), at most {} in a frame",
        s.upload.bytes.total,
        s.upload.updates,
        s.upload.patch_bytes,
        s.upload.rebuilds,
        s.upload.bytes.max
    );
    for (phase, m) in [
        ("fill", &s.soft_misses.fill),
        ("steady", &s.soft_misses.steady),
        ("moving", &s.soft_misses.moving),
    ] {
        eprintln!(
            "  soft misses, {phase}: {} bricks in {} of {} frames, at most {}",
            m.bricks.total, m.bricks.frames_with, m.bricks.frames, m.bricks.max
        );
    }
    eprintln!(
        "  holes: {} in {} frames. Diverged bricks {}, store {} at the complete set and {} at the end (budget {}), an Edit shows after {} frames",
        s.holes.total,
        s.holes.frames_with,
        s.residency.diverged_bricks,
        s.residency.store_when_complete,
        s.residency.store_at_end,
        s.residency.budget,
        s.residency.edit_latency_frames
    );
    eprintln!(
        "  scene work: follow {} steps {} bricks, pack {} steps {} bricks, upload {} steps {} bytes, eviction {} steps {} units {} bricks",
        s.work.follow_steps,
        s.work.follow_bricks,
        s.work.pack_steps,
        s.work.pack_bricks,
        s.work.upload_steps,
        s.work.upload_bytes,
        s.work.evict_steps,
        s.work.evicted_units,
        s.work.evicted_bricks
    );
    for c in &s.sizing.checks {
        eprintln!(
            "  {:<24} constant {:>7.2} (limit {:>7.2}), measured {:>7.2}{}",
            c.constant,
            c.value,
            c.limit,
            c.measured,
            if c.over { "  OVER" } else { "" }
        );
    }
    if let Some(t) = &s.timing {
        eprintln!(
            "  (not deterministic) copy {:?} ms, draw {:?} ms over {} drawn frames; the GPU wrote {} bytes",
            t.upload_ms.mean_ms, t.draw_ms.mean_ms, t.drawn_frames, t.gpu_uploaded_bytes
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `run_mode` puts in each of the flight's logs of the hardware backend's updates (#50):
    /// the jump to where a mode warms up and the warm-up in `warmup`, the first measured frame (the
    /// camera going back to the start) in `first_frame`, the rest in `measured` with the number of
    /// frames it is of, and a full rebuild from before the mode in none of the last two. The path
    /// is 30 000 Voxels long, past the TLAS's frame, so that the camera's moves make updates in the
    /// draws. Skips itself where the adapter has no ray queries. Makes an `Offscreen` (a Vulkan
    /// device), so the Windows job, which has none, skips it.
    #[test]
    fn the_flights_updates_are_logged_by_where_in_the_run_they_were_made() {
        let mut flight = Flight {
            scene: patch_scene(),
            store: None,
            diag: Diag::default(),
            accel: Default::default(),
        };
        let resolution = Resolution::native(32, 18);
        let mut gpu =
            Offscreen::with_backend(resolution, &flight.scene, RtBackend::Hardware).unwrap();
        if gpu.rt().active != RtBackend::Hardware {
            eprintln!("skipped: {:?}", gpu.rt().fallback);
            return;
        }
        gpu.take_hardware_updates();
        // A full rebuild before the mode flies, as the first follow of a real run is.
        gpu.update_scene(&flight.scene, &render::SceneUpdate::Rebuilt)
            .unwrap();
        let key = |x: f64| Keyframe {
            position: [x, 1565.0, 0.0],
            target: [x, 1500.0, 100.0],
        };
        let path = CameraPath::new(vec![key(0.0), key(30_000.0)]);
        let share = ModePlan {
            frames: 6,
            warmup: 1,
            spp: 1,
        };
        let budgets = Budgets::parse("[passes]\n").unwrap();
        run_mode(
            &mut gpu,
            &mut flight,
            &path,
            share,
            Mode::Primary,
            &budgets,
            None,
        )
        .unwrap();
        let log = &flight.accel;
        let kinds = |updates: &[HwUpdate]| updates.iter().map(|u| u.kind).collect::<Vec<_>>();
        assert!(kinds(&log.warmup).contains(&"build"), "{:?}", log.warmup);
        assert!(kinds(&log.warmup).contains(&"anchor"), "{:?}", log.warmup);
        assert_eq!(kinds(&log.first_frame), ["anchor"], "{:?}", log.first_frame);
        assert!(
            !log.measured.is_empty(),
            "the camera crosses the TLAS's frame"
        );
        assert!(
            log.measured.iter().all(|u| u.kind == "anchor"),
            "{:?}",
            log.measured
        );
        assert_eq!(log.measured_frames, 5);
        assert!(log.settle.is_empty());
        // And what the report sums is the steady flight's alone.
        let accel = accel_report(None, &flight, None, &gpu);
        // What the report says of the BLAS size is the context's own.
        assert_eq!(accel.group_regions, gpu.hardware_grouping().describe());
        assert_eq!(accel.flight.updates as usize, log.measured.len());
        assert_eq!(accel.flight_frames, 5);
        assert_eq!(accel.warmup.updates as usize, log.warmup.len());
        // Moving to where the Edit stress starts keeps what was still queued, in the warm-up's
        // part (#236: it used to throw it away).
        let before = flight.accel.warmup.len();
        gpu.update_scene(&flight.scene, &render::SceneUpdate::Rebuilt)
            .unwrap();
        settle_for_edits(&mut gpu, &mut flight, EDIT_HEIGHTS[1]).unwrap();
        assert_eq!(flight.accel.warmup.len(), before + 1);
        assert_eq!(flight.accel.warmup.last().map(|u| u.kind), Some("build"));
    }
}
