//! THROWAWAY (#99, branch `measure/reference-99`, never merged): analyses the captures that
//! `client measure-reference` wrote on the 4090. Runs anywhere.
//!
//! ```text
//! measure-ref-analyze CAP_DIR [--out DIR] [--ppd 67.02] [--thresholds 0.02,0.05] [--tile 16]
//! ```
//!
//! `CAP_DIR` holds one folder per view and scale, each with the folders `converge`, `truthA` and
//! `truthB` (and, if they were run, `batch` and `split`). It writes `analysis.json` and
//! `analysis.md` (the tables to paste into the issue) to `--out`, by default `CAP_DIR`.
//!
//! Metrics, per checkpoint, against truth A:
//!   - LDR-FLIP (flip-rs, a port of NVIDIA's FLIP v1.7) between the two 8-bit PNGs at a fixed
//!     ppd of 67.02: mean, and the weighted 99th percentile;
//!   - linear MSE between the two f32 accumulations (same traced size), and that minus half of
//!     MSE(A, B), which removes truth A's own noise (the truths are independent, so
//!     MSE(n, A) = s2/n + s2/N and MSE(A, B) = 2 s2/N);
//!   - MSE on the 8-bit PNGs (0..255 scale) and its PSNR.

use std::fs;
use std::path::{Path, PathBuf};

use flip_rs::{ErrorMap, RgbImage, Weighting, ldr_flip};
use serde_json::{Value, json};

const DEFAULT_PPD: f32 = 67.02;

type Res<T> = Result<T, String>;

struct Png {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

fn read_png(path: &Path) -> Res<Png> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("png too big")?];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if info.bit_depth != png::BitDepth::Eight || info.color_type != png::ColorType::Rgba {
        return Err(format!("{} is not 8-bit RGBA", path.display()));
    }
    let rgb = buffer[..info.buffer_size()]
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    Ok(Png {
        width: info.width as usize,
        height: info.height as usize,
        rgb,
    })
}

fn read_f32(path: &Path) -> Res<Vec<f32>> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect())
}

fn read_json(path: &Path) -> Res<Value> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// One capture: its PNG, its f32 accumulation and its sidecar.
struct Capture {
    spp: u32,
    png: Png,
    accumulation: Vec<f32>,
    sidecar: Value,
}

fn load_capture(dir: &Path, stem: &str) -> Res<Capture> {
    let sidecar = read_json(&dir.join(format!("{stem}.json")))?;
    let png = read_png(&dir.join(format!("{stem}.png")))?;
    let accumulation = read_f32(&dir.join(format!("{stem}.f32")))?;
    let channels = sidecar["channels"].as_u64().unwrap_or(3) as usize;
    let (w, h) = (
        sidecar["width"].as_u64().unwrap_or(0) as usize,
        sidecar["height"].as_u64().unwrap_or(0) as usize,
    );
    if accumulation.len() != w * h * channels {
        return Err(format!("{stem}: the f32 file does not match its sidecar"));
    }
    Ok(Capture {
        spp: sidecar["spp"].as_u64().unwrap_or(0) as u32,
        png,
        accumulation,
        sidecar,
    })
}

fn flip(reference: &Png, test: &Png, ppd: f32) -> Res<ErrorMap> {
    let image = |p: &Png| RgbImage::new(p.width, p.height, p.rgb.clone());
    let reference = image(reference).map_err(|e| e.to_string())?;
    let test = image(test).map_err(|e| e.to_string())?;
    ldr_flip(&reference, &test, ppd).map_err(|e| format!("FLIP failed: {e}"))
}

fn mse_f32(a: &[f32], b: &[f32]) -> f64 {
    let sum: f64 = a
        .iter()
        .zip(b)
        .map(|(a, b)| f64::from(a - b) * f64::from(a - b))
        .sum();
    sum / a.len() as f64
}

fn mse_u8(a: &[u8], b: &[u8]) -> f64 {
    let sum: f64 = a
        .iter()
        .zip(b)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
        .sum();
    sum / a.len() as f64
}

