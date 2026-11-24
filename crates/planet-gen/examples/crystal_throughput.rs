//! Measures how fast `Planet::brick` generates the bricks of the crystal country (#204), on one
//! core: the place where the clusters of crystals are, and the only place where they cost
//! anything. It uses nothing that a generator without clusters lacks but `Planet::sunlit_at`,
//! which the bench of the one without has added (the same rule), so the same file runs on both,
//! which is how the cost of the clusters is measured.
//!
//! `cargo run --release -p planet-gen --example crystal_throughput -- [SIZE] [SEED]`
//!
//! SIZE is the texels along a face of the world map (default 2048, the real Planet). The bricks
//! timed are those of 24 blocks, each at a texel of land in the world map's crystal country that
//! is a sunlit slope (the rule of the ground clusters), 64 bricks (25.6 m) across, from 40 bricks
//! (16 m, which holds the caves of the near ground) below its ground to 8 (3.2 m: the tallest
//! cluster is 1.6 m) above. The kinds are as in `brick_throughput`, and bricks with crystal in them
//! (a Material of id 56 to 63) are timed apart: there are none without clusters.

use std::time::{Duration, Instant};

use brick_store::{BRICK_SIZE, Brick, BrickCoord};
use planet_gen::planet::Planet;
use planet_gen::world_map::{Regions, WorldMap};

#[derive(Default)]
struct Tally {
    bricks: u64,
    time: Duration,
}

const BLOCKS: usize = 24;

fn main() {
    let mut args = std::env::args().skip(1);
    let size = args.next().map_or(2048, |s| s.parse().expect("SIZE"));
    let seed = args
        .next()
        .map_or(0xB4A7_A9AC, |s| s.parse().expect("SEED"));
    let started = Instant::now();
    let planet = Planet::new(WorldMap::generate(seed, size));
    println!(
        "world map of {size} texels generated in {:.1?}",
        started.elapsed()
    );
    let map = planet.map();
    let mut country = Vec::new();
    for face in 0..6 {
        for j in 0..map.size() {
            for i in 0..map.size() {
                let t = map.texel(face, i, j);
                if t.elevation >= 20 && t.regions.contains(Regions::CRYSTAL_CAVES) {
                    let d = map.texel_direction(face, i, j);
                    if planet.sunlit_at(d).is_lit() {
                        country.push(d);
                    }
                }
            }
        }
    }
    println!("{} sunlit texels of crystal country on land", country.len());
    let mut tallies: Vec<(&str, Tally)> = vec![
        ("air", Tally::default()),
        ("uniform (strata, deep rock, water)", Tally::default()),
        ("mixed (ground, caves), no crystal", Tally::default()),
        ("with crystal", Tally::default()),
    ];
    let mut total = Tally::default();
    for d in country
        .iter()
        .step_by((country.len() / BLOCKS).max(1))
        .take(BLOCKS)
    {
        let n = d.iter().map(|c| c * c).sum::<f64>().sqrt();
        let up = d.map(|c| c / n);
        let ground = planet.sea_radius() + planet.ground_height(up);
        let axis = (0..3)
            .max_by(|&a, &b| up[a].abs().total_cmp(&up[b].abs()))
            .unwrap();
        let sign = if up[axis] < 0.0 { -1 } else { 1 };
        let centre = up.map(|u| (u * ground / f64::from(BRICK_SIZE)).floor() as i32);
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        for dz in -40..=8 {
            for dx in -32..32 {
                for dy in -32..32 {
                    let mut c = centre;
                    c[axis] += sign * dz;
                    c[a] += dx;
                    c[b] += dy;
                    let coord = BrickCoord(c);
                    let t = Instant::now();
                    let brick = std::hint::black_box(planet.brick(coord));
                    let spent = t.elapsed();
                    let slot = match (brick.is_air(), brick.is_uniform()) {
                        (true, _) => 0,
                        (false, true) => 1,
                        (false, false) if has_crystal(&brick) => 3,
                        (false, false) => 2,
                    };
                    tallies[slot].1.bricks += 1;
                    tallies[slot].1.time += spent;
                    total.bricks += 1;
                    total.time += spent;
                }
            }
        }
    }
    tallies.push(("all", total));
    for (name, t) in &tallies {
        if t.bricks == 0 {
            println!("{name:<40} none");
            continue;
        }
        let each = t.time.as_secs_f64() / t.bricks as f64;
        println!(
            "{name:<40} {:>7} bricks {:>9.0} bricks/s/core {:>8.2} us each",
            t.bricks,
            1.0 / each,
            each * 1e6
        );
    }
}

/// Whether a brick has a crystal Material (ids 56 to 63 of `materials.toml`) in it.
fn has_crystal(brick: &Brick) -> bool {
    (0..512usize).any(|i| {
        brick
            .get([(i % 8) as u8, (i / 8 % 8) as u8, (i / 64) as u8])
            .is_some_and(|m| (56..64).contains(&m.0))
    })
}
