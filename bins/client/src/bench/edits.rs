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
use super::stats::Stats;

/// The name of the budget (in `[passes]`) that the longest frame-thread time of an Edit is held to.
const FRAME_THREAD_BUDGET: &str = "edit_frame_thread";

/// Frames of the stress, at 60 a second: 30 s, which is about ten Edits of the largest Brush.
const FRAMES: u32 = 1800;

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
    /// CPU time of the part of each Edit that is the scene thread's: the coarse levels remade and
    /// the changed bricks packed. Not held to a budget.
    pub scene_thread: Stats,
    /// CPU time of every frame, the ones with no Edit at 0, against the `edits` budget.
    pub per_frame: Entry,
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

/// Runs the stress on the flight's Planet, or returns `None` for a scene that has no store to
/// Edit (`--short`'s patch).
pub fn run(
    gpu: &mut Offscreen,
    flight: &mut Flight,
    budgets: &Budgets,
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
    let (mut edits, mut voxels) = (0u32, 0u64);
    for _ in 0..FRAMES {
        allowance.refill();
        let next = op(if edits % 2 == 0 {
            EditMode::Dig
        } else {
            EditMode::Place
        });
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
            let done = began.elapsed();
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
    let seconds = f64::from(FRAMES) / 60.0;
    let stats = Stats::of(&per_frame);
    let budget_ms = budgets.passes.get("edits").copied();
    let frame_thread_stats = Stats::of(&frame_thread);
    let frame_thread_budget_ms = budgets.passes.get(FRAME_THREAD_BUDGET).copied();
    Ok(Some(EditsReport {
        brush: format!(
            "cube of radius {radius} Voxels ({:.1} m), dig and place in turn",
            f64::from(radius) / VOXELS_PER_METRE
        ),
        frames: FRAMES,
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
        scene_thread: Stats::of(&scene_thread),
        per_frame: Entry {
            name: "edits".to_string(),
            status: status(budget_ms, stats.mean_ms),
            stats,
            budget_ms,
        },
    }))
}

#[cfg(test)]
mod tests {
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
        };
        let budgets =
            Budgets::parse("[passes]\nedits = 1.0\nedit_frame_thread = 1000.0\n").unwrap();
        let report = run(&mut gpu, &mut flight, &budgets).unwrap().unwrap();
        // The first from the burst and the rest as the allowance comes: about ten in 30 s.
        assert!(report.edits >= 4, "{report:?}");
        assert_eq!(report.per_edit.samples, report.edits as usize);
        // The parts of each Edit are timed apart, and the frame thread's is no more than all of it.
        assert_eq!(report.frame_thread.stats.samples, report.edits as usize);
        assert_eq!(report.scene_thread.samples, report.edits as usize);
        assert_eq!(report.frame_thread.name, "edit_frame_thread");
        assert!(report.frame_thread.stats.max_ms <= report.per_edit.max_ms);
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
        };
        assert!(run(&mut gpu, &mut bare, &budgets).unwrap().is_none());
    }
}
