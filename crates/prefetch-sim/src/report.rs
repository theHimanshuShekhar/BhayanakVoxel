//! The paths of the ticket, run at the ADR 0004 margins and the measured rates, and the report
//! of what each needs: holes, soft misses, peak memory, and the generation and intake rates
//! that give none.

use std::fmt::Write;

use crate::cache::{Params, Summary, run};
use crate::demand::{Point, build, traverse};
use crate::model::{EYE, Model, VOXELS_PER_METRE, add, scale, unit};
use crate::path::{self, Path};

/// Bytes the Guard takes, measured at 10 cm Voxels: 680 K bricks of levels 8 to 12, 73.9 MiB. The
/// Guard of 5 cm Voxels is levels 9 to 13: the same Voxel sizes, and so about the same bricks.
/// (Not measured again yet.)
pub const GUARD_BYTES: f64 = 73.9 * 1024.0 * 1024.0;

/// Bytes a drawn brick costs in one packed copy of the scene, as the client measures it: 792 MiB
/// for the 4.65 million bricks of the full fill at 5 cm on the 320 km Planet with no horizon cap
/// (#146), 178.5 B (it was 474 MiB for 3.44 million, 144.5 B, on the 16 km Planet: #142), which is
/// the 34 words of
/// `bricks` and `brick_refs` per brick that `room_for` in the client is sized from (136 B, as
/// #123 measured it) and the region records and tables that go with them. The memory the client
/// holds is more than this times the bricks: the scene thread's copy and the frame thread's copy
/// of it (ADR 0006: about 542 MiB each at full size), and the host `BrickStore`, which holds the
/// ancestors and the bricks the bands split but do not draw. Every MB in the report is one packed
/// copy; a copy for the frame thread doubles it.
pub const BRICK_BYTES: f64 = 792.0 * 1024.0 * 1024.0 / 4_652_474.0;

/// Drawn bricks a second that the loader and the scene thread get from the Planet to the scene
/// today, end to end: the full fill at 5 cm drew 3.44 million bricks
/// (`BRICKS_PER_D0_SQUARED`) in 97.8 s on the 8-core dev box (#142: the loader generating only
/// the children that can be drawn; 160 s for 3.56 million before), with the scene thread of ADR
/// 0006 taking them in as they came. An upper bound on today's intake, since the fill may be
/// limited by generation.
pub const CURRENT_INTAKE: f64 = 3_439_848.0 / 97.8;

/// The loader's `MAX_AHEAD`: most jobs generated and not taken in.
pub const LOADER_MAX_AHEAD: usize = 512;

/// What a generated brick costs on one core, in µs, from the real loader: the full fill (at 5 cm,
/// #142) generated 9.41 million bricks in 97.8 s on 6 workers of the 8-core dev box, so 16 K
/// bricks/s/worker, 62 µs. (It was 40 µs when the fill generated 23.9 million bricks in 160 s: #142
/// stopped generating the air and the buried bricks, which cost 7 µs each, so a brick that is
/// generated now costs more on average, and a drawn brick costs less in all.) If the workers were
/// ever idle the true cost is lower, so this is an upper bound on the cost of the loader's job and
/// the figure used. (Not measured by this crate: from the client's fill.)
pub const GENERATION_US: f64 = 6.0 * 97.8e6 / 9_410_000.0;

/// What `examples/gen_rate.rs` measures on one core, for the same job (`children_with_solid`, 8
/// children with `solid_below`, air and buried ones included): 1500 jobs sampled at random from
/// each level 0 to 7 of the fill's bricks on the real Planet, weighted by drawn bricks: 67 to
/// 75 µs over runs (level 0 is 49 to 55, levels 1 to 4 are 74 to 99). Cold, scattered over the
/// whole Planet, so slower than the fill's average; the cross-check, and the pessimistic rate.
pub const GENERATION_US_SAMPLE: f64 = 75.0;

/// The same job over every job within 150 m of the spawn, in order (what a camera there asks
/// for, with warm caches): 15.8 µs. Levels 0 to 2 only, so the optimistic rate.
pub const GENERATION_US_NEAR: f64 = 15.8;

