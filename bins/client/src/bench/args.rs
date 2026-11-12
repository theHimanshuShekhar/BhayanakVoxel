//! The command line of `client bench`.

use std::path::PathBuf;

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
                   with any of them is not a measurement. no-follow: the Planet does not follow
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
    pub no_fail: bool,
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
    Run(Args),
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
        no_fail: false,
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
            "--no-fail" => parsed.no_fail = true,
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
    Ok(Parsed::Run(parsed))
}

/// `--runs`: at least 1.
fn runs(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("`{text}` is not a number of runs like 3"))
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

    fn parse_all(args: &[&str]) -> Result<Parsed, String> {
        parse(args.iter().map(|a| a.to_string()))
    }

    fn run(args: &[&str]) -> Args {
        match parse_all(args) {
            Ok(Parsed::Run(args)) => args,
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
                no_fail: false,
                size: None,
                bias: 1.0,
                diag: Diag::default()
            }
        );
    }

    #[test]
    fn real_time_needs_streaming_and_runs_need_real_time() {
        let args = run(&["--streaming", "--real-time"]);
        assert!(args.real_time && args.streaming);
        assert_eq!(args.runs, DEFAULT_RUNS);
        assert_eq!(run(&["--streaming", "--real-time", "--runs", "1"]).runs, 1);
        for bad in [
            &["--real-time"][..],
            &["--streaming", "--runs", "3"],
            &["--streaming", "--real-time", "--runs"],
            &["--streaming", "--real-time", "--runs", "0"],
            &["--streaming", "--real-time", "--runs", "-1"],
            &["--streaming", "--real-time", "--runs", "many"],
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
}