fn psnr(mse: f64) -> Option<f64> {
    (mse > 0.0).then(|| 10.0 * (255.0f64 * 255.0 / mse).log10())
}

/// Mean of the error map over each `tile` x `tile` block, row-major; edge blocks are smaller.
fn tile_means(map: &ErrorMap, tile: usize) -> (usize, usize, Vec<f32>) {
    let (w, h) = (map.width(), map.height());
    let (tw, th) = (w.div_ceil(tile), h.div_ceil(tile));
    let mut sums = vec![0f64; tw * th];
    let mut counts = vec![0u32; tw * th];
    for (i, &v) in map.pixels().iter().enumerate() {
        let t = (i / w / tile) * tw + (i % w) / tile;
        sums[t] += f64::from(v);
        counts[t] += 1;
    }
    let means = sums
        .iter()
        .zip(&counts)
        .map(|(s, &c)| (s / f64::from(c)) as f32)
        .collect();
    (tw, th, means)
}

/// The sample count at which a curve of (spp, error) first reaches `tau`, interpolating
/// log-linearly (log error against log spp) between the two checkpoints it falls between.
/// Past the last checkpoint it extrapolates along the line through the last two, and says so.
fn spp_to_threshold(points: &[(f64, f64)], tau: f64) -> (Option<f64>, &'static str) {
    let Some(&(first_spp, first_error)) = points.first() else {
        return (None, "no data");
    };
    if first_error <= tau {
        return (Some(first_spp), "at or below the first checkpoint");
    }
    for pair in points.windows(2) {
        let ((s0, e0), (s1, e1)) = (pair[0], pair[1]);
        if e0 > tau && e1 <= tau {
            let t = (tau.ln() - e0.ln()) / (e1.ln() - e0.ln());
            return (
                Some((s0.ln() + t * (s1.ln() - s0.ln())).exp()),
                "interpolated",
            );
        }
    }
    if let [.., (s0, e0), (s1, e1)] = points {
        let slope = (e1.ln() - e0.ln()) / (s1.ln() - s0.ln());
        if slope < 0.0 {
            let at = s1.ln() + (tau.ln() - e1.ln()) / slope;
            return (Some(at.exp()), "EXTRAPOLATED past the last checkpoint");
        }
    }
    (None, "not reached")
}

fn num(v: Option<f64>, digits: usize) -> String {
    v.map_or("n/a".into(), |v| format!("{v:.digits$}"))
}

fn sig(v: Option<f64>) -> String {
    v.map_or("n/a".into(), |v| format!("{v:.3e}"))
}

fn spp_text(v: Option<f64>) -> String {
    v.map_or("n/a".into(), |v| {
        if v >= 100.0 {
            format!("{v:.0}")
        } else {
            format!("{v:.1}")
        }
    })
}

/// What one view's analysis gives: numbers, and the Markdown to paste.
struct ViewReport {
    json: Value,
    markdown: String,
    /// A row of the table across views.
    summary_row: String,
}

struct Options {
    ppd: f32,
    thresholds: Vec<f64>,
    tile: usize,
}

