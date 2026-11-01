//! THROWAWAY (#99, "Measure the current reference view on the 4090"; branch
//! `measure/reference-99`, never merged): `client measure-reference MODE`.
//!
//! Offscreen like `client bench`, on a still camera. Modes:
//!   converge    one sample a dispatch; at each checkpoint writes the 8-bit PNG the player would
//!               see (the window's own blit, at output resolution), the raw f32 accumulation and a
//!               JSON sidecar; records every sample's GPU time
//!   truth       the same, but only the final sample count (for two independent seeds)
//!   batch       k samples a pixel in an in-lane loop, one dispatch: k = 1, 2, 3 at the chosen
//!               size and k = 1, 2, 4, 8 at 1280x720; time per dispatch and per sample
//!   split       one sample by part: primary ray only, plus the sun's shadow ray, plus 1..4 bounces
//!   stop-clean  PNGs at fixed times after accumulation starts, for judging "clean" by eye
//! Each writes `summary.json` in `--out`.

mod views;

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use render::tracer::RNG_SEED;
use render::{Camera, Image, Offscreen, Resolution, Variant};
use serde_json::{Value, json};

use views::View;

const USAGE: &str = "\
Usage: client measure-reference MODE --out DIR [OPTIONS]

MODE is converge, truth, batch, split or stop-clean. Throwaway tooling for issue #99.

Options:
  --out DIR          Output folder (created) [required]
  --view NAME        golden or interreflect [default: golden]
  --scale F          Traced size as a share of the output's per axis: 0.5 or 1.0 [default: 0.5]
  --size WxH         Output (presented) size [default: 2560x1440]
  --checkpoints L    converge: sample counts to capture [default: 1,4,16,64,256,1024]
  --seed N           Random seed of the reference (decimal or 0x hex) [default: the engine's]
  --truth-spp N      truth: samples to accumulate [default: 8192]
  --k L              batch: samples a pixel a dispatch at the chosen size [default: 1,2,3]
  --k720 L           batch: the same at 1280x720 [default: 1,2,4,8]
  --dispatches N     batch, split: timed dispatches of each variant [default: 192]
  --bounces N        converge, truth: diagnostic only, trace with MAX_BOUNCES = N (0..=4) in the
                     temporary kernel instead of the production one [default: production]
  --times L          stop-clean: seconds after accumulation starts [default: 0.1,0.25,0.5,1,2,4]";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Converge,
    Truth,
    Batch,
    Split,
    StopClean,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Converge => "converge",
            Mode::Truth => "truth",
            Mode::Batch => "batch",
            Mode::Split => "split",
            Mode::StopClean => "stop-clean",
        }
    }
}

struct Args {
    mode: Mode,
    out: PathBuf,
    view: String,
    scale: f32,
    size: (u32, u32),
    checkpoints: Vec<u32>,
    seed: u32,
    truth_spp: u32,
    k: Vec<u32>,
    k720: Vec<u32>,
    dispatches: u32,
    times: Vec<f64>,
    bounces: Option<u32>,
}

