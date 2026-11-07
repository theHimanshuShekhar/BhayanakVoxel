//! The Edit stress of `client bench` (#147): repeated digs and places of the largest Brush, at the
//! rate a player is allowed, over the Planet that the flight was over. It times what an Edit costs
//! the frame (the Voxels changed, the coarse levels remade, the changed words packed and sent to
//! the GPU) and holds the mean over every frame, those without an Edit among them, to the
//! `edits` budget (Edit+BVH, 1 ms).
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
    /// updated, and the writes sent to the GPU.
    pub per_edit: Stats,
    /// CPU time of every frame, the ones with no Edit at 0, against the `edits` budget.
    pub per_frame: Entry,
}

impl EditsReport {
    pub fn status(&self) -> super::budget::Status {
        self.per_frame.status
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
            let changes = sim::plan(store, &next);
            let changed: Vec<BrickCoord> = {
                let undo = sim::apply_changes(store, &changes);
                undo.bricks().collect()
            };
            let update = scene.update(store, &changed)?;
            gpu.update_scene(scene, &update)?;
            took = began.elapsed().as_secs_f64() * 1e3;
            allowance.take_op();
            allowance.take_work(sim::work(&next));
            allowance.take_volume(changes.len() as u64);
            edits += 1;
            voxels += changes.len() as u64;
            per_edit.push(took);
        }
        per_frame.push(took);
    }
    let seconds = f64::from(FRAMES) / 60.0;
    let stats = Stats::of(&per_frame);
    let budget_ms = budgets.passes.get("edits").copied();
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
        let budgets = Budgets::parse("[passes]\nedits = 1.0\n").unwrap();
        let report = run(&mut gpu, &mut flight, &budgets).unwrap().unwrap();
        // The first from the burst and the rest as the allowance comes: about ten in 30 s.
        assert!(report.edits >= 4, "{report:?}");
        assert_eq!(report.per_edit.samples, report.edits as usize);
        // At the server's rate, or under it: 8 m3 a second, and the burst's 9 s on top of 30.
        let most = 8.0 * (1.0 + 9.0 / 30.0);
        assert!(
            report.m3_per_s > 1.0 && report.m3_per_s <= most,
            "{report:?}"
        );
        assert_eq!(report.per_frame.stats.samples, FRAMES as usize);
        assert_eq!(report.per_frame.budget_ms, Some(1.0));
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