fn analyse_view(name: &str, dir: &Path, options: &Options) -> Res<ViewReport> {
    let converge_dir = dir.join("converge");
    let summary = read_json(&converge_dir.join("summary.json"))?;
    let timing = &summary["per_sample_gpu_ms"];
    let (mean_ms, p95_ms, max_ms) = (
        timing["mean_ms"].as_f64(),
        timing["p95_ms"].as_f64(),
        timing["max_ms"].as_f64(),
    );
    let truth_a = load_capture(&dir.join("truthA"), "truth")?;
    let truth_b = load_capture(&dir.join("truthB"), "truth")?;
    let mut warnings = Vec::new();
    let seed_of = |c: &Capture| c.sidecar["seed"].as_u64();
    if seed_of(&truth_a) == seed_of(&truth_b) {
        warnings.push("truth A and B have the same seed: the floor is not a floor".to_string());
    }
    if truth_a.spp != truth_b.spp {
        warnings.push(format!(
            "truth A has {} spp and truth B {}",
            truth_a.spp, truth_b.spp
        ));
    }
    if seed_of(&truth_a) == summary["config"]["seed"].as_u64() {
        warnings.push(
            "the converge run and truth A share a seed: the first samples are identical, which \
             biases every error low"
                .to_string(),
        );
    }
    for (what, c) in [("truth A", &truth_a), ("truth B", &truth_b)] {
        if c.sidecar["max_bounces"] != json!(4) {
            warnings.push(format!(
                "{what} was not traced with the production 4 bounces"
            ));
        }
    }

    // The metric's floor: two independent truths.
    eprintln!("{name}: floor (truth A against truth B)...");
    let floor_map = flip(&truth_a.png, &truth_b.png, options.ppd)?;
    let floor_stats = floor_map.statistics();
    let floor_p99 = floor_map.percentile(0.99, Weighting::Weighted).ok();
    let floor_mse_lin = mse_f32(&truth_a.accumulation, &truth_b.accumulation);
    let floor_mse_8 = mse_u8(&truth_a.png.rgb, &truth_b.png.rgb);
    let (_, _, floor_tiles) = tile_means(&floor_map, options.tile);
    drop(floor_map);

    // The checkpoints, in order of sample count.
    let mut stems: Vec<String> = fs::read_dir(&converge_dir)
        .map_err(|e| format!("cannot read {}: {e}", converge_dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("spp") && n.ends_with(".json"))
        .map(|n| n.trim_end_matches(".json").to_string())
        .collect();
    stems.sort();
    if stems.is_empty() {
        return Err(format!("no checkpoints in {}", converge_dir.display()));
    }
    let mut rows = Vec::new();
    let mut tiles_by_checkpoint: Vec<Vec<f32>> = Vec::new();
    let mut tile_grid = (0, 0);
    for stem in &stems {
        let capture = load_capture(&converge_dir, stem)?;
        eprintln!("{name}: {} spp...", capture.spp);
        if capture.png.width != truth_a.png.width || capture.png.height != truth_a.png.height {
            return Err(format!("{stem} is not the size of the truth"));
        }
        let map = flip(&truth_a.png, &capture.png, options.ppd)?;
        let stats = map.statistics();
        let p99 = map.percentile(0.99, Weighting::Weighted).ok();
        let (tw, th, tiles) = tile_means(&map, options.tile);
        tile_grid = (tw, th);
        tiles_by_checkpoint.push(tiles);
        let mse_lin = mse_f32(&capture.accumulation, &truth_a.accumulation);
        let mse_8 = mse_u8(&capture.png.rgb, &truth_a.png.rgb);
        rows.push(json!({
            "spp": capture.spp,
            "flip_mean": stats.mean,
            "flip_p99_weighted": p99,
            "mse_linear": mse_lin,
            // MSE(n, A) = s2/n + s2/N; MSE(A, B) = 2 s2/N; so this is s2/n.
            "mse_linear_minus_half_floor": mse_lin - floor_mse_lin / 2.0,
            "mse_8bit": mse_8,
            "psnr_8bit_db": psnr(mse_8),
        }));
    }
    rows.sort_by_key(|r| r["spp"].as_u64());

    let points: Vec<(f64, f64)> = rows
        .iter()
        .map(|r| (r["spp"].as_f64().unwrap(), r["flip_mean"].as_f64().unwrap()))
        .collect();
    let mut thresholds = Vec::new();
    for &tau in &options.thresholds {
        let (spp, how) = spp_to_threshold(&points, tau);
        thresholds.push(json!({
            "flip_mean": tau,
            "spp_to_threshold": spp,
            "how": if floor_stats.mean > tau as f32 {
                format!("{how}; the floor FLIP(A,B) is itself above this threshold")
            } else {
                how.to_string()
            },
            "gpu_seconds_to_threshold_mean": spp.zip(mean_ms).map(|(s, m)| s * m / 1e3),
            "gpu_seconds_to_threshold_p95": spp.zip(p95_ms).map(|(s, m)| s * m / 1e3),
            "floor_is_above_threshold": floor_stats.mean > tau as f32,
        }));
    }
    let flip_at_64 = rows
        .iter()
        .find(|r| r["spp"] == 64)
        .map(|r| (r["flip_mean"].as_f64(), r["flip_p99_weighted"].as_f64()));

    // Stopped counts: the first checkpoint at which a tile is as clean as the threshold.
    let checkpoints: Vec<u64> = rows.iter().filter_map(|r| r["spp"].as_u64()).collect();
    let tile_count = tile_grid.0 * tile_grid.1;
    let mut stopped = Vec::new();
    for &tau in &options.thresholds {
        let mut by_checkpoint = vec![0usize; checkpoints.len()];
        let mut never = 0usize;
        let mut floor_limited = 0usize;
        let mut sum_n = 0f64;
        let mut sum_n_capped = 0f64;
        let last = *checkpoints.last().unwrap() as f64;
        for tile in 0..tile_count {
            if f64::from(floor_tiles[tile]) > tau {
                floor_limited += 1;
            }
            match tiles_by_checkpoint
                .iter()
                .position(|t| f64::from(t[tile]) <= tau)
            {
                Some(c) => {
                    by_checkpoint[c] += 1;
                    sum_n += checkpoints[c] as f64;
                    sum_n_capped += checkpoints[c] as f64;
                }
                None => {
                    never += 1;
                    sum_n_capped += last;
                }
            }
        }
        let stopping = tile_count - never;
        let mean_n = (stopping > 0).then(|| sum_n / stopping as f64);
        let mean_n_capped = sum_n_capped / tile_count as f64;
        let last_stop = by_checkpoint
            .iter()
            .rposition(|&n| n > 0)
            .map(|c| checkpoints[c] as f64);
        stopped.push(json!({
            "flip_mean_tile_threshold": tau,
            "tile": options.tile,
            "tiles": tile_count,
            "checkpoints": checkpoints,
            "tiles_stopping_at_checkpoint": by_checkpoint,
            "tiles_never_stopping_by_last_checkpoint": never,
            "tiles_whose_own_floor_is_above_threshold": floor_limited,
            "mean_stop_spp_of_tiles_that_stop": mean_n,
            "mean_stop_spp_never_counted_as_last_checkpoint": mean_n_capped,
            // N_all / mean(n): the adaptivity win for a whole-image threshold, as the scheduling
            // research defines G. If tiles never stop it is an upper bound.
            "gain_whole_image": last_stop.map(|n_all| n_all.max(if never > 0 { last } else { 0.0 }) / mean_n_capped),
            "gain_is_upper_bound_because_some_tiles_never_stop": never > 0,
        }));
    }

    // Markdown.
    let mut md = String::new();
    md += &format!("### {name}\n\n");
    let config = &summary["config"];
    md += &format!(
        "View `{}`, scale {}, traced {}x{}, presented {}x{}; converge seed {}, truths seeds {} and {}, truth {} spp. \
         Per-sample GPU time in this view: mean {} ms, p95 {} ms, max {} ms ({} samples).\n\n",
        config["view"].as_str().unwrap_or("?"),
        config["scale"],
        config["internal"][0],
        config["internal"][1],
        config["output"][0],
        config["output"][1],
        config["seed"],
        truth_a.sidecar["seed"],
        truth_b.sidecar["seed"],
        truth_a.spp,
        num(mean_ms, 3),
        num(p95_ms, 3),
        num(max_ms, 3),
        timing["samples"],
    );
    for w in &warnings {
        md += &format!("**WARNING: {w}**\n\n");
    }
    md += "| spp | GPU s (mean) | FLIP mean | FLIP p99 | MSE linear | MSE linear, truth noise removed | MSE 8-bit (LSB^2) | PSNR 8-bit (dB) |\n";
    md += "|---:|---:|---:|---:|---:|---:|---:|---:|\n";
    for r in &rows {
        let spp = r["spp"].as_f64().unwrap();
        md += &format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            spp as u64,
            num(mean_ms.map(|m| spp * m / 1e3), 3),
            num(r["flip_mean"].as_f64(), 4),
            num(r["flip_p99_weighted"].as_f64(), 4),
            sig(r["mse_linear"].as_f64()),
            sig(r["mse_linear_minus_half_floor"].as_f64()),
            num(r["mse_8bit"].as_f64(), 3),
            num(r["psnr_8bit_db"].as_f64(), 1),
        );
    }
    md += &format!(
        "\nFloor, truth A against truth B: FLIP mean {}, FLIP p99 {}, MSE linear {}, MSE 8-bit {} LSB^2.\n\n",
        num(Some(f64::from(floor_stats.mean)), 4),
        num(floor_p99.map(f64::from), 4),
        sig(Some(floor_mse_lin)),
        num(Some(floor_mse_8), 3),
    );
    md += "| FLIP mean threshold | spp to threshold | GPU s (mean per sample) | GPU s (p95 per sample) | how |\n|---:|---:|---:|---:|---|\n";
    for t in &thresholds {
        md += &format!(
            "| <= {} | {} | {} | {} | {} |\n",
            t["flip_mean"],
            spp_text(t["spp_to_threshold"].as_f64()),
            num(t["gpu_seconds_to_threshold_mean"].as_f64(), 2),
            num(t["gpu_seconds_to_threshold_p95"].as_f64(), 2),
            t["how"].as_str().unwrap_or(""),
        );
    }
    if let Some((mean, p99)) = flip_at_64 {
        md += &format!(
            "\nFLIP(64 spp raw, truth A): mean {}, p99 {}.\n",
            num(mean, 4),
            num(p99, 4)
        );
    }
    for s in &stopped {
        let tau = s["flip_mean_tile_threshold"].as_f64().unwrap();
        md += &format!(
            "\nStopped counts, {t}x{t} tiles ({n} tiles), smallest checkpoint with tile FLIP mean <= {tau}:\n\n",
            t = options.tile,
            n = tile_count
        );
        md += "| stops at spp |";
        for c in &checkpoints {
            md += &format!(" {c} |");
        }
        md += " never |\n|---|";
        for _ in &checkpoints {
            md += "---:|";
        }
        md += "---:|\n| tiles |";
        let counts = s["tiles_stopping_at_checkpoint"].as_array().unwrap();
        for c in counts {
            md += &format!(" {c} |");
        }
        md += &format!(" {} |\n", s["tiles_never_stopping_by_last_checkpoint"]);
        md += &format!(
            "\nMean stop count of tiles that stop: {}; whole-image gain N_all/mean(n): {}{}; tiles whose own floor is above the threshold: {}.\n",
            spp_text(s["mean_stop_spp_of_tiles_that_stop"].as_f64()),
            num(s["gain_whole_image"].as_f64(), 2),
            if s["gain_is_upper_bound_because_some_tiles_never_stop"] == json!(true) {
                " (upper bound: some tiles never stop)"
            } else {
                ""
            },
            s["tiles_whose_own_floor_is_above_threshold"],
        );
    }
    md += &timing_markdown(dir);

    let threshold_cell = |i: usize| {
        thresholds
            .get(i)
            .map_or(("n/a".to_string(), "n/a".to_string()), |t| {
                let tag = if t["how"].as_str().is_some_and(|h| h.starts_with("EXTRAP")) {
                    "~"
                } else {
                    ""
                };
                (
                    format!("{tag}{}", spp_text(t["spp_to_threshold"].as_f64())),
                    format!(
                        "{tag}{}",
                        num(t["gpu_seconds_to_threshold_mean"].as_f64(), 2)
                    ),
                )
            })
    };
    let (spp_a, gpu_a) = threshold_cell(0);
    let (spp_b, gpu_b) = threshold_cell(1);
    let summary_row = format!(
        "| {name} | {} | {} | {} | {} | {} | {} | {} | {} | {} / {} |\n",
        num(Some(f64::from(floor_stats.mean)), 4),
        sig(Some(floor_mse_lin)),
        spp_a,
        gpu_a,
        spp_b,
        gpu_b,
        flip_at_64.map_or("n/a".into(), |(m, _)| num(m, 4)),
        num(mean_ms, 2),
        num(p95_ms, 2),
        num(max_ms, 2),
    );

    Ok(ViewReport {
        json: json!({
            "view": name,
            "warnings": warnings,
            "config": summary["config"],
            "commit": summary["commit"],
            "machine": summary["machine"],
            "per_sample_gpu_ms": summary["per_sample_gpu_ms"],
            "floor_truth_a_vs_truth_b": {
                "flip_mean": floor_stats.mean,
                "flip_p99_weighted": floor_p99,
                "mse_linear": floor_mse_lin,
                "mse_8bit": floor_mse_8,
            },
            "checkpoints": rows,
            "thresholds": thresholds,
            "flip_64spp_raw_vs_truth": flip_at_64.map(|(m, p)| json!({"flip_mean": m, "flip_p99_weighted": p})),
            "stopped_counts": stopped,
        }),
        markdown: md,
        summary_row,
    })
}

