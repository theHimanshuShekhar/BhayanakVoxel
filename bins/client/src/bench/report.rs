//! The JSON report: what the bench measured, where, how, and how it fared against the budgets.
//! The shape of these types is the report's schema; bump [`SCHEMA_VERSION`] when it changes.

use serde::Serialize;

use super::budget::{Budgets, Status, status};
use super::convergence::ConvergenceReport;
use super::edits::EditsReport;
use super::realtime::RealTimeReport;
use super::rt::RtReport;
use super::stats::Stats;
use super::stream::StreamReport;

/// 8 added `streaming_real_time` (#162). Reports of the earlier versions are the same without it.
/// 9 added the slow-frame fields to its paths (`frames_over_limit`, `slowest`, `end_look_ms`) and
/// made a run's `edit_latency` null when there was no Edit (`edit_not_made` says why).
/// 10 added the GPU's own pass times of a slow frame and the frames over the limit by update kind.
/// 11 added the upload budget's figures (#145): a frame's copies, the 99th percentile of its bytes
/// and copies, the frames the budget cut and the longest backlog, in the stepped streaming mode's
/// `upload` and in each path of the real-time mode (`upload`).
/// 12 added what the review of #145 asked to be able to judge: in each real-time path's `upload`
/// the frames over the plain budget by cause (`over_budget`) and the growths (`growth_frames`,
/// `grown_bytes`, `lengthen_ms_max`, `grow_ms_max`); in each slow frame the growth's cost
/// (`lengthen_ms`, `grow_ms`, `grown_bytes`) and `over_budget`; in `memory` the peak during a
/// growth (`scene_gpu_peak_growth_mib`, `scene_host_peak_growth_mib`) and, apart, the GPU's peak
/// while a rebuilt scene was made (`scene_gpu_peak_rebuild_mib`); and in the stepped mode's
/// `timing` the copies the GPU side counted (`gpu_uploaded_writes`) and the growth copies
/// (`gpu_grown_bytes`). The stepped mode's counts, and so its hash, are not changed by these.
///
/// 13 added what the 4090 run of #145 could not answer: in each real-time path `largest_updates`
/// (the five frames whose update copied the most bytes, in the form of `slowest`) and
/// `over_limit_at` (the places of the first frames over the limit), to read the slow frames against
/// the big updates. The stepped mode's counts and hash are not changed by these.
///
/// 14 added in each real-time path how far the scene was behind the camera (`scene_lag_voxels_max`
/// and `scene_lag_d0_max`: the largest distance, at a publish, between the camera the scene thread
/// had been told and the scene's centre), and took `over_limit_at` out of `min` and `max`: a list
/// of the places of frames is of one run, and stays under `runs` only. The stepped mode's counts
/// and hash are not changed.
///
/// 15 measured that lag where it is seen: at each frame, the distance between the frame thread's
/// camera and the centre of the scene it draws (14's was between the camera the scene thread had
/// been told and the scene's centre at a publish, which a follow that ended at the camera it was
/// told brought to 0 however far the camera had gone since), with the 99th percentile
/// (`scene_lag_voxels_p99`, `scene_lag_d0_p99`) added to the most. The stepped mode's counts and
/// hash are not changed.
///
/// 16 added `convergence` (#187): the convergence phase's curve, floor and lines for two views,
/// null with `--no-convergence` or when the phase fails. Nothing else is changed.
///
/// 17 (#188) added to each convergence view `converged` (the mean brightness of the converged image:
/// per channel and as luminance, of both truths) and `flip_mean_gamma22` to the floor and to each
/// point of the curve (null unless `--flip-gamma-2-2`), and `flip_gamma22` to the block. The
/// images the phase scores are drawn with the true sRGB curve from this version on, where they
/// were drawn with a gamma of 2.2, so a FLIP number is not comparable with one of schema 16 or
/// before except through `flip_mean_gamma22`. Nothing else is changed.
///
/// 18 (#189) added to each convergence view `still`: the Reference view with the still-frame policy
/// on (batched samples, the 12 ms budget, 60 Hz), from the stop: the work of a frame that took a
/// batch (mean, p95, p99, max, how many were over 16.7 ms), the batch's size and GPU time, the
/// samples the run had at 0.1, 0.25, 0.5, 1 and 2 s and the FLIP of the image at each, and the time
/// from the stop to the FLIP lines. `convergence/VIEW/` also has `still-NNNNms.png`. Nothing
/// else is changed: the single-sample curve, the truths and every timed pass are as they were.
///
/// 19 added the frame's pacing to the real-time report (#167): each slow frame has `before_ms`,
/// `sleep_asked_ms`, `sleep_over_ms` and `cycle_ms`, and each path `frame_cycle` and
/// `sleep_overshoot`, so that a 30 ms frame says whether the frame thread woke late (the
/// harness's) or did the work. A slow frame also says how much of `before_ms` was the harness's own
/// look at a stop (`harness_ms`) and how much of `after_ms` was the store's lock
/// (`after_lock_ms`), and `between_ms` is the harness's work between the look at the memory and the
/// sleep, so that `time_ms`, `after_ms`, `between_ms`, `sleep_asked_ms` and `sleep_over_ms` add up
/// to `cycle_ms`. Each path has `slowest_cycles`, its frames with the longest turn.
/// Nothing else is changed.
///
/// 20 (#202) moved the lighting out of `primary`: `primary` excludes `normals` and `shade`; new
/// pass lines `normals`, `shade`, `exposure`, `exposure_adapt`, `bloom_down`, `bloom_h`, `bloom_v`
/// and `tone` are in `passes[]` (the report lists passes generically, so they need no field of
/// their own). So `passes[primary]`, the cave-still `primary` and the real-time `gpu_primary_ms`
/// no longer include the lighting: a number of schema 19 or before is not comparable with them
/// except as `primary` + `normals` + `shade`. Nothing else is changed.
///
/// 21 is not used: #168 (PR #219) takes 27, as #217, #52 and #213 landed first.
///
/// 22 (#217) counts the cuts of a Still view: each convergence view's `still` has `cuts` (`total`,
/// `view`, `scene`); each real-time run has `still_after_flight` (the view held Still after a
/// flight with the game's loading pause: `hold_s`, the cuts by cause, the catch-up and the holes
/// after it); and the real-time stop after a flight has `still_cuts` (null in the other paths).
/// Nothing else is changed.
///
/// 23 is not used: #168 (PR #219) takes 27, as #52 and #213 landed first.
///
/// 24 (#52) adds the sun-and-sky view: `convergence.shadows`, Primary's shadows against the
/// Reference's direct-sun image (the golden view's camera and sun, no GI and no emitters in both
/// views; `shadow_rays` is how many rays a pixel of Primary cast, 2 unless `--shadow-rays` said
/// another count): FLIP's floor in it, Primary's first frame and its settled image each with FLIP's mean
/// and p99 and the shadow-edge error (`edge`: the pixels on an edge, the mean and 99th percentile of
/// the luma difference over them, and the mean elsewhere), whether the target of FLIP 0.05 is met
/// and counts, and the files in `convergence/sun/`. A new pass line `shadows` is in `passes[]`
/// (the report lists passes generically), and `primary` is as it was in 22.
/// Nothing else is changed.
///
/// 25 is not used.
///
/// 26 (#213) added the real-time path `sea-level crossing` (the camera down to 30 m under the sea
/// and up to 80 m over it, 50 m/s), and in each real-time path `scene_round_max_ms`,
/// `scene_update_max_ms`, `cap_flips` and `cap_flip_max_ms`: the scene thread's longest round and
/// longest update in the path's frames, and the flips of the horizon cap with the longest update
/// that was a flip's or part of absorbing one; `backlog_peak`, `absorb_updates` and `absorb_ms`:
/// what absorbing a flip held and how long it took; and `scene_used_peak_mib`: the most MiB of the
/// scene in use at the end of one of the path's frames. Nothing else is changed.
///
/// 27 (#168) adds to the real-time
/// report's cold start `spawn_ring_s` (the 20 m spawn ring), `guard_s` (the Guard that the window
/// waits for as well) and `time_to_control_s` (from the world map being ready to the window
/// opening), to each path `camera_soft_miss_frames` (frames whose ground below the frame's camera
/// was drawn coarser than the bands ask), `generated_bricks` and `arrived_bricks` (what the loader
/// did in the path), `loader_queue_max` and `loader_queue_mean` (the bricks asked for and not yet
/// taken in, at each frame), `loader_generating_s`, `loader_held_back_s`, `stream_calls` and
/// `stream_slices_used` (what the loader's workers did with their time, and how often the scene
/// thread took in and used up its slice), `scene_lag_d0_mean`, `scene_follow_steps`, `scene_budget_cuts` and `camera_soft_miss_per_second` (to find where a path's misses and
/// memory come from), to `memory` `scene_used_peak_in` and `scene_used_peak_level_bricks`, to the `config` `stream_slice_ms`, to each `scene_thread` `reband_ms` (the change of the bands for the
/// camera's speed, which is not in `follow_ms`), to the stop's `settle` `walk_s`, `reband_s` and
/// `fill_s` (its three parts, which add up to `seconds`), to each path `scene_used_end_mib` and `scene_level_bricks_end` (the scene's MiB in use and the bricks drawn at each level when the path ended, against `scene_used_peak_mib`), `camera_soft_miss_lag_side`, `camera_soft_miss_loader_side`, `lag_over_floor_frames`, `scene_release_rebuilds`, `release_frame_ms_max` and `release_update_ms_max`, to each `scene_thread` `release_ms`, to the cold start `edit_during_fill` and `edit_during_fill_pending`, and to its gates `camera_soft_miss_frames`, `scene_soft_miss_frames` (reported),
/// `scene_lag_d0_max`, `spawn_ring_s`, `time_to_control_s` and `settle_s` (`over` may now also name
/// `soft_misses`, `scene_lag`, `spawn_ring`, `time_to_control` and `settle`). The budgets file has
/// the keys `camera_soft_miss_frames`, `scene_lag_d0_max`, `spawn_ring_s`, `time_to_control_s` and
/// `settle_s`, in `[streaming]`. Nothing else is changed.
///
/// 28 (#220) builds the Guard in two parts: the cells that a camera within 2 km of the Spawn could
/// see, before the window opens, and the rest in the background. The real-time report's cold start
/// gets
/// `guard_cells` (the Guard's cells in all), `guard_cells_at_control` (those made before the window
/// opened) and `guard_complete_s` (from the start of the load to the last cell being taken in);
/// `guard_s` is now the time of the first part only, `time_to_control_s` no longer waits for the
/// whole Guard, and its gate is 2 s in the budgets file (it was 10). Nothing else is changed.
///
/// 29 (#54; 27 is #168's, 28 is #220's) added the emissive room to the convergence phase (`convergence.views[]`
/// has a third view, `room`), and Primary's GI against truth: each view's `gi` (Primary's FLIP
/// mean and 99th percentile against truth A and its luminance over truth's, for the first frame
/// and the settled one, with the flat ambient of before the radiance cache (`flat`) and with it
/// (`gi`), and whether GI is no further from truth), and `convergence.gi`: the dig of the room's
/// wall (the time its lit result takes to settle, with the frames) and the gates of the ticket
/// (the room's FLIP, no view further from truth, the settling time, the Reference's room within
/// 0.3 s of GPU time: `gi.gates.reference_room_within_s`, held on the Reference line's `gpu_s`), with
/// the names of those that are over. Every view's `still.marks[]` has a 0.3 s mark, and
/// `still.flip_within_s` says which mark the still run's wall-clock acceptance is of (0.3 s in the
/// room, where it is reported and not gated, 0.25 s elsewhere). A new pass line `gi` is in `passes[]` (the
/// report lists passes generically). Each real-time path has `frame_parts` (the frame thread's
/// recentre, update, encode, submit and wait, the passes' GPU total and the wait that no pass
/// accounts for) and `gpu_passes` (the GPU time of every timed pass), and a real-time run has
/// `unpaced_control` (standing frames with no pacing sleep); the real-time gates have `gi_mean_ms`, `gi_p99_ms`
/// and `gi_paths_timed` (`over` may name `gi`), and the budgets file's `[streaming]` the keys `gi_mean_ms`
/// and `gi_p99_ms`. Nothing else is changed than what this note lists.
///
/// 30 (#226) adds a path to the real-time report: `held under the sea` (the camera dives to 30 m
/// under the sea at 8 m/s and is held there until the loader has caught up), between the sea-level
/// crossing and `stop after a flight`; the paths after it move one place down in `paths`, and its
/// peak is in the run's `scene_used_peak_mib`, which the `scene_mib` gate reads. Nothing else is
/// changed.
///
/// 31 (#50, a spike) adds the report's `rt` block: `requested` (`--rt-backend`: compute, hw or both),
/// `active` (the backend of the flight, the sweep and the Edit stress), `fallback` (why not hw, if
/// it was asked for and the adapter has no ray queries), `accel` (on hw: the acceleration
/// structures' first build, the updates that followed the camera and the Edit stress's, each with
/// what it rebuilt and its CPU and GPU time) and `comparison` (`both`: the same flight on the two
/// backends, the passes that cast rays side by side with their speed-ups, the same with nearest-hit
/// shadow rays (`hw_nearest`), the frame with the hardware's updates in, and the hardware context's
/// `accel` and Edit stress). `edits.accel` is the Edit stress's updates of the
/// acceleration structures (null on compute). Nothing else is changed.
///
/// 33 (#236; 32 is #234's and #203's) keeps the hardware backend's acceleration structures in step
/// off the frame thread: `rt.accel` gains `sweep` (the sweep's own updates, no longer counted as
/// the flight's or its warm-up's), each update `pending` (groups waiting for their rebuild) and
/// each summary `pending_max` and `updates_waiting` (the updates that left groups waiting); the streaming real-time run takes `--rt-backend` (`streaming_real_time.rt`:
/// `requested`, `active`, `fallback`, `accel`; `both` also puts the hardware run in `rt.real_time_hw`);
/// every `Spread` has `p95_ms`; the real-time `frame_parts.update` counts the hardware
/// backend's builds wherever they are encoded (also those of the draw), each path has `hardware` (null on
/// compute: that cost by part per frame, the draw's submit and the TLAS's instances, `update_rest`, the BLASes and boxes a frame, the cost per BLAS), each
/// summary `blas_urgent`, `blas_rebuilt_by_level`, `prims_rebuilt_by_level`, `tlas_instances_max`, `prims_total_max` and `phases_ms`, each Edit's update `phases_ms`, `rt.comparison.hw_edits_ground` (the Edit stress by the ground, on hw), `group_regions` of `rt.comparison.hw_accel` and of the Edit stress's and real-time runs' `accel` (the context's own), `rt.hw_group_regions` and `streaming_real_time.rt.hw_group_regions` (what `--hw-group-regions` said), and an adapter that cannot
/// run the hardware backend stops a real-time run that asked for it (an `error`, exit status 1). The `prepare_ms`/`build_ms` of an update are the frame thread's cost
/// only; the GPU's own time is only in the Edit stress (it waits there).
pub const SCHEMA_VERSION: u32 = 33;