/// Jobs the loader makes: all eight children of a split brick.
pub const JOB_SIZE: usize = 8;

/// The rates of the loader on the dev's 8-core box, with the intake unbounded and the generation
/// as measured ([`GENERATION_US`]), the loader's `cores - 2` workers, and a budget of the shell
/// at its largest over the path (no prefetch) with the ADR's 20% overflow, so that eviction runs
/// on prefetch, what a path leaves behind and the siblings that jobs bring. The intake and `max_ahead` are what
/// a column says (the nominal ones are unbounded: what #49 builds).
pub fn measured() -> Params {
    Params {
        workers: 6,
        gen_per_core: 1.0e6 / GENERATION_US,
        job_size: JOB_SIZE,
        intake: f64::INFINITY,
        max_ahead: None,
        brick_bytes: BRICK_BYTES,
        budget_bytes: None,
        overflow: 0.2,
        prefill: true,
    }
}

/// Bricks that a full fill draws for a camera at the ground: the band rule over the whole Planet,
/// at every level, with no horizon. What the client measured is [`DRAWN_PER_D0_SQUARED`] d0².
pub fn drawn_bricks(model: &Model) -> usize {
    built_size(model, EYE / VOXELS_PER_METRE)
}

/// Bricks that the client as built draws at rest at `altitude_m`: the band rule over the whole
/// Planet, every level, the Guard's too, with no horizon cap and no margin (the client has none of
/// the first yet, and the margins are residency, not drawing). This is what its scene holds, and
/// what [`BRICK_BYTES`] is measured over; [`shell_size`] is the shell that a horizon cap would
/// keep, which at the ground is about half of it, since the client also draws everything beyond
/// the horizon, from the far side of the Planet inward, at the coarse levels.
pub fn built_size(model: &Model, altitude_m: f64) -> usize {
    let up = unit([0.36, 0.48, 0.8]);
    let point = Point {
        pos: scale(up, model.planet_radius + altitude_m * VOXELS_PER_METRE),
        reach: f64::INFINITY,
    };
    let mut drawn = 0;
    traverse(model, &[point], |_, _, _| drawn += 1);
    drawn
}

/// A Planet of 50 R (the farthest of the Zoom range) in altitude, in Voxels.
fn orbit(model: &Model, radii: f64) -> f64 {
    model.planet_radius * (radii - 1.0)
}

/// The paths of the ticket.
pub fn paths(model: &Model) -> Vec<Path> {
    vec![
        path::flight(model, 2.0, 50.0, 5.0, 0.2),
        path::flight(model, 50.0, 50.0, 4.0, 0.2),
        path::turn(model, 180.0, 2.0, 1.0 / 30.0),
        path::turn(model, 360.0, 2.0, 1.0 / 30.0),
        path::takeoff(model, 50.0, 3.0, 0.1),
        path::zoom(model, EYE, orbit(model, 50.0), 10.0, 0.5),
        path::zoom(model, orbit(model, 50.0), EYE, 5.0, 0.5),
        path::zoom(model, orbit(model, 20.0), EYE, 1.0, 0.2),
        path::teleport(model, 8000.0, 1.0, 3.0, 0.1),
    ]
}

/// How much of the rate or margin a path needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Needed {
    /// The least rate, in bricks a second, that gives none.
    Rate(f64),
    /// No rate does: the shell does not hold what the path asks for.
    NoRate,
}

impl Needed {
    fn show(self) -> String {
        match self {
            Needed::Rate(r) if r < 1.0e3 => "< 1 K".into(),
            Needed::Rate(r) if r < 1.0e6 => format!("{:.0} K", r / 1.0e3),
            Needed::Rate(r) => format!("{:.2} M", r / 1.0e6),
            Needed::NoRate => "none (margin)".into(),
        }
    }

    /// The rate and the cores that make it at `per_core` bricks a second each.
    fn show_cores(self, per_core: f64) -> String {
        match self {
            Needed::Rate(r) if r >= 1.0e3 => format!("{} ({:.1} cores)", self.show(), r / per_core),
            _ => self.show(),
        }
    }
}

