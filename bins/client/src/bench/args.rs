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
  --size WxH       Resolution of the traced image [default: 1280x720]
  --short          A tiny run on a patch of the Planet at low resolution: for a smoke test,
                   not for timings
  --no-fail        Exit with status 0 even if a pass is over its budget
  -h, --help       Print this";

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub out: PathBuf,
    pub budgets: Option<PathBuf>,
    pub short: bool,
    pub no_fail: bool,
    pub size: Option<(u32, u32)>,
}

#[derive(Debug, PartialEq, Eq)]
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
        no_fail: false,
        size: None,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |option: &str| args.next().ok_or(format!("{option} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--short" => parsed.short = true,
            "--no-fail" => parsed.no_fail = true,
            "--out" => parsed.out = value("--out")?.into(),
            "--budgets" => parsed.budgets = Some(value("--budgets")?.into()),
            "--size" => parsed.size = Some(size(&value("--size")?)?),
            _ => return Err(format!("unexpected argument `{arg}`")),
        }
    }
    Ok(Parsed::Run(parsed))
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
                no_fail: false,
                size: None
            }
        );
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
        ]);
        assert_eq!(args.out, PathBuf::from("o"));
        assert_eq!(args.budgets, Some(PathBuf::from("b.toml")));
        assert!(args.short && args.no_fail);
        assert_eq!(args.size, Some((640, 360)));
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
            &["stray"],
        ] {
            assert!(parse_all(bad).is_err(), "{bad:?}");
        }
        assert!(parse_all(&["--nope"]).unwrap_err().contains("--nope"));
    }
}
