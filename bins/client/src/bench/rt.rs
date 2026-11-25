//! Which way rays find the Planet, and what the hardware way costs (#50, a spike): the report's
//! `rt` block. `--rt-backend compute|hw|both` (see `docs/agents/benchmarking.md`).
//!
//! * `compute` is the default: the report says so, and has no acceleration structures.
//! * `hw` runs the flight, the sweep and the Edit stress on the hardware backend (ray queries over
//!   the bricks' tight boxes, `render::hw`), or on the compute one, with a warning and the reason
//!   in `rt.fallback`, if the adapter has no ray queries. The convergence phase, the cave stills
//!   and the streaming modes are compute whichever is asked: they are not what is being compared.
//! * `both` runs the bench as `compute` does, and then the same flight and the same Edit stress on
//!   the hardware backend, on a context of its own, and compares the passes that cast rays
//!   (`primary`, `shadows`, `gi`) and the whole frame. The speed-ups are the compute backend's mean
//!   over the hardware one's, from the same flight on the same GPU.
//!
//! What the acceleration structures cost is the CPU's time to work out the boxes and make the
//! BLASes (`prepare_ms`) and the time to build them on the GPU and wait for it (`build_ms`), for
//! each update of the scene while the flight followed the camera, and for each Edit.

use render::{HwUpdate, RtBackend};
use serde::Serialize;

use super::edits::EditsReport;
use super::realtime::Spread;
use super::report::{Entry, ModeReport};
use super::stats::Stats;

/// What `--rt-backend` asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RtMode {
    #[default]
    Compute,
    Hardware,
    /// Ray queries with the shadow rays looking for the nearest hit (`RtBackend::HardwareNearest`).
    HardwareNearest,
    Both,
}

impl RtMode {
    /// `compute`, `hw`, `hw-nearest` or `both`.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "compute" => Ok(Self::Compute),
            "hw" | "hardware" => Ok(Self::Hardware),
            "hw-nearest" => Ok(Self::HardwareNearest),
            "both" => Ok(Self::Both),
            _ => Err(format!(
                "`{text}` is not a ray backend: compute, hw, hw-nearest or both"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Compute => "compute",
            Self::Hardware => "hw",
            Self::HardwareNearest => "hw-nearest",
            Self::Both => "both",
        }
    }

    /// The backend the main run (the flight, the sweep and the Edit stress) is on.
    pub fn main_backend(self) -> RtBackend {
        match self {
            Self::Hardware => RtBackend::Hardware,
            Self::HardwareNearest => RtBackend::HardwareNearest,
            Self::Compute | Self::Both => RtBackend::Compute,
        }
    }
}

/// One update of the acceleration structures: what it rebuilt and what it cost.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AccelUpdate {
    /// `build` (all of it, after a jump), `patch` (the groups a move or an Edit touched, as many as
    /// the frame's budget allowed), `anchor` (the camera left the TLAS's frame: instances only) or
    /// `none`.
    pub kind: String,
    pub blas_rebuilt: u32,
    /// Of them, the BLASes that were built at once because a group gained boxes or had no BLAS
    /// (the rest may wait for the frame's budget).
    pub blas_urgent: u32,
    pub prims_rebuilt: u64,
    pub tlas_rebuilt: bool,
    pub tlas_instances: u32,
    /// BLASes (regular groups of 4 x 4 x 4 regions) and boxes (one per non-empty brick) there are.
    pub blas_total: u32,
    pub prims_total: u64,
    /// Groups that were stale and waited for a later update's budget after this one.
    pub pending: u32,
    /// The frame thread's CPU time of planning the update: the groups to build (which the scene
    /// thread listed with the patch), the pool's room, the new BLASes and the TLAS's instances.
    /// No box is made by the CPU: the GPU makes them from the occupancy masks (#236).
    pub prepare_ms: f64,
    /// Recording the box pass and the builds and submitting them: the frame thread's cost of the
    /// builds. It does not wait for the GPU (#236), except in the Edit stress, which asks the
    /// update to wait so that the builds' own time is in it.
    pub build_ms: f64,
    /// `prepare_ms + build_ms` by part (see [`AccelPhases`]).
    pub phases_ms: PhasesMs,
}

/// One update's time by part, in milliseconds (`render::HwPhases`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct PhasesMs {
    pub before: f64,
    pub plan: f64,
    pub instances: f64,
    pub boxes_pass: f64,
    pub builds: f64,
    pub finish: f64,
    pub submit: f64,
    pub wait: f64,
}

impl From<&HwUpdate> for PhasesMs {
    fn from(u: &HwUpdate) -> Self {
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
        Self {
            before: ms(u.phases.before),
            plan: ms(u.phases.plan),
            instances: ms(u.phases.instances),
            boxes_pass: ms(u.phases.boxes),
            builds: ms(u.phases.builds),
            finish: ms(u.phases.finish),
            submit: ms(u.phases.submit),
            wait: ms(u.phases.wait),
        }
    }
}

/// The updates' time by part: each a `Stats` over the updates that rebuilt something.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AccelPhases {
    pub before: Stats,
    pub plan: Stats,
    pub instances: Stats,
    pub boxes_pass: Stats,
    pub builds: Stats,
    pub finish: Stats,
    pub submit: Stats,
    pub wait: Stats,
}