/// One timed thing (a pass, or the GPU total of a profiler frame) in one mode, against its
/// budget. In reference mode a profiler frame is one sample, so the entries of that mode have
/// `frames * spp` samples.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub name: String,
    #[serde(flatten)]
    pub stats: Stats,
    pub budget_ms: Option<f64>,
    pub status: Status,
}

/// One mode's run over the path.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModeReport {
    /// Frames of the path that were measured.
    pub frames: u32,
    /// Frames drawn before those and left out.
    pub warmup_frames: u32,
    /// Samples taken per frame of the path (more than 1 in reference mode only). Each sample is
    /// timed on its own, so `passes` and `gpu_total` are per sample, not per frame of the path.
    pub spp: u32,
    pub passes: Vec<Entry>,
    pub gpu_total: Entry,
    /// CPU time to draw one frame of the path and read the image back; in reference mode, a
    /// frame is all of its samples.
    pub frame_time: Stats,
    /// CPU time per frame to follow the camera with the Planet: select the bricks the LOD bands
    /// ask for, pack the ones that changed, and hand them to the GPU. It is done before the
    /// frame is timed, so it is in no pass and not in `frame_time`.
    pub scene_update_ms: Stats,
    /// What the bands selected at each keyframe of the path. Empty for a scene that is not
    /// drawn in levels (`--short`).
    pub lod_at_keyframes: Vec<KeyframeLod>,
}

