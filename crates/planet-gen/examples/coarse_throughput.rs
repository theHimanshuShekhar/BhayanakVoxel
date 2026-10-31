//! Measures how long `Planet::coarse_brick` takes to generate every coarse brick of the whole
//! Planet, level by level from the top, on all cores: the target is the whole Planet's coarse
//! levels within about 5 seconds of zooming out to orbit.
//!
//! `cargo run --release -p planet-gen --example coarse_throughput -- [LOWEST] [SIZE] [SEED]`
//!
//! LOWEST is the finest level timed (default 8), SIZE the texels along a face of the world map
//! (default 2048, the real Planet). Every brick of the box that holds the Planet is generated,
//! the sky and the interior included, as the renderer's rings read them; a level takes about
//! four times as long as the one above it, from the ground, and eight times for the box.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use brick_store::{BRICK_SIZE, BrickCoord};
use planet_gen::planet::Planet;
use planet_gen::world_map::{PEAK, WORLD_RADIUS, WorldMap};

/// The coarsest level worth generating: one Voxel there is wider than the Planet.
const TOP: usize = 19;

fn main() {
    let mut args = std::env::args().skip(1);
    let lowest: usize = args.next().map_or(8, |s| s.parse().expect("LOWEST"));
    let size = args.next().map_or(2048, |s| s.parse().expect("SIZE"));
    let seed = args
        .next()
        .map_or(0xB4A7_A9AC, |s| s.parse().expect("SEED"));
    let started = Instant::now();
    let planet = Planet::new(WorldMap::generate(seed, size));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    println!(
        "world map of {size} texels generated in {:.1?}; {threads} threads",
        started.elapsed()
    );
    println!("level  bricks in the box   not air      mixed       time   from the top");
    let mut total = Duration::ZERO;
    for level in (lowest..=TOP).rev() {
        // Bricks from -extent to extent - 1 along each axis hold every Voxel within the Planet.
        let width = i64::from(BRICK_SIZE) << level;
        let extent = i32::try_from((i64::from(WORLD_RADIUS + PEAK) + width - 1) / width).unwrap();
        let (not_air, mixed) = (AtomicU64::new(0), AtomicU64::new(0));
        let next_x = AtomicI32::new(-extent);
        let started = Instant::now();
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    loop {
                        let x = next_x.fetch_add(1, Ordering::Relaxed);
                        if x >= extent {
                            return;
                        }
                        let (mut some, mut many) = (0, 0);
                        for y in -extent..extent {
                            for z in -extent..extent {
                                let brick = planet.coarse_brick(level, BrickCoord([x, y, z]));
                                some += u64::from(!brick.is_air());
                                many += u64::from(!brick.is_uniform());
                            }
                        }
                        not_air.fetch_add(some, Ordering::Relaxed);
                        mixed.fetch_add(many, Ordering::Relaxed);
                    }
                });
            }
        });
        let took = started.elapsed();
        total += took;
        println!(
            "{level:>5}  {:>17}  {:>8}  {:>9}  {took:>9.2?}  {total:>9.2?}",
            (2 * u64::from(extent.unsigned_abs())).pow(3),
            not_air.into_inner(),
            mixed.into_inner(),
        );
    }
}