impl AccelPhases {
    fn of(done: &[&HwUpdate]) -> Self {
        let all: Vec<PhasesMs> = done.iter().map(|u| PhasesMs::from(*u)).collect();
        let col = |f: fn(&PhasesMs) -> f64| Stats::of(&all.iter().map(f).collect::<Vec<_>>());
        Self {
            before: col(|p| p.before),
            plan: col(|p| p.plan),
            instances: col(|p| p.instances),
            boxes_pass: col(|p| p.boxes_pass),
            builds: col(|p| p.builds),
            finish: col(|p| p.finish),
            submit: col(|p| p.submit),
            wait: col(|p| p.wait),
        }
    }
}

impl From<&HwUpdate> for AccelUpdate {
    fn from(u: &HwUpdate) -> Self {
        Self {
            kind: u.kind.to_string(),
            blas_rebuilt: u.blas_rebuilt,
            blas_urgent: u.blas_urgent,
            prims_rebuilt: u.prims_rebuilt,
            tlas_rebuilt: u.tlas_rebuilt,
            tlas_instances: u.tlas_instances,
            blas_total: u.blas_total,
            prims_total: u.prims_total,
            pending: u.pending,
            prepare_ms: u.prepare.as_secs_f64() * 1e3,
            build_ms: u.build.as_secs_f64() * 1e3,
            phases_ms: PhasesMs::from(u),
        }
    }
}

/// The updates of a stretch of the run (the flight's follows, or the Edit stress's Edits) that
/// rebuilt anything: how many, what they rebuilt, and what each cost.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AccelSummary {
    /// The BLAS size of the context that made these updates, read from the context (`--hw-group-
    /// regions`' syntax), where a summary says it (the Edit stress's and the real-time runs').
    #[serde(skip_serializing_if = "String::is_empty")]
    pub group_regions: String,
    pub updates: u32,
    /// Updates that found nothing to rebuild (a scene update that no box was stale after): not in
    /// `updates` or the figures below, which are of those that rebuilt something.
    pub noop_updates: u32,
    pub blas_rebuilt: u64,
    /// Of them, built at once because the group gained boxes or had no BLAS: what
    /// `--hw-build-blas` cannot spread (#236).
    pub blas_urgent: u64,
    pub prims_rebuilt: u64,
    pub tlas_rebuilds: u32,
    /// BLASes rebuilt by an update, mean and most.
    pub blas_rebuilt_per_update_mean: Option<f64>,
    pub blas_rebuilt_per_update_max: Option<u32>,
    /// The most groups that were stale and waiting for a later update's budget after one of the
    /// updates.
    pub pending_max: u32,
    /// How many of the updates left groups waiting (the frames in which a group showed the boxes
    /// it had, which only lost: see `RingPatch::grown`).
    pub updates_waiting: u32,
    /// The BLASes and boxes the updates rebuilt, by the level of the group (index 0 is level 0):
    /// where the builds are, which sizes of BLAS to try (#236).
    pub blas_rebuilt_by_level: Vec<u64>,
    pub prims_rebuilt_by_level: Vec<u64>,
    /// The most instances the TLAS had and the most boxes there were, after one of the updates.
    pub tlas_instances_max: u32,
    pub prims_total_max: u64,
    pub prepare_ms: Stats,
    pub build_ms: Stats,
    /// `prepare_ms` + `build_ms` of each update.
    pub total_ms: Stats,
    /// `prepare_ms` + `build_ms` by part, per update that rebuilt something.
    pub phases_ms: AccelPhases,
    /// The sum of `total_ms` over the updates, and the time of the search that found nothing stale
    /// in those that rebuilt nothing (`noop_updates`): what the stretch of the run spent on them.
    pub total_sum_ms: f64,
    /// The part of `total_sum_ms` that is the search of the updates that rebuilt nothing.
    pub noop_search_ms: f64,
}