fn list<T: std::str::FromStr>(text: &str, what: &str) -> Result<Vec<T>, String> {
    text.split(',')
        .map(|item| {
            item.trim()
                .parse()
                .map_err(|_| format!("`{item}` is not a valid {what}"))
        })
        .collect()
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Args>, String> {
    let mut args = args.into_iter();
    let mode = match args.next().as_deref() {
        Some("converge") => Mode::Converge,
        Some("truth") => Mode::Truth,
        Some("batch") => Mode::Batch,
        Some("split") => Mode::Split,
        Some("stop-clean") => Mode::StopClean,
        Some("-h" | "--help") | None => return Ok(None),
        Some(other) => return Err(format!("unknown mode `{other}`")),
    };
    let mut parsed = Args {
        mode,
        out: PathBuf::new(),
        view: "golden".into(),
        scale: 0.5,
        size: (2560, 1440),
        checkpoints: vec![1, 4, 16, 64, 256, 1024],
        seed: RNG_SEED,
        truth_spp: 8192,
        k: vec![1, 2, 3],
        k720: vec![1, 2, 4, 8],
        dispatches: 192,
        times: vec![0.1, 0.25, 0.5, 1.0, 2.0, 4.0],
        bounces: None,
    };
    while let Some(arg) = args.next() {
        let mut value = |option: &str| args.next().ok_or(format!("{option} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--out" => parsed.out = value("--out")?.into(),
            "--view" => parsed.view = value("--view")?,
            "--scale" => {
                parsed.scale = value("--scale")?
                    .parse()
                    .map_err(|_| "--scale is a number like 0.5")?
            }
            "--size" => {
                let text = value("--size")?;
                let (w, h) = text.split_once('x').ok_or("--size is like 2560x1440")?;
                parsed.size = (
                    w.parse().map_err(|_| "--size is like 2560x1440")?,
                    h.parse().map_err(|_| "--size is like 2560x1440")?,
                );
            }
            "--checkpoints" => {
                parsed.checkpoints = list(&value("--checkpoints")?, "sample count")?;
                parsed.checkpoints.sort_unstable();
                parsed.checkpoints.dedup();
            }
            "--seed" => {
                let text = value("--seed")?;
                parsed.seed = match text.strip_prefix("0x") {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => text.parse(),
                }
                .map_err(|_| "--seed is a 32-bit number")?;
            }
            "--truth-spp" => {
                parsed.truth_spp = value("--truth-spp")?
                    .parse()
                    .map_err(|_| "--truth-spp is a number")?
            }
            "--k" => parsed.k = list(&value("--k")?, "k")?,
            "--k720" => parsed.k720 = list(&value("--k720")?, "k")?,
            "--dispatches" => {
                parsed.dispatches = value("--dispatches")?
                    .parse()
                    .map_err(|_| "--dispatches is a number")?
            }
            "--bounces" => {
                parsed.bounces = Some(
                    value("--bounces")?
                        .parse()
                        .map_err(|_| "--bounces is a number")?,
                )
            }
            "--times" => parsed.times = list(&value("--times")?, "number of seconds")?,
            _ => return Err(format!("unexpected argument `{arg}`")),
        }
    }
    if parsed.out.as_os_str().is_empty() {
        return Err("--out DIR is required".into());
    }
    if !(parsed.scale > 0.0 && parsed.scale <= 1.0) {
        return Err("--scale must be in (0, 1]".into());
    }
    if parsed.size.0 == 0 || parsed.size.1 == 0 {
        return Err("--size must be at least 1x1".into());
    }
    if parsed.k.contains(&0) || parsed.k720.contains(&0) || parsed.checkpoints.contains(&0) {
        return Err("k and sample counts are at least 1".into());
    }
    Ok(Some(parsed))
}

pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    match parse(args) {
        Ok(None) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Some(args)) => match run(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("measure-reference failed: {err}");
                ExitCode::FAILURE
            }
        },
        Err(err) => {
            eprintln!("{err}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Mean, median, 95th percentile (nearest rank, like the bench) and extremes, in milliseconds.
fn stats(times: &[f64]) -> Value {
    if times.is_empty() {
        return json!({"samples": 0, "mean_ms": null, "p50_ms": null, "p95_ms": null,
                      "p99_ms": null, "min_ms": null, "max_ms": null});
    }
    let mut sorted = times.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = |p: usize| sorted[(sorted.len() * p).div_ceil(100).max(1) - 1];
    json!({
        "samples": sorted.len(),
        "mean_ms": sorted.iter().sum::<f64>() / sorted.len() as f64,
        "p50_ms": rank(50), "p95_ms": rank(95), "p99_ms": rank(99),
        "min_ms": sorted[0], "max_ms": sorted[sorted.len() - 1],
    })
}

fn mean(times: &[f64]) -> Option<f64> {
    (!times.is_empty()).then(|| times.iter().sum::<f64>() / times.len() as f64)
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

fn write_f32(path: &Path, values: &[f32]) -> Result<(), Box<dyn Error>> {
    let mut file = BufWriter::new(
        File::create(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?,
    );
    for v in values {
        file.write_all(&v.to_le_bytes())?;
    }
    file.flush()?;
    Ok(())
}

fn write_json(path: &Path, value: &Value) -> Result<(), Box<dyn Error>> {
    fs::write(path, serde_json::to_string_pretty(value)?)
        .map_err(|e| format!("cannot write {}: {e}", path.display()).into())
}

/// Everything a run is set up with.
struct Run<'a> {
    args: &'a Args,
    gpu: Offscreen,
    camera: Camera,
    resolution: Resolution,
    description: Value,
    started: Instant,
}

impl Run<'_> {
    /// The header of `summary.json`.
    fn header(&self) -> serde_json::Map<String, Value> {
        let info = self.gpu.gpu_info();
        let args = self.args;
        let mut summary = serde_json::Map::new();
        let mut put = |key: &str, value: Value| {
            summary.insert(key.to_string(), value);
        };
        put(
            "tool",
            json!("client measure-reference (throwaway, issue #99)"),
        );
        put("mode", json!(args.mode.name()));
        put("commit", json!(env!("MEASURE_COMMIT")));
        put("commit_dirty", json!(env!("MEASURE_DIRTY") == "true"));
        put(
            "machine",
            json!({
                "gpu": info.name, "device_type": info.device_type, "driver": info.driver,
                "driver_info": info.driver_info, "backend": info.backend,
                "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
                "timers_supported": self.gpu.timers_supported(),
            }),
        );
        put(
            "config",
            json!({
                "view": args.view, "scale": args.scale,
                "output": [self.resolution.output.0, self.resolution.output.1],
                "internal": [self.resolution.internal.0, self.resolution.internal.1],
                "seed": args.seed, "engine_seed": RNG_SEED, "max_bounces": args.bounces.unwrap_or(4),
                "checkpoints": args.checkpoints, "truth_spp": args.truth_spp,
                "k": args.k, "k720": args.k720, "dispatches": args.dispatches,
                "times_s": args.times,
            }),
        );
        put("view", self.description.clone());
        summary
    }

    fn finish(&self, mut summary: serde_json::Map<String, Value>) -> Result<(), Box<dyn Error>> {
        summary.insert(
            "wall_clock_s".into(),
            json!(self.started.elapsed().as_secs_f64()),
        );
        let value = Value::Object(summary);
        write_json(&self.args.out.join("summary.json"), &value)?;
        eprintln!(
            "summary written to {}",
            self.args.out.join("summary.json").display()
        );
        Ok(())
    }

    /// The PNG the player would see, the raw accumulation and the sidecar, at `spp` samples.
    fn capture(&mut self, stem: &str, spp: u32) -> Result<Value, Box<dyn Error>> {
        let png = format!("{stem}.png");
        let raw = format!("{stem}.f32");
        let image = self.gpu.blit_reference();
        write_png(&self.args.out.join(&png), &image)?;
        let accumulation = self.gpu.read_accumulation();
        write_f32(&self.args.out.join(&raw), &accumulation)?;
        let (w, h) = self.resolution.internal;
        let sidecar = json!({
            "spp": spp,
            "width": w, "height": h, "channels": 3,
            "format": "f32 little-endian, interleaved RGB, rows from the top, linear radiance \
                       (the running mean, before encode()) at the traced (internal) size",
            "png": png, "png_width": image.width, "png_height": image.height,
            "png_what": "the traced image stretched to the output size by the window's blit",
            "seed": self.args.seed, "view": self.args.view, "scale": self.args.scale,
            "max_bounces": self.args.bounces.unwrap_or(4), "commit": env!("MEASURE_COMMIT"),
        });
        write_json(&self.args.out.join(format!("{stem}.json")), &sidecar)?;
        Ok(sidecar)
    }

    /// A short run so that clocks, caches and the driver settle; then the image starts over.
    fn warm_up(&mut self) {
        for _ in 0..16 {
            self.gpu.reference_sample(&self.camera);
        }
        self.gpu.restart();
    }

    /// Accumulates one sample at a time up to `last`, timing each, and captures at the counts
    /// `keep` says. Returns every sample's GPU time.
    fn accumulate(
        &mut self,
        last: u32,
        keep: impl Fn(u32) -> bool,
        stem: impl Fn(u32) -> String,
    ) -> Result<(Vec<f64>, Vec<Value>), Box<dyn Error>> {
        let mut times = Vec::with_capacity(last as usize);
        let mut captures = Vec::new();
        let began = Instant::now();
        for spp in 1..=last {
            let ms = match self.args.bounces {
                None => self.gpu.reference_sample(&self.camera),
                Some(bounces) => self.gpu.variant_dispatch(
                    &self.camera,
                    Variant {
                        k: 1,
                        bounces,
                        shadow: true,
                    },
                    spp - 1,
                ),
            };
            if let Some(ms) = ms {
                times.push(ms);
            }
            if keep(spp) {
                let at = Instant::now();
                captures.push(self.capture(&stem(spp), spp)?);
                eprintln!("  {spp} spp captured ({:.1?})", at.elapsed());
            } else if spp % 256 == 0 {
                eprintln!("  {spp}/{last} spp, {:.1?} elapsed", began.elapsed());
            }
        }
        Ok((times, captures))
    }
}

fn run(args: &Args) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(&args.out)
        .map_err(|e| format!("cannot create {}: {e}", args.out.display()))?;
    let view: View = match args.view.as_str() {
        "golden" => views::golden(),
        "interreflect" => views::interreflect(),
        other => return Err(format!("unknown view `{other}` (golden or interreflect)").into()),
    };
    let resolution = Resolution::scaled(args.size, args.scale);
    let mut gpu = Offscreen::with_resolution(resolution, &view.scene)?;
    gpu.set_sun(view.sun);
    gpu.set_seed(args.seed);
    let info = gpu.gpu_info().clone();
    eprintln!(
        "GPU: {} ({}, {}, {}); traced {:?}, presented {:?}",
        info.name,
        info.device_type,
        info.backend,
        info.driver,
        resolution.internal,
        resolution.output
    );
    if info.device_type == "Cpu" {
        eprintln!("warning: this is a software renderer; the timings are not those of a GPU");
    }
    if !gpu.timers_supported() {
        eprintln!("warning: the GPU has no timestamp queries; passes cannot be timed");
    }
    let mut run = Run {
        args,
        gpu,
        camera: view.camera,
        resolution,
        description: view.description,
        started: Instant::now(),
    };
    match args.mode {
        Mode::Converge | Mode::Truth => converge_or_truth(&mut run),
        Mode::Batch => batch(&mut run),
        Mode::Split => split(&mut run),
        Mode::StopClean => stop_clean(&mut run),
    }
}

fn converge_or_truth(run: &mut Run) -> Result<(), Box<dyn Error>> {
    let args = run.args;
    let truth = args.mode == Mode::Truth;
    let last = if truth {
        args.truth_spp
    } else {
        *args.checkpoints.last().expect("at least one checkpoint")
    };
    run.warm_up();
    eprintln!("{}: {last} spp, seed {}...", args.mode.name(), args.seed);
    let (times, captures) = if truth {
        run.accumulate(last, |spp| spp == last, |_| "truth".to_string())?
    } else {
        let checkpoints = args.checkpoints.clone();
        run.accumulate(
            last,
            |spp| checkpoints.contains(&spp),
            |spp| format!("spp{spp:05}"),
        )?
    };
    write_json(&args.out.join("sample_ms.json"), &json!(times))?;
    let mut summary = run.header();
    summary.insert("per_sample_gpu_ms".into(), stats(&times));
    summary.insert("captures".into(), json!(captures));
    summary.insert(
        "gpu_seconds_total".into(),
        json!(times.iter().sum::<f64>() / 1e3),
    );
    if let Some(mean) = mean(&times) {
        eprintln!(
            "per-sample GPU time: mean {mean:.3} ms, p95 {:.3} ms, max {:.3} ms",
            summary["per_sample_gpu_ms"]["p95_ms"]
                .as_f64()
                .unwrap_or(f64::NAN),
            summary["per_sample_gpu_ms"]["max_ms"]
                .as_f64()
                .unwrap_or(f64::NAN),
        );
    }
    run.finish(summary)
}

/// What `time_probes` can time.
#[derive(Clone, Copy)]
enum Probe {
    Variant(Variant),
    Production,
    Primary,
}

impl Probe {
    fn name(self) -> String {
        match self {
            Probe::Variant(v) => v.name(),
            Probe::Production => "production".into(),
            Probe::Primary => "primary".into(),
        }
    }
}

/// Times `dispatches` of each probe, in three rounds that take turns, so that drift in the
/// GPU's clocks or temperature falls on all of them alike; after 16 dispatches of each to settle.
fn time_probes(run: &mut Run, probes: &[Probe], dispatches: u32) -> Vec<Vec<f64>> {
    let rounds = 3;
    let per_round = dispatches.div_ceil(rounds).max(1);
    let mut next_sample = vec![0u32; probes.len()];
    let mut times = vec![Vec::new(); probes.len()];
    let mut dispatch = |run: &mut Run, i: usize| -> Option<f64> {
        match probes[i] {
            Probe::Variant(v) => {
                let first = next_sample[i];
                next_sample[i] += v.k;
                run.gpu.variant_dispatch(&run.camera, v, first)
            }
            Probe::Production => run.gpu.reference_sample(&run.camera),
            Probe::Primary => run.gpu.primary_dispatch(&run.camera),
        }
    };
    for i in 0..probes.len() {
        for _ in 0..16 {
            dispatch(run, i);
        }
    }
    for _ in 0..rounds {
        for (i, times) in times.iter_mut().enumerate() {
            for _ in 0..per_round {
                if let Some(ms) = dispatch(run, i) {
                    times.push(ms);
                }
            }
        }
    }
    times
}

fn batch(run: &mut Run) -> Result<(), Box<dyn Error>> {
    let args = run.args;
    let chosen = run.resolution;
    let p720 = Resolution {
        internal: (1280, 720),
        output: (2560, 1440),
    };
    let mut groups = vec![(chosen, args.k.clone())];
    if chosen.internal == p720.internal {
        groups[0].1.extend(&args.k720);
        groups[0].1.sort_unstable();
        groups[0].1.dedup();
    } else {
        groups.push((p720, args.k720.clone()));
    }
    let mut results = Vec::new();
    for (resolution, ks) in groups {
        run.gpu.resize(resolution);
        run.gpu.restart();
        eprintln!("batch at {:?}: k = {ks:?}", resolution.internal);
        let probes: Vec<Probe> = ks
            .iter()
            .map(|&k| {
                Probe::Variant(Variant {
                    k,
                    ..Variant::PRODUCTION
                })
            })
            .collect();
        let times = time_probes(run, &probes, args.dispatches);
        let base = ks
            .iter()
            .position(|&k| k == 1)
            .and_then(|i| mean(&times[i]));
        for (&k, times) in ks.iter().zip(&times) {
            let per_sample: Vec<f64> = times.iter().map(|t| t / f64::from(k)).collect();
            let dispatch_mean = mean(times);
            eprintln!(
                "  k={k}: dispatch mean {:.3} ms, p95 {:.3} ms; per sample mean {:.3} ms",
                dispatch_mean.unwrap_or(f64::NAN),
                stats(times)["p95_ms"].as_f64().unwrap_or(f64::NAN),
                mean(&per_sample).unwrap_or(f64::NAN)
            );
            results.push(json!({
                "internal": [resolution.internal.0, resolution.internal.1],
                "k": k,
                "dispatches": times.len(),
                "per_dispatch": stats(times),
                "per_sample": stats(&per_sample),
                // 1.0 means cost is exactly linear in k.
                "dispatch_mean_over_k_times_k1": dispatch_mean.zip(base).map(|(d, b)| d / (b * f64::from(k))),
            }));
        }
    }
    let mut summary = run.header();
    summary.insert("batch".into(), json!(results));
    run.finish(summary)
}

fn split(run: &mut Run) -> Result<(), Box<dyn Error>> {
    let args = run.args;
    let mut probes = vec![Probe::Production, Probe::Primary];
    for bounces in 0..=4 {
        for shadow in [false, true] {
            probes.push(Probe::Variant(Variant {
                k: 1,
                bounces,
                shadow,
            }));
        }
    }
    eprintln!(
        "split: {} variants x {} dispatches",
        probes.len(),
        args.dispatches
    );
    let times = time_probes(run, &probes, args.dispatches);
    let named = |name: &str| {
        probes
            .iter()
            .position(|p| p.name() == name)
            .map(|i| &times[i])
    };
    let mean_of = |name: &str| named(name).and_then(|t| mean(t));
    let diff = |a: &str, b: &str| mean_of(a).zip(mean_of(b)).map(|(a, b)| a - b);
    let mut variants = Vec::new();
    for (probe, times) in probes.iter().zip(&times) {
        eprintln!(
            "  {:<22} mean {:.3} ms, p95 {:.3} ms",
            probe.name(),
            mean(times).unwrap_or(f64::NAN),
            stats(times)["p95_ms"].as_f64().unwrap_or(f64::NAN)
        );
        variants.push(json!({"name": probe.name(), "stats": stats(times)}));
    }
    // Differences of means: what each part adds.
    let mut parts = serde_json::Map::new();
    parts.insert(
        "primary_ray_only_mean_ms".into(),
        json!(mean_of("ref-k1-b0-noshadow")),
    );
    parts.insert(
        "shadow_ray_adds_mean_ms".into(),
        json!(diff("ref-k1-b0-shadow", "ref-k1-b0-noshadow")),
    );
    for b in 1..=4 {
        parts.insert(
            format!("bounce_{b}_adds_mean_ms"),
            json!(diff(
                &format!("ref-k1-b{b}-shadow"),
                &format!("ref-k1-b{}-shadow", b - 1)
            )),
        );
        parts.insert(
            format!("bounce_{b}_continuation_only_adds_mean_ms"),
            json!(diff(
                &format!("ref-k1-b{b}-noshadow"),
                &format!("ref-k1-b{}-noshadow", b - 1)
            )),
        );
    }
    parts.insert(
        "full_path_mean_ms".into(),
        json!(mean_of("ref-k1-b4-shadow")),
    );
    parts.insert(
        "variant_template_vs_production_mean_ms".into(),
        json!(diff("ref-k1-b4-shadow", "production")),
    );
    let mut summary = run.header();
    summary.insert("variants".into(), json!(variants));
    summary.insert("parts_by_difference_of_means".into(), Value::Object(parts));
    run.finish(summary)
}

fn stop_clean(run: &mut Run) -> Result<(), Box<dyn Error>> {
    let args = run.args;
    run.warm_up();
    let mut accumulating = Duration::ZERO;
    let mut captures = Vec::new();
    for &t in &args.times {
        let target = Duration::from_secs_f64(t);
        let began = Instant::now();
        while accumulating + began.elapsed() < target {
            run.gpu.reference_submit(&run.camera);
        }
        run.gpu.wait_idle();
        accumulating += began.elapsed();
        let spp = run.gpu.samples();
        let stem = format!("t{t:05.2}s_spp{spp:05}");
        let mut capture = run.capture(&stem, spp)?;
        capture["after_s"] = json!(t);
        capture["accumulated_s"] = json!(accumulating.as_secs_f64());
        eprintln!("  {t} s: {spp} spp");
        captures.push(capture);
    }
    let mut summary = run.header();
    summary.insert("captures".into(), json!(captures));
    summary.insert(
        "note".into(),
        json!(
            "samples are submitted back to back with at most 3 in flight (the GPU runs flat \
               out); time spent capturing is not counted"
        ),
    );
    run.finish(summary)
}
