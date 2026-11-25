//! The Edit stress of `client bench` (#147): repeated digs and places of the largest Brush, at the
//! rate a player is allowed, over the Planet that the flight was over. It times what an Edit costs
//! the frame (the Voxels changed, the coarse levels remade, the changed words packed and sent to
//! the GPU) and holds the mean over every frame, those without an Edit among them, to the
//! `edits` budget (Edit+BVH, 1 ms).
//!
//! That mean hides a hitch: an Edit that costs 40 ms is 0.1 ms of the 1 800 frames. So the cost
//! is also timed in the two parts that the client runs on two threads (#156, ADR 0006), and the
//! longest Edit of the part that is the frame thread's is held to the `edit_frame_thread` budget:
//! planning the Edit and changing level 0 of the Voxels, and sending the writes to the GPU. The
//! rest, which is the scene thread's, is the coarse levels remade and the changed bricks packed
//! (and what the frame thread pastes of it into its copy of the scene is a few memory copies,
//! which is not timed here). The frame thread does not wait for it: the Edit shows a frame later.
//!
//! This is the sum of the parts as a single thread runs them, at the pole, where the coarse levels
//! are held: it has none of the waits of the real frame thread (the store's lock, the wait for the
//! Edit's pack, the paste, the ghost), nor Voxels read from the generator. Those are held by
//! `streaming_tests::the_frame_thread_takes_a_few_milliseconds_of_the_largest_edit_at_the_spawn_
//! with_every_wait`, which runs the real `World` and `SceneWorker` in release. A whole-scene
//! upload after a `Rebuilt` update (the scene ran out of words) is in `frame_thread` here, as it
//! is on the real frame thread.
//!
//! The rate is the server's own: a [`sim::EditBudget`] is refilled once for each frame, at 60 a
//! second, and an Edit is made as soon as it affords one. So the first is made at once, from the
//! burst, and then the Edits come as the allowance does: 8 m³ a second, which is about one cube
//! of 4 m every four seconds.

use std::error::Error;
use std::time::Instant;

use brick_store::{BrickCoord, VOXELS_PER_METRE, VoxelCoord};
use net::{Alignment, Brush, EditMode, EditOp, Shape};
use render::Offscreen;
use serde::Serialize;

use super::Flight;
use super::budget::{Budgets, status};
use super::report::Entry;
use super::rt::AccelSummary;
use super::stats::Stats;

/// The name of the budget (in `[passes]`) that the longest frame-thread time of an Edit is held to.
const FRAME_THREAD_BUDGET: &str = "edit_frame_thread";

/// Frames of the stress, at 60 a second: 30 s, which is about ten Edits of the largest Brush.
pub const FRAMES: u32 = 1800;

/// The stress by the ground has three times as many (about a dozen Edits, not four): it is the
/// one the BLAS sizes are judged by, and its first Edit is the cold one (#236).
pub const GROUND_FRAMES: u32 = 3 * FRAMES;

/// Where an Edit's frame-thread time goes, by part, each over the Edits (#236): the parts add up
/// to `frame_thread` (a clock read between them, so the sum is the whole but for the reads).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FrameThreadParts {
    /// The Edit planned and level 0 written (`sim::plan_bricks`, `write_plan_from`).
    pub edit: Stats,
    /// `Offscreen::update_scene` less the hardware's update: the writes of the scene's buffers
    /// (and a buffer that grew). The same on both backends.
    pub scene_write: Stats,
    /// The scene's buffers that grew (a new buffer, the copy into it and a new bind group): 0 but
    /// for the Edit that needs more room than they had, which on a context that has not been to
    /// the ground yet is the first (`grown_bytes_each` says which).
    pub scene_grow: Stats,
    /// The host's time in the `Queue::submit` of the scene's copies: where a driver that holds the
    /// submit up behind earlier GPU work (a jump's builds, not waited for) shows.
    pub scene_submit: Stats,
    /// The emitter index's update (the list of lights an Edit may change).
    pub scene_emitters: Stats,
    /// The submit with no commands that sends them (`flush`): on the hardware backend it also
    /// has to get the box pass and the builds' commands to the driver.
    pub flush: Stats,
    /// The hardware backend's update less its wait (`prepare_ms` + `build_ms` of the Edit's
    /// updates, less `wait`): 0 on compute. What a frame of the game pays, which never waits.
    pub accel: Stats,
    /// The hardware backend's wait for the GPU to build (the stress waits, the game does not): the
    /// builds' own GPU time, and whatever the queue held before them.
    pub accel_wait: Stats,
    /// `Offscreen::edit_made` (the radiance cache follows the light round the Edit).
    pub edit_made: Stats,
}

