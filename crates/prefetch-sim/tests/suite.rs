//! The paths of #87, every one of them, at the ADR 0004 margins and the measured rates. CI's
//! step timeout is what bounds the time; on 8 cores the suite takes a few seconds to half a
//! minute (`cargo run --release -p prefetch-sim` is the report that sizes #49).
//!
//! What it holds the model to: with the Guard resident there is no hole on any path (every brick
//! below the Guard's levels has a Guard brick above it, so this fails only if the Guard or the
//! fallback is broken), and a turn, which only moves the view, has no soft miss at all. The other
//! paths are generated while they happen and their soft misses are the report's. A soft miss
//! inside the resident set is a bug per CONTEXT.md, so they are exempt here only because the
//! measured rates do not give none (flight and the zooms need more generation or a faster
//! intake: see the report), not because they are acceptable.

use prefetch_sim::model::Model;
use prefetch_sim::report::{measured, run_nominal};

#[test]
fn every_path_runs_with_no_holes_and_a_turn_has_no_soft_misses() {
    let model = Model::real();
    let results = run_nominal(&model, &measured());
    assert_eq!(results.len(), 9);
    for (path, summary) in &results {
        println!(
            "{}: {} frames, holes {}, soft-miss frames {} ({:.2} s)",
            path.name,
            summary.frames,
            summary.hole_frames,
            summary.soft_miss_frames,
            summary.soft_miss_seconds
        );
        assert_eq!(summary.hole_frames, 0, "{}", path.name);
        if path.name.starts_with("Turn") {
            assert_eq!(summary.soft_miss_frames, 0, "{}", path.name);
        }
    }
}
