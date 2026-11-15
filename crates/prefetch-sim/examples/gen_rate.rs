//! Measures what the loader's job costs on one core:
//! `cargo run --release -p prefetch-sim --example gen_rate`.
//!
//! A job makes the eight children of a split brick, air and buried ones included, with whether
//! each is solid all the way down (`brick_store::children_with_solid`, which is what a loader
//! worker calls). The jobs timed are the parents of bricks that the band rule draws in a full
//! fill of the real Planet (the same traversal as the simulator's): a uniform sample of them at
//! each level 0 to 7, from the whole Planet, so the mix is the fill's (ground, ocean, caves,
//! strata, the uniform deep bands), moved to the real ground. One thread, one level after
//! another.

use std::time::Instant;

use brick_store::{BrickCoord, children_with_solid};
use planet_gen::planet::{Planet, PlanetSource};
use planet_gen::world_map::WorldMap;
use prefetch_sim::demand::{Point, traverse};
use prefetch_sim::model::{EYE, Model, SIM_RADIUS, scale, unit};

const SAMPLE: usize = 1500;

fn main() {
    let started = Instant::now();
    let planet = Planet::new(WorldMap::generate(0xB4A7_A9AC, 2048));
    println!("world map in {:.1?}", started.elapsed());
    let source = PlanetSource::new(planet.clone());
    let model = Model::real();
    let point = Point {
        pos: scale(unit([0.36, 0.48, 0.8]), model.planet_radius + EYE),
        reach: f64::INFINITY,
    };
    let mut picks: Vec<Vec<BrickCoord>> = vec![Vec::new(); 8];
    let mut seen = [0u64; 8];
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    traverse(&model, &[point], |level, coord, _| {
        if level < 8 {
            // Reservoir sampling.
            seen[level] += 1;
            if picks[level].len() < SAMPLE {
                picks[level].push(coord);
            } else {
                rng = rng
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let at = (rng >> 33) % seen[level];
                if (at as usize) < SAMPLE {
                    picks[level][at as usize] = coord;
                }
            }
        }
    });
    let (mut total_us, mut total_bricks) = (0.0, 0.0);
    for level in 0..8 {
        let jobs: Vec<BrickCoord> = picks[level]
            .iter()
            .map(|coord| {
                // The model's Planet is a smooth sphere of 320 km at sea level: move the brick
                // radially to the real Planet's ground there (320 km, `GENERATOR_VERSION` 5: the
                // ground the detail is on too), so that the jobs are over ground and not open
                // sea.
                let size = (8u64 << level) as f64;
                let centre = coord.0.map(|c| (f64::from(c) + 0.5) * size);
                let r = centre.iter().map(|c| c * c).sum::<f64>().sqrt();
                let direction = centre.map(|c| c / r);
                let ground = planet.sea_radius() + planet.ground_height(direction);
                BrickCoord(direction.map(|d| (d * ground / size).floor() as i32)).parent()
            })
            .collect();
        let t = Instant::now();
        let mut generated = 0usize;
        for parent in jobs {
            generated += children_with_solid(&source, level + 1, parent, |_| false).len();
        }
        let us = t.elapsed().as_secs_f64() * 1.0e6 / generated as f64;
        let weight = seen[level] as f64;
        total_us += us * weight;
        total_bricks += weight;
        println!(
            "level {level}: {us:6.2} us per generated brick ({} jobs of {} drawn bricks)",
            picks[level].len(),
            seen[level]
        );
    }
    contiguous(&planet, &source);
    println!(
        "blended over the drawn bricks of levels 0 to 7: {:.2} us per generated brick",
        total_us / total_bricks
    );
}

/// The jobs of the ground around the spawn, one after another and in order, as a camera there
/// would ask for them (warm caches, and what a flight generates), all of them within 150 m.
fn contiguous(planet: &Planet, source: &PlanetSource) {
    let spawn = planet.spawn(100);
    let radius = spawn.iter().map(|c| c * c).sum::<f64>().sqrt();
    // The model's sphere is the real Planet's 320 km, and a key holds it: the jobs are found over
    // it, at the spawn's direction, and moved as a whole, a level's bricks by the same number of
    // bricks, to the spawn's ground (the ground is flat to 14 Voxels in 150 m, which is under two bricks).
    let model = Model::real();
    let up = unit(spawn);
    let point = Point {
        pos: scale(up, f64::from(SIM_RADIUS) + EYE),
        reach: 1500.0,
    };
    let shift = std::array::from_fn::<f64, 3, _>(|a| spawn[a] - point.pos[a]);
    let _ = radius;
    let mut jobs = Vec::new();
    traverse(&model, &[point], |level, coord, _| {
        if level < 8 {
            let size = (8u64 << (level + 1)) as f64;
            let moved = BrickCoord(std::array::from_fn(|a| {
                coord.parent().0[a] + (shift[a] / size).round() as i32
            }));
            jobs.push((level + 1, moved));
        }
    });
    jobs.sort();
    jobs.dedup();
    let t = Instant::now();
    let mut generated = 0usize;
    for (level, parent) in &jobs {
        generated += children_with_solid(source, *level, *parent, |_| false).len();
    }
    println!(
        "contiguous jobs within 150 m of the spawn: {} jobs, {:.2} us per generated brick",
        jobs.len(),
        t.elapsed().as_secs_f64() * 1.0e6 / generated as f64
    );
}