/// What the stress measured.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EditsReport {
    /// The Brush of every Edit, in words.
    pub brush: String,
    /// Frames drawn at 60 a second, and the Edits made in them (digs and places in turn).
    pub frames: u32,
    pub edits: u32,
    /// Voxels the Edits changed in all, and the rate that is, in m³ a second.
    pub voxels_changed: u64,
    pub m3_per_s: f64,
    /// CPU time of the frames that made an Edit: the Edit planned and applied, the scene
    /// updated, and the writes sent to the GPU: all of it, as if it were on one thread.
    pub per_edit: Stats,
    /// CPU time of the part of each Edit that is the client's frame thread's: the Edit planned,
    /// level 0 of the Voxels changed, and the writes sent to the GPU. Held to the
    /// `edit_frame_thread` budget by its longest Edit (`max_ms`), so that one hitch fails.
    pub frame_thread: Entry,
    /// `frame_thread` by part.
    pub frame_thread_parts: FrameThreadParts,
    /// The frame-thread time of each Edit in ms, in order (the first is the cold one).
    pub frame_thread_each: Vec<f64>,
    /// The bytes that each Edit's update copied on the GPU into scene buffers that grew, in order.
    pub grown_bytes_each: Vec<u64>,
    /// CPU time of the part of each Edit that is the scene thread's: the coarse levels remade and
    /// the changed bricks packed. Not held to a budget.
    pub scene_thread: Stats,
    /// CPU time of every frame, the ones with no Edit at 0, against the `edits` budget.
    pub per_frame: Entry,
    /// The updates of the hardware backend's acceleration structures that the Edits made (the
    /// groups they touched, and the TLAS again), which are in the times above: null on compute.
    pub accel: Option<AccelSummary>,
}

impl EditsReport {
    /// The two entries that are held to budgets.
    pub fn entries(&self) -> [&Entry; 2] {
        [&self.per_frame, &self.frame_thread]
    }

    /// Whether either is over, or could not be checked.
    pub fn statuses(&self) -> [super::budget::Status; 2] {
        self.entries().map(|e| e.status)
    }
}

/// The time of `Offscreen::update_scene` (`write`, which has the hardware's update in it) and of the
/// hardware's updates (`prepare` + `build`, which has their `wait` in it) in the parts the stress
/// reports: the scene's own writes (what is left of `write` once the parts that have a part of their
/// own are out of it: `grow`, the buffers that grew, and `named`, the submit and the emitter index),
/// the hardware's update less its wait, and its wait.
fn split_update(write: f64, grow: f64, named: f64, accel_total: f64, wait: f64) -> (f64, f64, f64) {
    let accel = (accel_total - wait).max(0.0);
    ((write - accel_total - grow - named).max(0.0), accel, wait)
}

/// The mode of Edit number `edit` (from 0): `first`, then the other, in turn.
fn mode_of(edit: u32, first: EditMode) -> EditMode {
    match (edit.is_multiple_of(2), first) {
        (true, mode) => mode,
        (false, EditMode::Dig) => EditMode::Place,
        (false, _) => EditMode::Dig,
    }
}