/// Tables from the `batch` and `split` runs of this view, if they were made.
fn timing_markdown(dir: &Path) -> String {
    let mut md = String::new();
    if let Ok(batch) = read_json(&dir.join("batch/summary.json"))
        && let Some(rows) = batch["batch"].as_array()
    {
        md += "\nBatching: k samples a pixel in one dispatch (in-lane loop), GPU ms:\n\n";
        md += "| traced | k | dispatch mean | dispatch p95 | dispatch max | per sample mean | per sample p95 | cost vs k x (k=1) |\n|---|---:|---:|---:|---:|---:|---:|---:|\n";
        for r in rows {
            md += &format!(
                "| {}x{} | {} | {} | {} | {} | {} | {} | {} |\n",
                r["internal"][0],
                r["internal"][1],
                r["k"],
                num(r["per_dispatch"]["mean_ms"].as_f64(), 3),
                num(r["per_dispatch"]["p95_ms"].as_f64(), 3),
                num(r["per_dispatch"]["max_ms"].as_f64(), 3),
                num(r["per_sample"]["mean_ms"].as_f64(), 3),
                num(r["per_sample"]["p95_ms"].as_f64(), 3),
                num(r["dispatch_mean_over_k_times_k1"].as_f64(), 3),
            );
        }
    }
    if let Ok(split) = read_json(&dir.join("split/summary.json"))
        && let Some(rows) = split["variants"].as_array()
    {
        md += "\nOne sample by part (variants of the reference kernel), GPU ms:\n\n";
        md += "| variant | mean | p95 | max |\n|---|---:|---:|---:|\n";
        for r in rows {
            md += &format!(
                "| {} | {} | {} | {} |\n",
                r["name"].as_str().unwrap_or("?"),
                num(r["stats"]["mean_ms"].as_f64(), 3),
                num(r["stats"]["p95_ms"].as_f64(), 3),
                num(r["stats"]["max_ms"].as_f64(), 3),
            );
        }
        if let Some(parts) = split["parts_by_difference_of_means"].as_object() {
            md += "\nBy difference of means (ms):\n\n";
            for (k, v) in parts {
                md += &format!("- {k}: {}\n", num(v.as_f64(), 3));
            }
        }
    }
    md
}

