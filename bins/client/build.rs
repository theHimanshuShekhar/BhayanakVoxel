// THROWAWAY (#99): the commit the measurement tooling was built from, for summary.json.
use std::process::Command;

fn vcs(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let commit = vcs(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty =
        vcs(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    println!("cargo:rustc-env=MEASURE_COMMIT={commit}");
    println!("cargo:rustc-env=MEASURE_DIRTY={dirty}");
    // A file that does not exist is always out of date, so this runs on every build: a new
    // commit changes no file that the package depends on.
    println!("cargo:rerun-if-changed=always-rerun-this-build-script");
}