impl AccelSummary {
    pub fn of(updates: &[HwUpdate]) -> Self {
        let done: Vec<&HwUpdate> = updates.iter().filter(|u| u.kind != "none").collect();
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
        let noop_search: f64 = updates
            .iter()
            .filter(|u| u.kind == "none")
            .map(|u| ms(u.prepare + u.build))
            .sum();
        let per: Vec<u32> = done.iter().map(|u| u.blas_rebuilt).collect();
        Self {
            group_regions: String::new(),
            updates: done.len() as u32,
            noop_updates: (updates.len() - done.len()) as u32,
            blas_rebuilt: per.iter().map(|&n| u64::from(n)).sum(),
            blas_urgent: done.iter().map(|u| u64::from(u.blas_urgent)).sum(),
            prims_rebuilt: done.iter().map(|u| u.prims_rebuilt).sum(),
            tlas_rebuilds: done.iter().filter(|u| u.tlas_rebuilt).count() as u32,
            blas_rebuilt_per_update_mean: (!per.is_empty())
                .then(|| per.iter().map(|&n| f64::from(n)).sum::<f64>() / per.len() as f64),
            blas_rebuilt_per_update_max: per.iter().copied().max(),
            pending_max: updates.iter().map(|u| u.pending).max().unwrap_or(0),
            updates_waiting: updates.iter().filter(|u| u.pending > 0).count() as u32,
            blas_rebuilt_by_level: (0..render::lod::LEVELS)
                .map(|l| updates.iter().map(|u| u64::from(u.blas_by_level[l])).sum())
                .collect(),
            prims_rebuilt_by_level: (0..render::lod::LEVELS)
                .map(|l| updates.iter().map(|u| u.prims_by_level[l]).sum())
                .collect(),
            tlas_instances_max: updates.iter().map(|u| u.tlas_instances).max().unwrap_or(0),
            prims_total_max: updates.iter().map(|u| u.prims_total).max().unwrap_or(0),
            prepare_ms: Stats::of(&done.iter().map(|u| ms(u.prepare)).collect::<Vec<_>>()),
            build_ms: Stats::of(&done.iter().map(|u| ms(u.build)).collect::<Vec<_>>()),
            total_ms: Stats::of(
                &done
                    .iter()
                    .map(|u| ms(u.prepare + u.build))
                    .collect::<Vec<_>>(),
            ),
            phases_ms: AccelPhases::of(&done),
            total_sum_ms: done.iter().map(|u| ms(u.prepare + u.build)).sum::<f64>() + noop_search,
            noop_search_ms: noop_search,
        }
    }
}

/// What the hardware backend's updates cost the frame thread in one frame, by part, in
/// milliseconds: the parts of [`render::HwPhases`] summed over the frame's updates (the one of the
/// update's own call and the draw's follow of the camera), and how much they built (#236).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HwFrame {
    pub before: f64,
    pub plan: f64,
    pub instances: f64,
    pub boxes_pass: f64,
    pub builds: f64,
    pub finish: f64,
    pub submit: f64,
    pub wait: f64,
    /// BLASes built and the boxes in them, and the submits made (updates that did something).
    pub blas: u32,
    /// Of them, the urgent ones.
    pub blas_urgent: u32,
    pub boxes: u64,
    pub submits: u32,
    /// The TLAS's instances at the end of the frame, and the draw's own submit (`frame_parts.submit`,
    /// in ms), which on this backend checks every instance of the TLAS it traces (#236).
    pub tlas_instances: u32,
    pub draw_submit: f64,
    /// The frame's wait for the GPU less what its passes took (`frame_parts.wait_not_passes`): the
    /// part of the wait that is the update's boxes, builds and copies, the driver and the rest.
    /// Zero where the device has no timers.
    pub wait_not_passes: f64,
    /// BLASes made (not only built) in the frame, the updates in which the box pool grew and those
    /// in which the TLAS was made again for more instances: the frame's allocations.
    pub blas_created: u32,
    pub pool_grew: u32,
    pub tlas_made: u32,
    /// With `--hw-wait`: the GPU's time of the box pass alone, which `wait` then leaves out.
    pub wait_boxes: f64,
}

impl HwFrame {
    pub fn of(updates: &[HwUpdate]) -> Self {
        let mut f = Self::default();
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
        for u in updates {
            f.before += ms(u.phases.before);
            f.plan += ms(u.phases.plan);
            f.instances += ms(u.phases.instances);
            f.boxes_pass += ms(u.phases.boxes);
            f.builds += ms(u.phases.builds);
            f.finish += ms(u.phases.finish);
            f.submit += ms(u.phases.submit);
            f.wait += ms(u.phases.wait);
            f.blas += u.blas_rebuilt;
            f.blas_urgent += u.blas_urgent;
            f.boxes += u.prims_rebuilt;
            f.submits += u32::from(u.kind != "none");
            f.blas_created += u.blas_created;
            f.pool_grew += u32::from(u.pool_grew);
            f.tlas_made += u32::from(u.tlas_made);
            f.wait_boxes += ms(u.phases.wait_boxes);
        }
        f
    }

    /// All of the frame's hardware time: the sum of the parts.
    pub fn total(&self) -> f64 {
        self.before
            + self.plan
            + self.instances
            + self.boxes_pass
            + self.builds
            + self.finish
            + self.submit
            + self.wait
            + self.wait_boxes
    }
}

/// A count per frame: the mean, the 95th percentile (nearest rank) and the most.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Count {
    pub mean: f64,
    pub p95: f64,
    pub max: f64,
}

impl Count {
    fn of(values: &[f64]) -> Self {
        let s = Spread::of(values);
        Self {
            mean: s.mean_ms,
            p95: s.p95_ms,
            max: s.worst_ms,
        }
    }
}

/// Microseconds per BLAS built, by part, over the frames that built any: the sum of a part over
/// them over the BLASes they built.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct PerBlasUs {
    pub total: f64,
    pub before: f64,
    pub plan: f64,
    pub instances: f64,
    pub boxes_pass: f64,
    pub builds: f64,
    pub finish: f64,
    pub submit: f64,
}

