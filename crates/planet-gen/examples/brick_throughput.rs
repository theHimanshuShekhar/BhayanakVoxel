//! Measures how fast `Planet::brick` generates bricks, on one core, for each kind of brick, so
//! that streaming (#49) can be sized.
//!
//! `cargo run --release -p planet-gen --example brick_throughput -- [SIZE] [SEED]`
//!
//! SIZE is the texels along a face of the world map (default 2048, the real Planet). The bricks
//! timed are those of a block around a spawn on gentle land, 512 Voxels across, from 528 Voxels
//! (all the strata) below the spawn to 40 above; the interior is timed on a line of bricks from
//! the centre out. The kinds are what `Brick::AIR` or `Brick::solid` or a
//! mixed brick come out as, so the time of a kind includes working out that it is that kind.

use std::time::{Duration, Instant};

use brick_store::{BRICK_SIZE, Brick, BrickCoord};
use planet_gen::planet::{INTERIOR_RADIUS, Planet};
use planet_gen::world_map::{WORLD_RADIUS, WorldMap};

#[derive(Default)]
struct Tally {
    bricks: u64,
    time: Duration,
}

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
    let spawn = planet.spawn(640);
    let length = spawn.iter().map(|c| c * c).sum::<f64>().sqrt();
    println!(
        "spawn at {spawn:?}, {:.0} Voxels out",
        length - f64::from(WORLD_RADIUS)
    );

    // A block of bricks around the spawn, aligned to the axes: 64 across, 72 high along the axis
    // most like the spawn's up.
    let up = spawn.map(|c| c / length);
    let axis = (0..3)
        .max_by(|&a, &b| up[a].abs().total_cmp(&up[b].abs()))
        .unwrap();
    let centre = spawn.map(|c| (c / f64::from(BRICK_SIZE)).floor() as i32);
    let mut tallies: [(&str, Tally); 3] = [
        ("air", Tally::default()),
        ("uniform (strata, deep rock, water)", Tally::default()),
        ("mixed (ground, boundaries)", Tally::default()),
    ];
    for dz in -66..=5 {
        for dx in -32..32 {
            for dy in -32..32 {
                let mut c = centre;
                let sign = if up[axis] < 0.0 { -1 } else { 1 };
                let (across_a, across_b) = ((axis + 1) % 3, (axis + 2) % 3);
                c[axis] += sign * dz;
                c[across_a] += dx;
                c[across_b] += dy;
                time(&planet, BrickCoord(c), &mut tallies);
            }
        }
    }
    // Along a line from the centre out through the interior, to the ground.
    let line = (0..INTERIOR_RADIUS / BRICK_SIZE).step_by(7);
    let mut interior = Tally::default();
    for r in line {
        let coord = BrickCoord(up.map(|u| (u * f64::from(r)).floor() as i32));
        let t = Instant::now();
        std::hint::black_box(planet.brick(coord));
        interior.time += t.elapsed();
        interior.bricks += 1;
    }
    for (name, t) in tallies
        .iter()
        .map(|(n, t)| (*n, t))
        .chain([("interior (by distance)", &interior)])
    {
        let rate = t.bricks as f64 / t.time.as_secs_f64();
        println!(
            "{name:<38} {:>9} bricks  {:>10.0} bricks/s/core  {:>8.2} us each",
            t.bricks,
            rate,
            1e6 / rate
        );
    }
}

fn time(planet: &Planet, coord: BrickCoord, tallies: &mut [(&str, Tally); 3]) {
    let t = Instant::now();
    let brick: Brick = planet.brick(coord);
    let elapsed = t.elapsed();
    let kind = if brick.is_air() {
        0
    } else if brick.is_uniform() {
        1
    } else {
        2
    };
    tallies[kind].1.bricks += 1;
    tallies[kind].1.time += elapsed;
}