/// The least rate for which `clean` holds, to within 3%, or none if it never does. `clean` is
/// taken to hold for every rate above the answer.
pub fn least_rate(mut clean: impl FnMut(f64) -> bool) -> Needed {
    const HIGH: f64 = 1.0e10;
    const LOW: f64 = 1.0e2;
    if !clean(HIGH) {
        return Needed::NoRate;
    }
    if clean(LOW) {
        return Needed::Rate(LOW);
    }
    let (mut lo, mut hi) = (LOW, HIGH);
    while hi / lo > 1.03 {
        let mid = (lo * hi).sqrt();
        if clean(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Needed::Rate(hi)
}

/// Everything about one path.
#[derive(Clone, Debug)]
pub struct PathResult {
    pub name: String,
    pub seconds: f64,
    pub frames: usize,
    /// At the measured rates, with the intake unbounded (what #49 builds).
    pub nominal: Summary,
    /// At the measured generation rate and today's intake and `max_ahead`.
    pub current: Summary,
    /// With generation and intake unbounded: what a cache that always keeps up holds, and
    /// evicts.
    pub unbounded: Summary,
    /// The same, with no Guard: what the shell alone leaves.
    pub no_guard: Summary,
    /// The least generation rate (intake unbounded) and the least intake rate (generation
    /// unbounded) that give no holes and no avoidable soft misses.
    pub generation: Needed,
    pub intake: Needed,
    /// The least generation rate for no holes at all with no Guard.
    pub generation_no_guard: Needed,
    pub allowed_soft_seconds: f64,
}

fn with_generation(params: &Params, rate: f64) -> Params {
    Params {
        gen_per_core: rate / params.workers as f64,
        ..params.clone()
    }
}

/// Runs one path at `params`, and works out the rates it needs.
pub fn analyse(model: &Model, path: &Path, params: &Params) -> PathResult {
    let demand = build(model, path);
    let allowed = path.allowed_soft_seconds;
    let nominal = run(model, &demand, params);
    let current = run(
        model,
        &demand,
        &Params {
            intake: CURRENT_INTAKE,
            max_ahead: Some(LOADER_MAX_AHEAD),
            ..params.clone()
        },
    );
    let unbounded = run(model, &demand, &with_generation(params, 1.0e10));
    let generation =
        least_rate(|r| run(model, &demand, &with_generation(params, r)).is_clean(allowed));
    let intake = least_rate(|r| {
        let p = Params {
            intake: r,
            ..with_generation(params, 1.0e10)
        };
        run(model, &demand, &p).is_clean(allowed)
    });
    let bare = Model {
        guard_first: None,
        ..model.clone()
    };
    let bare_demand = build(&bare, path);
    let no_guard = run(&bare, &bare_demand, params);
    // Holes only: with no Guard a soft miss is what the shell is for, and a hole is a failure.
    let generation_no_guard =
        least_rate(|r| run(&bare, &bare_demand, &with_generation(params, r)).hole_frames == 0);
    PathResult {
        name: path.name.clone(),
        seconds: path.samples.len() as f64 * path.dt,
        frames: path.samples.len(),
        nominal,
        unbounded,
        current,
        no_guard,
        generation,
        intake,
        generation_no_guard,
        allowed_soft_seconds: allowed,
    }
}

/// Runs every path, each on its own thread.
pub fn run_suite(model: &Model, params: &Params) -> Vec<PathResult> {
    let paths = paths(model);
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .iter()
            .map(|path| scope.spawn(|| analyse(model, path, params)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a path panicked"))
            .collect()
    })
}

/// Runs every path once at `params`, each on its own thread: the demand of the path, replayed.
pub fn run_nominal(model: &Model, params: &Params) -> Vec<(Path, Summary)> {
    let paths = paths(model);
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .iter()
            .map(|path| scope.spawn(|| run(model, &build(model, path), params)))
            .collect();
        handles
            .into_iter()
            .zip(paths.iter())
            .map(|(h, path)| (path.clone(), h.join().expect("a path panicked")))
            .collect()
    })
}