/// Runs the stress on the flight's Planet, or returns `None` for a scene that has no store to
/// Edit (`--short`'s patch).
pub fn run(
    gpu: &mut Offscreen,
    flight: &mut Flight,
    budgets: &Budgets,
    first: EditMode,
    frames: u32,
) -> Result<Option<EditsReport>, Box<dyn Error>> {
    let Flight { scene, store, .. } = flight;
    let Some(store) = store.as_mut() else {
        return Ok(None);
    };
    let radius = sim::MAX_BRUSH_RADIUS;
    // On the ground at the +y pole, where the sweep looks.
    let centre = VoxelCoord([0, planet_gen::PLANET_RADIUS, 0]);
    let op = |mode| EditOp {
        mode,
        brush: Brush {
            shape: Shape::Cube,
            radius,
        },
        centre,
        material: planet_gen::ROCK,
        alignment: Alignment::World,
        rotation: 0,
    };
    let side = 2 * u64::from(radius) + 1;
    let mut allowance = sim::EditBudget::full();
    let (mut per_frame, mut per_edit) = (Vec::new(), Vec::new());
    let (mut frame_thread, mut scene_thread) = (Vec::new(), Vec::new());
    let mut parts: [Vec<f64>; 9] = Default::default();
    let mut grown: Vec<u64> = Vec::new();
    let mut accel_log = Vec::new();
    let (mut edits, mut voxels) = (0u32, 0u64);
    // What the flight did to the acceleration structures is not the Edits'.
    gpu.take_hardware_updates();
    // An update of the hardware backend's structures does not wait for the GPU, so that a frame
    // never does; here it waits, so that the time of an Edit has the builds' own in it (#236). The
    // wait is for everything the queue holds, the previous frame's passes and the scene's own
    // writes too: the Edit+BVH figure is an upper bound on the builds' cost, not their exact one.
    gpu.set_hardware_wait(true);
    for _ in 0..frames {
        allowance.refill();
        let next = op(mode_of(edits, first));
        let mut took = 0.0;
        // Affordable by the whole cube, which is more than the Edit changes.
        if allowance.has_op()
            && allowance.has_work(sim::work(&next))
            && allowance.has_volume(side.pow(3))
        {
            let began = Instant::now();
            // The frame thread: the Edit planned, and level 0 written.
            let plan = sim::plan_bricks(store, &next);
            let changed: Vec<BrickCoord> = {
                let undo = sim::write_plan_from(store, &plan, None, None);
                undo.bricks().collect()
            };
            let written = began.elapsed();
            // The scene thread: the coarse levels, which the predictor leaves to it, and the
            // packing.
            store.refresh_lod(changed.iter().copied());
            let update = scene.update(store, &changed)?;
            let packed = began.elapsed();
            // The frame thread again: the writes to the GPU.
            gpu.update_scene(scene, &update)?;
            let updated = began.elapsed();
            gpu.edit_made(&changed);
            let done = began.elapsed();
            let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
            let grow_ms = ms(gpu.last_grow_time());
            grown.push(gpu.last_grown_bytes());
            let hw = gpu.take_hardware_updates();
            let (scene_write_ms, accel_ms, wait_ms) = split_update(
                ms(gpu.last_update_times().write),
                grow_ms,
                ms(gpu.last_update_times().submit) + ms(gpu.last_update_times().emitters),
                hw.iter().map(|u| ms(u.prepare + u.build)).sum(),
                hw.iter().map(|u| ms(u.phases.wait)).sum(),
            );
            let times = gpu.last_update_times();
            parts[0].push(ms(written));
            parts[1].push(scene_write_ms);
            parts[2].push(ms(times.flush));
            parts[3].push(accel_ms);
            parts[4].push(ms(done - updated));
            parts[5].push(wait_ms);
            parts[6].push(grow_ms);
            parts[7].push(ms(times.submit));
            parts[8].push(ms(times.emitters));
            accel_log.extend(hw);
            took = done.as_secs_f64() * 1e3;
            frame_thread.push(((done - packed) + written).as_secs_f64() * 1e3);
            scene_thread.push((packed - written).as_secs_f64() * 1e3);
            allowance.take_op();
            allowance.take_work(sim::work(&next));
            allowance.take_volume(plan.len() as u64);
            edits += 1;
            voxels += plan.len() as u64;
            per_edit.push(took);
        }
        per_frame.push(took);
    }
    let seconds = f64::from(frames) / 60.0;
    let stats = Stats::of(&per_frame);
    let budget_ms = budgets.passes.get("edits").copied();
    gpu.set_hardware_wait(false);
    let accel = accel_log;
    let frame_thread_stats = Stats::of(&frame_thread);
    let frame_thread_budget_ms = budgets.passes.get(FRAME_THREAD_BUDGET).copied();
    Ok(Some(EditsReport {
        brush: format!(
            "cube of radius {radius} Voxels ({:.1} m), dig and place in turn",
            f64::from(radius) / VOXELS_PER_METRE
        ),
        frames,
        edits,
        voxels_changed: voxels,
        m3_per_s: voxels as f64 / seconds / VOXELS_PER_METRE.powi(3),
        per_edit: Stats::of(&per_edit),
        frame_thread: Entry {
            name: FRAME_THREAD_BUDGET.to_string(),
            // The longest Edit, not the mean: that is what the budget is for.
            status: status(frame_thread_budget_ms, frame_thread_stats.max_ms),
            stats: frame_thread_stats,
            budget_ms: frame_thread_budget_ms,
        },
        frame_thread_parts: FrameThreadParts {
            edit: Stats::of(&parts[0]),
            scene_write: Stats::of(&parts[1]),
            scene_grow: Stats::of(&parts[6]),
            scene_submit: Stats::of(&parts[7]),
            scene_emitters: Stats::of(&parts[8]),
            flush: Stats::of(&parts[2]),
            accel: Stats::of(&parts[3]),
            accel_wait: Stats::of(&parts[5]),
            edit_made: Stats::of(&parts[4]),
        },
        frame_thread_each: frame_thread.clone(),
        grown_bytes_each: grown,
        scene_thread: Stats::of(&scene_thread),
        per_frame: Entry {
            name: "edits".to_string(),
            status: status(budget_ms, stats.mean_ms),
            stats,
            budget_ms,
        },
        accel: (!accel.is_empty()).then(|| AccelSummary {
            group_regions: gpu.hardware_grouping().describe(),
            ..AccelSummary::of(&accel)
        }),
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_updates_time_is_split_into_the_scenes_writes_the_hardwares_work_and_its_wait() {
        // 5 ms of the call: 1 of the scene's, and 4 of the hardware's, 3 of which were the wait.
        let (scene, accel, wait) = split_update(6.0, 0.5, 0.5, 4.0, 3.0);
        assert_eq!((scene, accel, wait), (1.0, 1.0, 3.0));
        // The three add up to the call.
        assert_eq!(scene + accel + wait + 0.5 + 0.5, 6.0);
        // On compute there is no hardware: it is all the scene's.
        assert_eq!(split_update(0.5, 0.0, 0.0, 0.0, 0.0), (0.5, 0.0, 0.0));
    }

    #[test]
    fn the_edits_alternate_from_the_mode_the_stress_starts_with() {
        let modes = |first| (0..4).map(|n| mode_of(n, first)).collect::<Vec<_>>();
        use EditMode::{Dig, Place};
        assert_eq!(modes(Dig), [Dig, Place, Dig, Place]);
        assert_eq!(modes(Place), [Place, Dig, Place, Dig]);
    }

    use super::*;
    use brick_store::BrickCoord;
    use render::Scene;

    #[test]
    fn the_largest_brush_is_dug_and_placed_in_turn_at_no_more_than_the_metric_rate() {
        // The patch of the test Planet near the +y pole, with the Edit centre on its ground.
        let store = planet_gen::generate_region(
            crate::SEED,
            BrickCoord([-55, 170, -45]),
            BrickCoord([55, 196, 32]),
        );
        let scene = Scene::new(&store, None);
        let mut gpu = Offscreen::new(64, 36, &scene).unwrap();
        let mut flight = Flight {
            scene,
            store: Some(store),
            diag: Default::default(),
            accel: Default::default(),
        };
        let budgets =
            Budgets::parse("[passes]\nedits = 1.0\nedit_frame_thread = 1000.0\n").unwrap();
        let report = run(&mut gpu, &mut flight, &budgets, EditMode::Dig, FRAMES)
            .unwrap()
            .unwrap();
        // The first from the burst and the rest as the allowance comes: about ten in 30 s.
        assert!(report.edits >= 4, "{report:?}");
        assert_eq!(report.per_edit.samples, report.edits as usize);
        // The parts of each Edit are timed apart, and the frame thread's is no more than all of it.
        assert_eq!(report.frame_thread.stats.samples, report.edits as usize);
        assert_eq!(report.scene_thread.samples, report.edits as usize);
        assert_eq!(report.frame_thread.name, "edit_frame_thread");
        assert!(report.frame_thread.stats.max_ms <= report.per_edit.max_ms);
        // Each Edit's frame-thread time is kept, and its parts (one sample each) are no more than it:
        // the parts are the clock's reads between, so they add up to it but for the reads.
        assert_eq!(report.frame_thread_each.len(), report.edits as usize);
        // No Edit needs a scene buffer that is bigger: a growth is a new buffer and a copy, which
        // the first Edit of a context that has not been where the others were would pay.
        assert_eq!(report.grown_bytes_each.len(), report.edits as usize);
        assert!(
            report.grown_bytes_each.iter().all(|&b| b == 0),
            "{report:?}"
        );
        let parts = &report.frame_thread_parts;
        for part in [
            &parts.edit,
            &parts.scene_write,
            &parts.scene_grow,
            &parts.scene_submit,
            &parts.scene_emitters,
            &parts.flush,
            &parts.edit_made,
        ] {
            assert_eq!(part.samples, report.edits as usize);
        }
        let mean = |s: &Stats| s.mean_ms.unwrap_or(0.0);
        let sum = [
            &parts.edit,
            &parts.scene_write,
            &parts.scene_grow,
            &parts.scene_submit,
            &parts.scene_emitters,
            &parts.flush,
            &parts.accel,
            &parts.accel_wait,
            &parts.edit_made,
        ]
        .into_iter()
        .map(mean)
        .sum::<f64>();
        let whole = mean(&report.frame_thread.stats);
        assert!(sum <= whole * 1.05 + 0.05, "{sum} of {whole}: {parts:?}");
        // At the server's rate, or under it: 8 m3 a second, and the burst's 9 s on top of 30.
        let most = 8.0 * (1.0 + 9.0 / 30.0);
        assert!(
            report.m3_per_s > 1.0 && report.m3_per_s <= most,
            "{report:?}"
        );
        assert_eq!(report.per_frame.stats.samples, FRAMES as usize);
        assert_eq!(report.per_frame.budget_ms, Some(1.0));
        assert_eq!(report.frame_thread.budget_ms, Some(1000.0));
        assert_eq!(
            report.frame_thread.status,
            super::super::budget::Status::Within
        );
        // Without a store there is nothing to Edit.
        let scene = Scene::new(
            &planet_gen::generate_region(1, BrickCoord([0; 3]), BrickCoord([0; 3])),
            None,
        );
        let mut bare = Flight {
            scene,
            store: None,
            diag: Default::default(),
            accel: Default::default(),
        };
        assert!(
            run(&mut gpu, &mut bare, &budgets, EditMode::Dig, FRAMES)
                .unwrap()
                .is_none()
        );
    }
}
