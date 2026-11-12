//! The bench smoke test: runs the built `client bench --short` on whatever Vulkan device there is
//! (lavapipe in CI) and checks the report's schema, the screenshots and the exit status. The
//! timings of a software driver mean nothing, so none of them are checked, except against a
//! budget that no time could meet.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// A budgets file in which every pass is over budget, if it is timed at all.
const IMPOSSIBLE: &str =
    "[passes]\nprimary = 1e-9\nreference = 1e-9\n[gpu_total]\nprimary = 1e-9\n";

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("bhayanak-bench-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn bench(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_client"))
        .arg("bench")
        .args(args)
        .output()
        .expect("the client runs")
}

fn read_report(out: &Path) -> Value {
    let text = std::fs::read_to_string(out.join("report.json")).expect("report.json was written");
    serde_json::from_str(&text).expect("the report is JSON")
}

fn string(v: &Value) -> &str {
    v.as_str().unwrap_or_else(|| panic!("{v} is not a string"))
}

fn number(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("{v} is not a number"))
}

/// A stat that is a number, or null when nothing was timed.
fn number_or_null(v: &Value) {
    assert!(v.is_null() || v.is_number(), "{v}");
}

fn check_stats(stats: &Value) {
    assert!(stats["samples"].is_u64(), "{stats}");
    for key in ["mean_ms", "p95_ms", "max_ms"] {
        number_or_null(&stats[key]);
    }
}

fn check_entry(entry: &Value) {
    assert!(!string(&entry["name"]).is_empty());
    check_stats(entry);
    number_or_null(&entry["budget_ms"]);
    assert!(
        ["within", "over", "no-budget", "no-data"].contains(&string(&entry["status"])),
        "{entry}"
    );
}

/// The hash of the counts of the streaming run that is committed, which the unit tests of
/// `bench::stream` check as well (on Windows too, where there is no GPU for this test).
const GOLDEN_COUNTS: &str = include_str!("../src/bench/streaming-counts.golden");

fn check_schema(report: &Value, out: &Path) {
    assert_eq!(report["schema_version"], 7);
    // The Edit stress is of the whole Planet, which the patch is not.
    assert!(report["edits"].is_null());
    // The streaming mode is asked for (see its own test); the other tests do not.
    assert!(report["streaming"].is_null() || report["streaming"].is_object());
    assert!(!string(&report["build"]["version"]).is_empty());
    assert!(report["build"]["commit"].is_null() || report["build"]["commit"].is_string());

    let machine = &report["machine"];
    for key in [
        "gpu",
        "device_type",
        "driver",
        "driver_info",
        "backend",
        "os",
        "arch",
    ] {
        string(&machine[key]);
    }
    assert!(!string(&machine["gpu"]).is_empty());
    assert_eq!(string(&machine["backend"]), "Vulkan");
    let timers = machine["timers_supported"].as_bool().expect("a bool");

    let config = &report["config"];
    assert!(config["width"].is_u64() && config["height"].is_u64());
    // The upscaled image is twice the traced one in each direction.
    assert_eq!(
        config["output_width"],
        config["width"].as_u64().unwrap() * 2
    );
    assert_eq!(
        config["output_height"],
        config["height"].as_u64().unwrap() * 2
    );
    assert_eq!(string(&config["scene"]), "patch");
    assert!(!string(&config["path"]).is_empty());
    assert!(config["keyframes"].as_u64().unwrap() >= 2);
    // The LOD bias is recorded (1 unless asked).
    assert_eq!(config["bias"], 1.0);

    // The sweep over heights and the cave stills are of the whole Planet; the patch has none.
    assert_eq!(report["altitudes"], serde_json::json!([]));
    assert_eq!(report["caves"], serde_json::json!([]));

    for mode in ["primary", "reference"] {
        let run = &report["modes"][mode];
        let frames = run["frames"].as_u64().expect("frames");
        let spp = run["spp"].as_u64().expect("spp");
        assert!(frames > 0 && spp > 0 && run["warmup_frames"].is_u64());
        if mode == "primary" {
            assert_eq!(spp, 1);
        }
        // Every sample is timed as a profiler frame of its own, and `Offscreen` has its times in
        // when a frame returns. So the series hold exactly the measured frames' samples: none of
        // the warm-up's, and none of the other mode's.
        let timed = frames * spp;
        check_stats(&run["frame_time"]);
        assert_eq!(run["frame_time"]["samples"], frames);
        // Following the camera with the Planet is timed on its own, a sample for each frame; the
        // patch is not drawn in levels, so it has no bricks to say.
        check_stats(&run["scene_update_ms"]);
        assert_eq!(run["scene_update_ms"]["samples"], frames);
        assert_eq!(run["lod_at_keyframes"], serde_json::json!([]));
        check_entry(&run["gpu_total"]);
        if timers {
            assert_eq!(run["gpu_total"]["samples"], timed, "{mode} gpu_total");
        }
        let passes = run["passes"].as_array().expect("passes");
        // Each mode runs the pass of its own name, and passes are listed even if untimed.
        assert!(
            passes.iter().any(|p| string(&p["name"]) == mode),
            "{mode}: {passes:?}"
        );
        // Primary mode also upscales.
        if mode == "primary" {
            assert!(
                passes.iter().any(|p| string(&p["name"]) == "upscale"),
                "{passes:?}"
            );
        }
        for pass in passes {
            check_entry(pass);
            if timers {
                assert!(number(&pass["mean_ms"]) >= 0.0);
                assert!(number(&pass["max_ms"]) >= number(&pass["p95_ms"]));
                assert_eq!(pass["samples"], timed, "{mode}: {pass}");
            } else {
                assert_eq!(pass["samples"], 0);
                assert!(pass["mean_ms"].is_null());
            }
        }
    }

    let budgets = &report["budgets"];
    string(&budgets["source"]);
    assert!(budgets["ok"].is_boolean());
    assert!(budgets["failed"].is_array() && budgets["unmatched"].is_array());

    let screenshots = report["screenshots"].as_array().expect("screenshots");
    assert!(screenshots.len() >= 2);
    for shot in screenshots {
        let file = out.join(string(shot));
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&file).unwrap()));
        let reader = decoder.read_info().expect("a PNG");
        // Primary mode presents the upscaled image; reference mode the traced one.
        let (width, height) = if string(shot).starts_with("primary") {
            (&config["output_width"], &config["output_height"])
        } else {
            (&config["width"], &config["height"])
        };
        assert_eq!(
            (reader.info().width, reader.info().height),
            (
                width.as_u64().unwrap() as u32,
                height.as_u64().unwrap() as u32
            ),
            "{shot}"
        );
    }
}