/// The least radial margin (from 1.0 in steps of 0.25) at which `path` has no holes and no
/// avoidable soft misses at `params`, or none up to 3.
pub fn margin_needed(model: &Model, path: &Path, params: &Params) -> Option<f64> {
    (0..=8).map(|i| 1.0 + 0.25 * f64::from(i)).find(|&margin| {
        let m = Model {
            radial_margin: margin,
            ..model.clone()
        };
        run(&m, &build(&m, path), params).is_clean(path.allowed_soft_seconds)
    })
}

/// The shell at rest at `altitude_m`: its bricks of the shell levels, in all, per level.
pub struct ShellSize {
    pub altitude_m: f64,
    pub reach_m: f64,
    pub bricks: usize,
    pub levels: (usize, usize),
    /// What the client as built draws at rest there, with no horizon cap (see [`built_size`]).
    pub built: usize,
}

/// The bricks of the shell (the camera and its vertical margin) at rest at `altitude_m`, not
/// counting the Guard's levels.
pub fn shell_size(model: &Model, altitude_m: f64) -> ShellSize {
    let up = unit([0.36, 0.48, 0.8]);
    let pos = scale(up, model.planet_radius + altitude_m * VOXELS_PER_METRE);
    let points: Vec<Point> = [0.0, model.vertical_margin]
        .iter()
        .map(|&lift| {
            let pos = add(pos, scale(up, lift));
            Point {
                pos,
                reach: model.shell_reach(model.altitude(pos)),
            }
        })
        .collect();
    let mut keys = crate::key::KeySet::default();
    traverse(model, &points, |level, coord, _| {
        if !model.in_guard(level) {
            keys.insert(crate::key::Key::new(level, coord));
        }
    });
    let levels = keys
        .iter()
        .map(|k| k.level())
        .fold((usize::MAX, 0), |(lo, hi), l| (lo.min(l), hi.max(l)));
    ShellSize {
        altitude_m,
        reach_m: points[0].reach / VOXELS_PER_METRE,
        bricks: keys.len(),
        levels,
        built: built_size(model, altitude_m),
    }
}

fn mib(bytes: f64) -> f64 {
    bytes / (1024.0 * 1024.0)
}