/// What the hardware backend cost the frame thread over the frames of a path, by part (#236): each
/// `Spread` is of every frame of the path (zero where nothing was built), like
/// `frame_parts.update`, which has all of it: `total` is the hardware's part of it and
/// `update_rest` what is left, which is what the compute backend's update costs as well.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct PathHardware {
    /// Frames in which a BLAS was built.
    pub frames_with_builds: usize,
    /// The parts' sum, per frame.
    pub total: Spread,
    pub before: Spread,
    pub plan: Spread,
    pub instances: Spread,
    pub boxes_pass: Spread,
    pub builds: Spread,
    pub finish: Spread,
    pub submit: Spread,
    pub wait: Spread,
    /// `frame_parts.update` less `total`, per frame.
    pub update_rest: Spread,
    /// BLASes and boxes built per frame, and the submits made.
    pub blas: Count,
    pub blas_urgent: Count,
    pub boxes: Count,
    pub submits: Count,
    /// The TLAS's instances, and the draw's own submit in ms (`frame_parts.submit`: with the
    /// instances, what a submit that traces costs by them).
    pub tlas_instances: Count,
    pub draw_submit: Spread,
    pub per_blas_us: PerBlasUs,
    /// With `--hw-wait`: the GPU's time of the box pass alone per frame (`wait` is then the
    /// builds' and the TLAS's); zero without it.
    pub wait_boxes: Spread,
    /// `frame_parts.wait_not_passes` of the frames, by what the frame built (#236): the GPU's
    /// time that is not the passes lands in the wait, and this says which frames carry it.
    pub wait_not_passes: WaitByBuilds,
}

/// The frames of a path in which the GPU's wait was longer than its passes, by what they built:
/// each is a `Spread` of `wait_not_passes` over the frames of that kind (empty, so all zero, where
/// there were none). The kinds overlap: `big` and `urgent` frames are `built` frames too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct WaitByBuilds {
    /// No BLAS was built.
    pub idle: Spread,
    /// Some were, all of them groups that could wait.
    pub waiting_only: Spread,
    /// At least one that could not wait was built.
    pub urgent: Spread,
    /// The frame's boxes were at or over [`render::hw::BUILD_BUDGET`].
    pub big: Spread,
    /// The frame made more than one submit of the acceleration structures.
    pub several_submits: Spread,
    /// At least one BLAS was made (allocated) in the frame, the box pool grew, the TLAS was made
    /// again.
    pub created: Spread,
    pub pool_grew: Spread,
    pub tlas_made: Spread,
}

impl WaitByBuilds {
    fn of(frames: &[(f64, HwFrame)]) -> Self {
        let of = |keep: fn(&HwFrame) -> bool| {
            Spread::of(
                &frames
                    .iter()
                    .map(|(_, h)| h)
                    .filter(|h| keep(h))
                    .map(|h| h.wait_not_passes)
                    .collect::<Vec<_>>(),
            )
        };
        Self {
            idle: of(|h| h.blas == 0),
            waiting_only: of(|h| h.blas > 0 && h.blas_urgent == 0),
            urgent: of(|h| h.blas_urgent > 0),
            big: of(|h| h.boxes >= render::hw::BUILD_BUDGET),
            several_submits: of(|h| h.submits > 1),
            created: of(|h| h.blas_created > 0),
            pool_grew: of(|h| h.pool_grew > 0),
            tlas_made: of(|h| h.tlas_made > 0),
        }
    }
}

impl PathHardware {
    /// `frames` are each frame's update (`frame_parts.update`, in ms) and its hardware parts.
    pub fn of(frames: &[(f64, HwFrame)]) -> Self {
        let col =
            |f: fn(&HwFrame) -> f64| -> Vec<f64> { frames.iter().map(|(_, h)| f(h)).collect() };
        let built: Vec<&HwFrame> = frames
            .iter()
            .map(|(_, h)| h)
            .filter(|h| h.blas > 0)
            .collect();
        let blas: f64 = built.iter().map(|h| f64::from(h.blas)).sum();
        let per = |f: fn(&HwFrame) -> f64| -> f64 {
            if blas == 0.0 {
                0.0
            } else {
                built.iter().map(|h| f(h)).sum::<f64>() / blas * 1e3
            }
        };
        Self {
            frames_with_builds: built.len(),
            total: Spread::of(&col(HwFrame::total)),
            before: Spread::of(&col(|h| h.before)),
            plan: Spread::of(&col(|h| h.plan)),
            instances: Spread::of(&col(|h| h.instances)),
            boxes_pass: Spread::of(&col(|h| h.boxes_pass)),
            builds: Spread::of(&col(|h| h.builds)),
            finish: Spread::of(&col(|h| h.finish)),
            submit: Spread::of(&col(|h| h.submit)),
            wait: Spread::of(&col(|h| h.wait)),
            update_rest: Spread::of(
                &frames
                    .iter()
                    .map(|(update, h)| update - h.total())
                    .collect::<Vec<_>>(),
            ),
            blas: Count::of(&col(|h| f64::from(h.blas))),
            blas_urgent: Count::of(&col(|h| f64::from(h.blas_urgent))),
            boxes: Count::of(&col(|h| h.boxes as f64)),
            submits: Count::of(&col(|h| f64::from(h.submits))),
            tlas_instances: Count::of(&col(|h| f64::from(h.tlas_instances))),
            draw_submit: Spread::of(&col(|h| h.draw_submit)),
            wait_boxes: Spread::of(&col(|h| h.wait_boxes)),
            wait_not_passes: WaitByBuilds::of(frames),
            per_blas_us: PerBlasUs {
                total: per(HwFrame::total),
                before: per(|h| h.before),
                plan: per(|h| h.plan),
                instances: per(|h| h.instances),
                boxes_pass: per(|h| h.boxes_pass),
                builds: per(|h| h.builds),
                finish: per(|h| h.finish),
                submit: per(|h| h.submit),
            },
        }
    }
}

