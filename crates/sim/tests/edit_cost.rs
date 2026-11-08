//! What the largest Brushes cost to plan, write and remake the coarse levels for (#156): on the real Planet at its spawn, as the
//! server has it, with and without the coarse levels the client keeps, and on the test Planet near
//! the +y pole where the client's bench Edits. It prints, and holds nothing: a timing is a
//! machine's as much as the code's. Run it in release:
//!
//! `cargo test --release -p sim --test edit_cost -- --ignored --nocapture`

use std::time::Instant;

use brick_store::{BrickCoord, BrickStore, VoxelCoord};
use net::{Alignment, Brush, EditMode, EditOp, Shape};
use planet_gen::planet::{Planet, PlanetSource};
use planet_gen::world_map::WorldMap;
use sim::{BrickCache, Reads};

const SEED: u64 = 0xB4A7_A9AC;

/// A Planet to Edit, where, and how the Brush is aligned there.
type Scene = (BrickStore, VoxelCoord, Alignment);

/// How to make one, and how many coarse levels to build over it.
type Case = (&'static str, fn() -> Scene, usize);

/// The test Planet near the +y pole, held in bricks, as the client's bench has it.
fn pole() -> Scene {
    let store =
        planet_gen::generate_region(SEED, BrickCoord([-30, 170, -30]), BrickCoord([30, 200, 30]));
    let centre = VoxelCoord([0, planet_gen::PLANET_RADIUS, 0]);
    (store, centre, Alignment::World)
}

/// The real Planet at its spawn, nothing held and everything read from the generator, as the
/// server has it.
fn spawn() -> Scene {
    let planet = Planet::new(WorldMap::generate(SEED, 512));
    let at = planet.spawn(100).map(|c| c as i32);
    let mut store = BrickStore::new();
    store.set_source(PlanetSource::new(planet));
    (store, VoxelCoord(at), Alignment::Local)
}

/// The cost of each Edit in turn: milliseconds to plan it, to write level 0, and to remake the
/// coarse levels, and the Voxels changed. Planning and writing are what the client's frame thread
/// does of an Edit, and the server's tick does all three.
fn costs(scene: Scene) -> Vec<(String, [f64; 3], usize)> {
    let (mut store, centre, alignment) = scene;
    let mut cache = BrickCache::new();
    let script = [
        (EditMode::Dig, Shape::Sphere),
        (EditMode::Place, Shape::Cube),
        (EditMode::Dig, Shape::Cube),
        (EditMode::Place, Shape::Cube),
    ];
    let mut out = Vec::new();
    // Three times round, the first to warm the cache and the allocator.
    for round in 0..3 {
        for &(mode, shape) in &script {
            let edit = EditOp {
                mode,
                brush: Brush {
                    shape,
                    radius: sim::MAX_BRUSH_RADIUS,
                },
                centre,
                material: planet_gen::ROCK,
                alignment,
                rotation: 0,
            };
            let began = Instant::now();
            let plan = sim::plan_bricks_from(&mut Reads::new(&store).with_cache(&mut cache), &edit);
            let planned = began.elapsed().as_secs_f64() * 1e3;
            let began = Instant::now();
            let undo = sim::write_plan_from(&mut store, &plan, None, Some(&mut cache));
            let written = began.elapsed().as_secs_f64() * 1e3;
            let began = Instant::now();
            store.refresh_lod(undo.bricks());
            let remade = began.elapsed().as_secs_f64() * 1e3;
            if round > 0 {
                out.push((
                    format!("{mode:?} {shape:?}"),
                    [planned, written, remade],
                    plan.len(),
                ));
            }
        }
    }
    out
}

#[test]
#[ignore = "a measurement"]
fn the_cost_of_the_largest_edits() {
    let scenes: [Case; 3] = [
        ("server at the spawn (no pyramid)", spawn, 0),
        ("client at the spawn (4 coarse levels)", spawn, 4),
        ("client at the pole (4 coarse levels, all held)", pole, 4),
    ];
    for (name, scene, levels) in scenes {
        let mut scene = scene();
        scene.0.build_lod(levels);
        println!("{name}");
        for (edit, [planned, written, remade], changed) in costs(scene) {
            println!(
                "  {edit:14} plan {planned:6.2} ms  write {written:6.2} ms  coarse levels {remade:6.2} ms  {changed} Voxels"
            );
        }
    }
}