/// A still camera at one height above the test Planet: the times of the passes there, and the
/// bricks the bands select for it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Altitude {
    /// Above the Planet's mean radius, in metres.
    pub height_m: f64,
    /// Bricks drawn at each level, level 0 first.
    pub bricks: Vec<usize>,
    /// The `primary` and `upscale` passes of primary mode, and the `reference` pass of reference
    /// mode (one sample): no budget is held against them.
    pub primary: Stats,
    pub upscale: Stats,
    pub reference: Stats,
}

/// A still camera at a cave of the Planet: the times of the passes there, as for an
/// [`Altitude`], and the bricks the bands select for it. The bands are for a tenth of the
/// flight's `d0`, so the bricks are far fewer than the flight's.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CaveStill {
    /// `mouth`: at the cave's mouth on a slope, looking at it. `tunnel`: inside the cave,
    /// looking down it.
    pub name: String,
    /// Bricks drawn at each level, level 0 first.
    pub bricks: Vec<usize>,
    pub primary: Stats,
    pub upscale: Stats,
    pub reference: Stats,
}

/// The bricks drawn at each level of detail from one keyframe of the path.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KeyframeLod {
    /// The frame of the run the camera is at the keyframe.
    pub frame: u32,
    /// How high the camera is above the Planet's mean radius, in metres.
    pub height_m: f64,
    /// Bricks drawn at each level, level 0 first.
    pub bricks: Vec<usize>,
}