/// The acceleration structures over a run: the first build, the updates that followed the camera
/// in the flight, and the Edit stress's.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AccelReport {
    /// The BLAS size (`--hw-group-regions`' syntax) of the context that made all of this, read from
    /// the context itself and not from what the flag said (#236).
    pub group_regions: String,
    pub initial: AccelUpdate,
    /// The updates of the measured frames, but the first of each mode: the flight.
    pub flight: AccelSummary,
    /// How many frames `flight` is of.
    pub flight_frames: u32,
    /// The jump to the end of the path where a mode warms up (all of the structures are made again,
    /// as the camera is thousands of Voxels away), and the warm-up frames.
    pub warmup: AccelSummary,
    /// The first measured frame of each mode, which follows the camera back to the start of the
    /// path: another jump, which `scene_update_ms` has in its maximum and `flight` does not.
    pub first_frame: AccelSummary,
    /// The sweep's still frames, its jumps to each height and its warm-ups, all of them (the
    /// sweep is not the flight, and its jumps are not the flight's): with `--rt-backend hw` alone
    /// the sweep runs on the hardware backend, which `both` does not.
    pub sweep: AccelSummary,
    /// The jump to where the Edit stress starts, if it ran.
    pub settle: AccelSummary,
    /// The Edits' updates (after each Edit, the groups it touched rebuilt and the TLAS built
    /// again); null if the Edit stress did not run.
    pub edits: Option<AccelSummary>,
}

/// One backend's times over the flight, from its primary mode.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BackendTimes {
    pub backend: String,
    pub frames: u32,
    /// Every timed pass of the frame, by name (`primary`, `normals`, `shadows`, `gi`, `shade`...).
    pub passes: Vec<PassTimes>,
    pub gpu_total: Stats,
    /// CPU time to draw a frame (the commands, the submit, and the hardware backend's move of the
    /// TLAS's frame when the camera leaves it, which waits for the GPU).
    pub frame_time: Stats,
    /// CPU time per frame to follow the camera with the Planet, the update of the hardware
    /// backend's acceleration structures included (`max_ms` is the stutter it makes). Not in
    /// `frame_time` or in any pass. There is no p99: `p95_ms` and `max_ms` are what there are.
    pub scene_update_ms: Stats,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PassTimes {
    pub name: String,
    #[serde(flatten)]
    pub stats: Stats,
}

impl BackendTimes {
    pub fn of(backend: &str, report: &ModeReport) -> Self {
        let pass = |e: &Entry| PassTimes {
            name: e.name.clone(),
            stats: e.stats.clone(),
        };
        Self {
            backend: backend.to_string(),
            frames: report.frames,
            passes: report.passes.iter().map(pass).collect(),
            gpu_total: report.gpu_total.stats.clone(),
            frame_time: report.frame_time.clone(),
            scene_update_ms: report.scene_update_ms.clone(),
        }
    }

    fn mean(&self, pass: &str) -> Option<f64> {
        match pass {
            "gpu_total" => self.gpu_total.mean_ms,
            _ => self
                .passes
                .iter()
                .find(|p| p.name == pass)
                .and_then(|p| p.stats.mean_ms),
        }
    }
}

/// The passes that cast rays, and the whole frame, compared.
const COMPARED: [&str; 4] = ["primary", "shadows", "gi", "gpu_total"];

/// One pass on both backends: the means, and the compute backend's over the hardware one's (above
/// 1, hardware is faster). Null where a pass was not timed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Speedup {
    pub pass: String,
    pub compute_mean_ms: Option<f64>,
    pub hw_mean_ms: Option<f64>,
    pub compute_over_hw: Option<f64>,
    /// With `hw-nearest` (shadow rays that look for the nearest hit), if it ran.
    pub hw_nearest_mean_ms: Option<f64>,
    pub compute_over_hw_nearest: Option<f64>,
}