const USAGE: &str = "Usage: measure-ref-analyze CAP_DIR [--out DIR] [--ppd 67.02] \
[--thresholds 0.02,0.05] [--tile 16]";

fn run() -> Res<()> {
    let mut args = std::env::args().skip(1);
    let mut cap: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut options = Options {
        ppd: DEFAULT_PPD,
        thresholds: vec![0.02, 0.05],
        tile: 16,
    };
    while let Some(arg) = args.next() {
        let mut value = |o: &str| args.next().ok_or(format!("{o} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--out" => out = Some(value("--out")?.into()),
            "--ppd" => options.ppd = value("--ppd")?.parse().map_err(|_| "bad --ppd")?,
            "--tile" => options.tile = value("--tile")?.parse().map_err(|_| "bad --tile")?,
            "--thresholds" => {
                options.thresholds = value("--thresholds")?
                    .split(',')
                    .map(|t| t.parse().map_err(|_| "bad --thresholds".to_string()))
                    .collect::<Res<_>>()?
            }
            _ if cap.is_none() && !arg.starts_with("--") => cap = Some(arg.into()),
            _ => return Err(format!("unexpected argument `{arg}`\n{USAGE}")),
        }
    }
    let cap = cap.ok_or(USAGE)?;
    let out = out.unwrap_or_else(|| cap.clone());

    let is_view = |p: &Path| p.join("converge/summary.json").exists() && p.join("truthA").exists();
    let mut views: Vec<(String, PathBuf)> = if is_view(&cap) {
        vec![(
            cap.file_name()
                .map_or("view".into(), |n| n.to_string_lossy().into_owned()),
            cap.clone(),
        )]
    } else {
        let mut v: Vec<_> = fs::read_dir(&cap)
            .map_err(|e| format!("cannot read {}: {e}", cap.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_view(p))
            .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p))
            .collect();
        v.sort();
        v
    };
    if views.is_empty() {
        return Err(format!(
            "no views in {} (each needs converge/, truthA/ and truthB/)",
            cap.display()
        ));
    }
    views.sort();

    let mut reports = Vec::new();
    for (name, dir) in &views {
        reports.push(analyse_view(name, dir, &options)?);
    }

    let first = &reports[0].json;
    let mut md = String::new();
    md += "## Reference measurement (#99)\n\n";
    md += &format!(
        "Commit `{}`{}; GPU {} ({}), driver {} {}; FLIP: LDR-FLIP via flip-rs, ppd {}, on the 8-bit PNGs, against truth A.\n\n",
        first["commit"].as_str().unwrap_or("?"),
        if first["machine"]["timers_supported"] == json!(false) {
            " (no GPU timers)"
        } else {
            ""
        },
        first["machine"]["gpu"].as_str().unwrap_or("?"),
        first["machine"]["device_type"].as_str().unwrap_or("?"),
        first["machine"]["driver"].as_str().unwrap_or("?"),
        first["machine"]["driver_info"].as_str().unwrap_or(""),
        options.ppd,
    );
    md += "| view | floor FLIP(A,B) | floor MSE(A,B) linear | spp to FLIP<=0.02 | GPU s | spp to FLIP<=0.05 | GPU s | FLIP(64 spp) | per-sample ms mean | p95 / max |\n";
    md += "|---|---:|---:|---:|---:|---:|---:|---:|---:|---|\n";
    for r in &reports {
        md += &r.summary_row;
    }
    md += "\nA leading `~` marks a number extrapolated past the last checkpoint. GPU seconds are spp x the mean per-sample GPU time measured in that view.\n\n";
    for r in &reports {
        md += &r.markdown;
        md += "\n";
    }
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let json = json!({
        "ppd": options.ppd, "thresholds": options.thresholds, "tile": options.tile,
        "views": reports.iter().map(|r| r.json.clone()).collect::<Vec<_>>(),
    });
    fs::write(
        out.join("analysis.json"),
        serde_json::to_string_pretty(&json).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    fs::write(out.join("analysis.md"), &md).map_err(|e| e.to_string())?;
    println!("{md}");
    eprintln!(
        "wrote {} and analysis.md",
        out.join("analysis.json").display()
    );
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("measure-ref-analyze: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
