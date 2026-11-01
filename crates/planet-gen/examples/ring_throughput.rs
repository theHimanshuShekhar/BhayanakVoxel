//! Times what the client does at start-up that is the generator's: the spawn ring
//! (`Planet::crust_around`, level 0 on all cores) and the Guard (`Planet::guard`, levels 8 to 12
//! of the whole Planet), and then level 0 bricks on one core under the spawn and under a cave's
//! mouth, which is what the loader and a character's Ground (#106) make: first at the ring's
//! depth, then deeper, in the cave band. The spawn is clear of caves; the ring is timed at the
//! mouth as well, as the worst place for it to be.
//!
//! `cargo run --release -p planet-gen --example ring_throughput -- [REACH] [SIZE] [SEED]`
//!
//! REACH is the ring's radius in Voxels (default 400, the client's), SIZE the texels along a
//! face of the world map (default 2048, the real Planet).

use std::time::Instant;

use brick_store::{BRICK_SIZE, BrickCoord};
use planet_gen::planet::{Planet, tangents};
use planet_gen::world_map::{WORLD_RADIUS, WorldMap};

fn main() {
    let mut args = std::env::args().skip(1);
    let reach: i32 = args.next().map_or(400, |s| s.parse().expect("REACH"));
    let size = args.next().map_or(2048, |s| s.parse().expect("SIZE"));
    let seed = args
        .next()
        .map_or(0xB4A7_A9AC, |s| s.parse().expect("SEED"));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let started = Instant::now();
    let map = WorldMap::generate(seed, size);
    println!(
        "world map of {size} texels made in {:.1?}; {threads} threads",
        started.elapsed()
    );
    let started = Instant::now();
    let planet = Planet::new(map);
    println!("the Planet (its caves) made in {:.1?}", started.elapsed());
    let spawn = planet.spawn(reach);
    println!(
        "spawn at {spawn:?}, {:.0} Voxels out",
        spawn.iter().map(|c| c * c).sum::<f64>().sqrt() - f64::from(WORLD_RADIUS)
    );

    let started = Instant::now();
    let ring = planet.crust_around(spawn, reach, 12);
    println!(
        "spawn ring ({} m): {} bricks in {:.2?}",
        reach / 10,
        ring.len(),
        started.elapsed()
    );

    let mouths = planet.cave_mouths();
    println!("{} cave mouths", mouths.len());
    let mouth = mouths[mouths.len() / 2];
    let started = Instant::now();
    let ring = planet.crust_around(mouth, reach, 12);
    println!(
        "a ring at a cave mouth ({} m): {} bricks in {:.2?}",
        reach / 10,
        ring.len(),
        started.elapsed()
    );
    drop(ring);

    let started = Instant::now();
    let guard = planet.guard(8..=12);
    println!(
        "Guard (levels 8 to 12): {} bricks in {:.2?}",
        guard.len(),
        started.elapsed()
    );

    // Level 0 bricks, on one core, in columns under a point on the ground, 64 voxels apart: under
    // the spawn, which is clear of caves, and under a cave's mouth, in the ring's depth and then
    // deeper, in the cave band.
    for (place, ground) in [("spawn", spawn), ("a cave mouth", mouth)] {
        for (name, depths, across) in [
            ("the ring's depth (0 to 53 m)", 0..66, 24),
            ("the cave band (53 to 300 m)", 66..375, 6),
        ] {
            let (made, took) = time_bricks(&planet, ground, depths, across);
            println!(
                "level 0 at {place}, {name}: {made} bricks, {:.0} bricks/s on one core, {:.1} us each",
                made as f64 / took,
                took * 1e6 / made as f64
            );
        }
    }
}

/// Makes the bricks of columns under `ground`, `across` of them either way along the ground,
/// each from `depths` bricks down, and says how many, and how long it took in seconds.
fn time_bricks(
    planet: &Planet,
    ground: [f64; 3],
    depths: std::ops::Range<i32>,
    across: i32,
) -> (u64, f64) {
    let length = ground.iter().map(|c| c * c).sum::<f64>().sqrt();
    let up = ground.map(|c| c / length);
    let (ahead, aside) = tangents(up);
    let mut made = 0u64;
    let started = Instant::now();
    for d in depths {
        for s in -across..=across {
            for t in -across..=across {
                let at: [f64; 3] = std::array::from_fn(|a| {
                    ground[a] - up[a] * f64::from(d * BRICK_SIZE)
                        + ahead[a] * f64::from(s * BRICK_SIZE * 8)
                        + aside[a] * f64::from(t * BRICK_SIZE * 8)
                });
                let coord = BrickCoord(at.map(|c| (c / f64::from(BRICK_SIZE)).floor() as i32));
                std::hint::black_box(planet.brick(coord));
                made += 1;
            }
        }
    }
    (made, started.elapsed().as_secs_f64())
}