/// The report as Markdown.
pub fn markdown(
    model: &Model,
    params: &Params,
    results: &[PathResult],
    sizes: &[ShellSize],
) -> String {
    let mut out = String::new();
    let w = &mut out;
    let _ = writeln!(
        w,
        "| Path | Soft-miss frames / s (Guard on, unbounded intake) | Holes with no Guard (frames) | Holes with the Guard | Today's loader: soft-miss frames / s | Peak MB: shell + prefetch wanted / resident at the measured rate / resident with unbounded generation | Evictions (unbounded) | Soft misses in the onset frame (inevitable) | Hit rate |"
    );
    let _ = writeln!(w, "|---|---|---|---|---|---|---|---|---|");
    for r in results {
        let n = &r.nominal;
        let _ = writeln!(
            w,
            "| {} | {} / {:.2} | {} | {} | {} / {:.2} | {:.0} / {:.0} / {:.0} | {} | {} | {:.1}% |",
            r.name,
            n.soft_miss_frames,
            n.soft_miss_seconds,
            r.no_guard.hole_frames,
            n.hole_frames,
            r.current.soft_miss_frames,
            r.current.soft_miss_seconds,
            mib(n.peak_target_bricks as f64 * params.brick_bytes),
            mib(n.peak_resident_bytes),
            mib(r.unbounded.peak_resident_bytes),
            r.unbounded.evictions,
            n.onset_soft_misses,
            n.hit_rate() * 100.0,
        );
    }
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "| Path | Peak demand, generated bricks/s (0.16 s burst) | Generated per drawn brick | Peak queue (bricks) | Generation for no soft misses, generated bricks/s | Cores at {:.0} µs | Cores at {:.0} µs | Cores at {:.0} µs | Drawn-brick intake for the same, bricks/s | Generation for zero holes with no Guard |",
        GENERATION_US, GENERATION_US_SAMPLE, GENERATION_US_NEAR
    );
    let _ = writeln!(w, "|---|---|---|---|---|---|---|---|---|---|");
    for r in results {
        let n = &r.nominal;
        let cores = |us: f64| match r.generation {
            Needed::Rate(g) if g >= 1.0e3 => format!("{:.1}", g * us / 1.0e6),
            Needed::Rate(_) => "< 0.1".into(),
            Needed::NoRate => "none".into(),
        };
        let _ = writeln!(
            w,
            "| {} | {:.0} K ({:.0} K) | {:.1} | {} | {} | {} | {} | {} | {} | {} |",
            r.name,
            n.peak_demand_per_s / 1.0e3,
            n.peak_demand_burst / 1.0e3,
            n.generated_per_drawn(),
            n.peak_queue,
            r.generation.show(),
            cores(GENERATION_US),
            cores(GENERATION_US_SAMPLE),
            cores(GENERATION_US_NEAR),
            r.intake.show(),
            r.generation_no_guard.show(),
        );
    }
    let _ = writeln!(w);
    let _ = writeln!(
        w,
        "| Altitude | Shell reach | Levels | Bricks | MB at {:.1} B | As built (no horizon cap): bricks | MB |",
        params.brick_bytes
    );
    let _ = writeln!(w, "|---|---|---|---|---|---|---|");
    for s in sizes {
        let _ = writeln!(
            w,
            "| {} m | {:.0} m | {}-{} | {} | {:.1} | {} | {:.1} |",
            s.altitude_m,
            s.reach_m,
            s.levels.0,
            s.levels.1,
            s.bricks,
            mib(s.bricks as f64 * params.brick_bytes),
            s.built,
            mib(s.built as f64 * params.brick_bytes),
        );
    }
    let _ = writeln!(
        w,
        "\nRadial margin {}, vertical margin {} m, crust {}, Guard from level {:?} ({:.1} MiB, not in the shell columns), {} workers at {:.0} generated bricks/s/core ({:.0} µs, as the fill implies), jobs of {}, today's intake {:.1} K drawn bricks/s. The client generated {:.2} bricks per drawn brick in its fill (9.41 million for 3.44 million, on the 16 km Planet), the model {:.2} on the paths: scale the generation columns by that ratio for the client's.",
        model.radial_margin,
        model.vertical_margin / VOXELS_PER_METRE,
        model.crust,
        model.guard_first,
        mib(GUARD_BYTES),
        params.workers,
        params.gen_per_core,
        GENERATION_US,
        params.job_size,
        CURRENT_INTAKE / 1.0e3,
        9_410_000.0 / 3_439_848.0,
        results
            .iter()
            .map(|r| r.nominal.generated_per_drawn())
            .filter(|g| *g > 0.0)
            .sum::<f64>()
            / results
                .iter()
                .filter(|r| r.nominal.generated_per_drawn() > 0.0)
                .count()
                .max(1) as f64,
    );
    out
}

/// The altitudes of the shell table, in metres.
pub const ALTITUDES: [f64; 9] = [
    2.0, 10.0, 50.0, 100.0, 200.0, 500.0, 2000.0, 5000.0, 20000.0,
];

/// One margin or prefetch setting of the model, for the ablation table.
pub struct Variant {
    pub label: &'static str,
    pub model: Model,
}

/// The ADR's settings and what taking each away does.
pub fn variants(model: &Model) -> Vec<Variant> {
    let with = |label, change: &dyn Fn(&mut Model)| {
        let mut model = model.clone();
        change(&mut model);
        Variant { label, model }
    };
    vec![
        with("ADR 0004: 1.25x, 20 m, prefetch", &|_| {}),
        with("no prefetch", &|m| m.lead = None),
        with("no vertical margin", &|m| m.vertical_margin = 0.0),
        with("no prefetch, no vertical margin", &|m| {
            m.lead = None;
            m.vertical_margin = 0.0;
        }),
        with("radial margin 1.0", &|m| m.radial_margin = 1.0),
    ]
}

/// What a path has under one variant.
pub struct VariantResult {
    pub label: &'static str,
    pub path: String,
    pub summary: Summary,
    pub generation: Needed,
    /// The most bricks in the shell, with prefetch, in MB.
    pub shell_mb: f64,
}

