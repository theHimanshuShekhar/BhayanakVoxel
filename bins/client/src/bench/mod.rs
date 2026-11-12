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
mod edits;
mod path;
mod process;
mod realtime;
mod report;
mod stats;
mod stream;

use std::error::Error;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use brick_store::{BrickCoord, BrickStore};
use render::{Camera, Image, Mode, Offscreen, Resolution, Scene};

use args::{Args, Diag, Parsed, USAGE};
use budget::{Budgets, Status};
use path::{CameraPath, Keyframe};
use report::{
    Altitude, Build, CaveStill, Config, KeyframeLod, Machine, Measured, ModeReport, Modes, Report,
    SCHEMA_VERSION,
};
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
    let mut flight = if plan.patch {
        Flight {
            scene: patch_scene(),
            store: None,
            diag: args.diag,
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
        }
    };
    eprintln!("scene ready after {:.1?}", started.elapsed());
    let resolution = Resolution {
        internal: (plan.width, plan.height),
        output: (plan.width * 2, plan.height * 2),
    };
    let mut gpu = Offscreen::with_resolution(resolution, &flight.scene)?;
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
    gpu.set_keep_history(true);

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
        edits::run(&mut gpu, &mut flight, &budgets)?
    };
    let timers_supported = gpu.timers_supported();
    // The cave stills are of another scene, and another GPU context: this one's is done with.
    drop(gpu);
    drop(flight);
    let (caves, cave_shots) = if plan.patch {
        (Vec::new(), Vec::new())
    } else {
        cave_stills(&plan, &budgets, &args.out)?
    };
    screenshots.extend(cave_shots);
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
    let streaming_real_time = match real_time_budgets {
        Some(budgets) if args.real_time => {
            Some(realtime::run(!plan.patch, args.runs, resolution, &budgets))
        }
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
        modes,
        streaming,
        streaming_real_time,
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
    let code = code.max(streaming_code).max(real_time_code);
    // An error of the real-time mode is exit status 1, not 2 (a gate that is over): nothing says
    // whether its gates would have held.
    let code = match &report.streaming_real_time {
        Some(r) if r.error.is_some() => 1,
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

/// The streaming mode (see [`stream`]): the test Planet streamed in, stepped, and drawn on a GPU of
/// its own, which is the one thing in the report that is timed.
fn run_streaming(
    plan: &Plan,
    resolution: Resolution,
) -> Result<stream::StreamReport, Box<dyn Error>> {
    eprintln!("streaming: making the test Planet's world (this takes a while)...");
    let streamer = stream::Streamer::new();
    let mut gpu = Offscreen::with_resolution(resolution, streamer.scene())?;
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
    let store = planet_gen::generate_region(
        crate::SEED,
        BrickCoord([-55, 170, -45]),
        BrickCoord([55, 196, 32]),
    );
    Scene::new(&store, None)
}

/// The Planet being flown over: its packed bricks and, for rings, the bricks they are made from.
struct Flight {
    scene: Scene,
    store: Option<BrickStore>,
    /// What `--diag` asked for.
    diag: Diag,
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
fn draw(
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
        if let (true, Some(image), Some((out, names))) = (shot, image, shots.as_mut()) {
            let file = format!("{name}-frame{frame:04}.png");
            write_png(&out.join(&file), &image)?;
            names.push(file);
        }
    }

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
    let radius = f64::from(planet_gen::PLANET_RADIUS);
    let mut altitudes = Vec::new();
    for height in SWEEP_HEIGHTS {
        let at = Keyframe {
            position: [0.0, radius + height, 0.0],
            target: [0.0, radius, -1500.0],
        };
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
    if let Some(s) = &report.streaming {
        print_streaming(s);
    }
    if let Some(r) = &report.streaming_real_time {
        print_real_time(r);
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
            "  run {}: level 0 under the camera {:.1} s after the load began (load {:.1} s), resident set complete after {:.1} s",
            n + 1,
            c.time_to_level_0_under_camera_s,
            c.load_s,
            c.fill_s
        );
        for p in &run.paths {
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
            "    scene thread: follow {:.0}, pack {:.0}, upload {:.0}, eviction {:.0}, coarse {:.0}, hole looks {:.0} ms; scene used peak {:.0} MiB (reserved {:.0} MiB), store peak {} of {} bricks, resident peak {} MiB (whole process {} MiB); {} looks, {} holes in the run; {}",
            t.follow_ms,
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