#[test]
fn the_short_bench_writes_a_report_and_screenshots_and_no_fail_ignores_the_budgets() {
    let out = scratch("report");
    std::fs::write(out.join("impossible.toml"), IMPOSSIBLE).unwrap();
    let budgets = out.join("impossible.toml");
    let run = bench(&[
        "--short",
        "--no-fail",
        "--out",
        out.join("run").to_str().unwrap(),
        "--budgets",
        budgets.to_str().unwrap(),
    ]);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let report = read_report(&out.join("run"));
    check_schema(&report, &out.join("run"));
    // The report is also on stdout.
    assert_eq!(
        serde_json::from_slice::<Value>(&run.stdout).unwrap(),
        report
    );
    // The budgets could not be met: it is in the report, though the run did not fail.
    assert_eq!(report["budgets"]["ok"], false);
    assert_eq!(
        report["modes"]["primary"]["passes"][0]["status"],
        if report["machine"]["timers_supported"] == true {
            "over"
        } else {
            "no-data"
        }
    );
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_pass_over_budget_fails_the_run_with_status_2() {
    let out = scratch("over");
    let budgets = out.join("impossible.toml");
    std::fs::write(&budgets, IMPOSSIBLE).unwrap();
    let run = bench(&[
        "--short",
        "--out",
        out.join("run").to_str().unwrap(),
        "--budgets",
        budgets.to_str().unwrap(),
    ]);
    assert_eq!(
        run.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    // The report is still written.
    assert_eq!(read_report(&out.join("run"))["budgets"]["ok"], false);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_bad_budgets_file_is_an_error_not_a_pass() {
    let out = scratch("bad");
    let budgets = out.join("bad.toml");
    std::fs::write(&budgets, "[pass]\nprimary = 1.0\n").unwrap();
    let run = bench(&[
        "--short",
        "--out",
        out.join("run").to_str().unwrap(),
        "--budgets",
        budgets.to_str().unwrap(),
    ]);
    assert_eq!(run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&run.stderr).contains("bad.toml"));
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_diagnostic_run_is_marked_in_its_report_and_is_not_held_to_the_budgets() {
    let out = scratch("diag");
    let budgets = out.join("impossible.toml");
    std::fs::write(&budgets, IMPOSSIBLE).unwrap();
    let run = bench(&[
        "--short",
        "--out",
        out.join("run").to_str().unwrap(),
        "--budgets",
        budgets.to_str().unwrap(),
        "--diag",
        "no-flush,gap-ms=1",
    ]);
    // The budgets cannot be met, and the exit status is 0 all the same.
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let report = read_report(&out.join("run"));
    assert_eq!(
        report["config"]["diag"],
        serde_json::json!(["no-flush", "gap-ms=1"])
    );
    assert_eq!(report["budgets"]["ok"], false);
    assert_eq!(report["budgets"]["failed"], serde_json::json!([]));
    let reason = report["budgets"]["skipped"].as_str().expect("a reason");
    assert!(reason.contains("not checked"), "{reason}");
    assert!(String::from_utf8_lossy(&run.stderr).contains("WARNING: a diagnostic run"));
    // A measurement has no switches and no reason.
    let plain = scratch("plain");
    bench(&["--short", "--out", plain.join("run").to_str().unwrap()]);
    let report = read_report(&plain.join("run"));
    assert_eq!(report["config"]["diag"], serde_json::json!([]));
    assert!(report["budgets"]["skipped"].is_null());
    let _ = std::fs::remove_dir_all(&out);
    let _ = std::fs::remove_dir_all(&plain);
}

/// The streaming section of a report made with `--streaming` (#181).
fn check_streaming(streaming: &Value) {
    // Everything outside `timing` is made by the stepped mode.
    assert_eq!(streaming["deterministic"], true);
    assert_eq!(string(&streaming["counts_hash"]), GOLDEN_COUNTS.trim());
    let series = |v: &Value| {
        for key in ["frames", "total", "max", "frames_with"] {
            assert!(v[key].is_u64(), "{v}");
        }
    };
    let schedule = &streaming["schedule"];
    for key in [
        "fill_frames",
        "steady_frames",
        "moving_frames",
        "edit_frame",
    ] {
        assert!(schedule[key].as_u64().unwrap() > 0, "{key}");
    }
    let upload = &streaming["upload"];
    series(&upload["bytes"]);
    assert!(upload["bytes"]["total"].as_u64().unwrap() > 0);
    assert!(upload["patch_bytes"].is_u64() && upload["rebuilds"].as_u64().unwrap() >= 1);
    for phase in ["fill", "steady", "moving"] {
        let misses = &streaming["soft_misses"][phase];
        series(&misses["bricks"]);
        assert_eq!(misses["by_level"].as_array().unwrap().len(), 14, "{phase}");
    }
    let rings = streaming["rings"].as_array().unwrap();
    assert_eq!(rings.len(), 14);
    for ring in rings {
        for key in ["level", "at_start", "when_complete", "at_end"] {
            assert!(ring[key].is_u64(), "{ring}");
        }
        number(&ring["band_start"]);
    }
    let work = &streaming["work"];
    for key in [
        "follow_steps",
        "follow_bricks",
        "pack_steps",
        "pack_bricks",
        "arrived_bricks",
        "upload_steps",
        "upload_bytes",
        "evict_steps",
        "evicted_units",
        "evicted_bricks",
    ] {
        assert!(work[key].is_u64(), "{key}");
    }
    assert_eq!(work["upload_bytes"], upload["bytes"]["total"]);
    let residency = &streaming["residency"];
    for key in [
        "diverged_bricks",
        "store_at_start",
        "store_when_complete",
        "store_max",
        "store_at_end",
        "budget",
        "edit_latency_frames",
    ] {
        assert!(residency[key].is_u64(), "{key}");
    }
    // An Edit made in a frame is taken in the next (ADR 0006); results are taken in the frame they
    // are published in until #145 caps the uploads.
    assert_eq!(residency["edit_latency_frames"], 1);
    series(&residency["outbox_latency_frames"]);
    assert_eq!(residency["outbox_latency_frames"]["total"], 0);
    assert!(residency["diverged_bricks"].as_u64().unwrap() > 0);
    // The gate: no frame has a hole.
    series(&streaming["holes"]);
    assert_eq!(streaming["holes"]["total"], 0);
    // The sizing constants beside what was measured, and none of them is over.
    let checks = streaming["sizing"]["checks"].as_array().unwrap();
    let constants: Vec<&str> = checks.iter().map(|c| string(&c["constant"])).collect();
    assert_eq!(
        constants,
        [
            "BRICKS_PER_D0_SQUARED",
            "REFS_PER_BRICK",
            "WORDS_PER_BRICK",
            "STREAMED_PER_D0_SQUARED"
        ]
    );
    for check in checks {
        assert!(string(&check["field"]).starts_with("streaming.sizing.measured."));
        assert!(number(&check["measured"]) > 0.0);
        assert!(number(&check["compared"]) <= number(&check["value"]));
        assert!(number(&check["limit"]) >= number(&check["compared"]));
        assert_eq!(check["over"], false, "{check}");
    }
    // The words are counted exactly, by level, and the values are worked out from those integers.
    let measured = &streaming["sizing"]["measured"];
    for key in [
        "ring_bricks",
        "guard_bricks",
        "ring_refs_words",
        "ring_bricks_words",
        "guard_refs_words",
        "guard_bricks_words",
        "streamed",
        "streamed_at_start",
    ] {
        assert!(measured[key].as_u64().unwrap() > 0, "{key}");
    }
    // What the GPU wrote is what the CPU worked out that it would: the count that the Linux and
    // Windows runners compare is made without a GPU, and this is what ties it to one.
    let timing = &streaming["timing"];
    assert_eq!(timing["deterministic"], false);
    assert_eq!(timing["gpu_uploaded_bytes"], upload["bytes"]["total"]);
    check_stats(&timing["upload_ms"]);
    check_stats(&timing["draw_ms"]);
    assert!(timing["drawn_frames"].as_u64().unwrap() > 0);
}

#[test]
fn the_streaming_mode_reports_the_counts_of_a_stepped_run_that_has_the_committed_hash() {
    let out = scratch("streaming");
    let run = bench(&[
        "--short",
        "--streaming",
        "--no-fail",
        "--out",
        out.join("run").to_str().unwrap(),
    ]);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let report = read_report(&out.join("run"));
    check_schema(&report, &out.join("run"));
    check_streaming(&report["streaming"]);
    let _ = std::fs::remove_dir_all(&out);
}
