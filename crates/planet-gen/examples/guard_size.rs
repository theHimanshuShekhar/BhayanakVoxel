//! Measures the Guard (ADR 0004, ADR 0008): the bricks of the whole Planet from a first level to
//! the top of the pyramid, as `Planet::guard` makes them, for each first level asked: how many
//! bricks, how many bytes they hold, and how long they take on all the cores. The answer to "which
//! level should `GUARD_FIRST` be at this radius": the first whose Guard is about 74 MiB and
//! about 5 s (ADR 0008).
//!
//! `cargo run --release -p planet-gen --example guard_size -- [RADIUS_M] [TOP] [FIRSTS] [SIZE]`
//!
//! RADIUS_M is the Planet's radius in metres (default 320000), TOP the top level of the pyramid
//! (default 17), FIRSTS the first levels to try, separated by commas (default 9..=TOP), and SIZE
//! the world map's texels a face (default 2048, the real Planet; a smaller map is faster and
//! differs from the real Planet's coarse bricks only in detail). Run it with `uptime` beside it:
//! the time is wall time and counts other processes' load.

use std::time::Instant;

use brick_store::VOXELS_PER_METRE;
use planet_gen::planet::Planet;
use planet_gen::world_map::WorldMap;

const SEED: u64 = 0xB4A7_A9AC;

fn main() {
    let mut args = std::env::args().skip(1);
    let radius_m: f64 = args
        .next()
        .map_or(320_000.0, |s| s.parse().expect("RADIUS_M"));
    let top: usize = args.next().map_or(17, |s| s.parse().expect("TOP"));
    let firsts: Vec<usize> = match args.next() {
        Some(list) => list
            .split(',')
            .map(|s| s.parse().expect("FIRSTS"))
            .collect(),
        None => (9..=top).collect(),
    };
    let size: u32 = args.next().map_or(2048, |s| s.parse().expect("SIZE"));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    println!(
        "{threads} threads, load average at the start: {}",
        load.trim()
    );
    let started = Instant::now();
    let map = WorldMap::with_radius(SEED, size, (radius_m * VOXELS_PER_METRE) as i32);
    let planet = Planet::new(map);
    println!(
        "world map of {size} texels a face, {radius_m} m radius, made in {:.1?}",
        started.elapsed()
    );
    println!(
        "first  levels  bricks      mixed       payload MiB  with handles MiB  seconds  load after"
    );
    for first in firsts {
        let started = Instant::now();
        let guard = planet.guard(first..=top);
        let took = started.elapsed();
        let bricks = guard.len();
        let mixed = guard.iter().filter(|g| !g.2.is_uniform()).count();
        let payload: usize = guard.iter().map(|g| g.2.storage_bytes()).sum();
        let handles = bricks * size_of::<brick_store::Brick>();
        let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
        let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        println!(
            "{first:>5}  {first:>2}..={top:<3}  {bricks:>9}  {mixed:>9}  {:>11.1}  {:>16.1}  {:>7.2}  {}",
            mib(payload),
            mib(payload + handles),
            took.as_secs_f64(),
            load.split(' ').next().unwrap_or("?"),
        );
    }
}