pub fn ablate(variant: &Variant, path: &Path, params: &Params) -> VariantResult {
    let demand = build(&variant.model, path);
    let summary = run(&variant.model, &demand, params);
    let generation = least_rate(|r| {
        run(&variant.model, &demand, &with_generation(params, r))
            .is_clean(path.allowed_soft_seconds)
    });
    VariantResult {
        label: variant.label,
        path: path.name.clone(),
        shell_mb: mib(summary.peak_target_bricks as f64 * params.brick_bytes),
        summary,
        generation,
    }
}

/// The ablation of the paths that margins and prefetch are for: flight along the ground and the
/// take-off. Each on its own thread.
pub fn run_ablation(model: &Model, params: &Params) -> Vec<VariantResult> {
    let paths = [
        path::flight(model, 2.0, 50.0, 8.0, 0.1),
        path::takeoff(model, 50.0, 3.0, 0.1),
    ];
    let variants = variants(model);
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .iter()
            .flat_map(|path| variants.iter().map(move |v| (v, path)))
            .map(|(variant, path)| scope.spawn(|| ablate(variant, path, params)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a variant panicked"))
            .collect()
    })
}

pub fn ablation_markdown(results: &[VariantResult], params: &Params) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| Path | Setting | Holes (frames) | Soft misses (frames / s) | Hit rate | Shell + prefetch MB | Peak demand /s (0.16 s burst) | Gen needed |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
    for r in results {
        let s = &r.summary;
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} / {:.2} | {:.2}% | {:.0} | {:.0} K ({:.0} K) | {} |",
            r.path,
            r.label,
            s.hole_frames,
            s.soft_miss_frames,
            s.soft_miss_seconds,
            s.hit_rate() * 100.0,
            r.shell_mb,
            s.peak_demand_per_s / 1.0e3,
            s.peak_demand_burst / 1.0e3,
            r.generation.show_cores(params.gen_per_core),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_rate_is_found_to_within_its_tolerance() {
        let Needed::Rate(r) = least_rate(|rate| rate >= 12_345.0) else {
            panic!("a rate is enough");
        };
        assert!((12_345.0..12_345.0 * 1.04).contains(&r), "{r}");
        assert_eq!(least_rate(|_| false), Needed::NoRate);
        assert_eq!(least_rate(|_| true), Needed::Rate(100.0));
    }

    /// A small Planet with no Guard, bands from 20 Voxels and a tight budget, so that holes and
    /// eviction both happen.
    fn small() -> (Model, Params) {
        let mut model = Model::real();
        model.planet_radius = 800.0;
        model.crust = 0.0;
        model.bands = render::lod::Bands::with_d0(20.0);
        model.levels = 6;
        model.guard_first = None;
        model.vertical_margin = 40.0;
        let params = Params {
            workers: 1,
            budget_bytes: None,
            overflow: 0.0,
            ..measured()
        };
        (model, params)
    }

    #[test]
    fn more_generation_never_gives_more_holes_or_soft_misses() {
        let (model, params) = small();
        for path in [
            path::flight(&model, 0.2, 40.0, 3.0, 0.1),
            path::takeoff(&model, 40.0, 3.0, 0.1),
        ] {
            let demand = build(&model, &path);
            // Bricks missing, summed over frames, as holes and as holes and soft misses: more
            // generation turns a hole into a soft miss (an ancestor arrives), so the soft misses
            // alone can rise, but what is missing in all never does.
            let mut last: Option<(u64, u64)> = None;
            for rate in [20.0, 100.0, 500.0, 2.5e3, 12.5e3, 60.0e3, 1.0e6] {
                let s = run(&model, &demand, &with_generation(&params, rate));
                let now = (s.hole_bricks, s.hole_bricks + s.soft_miss_bricks);
                if let Some(before) = last {
                    assert!(
                        now.0 <= before.0 && now.1 <= before.1,
                        "{}: {before:?} at the rate before, {now:?} at {rate}",
                        path.name
                    );
                }
                last = Some(now);
            }
        }
    }

    #[test]
    fn flight_with_unbounded_generation_misses_only_in_its_onset_frame() {
        let model = Model::real();
        let params = measured();
        let path = path::flight(&model, 2.0, 50.0, 3.0, 0.2);
        let demand = build(&model, &path);
        let s = run(&model, &demand, &with_generation(&params, 1.0e12));
        assert_eq!(s.soft_miss_frames, 0, "after the onset frame");
        assert!(s.onset_soft_misses > 0, "a standing camera cannot know");
        assert!(s.is_clean(0.0));
        // And a slower generator does miss after it.
        let slow = run(&model, &demand, &with_generation(&params, 1.0e4));
        assert!(slow.soft_miss_frames > 0);
    }

    #[test]
    fn a_camera_at_rest_needs_no_generation_and_has_no_soft_misses() {
        let model = Model::real();
        let at_rest = path::turn(&model, 0.0, 1.0, 0.25);
        let demand = build(&model, &at_rest);
        let params = Params {
            gen_per_core: 0.0,
            ..measured()
        };
        let s = run(&model, &demand, &params);
        assert_eq!((s.hole_frames, s.soft_miss_frames), (0, 0));
        assert_eq!(
            least_rate(|r| run(&model, &demand, &with_generation(&params, r)).is_clean(0.0)),
            Needed::Rate(100.0)
        );
    }

    #[test]
    fn a_full_fill_draws_what_the_client_measured() {
        // 4.40 d0^2 bricks (4.65 million at 320 km), within 10%.
        let model = Model::real();
        let d0 = model.bands.d0();
        let drawn = drawn_bricks(&model) as f64 / (d0 * d0);
        assert!(
            (crate::model::DRAWN_PER_D0_SQUARED * 0.9..crate::model::DRAWN_PER_D0_SQUARED * 1.1)
                .contains(&drawn),
            "{drawn} d0^2"
        );
    }

    #[test]
    fn the_scene_at_rest_is_the_client_s_792_mib_and_the_shell_is_the_part_inside_the_horizon() {
        // The client without a horizon cap draws the whole Planet, as the bands say: 4.65 million
        // bricks, 792 MiB per scene copy at BRICK_BYTES, within 10%.
        let model = Model::real();
        let built = built_size(&model, 2.0);
        let mib_built = mib(built as f64 * BRICK_BYTES);
        assert!(
            (713.0..871.0).contains(&mib_built),
            "{built} bricks, {mib_built} MiB"
        );
        // The simulator's shell at rest, 227 to 259 MB (#49), is what a horizon cap would keep:
        // about half of it. That was the undercount; the scene was never the shell.
        let shell = shell_size(&model, 2.0);
        assert_eq!(shell.built, built);
        let share = shell.bricks as f64 / built as f64;
        assert!((0.35..0.65).contains(&share), "{share}");
    }

    #[test]
    fn a_shell_at_the_ground_holds_about_the_surface_in_its_band() {
        // No margins: level 0 owns 2 d0 = 102.8 m, each level twice as far as the one before, up to
        // the 1131 m horizon at 2 m over a 320 km sea (level 4 owns the last of it). The surface
        // there is a disc of 1131 m: pi r^2 over the bricks' areas (0.16 m^2 at level 0, four times
        // as much a level), times what a cubic grid makes of a surface (between 1 and sqrt 3 times
        // the area, 1.5 on average).
        let mut model = Model::real();
        model.crust = 0.0;
        model.vertical_margin = 0.0;
        model.radial_margin = 1.0;
        let size = shell_size(&model, 2.0);
        let horizon = 1131.0_f64;
        let (mut flat, mut inner) = (0.0, 0.0);
        for level in 0..=4 {
            let outer = (102.8_f64 * f64::from(1 << level)).min(horizon);
            flat += std::f64::consts::PI * (outer * outer - inner * inner)
                / (0.16 * 4.0_f64.powi(level));
            inner = outer;
        }
        let ratio = size.bricks as f64 / flat;
        assert!(
            (1.0..1.75).contains(&ratio),
            "{} bricks, {ratio} of flat",
            size.bricks
        );
        assert_eq!(size.levels, (0, 4));
    }
}
