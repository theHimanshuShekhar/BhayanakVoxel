//! Builds the real Planet's world map and prints how long it took and its content hash: the
//! quick check that a change to the build is faster and the same (#264).
//!
//! `cargo run --release -p planet-gen --example map_build -- [SIZE]`
//!
//! SIZE is the texels a face (default 2048, the real Planet).

use std::time::Instant;

use planet_gen::world_map::{WORLD_RADIUS, WorldMap};

const SEED: u64 = 0xB4A7_A9AC;

fn main() {
    let size: u32 = std::env::args()
        .nth(1)
        .map_or(2048, |s| s.parse().expect("SIZE"));
    let started = Instant::now();
    let (map, stages) = WorldMap::with_threads(SEED, size, WORLD_RADIUS, machine::threads());
    let took = started.elapsed();
    println!("stages on {} threads: {stages:.2?}", stages.threads);
    let hashed = Instant::now();
    let hash = map.content_hash();
    println!(
        "world map of {size} texels a face: built in {took:.2?}, hash {hash:#018x} (hashed in {:.2?})",
        hashed.elapsed()
    );
}