/// `--rt-backend both`: the same flight on both backends.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comparison {
    pub compute: BackendTimes,
    /// `hw`: shadow rays that end at the first hit.
    pub hw: BackendTimes,
    /// `hw-nearest`, the same flight (the primary mode only) with shadow rays that look for the
    /// nearest hit, to tell what the early-out is worth; null if it did not run.
    pub hw_nearest: Option<BackendTimes>,
    pub speedup: Vec<Speedup>,
    /// The hardware backend's mean `gpu_total` plus what its acceleration structures' updates cost
    /// the flight per frame (`hw_accel.flight.total_sum_ms` over `hw_accel.flight_frames`): the
    /// frame as the player would have it if the updates are on the frame thread (they are in this
    /// bench), which `speedup.gpu_total` leaves out. Compute has no such cost.
    pub hw_gpu_total_plus_accel_ms: Option<f64>,
    /// `compute`'s mean `gpu_total` over it.
    pub compute_over_hw_with_accel: Option<f64>,
    /// The hardware context's acceleration structures: its first build, the flight's updates and
    /// the Edit stress's.
    pub hw_accel: AccelReport,
    /// The Edit stress on the hardware backend (the compute one's is the report's `edits`): its
    /// budgets are not gated, the compute run's are. Null on `--short`.
    pub hw_edits: Option<EditsReport>,
    /// The Edit stress again on the hardware backend with the camera by the ground (2.4 m up), where
    /// the Edits are in level 0 and in the finest BLASes: what a BLAS of `rt.hw_group_regions`
    /// regions costs to make again by an Edit (`accel` has the builds, `frame_thread` the Edit
    /// +BVH on the frame thread). Null on `--short`.
    pub hw_edits_ground: Option<EditsReport>,
}

impl Comparison {
    pub fn new(
        compute: BackendTimes,
        hw: BackendTimes,
        hw_nearest: Option<BackendTimes>,
        hw_accel: AccelReport,
        hw_edits: Option<EditsReport>,
    ) -> Self {
        let over =
            |c: Option<f64>, h: Option<f64>| c.zip(h).filter(|(_, h)| *h > 0.0).map(|(c, h)| c / h);
        let speedup = COMPARED
            .iter()
            .map(|&pass| {
                let (c, h) = (compute.mean(pass), hw.mean(pass));
                let n = hw_nearest.as_ref().and_then(|n| n.mean(pass));
                Speedup {
                    pass: pass.to_string(),
                    compute_mean_ms: c,
                    hw_mean_ms: h,
                    compute_over_hw: over(c, h),
                    hw_nearest_mean_ms: n,
                    compute_over_hw_nearest: over(c, n),
                }
            })
            .collect();
        let with_accel = (hw_accel.flight_frames > 0)
            .then(|| {
                hw.mean("gpu_total")
                    .map(|t| t + hw_accel.flight.total_sum_ms / f64::from(hw_accel.flight_frames))
            })
            .flatten();
        Self {
            compute_over_hw_with_accel: over(compute.mean("gpu_total"), with_accel),
            hw_gpu_total_plus_accel_ms: with_accel,
            compute,
            hw,
            hw_nearest,
            speedup,
            hw_accel,
            hw_edits,
            hw_edits_ground: None,
        }
    }
}

