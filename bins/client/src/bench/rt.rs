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
    /// `build` (all of it), `patch` (the groups a move or an Edit touched), `anchor` (the camera
    /// left the TLAS's frame: instances only) or `none`.
    pub kind: String,
    pub blas_rebuilt: u32,
    pub prims_rebuilt: u64,
    pub tlas_rebuilt: bool,
    pub tlas_instances: u32,
    /// BLASes (regular groups of 4 x 4 x 4 regions) and boxes (one per non-empty brick) there are.
    pub blas_total: u32,
    pub prims_total: u64,
    /// CPU time to work out the boxes and make the BLASes.
    pub prepare_ms: f64,
    /// Recording the builds, submitting them and waiting for the GPU.
    pub build_ms: f64,
}

impl From<&HwUpdate> for AccelUpdate {
    fn from(u: &HwUpdate) -> Self {
        Self {
            kind: u.kind.to_string(),
            blas_rebuilt: u.blas_rebuilt,
            prims_rebuilt: u.prims_rebuilt,
            tlas_rebuilt: u.tlas_rebuilt,
            tlas_instances: u.tlas_instances,
            blas_total: u.blas_total,
            prims_total: u.prims_total,
            prepare_ms: u.prepare.as_secs_f64() * 1e3,
            build_ms: u.build.as_secs_f64() * 1e3,
        }
    }
}

/// The updates of a stretch of the run (the flight's follows, or the Edit stress's Edits) that
/// rebuilt anything: how many, what they rebuilt, and what each cost.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AccelSummary {
    pub updates: u32,
    /// Updates that found nothing to rebuild (a scene update that no box was stale after): not in
    /// `updates` or the figures below, which are of those that rebuilt something.
    pub noop_updates: u32,
    pub blas_rebuilt: u64,
    pub prims_rebuilt: u64,
    pub tlas_rebuilds: u32,
    /// BLASes rebuilt by an update, mean and most.
    pub blas_rebuilt_per_update_mean: Option<f64>,
    pub blas_rebuilt_per_update_max: Option<u32>,
    pub prepare_ms: Stats,
    pub build_ms: Stats,
    /// `prepare_ms` + `build_ms` of each update.
    pub total_ms: Stats,
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
            updates: done.len() as u32,
            noop_updates: (updates.len() - done.len()) as u32,
            blas_rebuilt: per.iter().map(|&n| u64::from(n)).sum(),
            prims_rebuilt: done.iter().map(|u| u.prims_rebuilt).sum(),
            tlas_rebuilds: done.iter().filter(|u| u.tlas_rebuilt).count() as u32,
            blas_rebuilt_per_update_mean: (!per.is_empty())
                .then(|| per.iter().map(|&n| f64::from(n)).sum::<f64>() / per.len() as f64),
            blas_rebuilt_per_update_max: per.iter().copied().max(),
            prepare_ms: Stats::of(&done.iter().map(|u| ms(u.prepare)).collect::<Vec<_>>()),
            build_ms: Stats::of(&done.iter().map(|u| ms(u.build)).collect::<Vec<_>>()),
            total_ms: Stats::of(
                &done
                    .iter()
                    .map(|u| ms(u.prepare + u.build))
                    .collect::<Vec<_>>(),
            ),
            total_sum_ms: done.iter().map(|u| ms(u.prepare + u.build)).sum::<f64>() + noop_search,
            noop_search_ms: noop_search,
        }
    }
}

/// The acceleration structures over a run: the first build, the updates that followed the camera
/// in the flight, and the Edit stress's.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AccelReport {
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
            initial: AccelUpdate::default(),
            // 3 frames, and the updates cost 6 ms in all: 2 ms a frame on top of the hardware's 4.
            flight: AccelSummary::of(&[update("patch", 2, 4), update("patch", 1, 0)]),
            flight_frames: 3,
            warmup: AccelSummary::default(),
            first_frame: AccelSummary::default(),
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