/// The times that one mode's measured frames recorded, as the GPU timers kept them.
pub struct Measured<'a> {
    pub frames: u32,
    pub warmup_frames: u32,
    pub spp: u32,
    /// `(pass, times)` for each pass that ran.
    pub passes: Vec<(String, &'a [f64])>,
    pub gpu_total: &'a [f64],
    pub wall: &'a [f64],
    /// CPU milliseconds to follow the camera, for each frame.
    pub scene_update: &'a [f64],
    pub lod_at_keyframes: Vec<KeyframeLod>,
}

fn entry(name: &str, times: &[f64], budget_ms: Option<f64>) -> Entry {
    let stats = Stats::of(times);
    Entry {
        name: name.to_string(),
        status: status(budget_ms, stats.mean_ms),
        stats,
        budget_ms,
    }
}

/// `mode` is `primary` or `reference`, which picks the budget of the whole frame.
pub fn mode_report(mode: &str, measured: &Measured, budgets: &Budgets) -> ModeReport {
    ModeReport {
        frames: measured.frames,
        warmup_frames: measured.warmup_frames,
        spp: measured.spp,
        passes: measured
            .passes
            .iter()
            .map(|(name, times)| entry(name, times, budgets.passes.get(name).copied()))
            .collect(),
        gpu_total: entry(
            "gpu_total",
            measured.gpu_total,
            budgets.gpu_total.get(mode).copied(),
        ),
        frame_time: Stats::of(measured.wall),
        scene_update_ms: Stats::of(measured.scene_update),
        lod_at_keyframes: measured.lod_at_keyframes.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Modes {
    pub primary: ModeReport,
    pub reference: ModeReport,
}

impl Modes {
    /// Every entry, with the name of its mode.
    fn entries(&self) -> impl Iterator<Item = (&'static str, &Entry)> {
        [("primary", &self.primary), ("reference", &self.reference)]
            .into_iter()
            .flat_map(|(mode, report)| {
                report
                    .passes
                    .iter()
                    .chain([&report.gpu_total])
                    .map(move |entry| (mode, entry))
            })
    }

    /// `mode/name` of every entry with `status`.
    pub fn names_with(&self, status: Status) -> Vec<String> {
        self.entries()
            .filter(|(_, e)| e.status == status)
            .map(|(mode, e)| format!("{mode}/{}", e.name))
            .collect()
    }

    pub fn statuses(&self) -> Vec<Status> {
        self.entries().map(|(_, e)| e.status).collect()
    }
}

/// How the budgets came out, over both modes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BudgetSummary {
    /// `embedded`, or the path of the file.
    pub source: String,
    /// Whether nothing failed: no pass over budget, and none that could not be checked. False for
    /// a diagnostic run, which is not checked at all: see `skipped`.
    pub ok: bool,
    /// Why the budgets were not checked, for a run with `--diag` switches on; otherwise null.
    pub skipped: Option<String>,
    /// `mode/name` of every entry that failed.
    pub failed: Vec<String>,
    /// Budgets that matched nothing that ran, so held nothing to account.
    pub unmatched: Vec<String>,
}

/// How `modes` fared against `budgets`; a run with `diag` switches on is not held to them.
pub fn summarize(
    source: &str,
    budgets: &Budgets,
    modes: &Modes,
    edits: &[&Entry],
    diag: &[String],
) -> BudgetSummary {
    if !diag.is_empty() {
        return BudgetSummary {
            source: source.to_string(),
            ok: false,
            skipped: Some(format!(
                "a diagnostic run (--diag {}), not a measurement: the budgets were not checked",
                diag.join(",")
            )),
            failed: Vec::new(),
            unmatched: Vec::new(),
        };
    }
    let failed: Vec<String> = modes
        .entries()
        .filter(|(_, e)| e.status.fails())
        .map(|(mode, e)| format!("{mode}/{}", e.name))
        .chain(
            edits
                .iter()
                .filter(|e| e.status.fails())
                .map(|e| format!("edits/{}", e.name)),
        )
        .collect();
    let unmatched = budgets
        .passes
        .keys()
        .filter(|name| !modes.entries().any(|(_, e)| &e.name == *name))
        .filter(|name| !edits.iter().any(|e| &e.name == *name))
        .map(|name| format!("passes.{name}"))
        .collect();
    BudgetSummary {
        source: source.to_string(),
        ok: failed.is_empty(),
        skipped: None,
        failed,
        unmatched,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Machine {
    pub gpu: String,
    /// `Discrete`, `Integrated`, `Virtual`, `Cpu` (software, such as lavapipe) or `Other`.
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    pub backend: String,
    pub os: String,
    pub arch: String,
    /// Without GPU timestamp queries there are no pass times, only frame times.
    pub timers_supported: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Config {
    /// The traced (internal) resolution: what primary and reference mode trace.
    pub width: u32,
    pub height: u32,
    /// The resolution of the image primary mode presents, upscaled from the traced one.
    pub output_width: u32,
    pub output_height: u32,
    /// `planet` is the whole test Planet; `patch` the cut of it near the pole that `--short` uses.
    pub scene: String,
    /// The camera path, which has a version in its name: the same name is the same flight.
    pub path: String,
    pub keyframes: usize,
    /// The LOD bias: output pixels one Voxel may cover (`--bias`).
    pub bias: f64,
    /// How long the scene thread takes in what the loader made for in a round while the loader's
    /// workers are as far ahead as they may be, in milliseconds: the client's
    /// (`scene_worker::BACKED_UP_SLICE`) unless `--stream-slice-ms` said (the real-time mode).
    pub stream_slice_ms: f64,
    /// The `--diag` switches that were on, as spelled there. Empty for a measurement; a report
    /// that has any is not one, and its budgets are not checked.
    pub diag: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Build {
    pub version: String,
    /// The commit the executable was built from, when CI built it.
    pub commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub build: Build,
    pub machine: Machine,
    pub config: Config,
    pub modes: Modes,
    /// The Planet from a still camera at several heights, one after the other, after the flight;
    /// empty for `--short`.
    pub altitudes: Vec<Altitude>,
    /// Still cameras at a cave of the Planet, one after the other, after the sweep; empty for
    /// `--short`. Their screenshots are in `screenshots`, as `primary-cave-NAME.png` and
    /// `reference-cave-NAME.png`.
    pub caves: Vec<CaveStill>,
    /// File names, in the output folder.
    pub screenshots: Vec<String>,
    pub budgets: BudgetSummary,
    /// The Edit stress: the largest Brush, dug and placed in turn at the metric rate, on the
    /// whole Planet; null for `--short`.
    pub edits: Option<EditsReport>,
    /// The convergence phase (#187, left out by `--no-convergence`): how fast the Reference view
    /// gets clean, in two still views, scored against a truth. Not held to any budget; null with
    /// `--no-convergence` or when the phase fails (an error or a panic: the rest is still written).
    pub convergence: Option<ConvergenceReport>,
    /// The streaming mode (`--streaming`): counts made by the stepped, deterministic mode on the
    /// test Planet, and the GPU's timing of it, which is not deterministic; null without the flag.
    pub streaming: Option<StreamReport>,
    /// The real-time mode of the streaming bench (`--streaming --real-time`, #162): the full-size
    /// Planet with real threads, the paths, the gates and the sizing at full size, over several
    /// runs; not deterministic. Null without the flag.
    pub streaming_real_time: Option<RealTimeReport>,
    /// How rays found the Planet in the flight, and what the hardware backend costs (#50).
    pub rt: RtReport,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budgets() -> Budgets {
        Budgets::parse(
            "[passes]\nprimary = 1.0\nreference = 25.0\nshadows = 0.4\n[gpu_total]\nprimary = 8.6\n",
        )
        .unwrap()
    }

    fn measured<'a>(passes: Vec<(String, &'a [f64])>, total: &'a [f64]) -> Measured<'a> {
        Measured {
            frames: 4,
            warmup_frames: 2,
            spp: 1,
            passes,
            gpu_total: total,
            wall: &[16.0, 18.0],
            scene_update: &[2.0, 4.0],
            lod_at_keyframes: vec![KeyframeLod {
                frame: 0,
                height_m: 12.5,
                bricks: vec![10, 0, 4],
            }],
        }
    }

    #[test]
    fn a_diagnostic_run_is_marked_and_not_held_to_its_budgets() {
        // Far over the 1 ms budget, which a measurement would fail.
        let slow = modes(5.0);
        let measured = summarize("embedded", &budgets(), &slow, &[], &[]);
        assert!(!measured.ok && !measured.failed.is_empty() && measured.skipped.is_none());
        let diag = vec!["no-follow".to_string(), "gap-ms=2".to_string()];
        let skipped = summarize("embedded", &budgets(), &slow, &[], &diag);
        assert!(!skipped.ok && skipped.failed.is_empty());
        let reason = skipped.skipped.expect("a reason");
        assert!(reason.contains("no-follow,gap-ms=2") && reason.contains("not checked"));
    }

    #[test]
    fn a_mode_lists_each_pass_with_its_stats_and_how_it_fared_against_its_budget() {
        let primary: &[f64] = &[0.5, 0.7, 0.9, 1.7];
        let blit: &[f64] = &[0.1, 0.1];
        let report = mode_report(
            "primary",
            &measured(
                vec![("primary".into(), primary), ("blit".into(), blit)],
                &[1.0, 2.0],
            ),
            &budgets(),
        );
        assert_eq!((report.frames, report.warmup_frames, report.spp), (4, 2, 1));
        let [p, b] = &report.passes[..] else {
            panic!("{:?}", report.passes)
        };
        assert_eq!(p.name, "primary");
        assert_eq!(p.stats.mean_ms, Some(0.95));
        assert_eq!((p.budget_ms, p.status), (Some(1.0), Status::Within));
        assert_eq!(b.name, "blit");
        assert_eq!((b.budget_ms, b.status), (None, Status::NoBudget));
        // The whole frame is held to the budget of this mode.
        assert_eq!(report.gpu_total.name, "gpu_total");
        assert_eq!(report.gpu_total.stats.mean_ms, Some(1.5));
        assert_eq!(report.gpu_total.budget_ms, Some(8.6));
        assert_eq!(report.frame_time.mean_ms, Some(17.0));
        assert_eq!(report.scene_update_ms.mean_ms, Some(3.0));
        assert_eq!(report.lod_at_keyframes[0].bricks, [10, 0, 4]);
    }

    #[test]
    fn a_pass_slower_than_its_budget_is_marked_over() {
        let slow: &[f64] = &[1.5, 1.5];
        let report = mode_report(
            "primary",
            &measured(vec![("primary".into(), slow)], &[1.5, 1.5]),
            &budgets(),
        );
        assert_eq!(report.passes[0].status, Status::Over);
    }

    #[test]
    fn a_pass_that_ran_without_times_is_no_data() {
        let report = mode_report(
            "primary",
            &measured(vec![("primary".into(), &[])], &[]),
            &budgets(),
        );
        assert_eq!(report.passes[0].status, Status::NoData);
        assert_eq!(report.passes[0].stats.samples, 0);
        assert_eq!(report.gpu_total.status, Status::NoData);
    }

    fn modes(primary_ms: f64) -> Modes {
        let primary: &[f64] = &[primary_ms];
        let reference: &[f64] = &[5.0];
        Modes {
            primary: mode_report(
                "primary",
                &measured(vec![("primary".into(), primary)], primary),
                &budgets(),
            ),
            reference: mode_report(
                "reference",
                &measured(vec![("reference".into(), reference)], reference),
                &budgets(),
            ),
        }
    }

    #[test]
    fn the_summary_is_ok_when_nothing_is_over() {
        let summary = summarize("embedded", &budgets(), &modes(0.5), &[], &[]);
        assert!(summary.ok, "{summary:?}");
        assert!(summary.failed.is_empty());
        assert_eq!(summary.source, "embedded");
    }

    #[test]
    fn the_summary_names_what_failed_and_which_budgets_matched_nothing() {
        let summary = summarize("b.toml", &budgets(), &modes(3.0), &[], &[]);
        assert!(!summary.ok);
        // 3.0 ms is over the pass's 1.0 ms, but within the 8.6 ms of the whole frame.
        assert_eq!(summary.failed, ["primary/primary"]);
        // `shadows` is budgeted but no such pass ran.
        assert_eq!(summary.unmatched, ["passes.shadows"]);
    }

    #[test]
    fn the_report_serialises_to_the_documented_shape() {
        let report = Report {
            schema_version: SCHEMA_VERSION,
            build: Build {
                version: "0.1.0".into(),
                commit: None,
            },
            machine: Machine {
                gpu: "Test GPU".into(),
                device_type: "Cpu".into(),
                driver: "d".into(),
                driver_info: "i".into(),
                backend: "Vulkan".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                timers_supported: true,
            },
            config: Config {
                width: 64,
                height: 36,
                output_width: 128,
                output_height: 72,
                scene: "patch".into(),
                path: "flyover".into(),
                keyframes: 3,
                bias: 1.0,
                stream_slice_ms: 32.0,
                diag: Vec::new(),
            },
            modes: modes(0.5),
            altitudes: vec![Altitude {
                height_m: 100.0,
                bricks: vec![5, 6],
                primary: Stats::of(&[0.2, 0.4]),
                upscale: Stats::of(&[0.1]),
                reference: Stats::of(&[1.0]),
            }],
            caves: vec![CaveStill {
                name: "tunnel".into(),
                bricks: vec![7],
                primary: Stats::of(&[0.3]),
                upscale: Stats::of(&[0.1]),
                reference: Stats::of(&[1.5]),
            }],
            screenshots: vec!["primary-0.png".into()],
            budgets: summarize("embedded", &budgets(), &modes(0.5), &[], &[]),
            edits: None,
            convergence: None,
            streaming: None,
            streaming_real_time: None,
            rt: RtReport {
                requested: "compute".into(),
                active: "compute".into(),
                fallback: None,
                accel: None,
                comparison: None,
                real_time_hw: None,
                hw_group_regions: "4".into(),
            },
        };
        let json = serde_json::to_value(&report).unwrap();
        let keys = |v: &serde_json::Value| -> Vec<String> {
            v.as_object().unwrap().keys().cloned().collect()
        };
        assert_eq!(
            keys(&json),
            [
                "altitudes",
                "budgets",
                "build",
                "caves",
                "config",
                "convergence",
                "edits",
                "machine",
                "modes",
                "rt",
                "schema_version",
                "screenshots",
                "streaming",
                "streaming_real_time"
            ]
        );
        assert_eq!(json["schema_version"], 33);
        assert_eq!(json["rt"]["active"], "compute");
        assert_eq!(json["rt"]["comparison"], serde_json::Value::Null);
        assert_eq!(json["rt"]["real_time_hw"], serde_json::Value::Null);
        assert_eq!(json["convergence"], serde_json::Value::Null);
        assert_eq!(json["streaming"], serde_json::Value::Null);
        assert_eq!(json["streaming_real_time"], serde_json::Value::Null);
        assert_eq!(
            keys(&json["caves"][0]),
            ["bricks", "name", "primary", "reference", "upscale"]
        );
        assert_eq!(json["build"]["commit"], serde_json::Value::Null);
        assert_eq!(json["machine"]["timers_supported"], true);
        let pass = &json["modes"]["primary"]["passes"][0];
        // Stats are flattened into the entry; a figure that is missing is null, not absent.
        assert_eq!(
            keys(pass),
            [
                "budget_ms",
                "max_ms",
                "mean_ms",
                "name",
                "p95_ms",
                "samples",
                "status"
            ]
        );
        assert_eq!(pass["status"], "within");
        assert_eq!(
            keys(&json["modes"]["primary"]),
            [
                "frame_time",
                "frames",
                "gpu_total",
                "lod_at_keyframes",
                "passes",
                "scene_update_ms",
                "spp",
                "warmup_frames"
            ]
        );
        assert_eq!(
            keys(&json["modes"]["primary"]["frame_time"]),
            ["max_ms", "mean_ms", "p95_ms", "samples"]
        );
    }
}