/// The report's `rt` block.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RtReport {
    /// What `--rt-backend` said: `compute` (the default), `hw` or `both`.
    pub requested: String,
    /// The backend the flight, the sweep and the Edit stress ran on: `hw` only if it was asked for
    /// and the adapter has ray queries.
    pub active: String,
    /// Why not `hw`, when it was asked for and did not run: the run is then a compute run.
    pub fallback: Option<String>,
    /// The acceleration structures of the main run; null unless it was on `hw`.
    pub accel: Option<AccelReport>,
    /// The two backends side by side (`both`); null otherwise, or if the adapter has no ray
    /// queries (`fallback` says so).
    pub comparison: Option<Comparison>,
    /// With `--streaming --real-time --rt-backend both`: the real-time mode's runs again on the
    /// hardware backend (#236), to read its frame thread's update and its frame time against
    /// the compute backend's, which are the report's `streaming_real_time`. Null otherwise.
    pub real_time_hw: Option<super::realtime::RealTimeReport>,
    /// What `--hw-group-regions` said (`4` if nothing did): the regions along an edge of the
    /// hardware backend's BLASes, or `NEAR:FAR@LEVEL` (#236).
    pub hw_group_regions: String,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn update(kind: &'static str, blas: u32, build_ms: u64) -> HwUpdate {
        HwUpdate {
            kind,
            blas_rebuilt: blas,
            prims_rebuilt: u64::from(blas) * 10,
            tlas_rebuilt: kind != "none",
            prepare: Duration::from_millis(1),
            build: Duration::from_millis(build_ms),
            ..HwUpdate::default()
        }
    }

    fn phased(blas: u32, ms: f64) -> HwUpdate {
        let d = |x: f64| Duration::from_secs_f64(x / 1e3);
        HwUpdate {
            kind: "patch",
            blas_rebuilt: blas,
            blas_urgent: blas / 2,
            prims_rebuilt: u64::from(blas) * 100,
            phases: render::HwPhases {
                before: d(ms * 0.5),
                plan: d(ms),
                instances: d(ms * 0.5),
                boxes: d(ms),
                builds: d(ms * 2.0),
                finish: d(ms),
                submit: d(ms * 4.0),
                wait: Duration::ZERO,
                wait_boxes: Duration::ZERO,
            },
            ..HwUpdate::default()
        }
    }

    #[test]
    fn a_frames_hardware_time_is_its_updates_parts_summed_and_its_builds_counted() {
        let f = HwFrame::of(&[phased(10, 1.0), phased(5, 2.0), update("none", 0, 0)]);
        assert!((f.total() - 10.0 * 3.0).abs() < 1e-9, "{f:?}");
        assert!(
            (f.plan - 3.0).abs() < 1e-9 && (f.submit - 12.0).abs() < 1e-9,
            "{f:?}"
        );
        assert_eq!((f.blas, f.boxes, f.submits), (15, 1500, 2));
        assert_eq!(f.blas_urgent, 5 + 2);
        assert_eq!(HwFrame::of(&[]), HwFrame::default());
    }

    #[test]
    fn a_paths_hardware_report_has_every_frame_the_rest_of_its_update_and_the_cost_per_blas() {
        let a = HwFrame::of(&[phased(10, 1.0)]);
        let idle = HwFrame::default();
        // 10 ms of the first frame's 12 is the hardware's, and the other frame has an update of
        // 0.5 ms with no hardware in it.
        let h = PathHardware::of(&[(12.0, a), (0.5, idle)]);
        assert_eq!(h.frames_with_builds, 1);
        assert_eq!(h.total.samples, 2);
        assert!((h.total.mean_ms - 5.0).abs() < 1e-9, "{h:?}");
        assert!((h.update_rest.worst_ms - 2.0).abs() < 1e-9, "{h:?}");
        assert!((h.update_rest.mean_ms - 1.25).abs() < 1e-9, "{h:?}");
        assert_eq!((h.blas.max, h.blas.mean), (10.0, 5.0));
        assert_eq!((h.blas_urgent.max, h.blas_urgent.mean), (5.0, 2.5));
        assert_eq!(h.submits.max, 1.0);
        // Over the frames that built: 10 ms / 10 BLAS = 1000 us each, 400 of them the submit.
        assert!((h.per_blas_us.total - 1000.0).abs() < 1e-6, "{h:?}");
        assert!((h.per_blas_us.submit - 400.0).abs() < 1e-6, "{h:?}");
        assert_eq!(PathHardware::of(&[]).per_blas_us, PerBlasUs::default());
    }

    #[test]
    fn a_paths_hardware_report_has_the_draws_submit_and_the_tlas_instances_of_every_frame() {
        let a = HwFrame {
            tlas_instances: 12_000,
            draw_submit: 0.9,
            ..HwFrame::of(&[phased(10, 1.0)])
        };
        let b = HwFrame {
            tlas_instances: 8_000,
            draw_submit: 0.5,
            ..HwFrame::default()
        };
        let h = PathHardware::of(&[(12.0, a), (0.5, b)]);
        assert_eq!(h.tlas_instances.max, 12_000.0);
        assert_eq!(h.tlas_instances.mean, 10_000.0);
        assert!((h.draw_submit.mean_ms - 0.7).abs() < 1e-9, "{h:?}");
        assert_eq!(h.draw_submit.samples, 2);
    }

    #[test]
    fn the_waits_that_are_not_passes_are_told_by_what_the_frame_built() {
        let frame = |blas, urgent, boxes, submits, wait| HwFrame {
            blas,
            blas_urgent: urgent,
            boxes,
            submits,
            wait_not_passes: wait,
            ..HwFrame::default()
        };
        let big = render::hw::BUILD_BUDGET;
        let h = PathHardware::of(&[
            (0.0, frame(0, 0, 0, 0, 0.1)),
            (0.0, frame(0, 0, 0, 0, 0.3)),
            (0.0, frame(4, 0, 1000, 1, 0.5)),
            (0.0, frame(4, 2, 1000, 1, 2.0)),
            (0.0, frame(9, 9, big, 2, 8.0)),
        ]);
        let w = h.wait_not_passes;
        assert_eq!((w.idle.samples, w.idle.worst_ms), (2, 0.3));
        assert_eq!((w.waiting_only.samples, w.waiting_only.worst_ms), (1, 0.5));
        assert_eq!((w.urgent.samples, w.urgent.worst_ms), (2, 8.0));
        assert_eq!((w.big.samples, w.big.worst_ms), (1, 8.0));
        assert_eq!(
            (w.several_submits.samples, w.several_submits.worst_ms),
            (1, 8.0)
        );
        assert_eq!(
            PathHardware::of(&[]).wait_not_passes,
            WaitByBuilds::default()
        );
    }

    #[test]
    fn a_summary_says_where_the_builds_were_by_level_and_how_big_the_tlas_got() {
        let mut a = phased(10, 1.0);
        a.blas_by_level[0] = 7;
        a.blas_by_level[3] = 3;
        a.prims_by_level[0] = 700;
        a.prims_by_level[3] = 300;
        a.tlas_instances = 500;
        a.prims_total = 9000;
        let mut b = phased(2, 1.0);
        b.blas_by_level[0] = 2;
        b.prims_by_level[0] = 50;
        b.tlas_instances = 400;
        let s = AccelSummary::of(&[a, b]);
        assert_eq!(s.blas_rebuilt_by_level.len(), render::lod::LEVELS);
        assert_eq!(
            (s.blas_rebuilt_by_level[0], s.blas_rebuilt_by_level[3]),
            (9, 3)
        );
        assert_eq!(
            (s.prims_rebuilt_by_level[0], s.prims_rebuilt_by_level[3]),
            (750, 300)
        );
        assert_eq!((s.tlas_instances_max, s.prims_total_max), (500, 9000));
    }

    #[test]
    fn a_summary_counts_the_blases_that_were_built_at_once() {
        let urgent = |blas, urgent| HwUpdate {
            blas_urgent: urgent,
            ..update("patch", blas, 1)
        };
        let s = AccelSummary::of(&[urgent(5, 2), urgent(3, 3)]);
        assert_eq!((s.blas_rebuilt, s.blas_urgent), (8, 5));
    }

    #[test]
    fn the_flag_has_four_values_and_compute_is_the_default() {
        assert_eq!(RtMode::default(), RtMode::Compute);
        assert_eq!(RtMode::parse("hw"), Ok(RtMode::Hardware));
        assert_eq!(RtMode::parse("hw-nearest"), Ok(RtMode::HardwareNearest));
        assert_eq!(RtMode::parse("both"), Ok(RtMode::Both));
        assert!(RtMode::parse("rtx").is_err());
        assert_eq!(RtMode::Both.main_backend(), RtBackend::Compute);
        assert_eq!(RtMode::Hardware.main_backend(), RtBackend::Hardware);
        assert_eq!(
            RtMode::HardwareNearest.main_backend(),
            RtBackend::HardwareNearest
        );
    }

    #[test]
    fn a_summary_says_how_many_groups_waited_at_the_most_and_in_how_many_updates() {
        let waiting = |n| HwUpdate {
            pending: n,
            ..update("patch", 1, 1)
        };
        let s = AccelSummary::of(&[waiting(0), waiting(3), waiting(5), waiting(0), waiting(1)]);
        assert_eq!((s.pending_max, s.updates_waiting), (5, 3));
        assert_eq!(AccelSummary::of(&[]).updates_waiting, 0);
    }

    #[test]
    fn a_summary_counts_the_updates_that_rebuilt_something_and_what_they_rebuilt() {
        let s = AccelSummary::of(&[
            update("patch", 2, 4),
            update("none", 0, 0),
            update("patch", 6, 8),
        ]);
        assert_eq!(
            (s.updates, s.blas_rebuilt, s.prims_rebuilt, s.tlas_rebuilds),
            (2, 8, 80, 2)
        );
        assert_eq!(s.blas_rebuilt_per_update_max, Some(6));
        assert_eq!(s.blas_rebuilt_per_update_mean, Some(4.0));
        // 1 + 4 and 1 + 8 ms.
        assert_eq!(s.total_ms.mean_ms, Some(7.0));
        assert_eq!(AccelSummary::of(&[]).updates, 0);
        assert_eq!(AccelSummary::of(&[update("none", 0, 0)]).noop_updates, 1);
        // The search of an update that found nothing stale is in the total, and apart.
        let noop = HwUpdate {
            kind: "none",
            prepare: Duration::from_millis(3),
            ..HwUpdate::default()
        };
        let s = AccelSummary::of(&[update("patch", 1, 4), noop]);
        assert_eq!((s.updates, s.noop_updates), (1, 1));
        assert_eq!((s.noop_search_ms, s.total_sum_ms), (3.0, 8.0));
        assert_eq!(s.noop_updates, 1);
    }

    #[test]
    fn the_speed_up_is_the_compute_mean_over_the_hardware_mean() {
        let times = |name: &str, ms: f64| BackendTimes {
            backend: name.to_string(),
            frames: 3,
            passes: vec![PassTimes {
                name: "primary".to_string(),
                stats: Stats::of(&[ms]),
            }],
            gpu_total: Stats::of(&[ms * 2.0]),
            frame_time: Stats::default(),
            scene_update_ms: Stats::default(),
        };
        let accel = AccelReport {
            group_regions: "16".into(),
            initial: AccelUpdate::default(),
            // 3 frames, and the updates cost 6 ms in all: 2 ms a frame on top of the hardware's 4.
            flight: AccelSummary::of(&[update("patch", 2, 4), update("patch", 1, 0)]),
            flight_frames: 3,
            warmup: AccelSummary::default(),
            first_frame: AccelSummary::default(),
            sweep: AccelSummary::default(),
            settle: AccelSummary::default(),
            edits: None,
        };
        let c = Comparison::new(
            times("compute", 4.0),
            times("hw", 2.0),
            Some(times("hw-nearest", 3.0)),
            accel,
            None,
        );
        let by = |pass: &str| c.speedup.iter().find(|s| s.pass == pass).unwrap();
        assert_eq!(by("primary").compute_over_hw, Some(2.0));
        assert_eq!(by("primary").compute_over_hw_nearest, Some(4.0 / 3.0));
        // The whole frame with the updates: 4 ms of GPU and 6 ms over 3 frames of them.
        assert_eq!(c.hw_gpu_total_plus_accel_ms, Some(6.0));
        assert_eq!(c.compute_over_hw_with_accel, Some(8.0 / 6.0));
        assert_eq!(by("gpu_total").compute_over_hw, Some(2.0));
        // A pass that was not timed has no figure, and not a made-up one.
        assert_eq!(by("shadows").compute_over_hw, None);
    }
}
