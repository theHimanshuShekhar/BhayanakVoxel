//! PROTOTYPE (#206), throwaway: what a river costs a brick, per method, on one core.
//!
//! `cargo run --release -p planet-gen --example proto_rivers_cost -- d8|mfd [BRICKS] [SIZE]`
//!
//! Builds the world map with the method (`PROTO_MAP_CACHE=DIR` keeps the landform part between
//! runs) and times the same bricks on the same map with the rivers off and on, at level 0 and at
//! coarser levels, for two sets: bricks on the ground of land anywhere (what a typical surface brick
//! costs), and bricks at a river or lake (what the corridor costs). Mean and 99th percentile in
//! microseconds, a second pass after a warm-up.

use std::time::Instant;

use brick_store::{BRICK_SIZE, BrickCoord, VOXELS_PER_METRE};
use planet_gen::planet::Planet;
use planet_gen::rivers_proto::{self, RiverMode};
use planet_gen::world_map::{WORLD_RADIUS, WorldMap};

fn h(n: u64) -> u64 {
    let mut z = n.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn coord_at(point: [f64; 3], level: usize) -> BrickCoord {
    let width = f64::from(BRICK_SIZE) * f64::from(1u32 << level);
    BrickCoord(point.map(|c| (c / width).floor() as i32))
}

fn stats(label: &str, planet: &Planet, level: usize, coords: &[BrickCoord]) -> (f64, f64) {
    let run = |planet: &Planet| -> Vec<f64> {
        coords
            .iter()
            .map(|&c| {
                let t = Instant::now();
                let b = if level == 0 { planet.brick(c) } else { planet.coarse_brick(level, c) };
                std::hint::black_box(&b);
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect()
    };
    run(planet);
    let mut times = run(planet);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    times.sort_by(|a, b| a.total_cmp(b));
    let p99 = times[(times.len() * 99 / 100).min(times.len() - 1)];
    println!("    {label:<4} mean {mean:>8.2} us   p99 {p99:>9.2} us");
    (mean, p99)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = RiverMode::parse(&args.next().expect("d8|mfd")).expect("d8|mfd");
    let count: usize = args.next().map_or(3000, |s| s.parse().unwrap());
    let size: u32 = args.next().map_or(2048, |s| s.parse().unwrap());
    rivers_proto::set_default_mode(mode);
    let t = Instant::now();
    let on = Planet::new(WorldMap::generate(0xB4A7_A9AC, size));
    println!("world map ({mode:?}) in {:.1} s", t.elapsed().as_secs_f64());
    let off = on.clone().with_rivers(RiverMode::Off);
    let on = on.with_rivers(mode);
    let map = on.map();
    let rd = map.rivers().unwrap();
    let n = size as usize;
    let total = 6 * n * n;

    // Land anywhere.
    let mut land = Vec::new();
    let mut k = 0u64;
    while land.len() < count {
        k += 1;
        let idx = (h(k) % total as u64) as usize;
        let (f, i, j) = rivers_proto::texel_index(map, idx);
        if map.texel(f, i, j).elevation <= 40 {
            continue;
        }
        let d = map.texel_direction(f, i, j);
        // A point on the ground: a hash offset within the texel.
        let off_a = (h(k ^ 0xaaaa) % 1000) as f64 / 1000.0 - 0.5;
        let off_b = (h(k ^ 0xbbbb) % 1000) as f64 / 1000.0 - 0.5;
        let step = 245.0 * VOXELS_PER_METRE / f64::from(WORLD_RADIUS);
        let (e, nth) = {
            let r = if d[1].abs() > 0.99 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
            let e = [r[1] * d[2] - r[2] * d[1], r[2] * d[0] - r[0] * d[2], r[0] * d[1] - r[1] * d[0]];
            let l = (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt();
            let e = e.map(|c| c / l);
            (e, [d[1] * e[2] - d[2] * e[1], d[2] * e[0] - d[0] * e[2], d[0] * e[1] - d[1] * e[0]])
        };
        let dir: [f64; 3] = std::array::from_fn(|a| d[a] + e[a] * off_a * step + nth[a] * off_b * step);
        let l = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        let dir = dir.map(|c| c / l);
        let p = dir.map(|c| c * f64::from(WORLD_RADIUS));
        let top = off.ground_height(p);
        land.push(dir.map(|c| c * (f64::from(WORLD_RADIUS) + top)));
    }

    // At rivers and lakes: a river node of 1500+ texels, or a lake texel, a hash offset off its line.
    let mut wet = Vec::new();
    let mut k = 0u64;
    while wet.len() < count {
        k += 1;
        let idx = (h(k ^ 0x1234) % total as u64) as usize;
        let lake = rd.is_lake(idx);
        if !(lake || (rd.is_river(idx) && rd.flow[idx] >= 12 * 10 + 6)) {
            continue;
        }
        let (f, i, j) = rivers_proto::texel_index(map, idx);
        let d = rivers_proto::node_point(map, f, i, j);
        let hw = if lake { 20.0 } else { rivers_proto::half_width(rd.flow[idx]) };
        let off_m = ((h(k ^ 0x77) % 1000) as f64 / 500.0 - 1.0) * hw * 1.5;
        let mut dir = d;
        dir[0] += off_m * VOXELS_PER_METRE / f64::from(WORLD_RADIUS);
        let l = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        let dir = dir.map(|c| c / l);
        let p = dir.map(|c| c * f64::from(WORLD_RADIUS));
        let top = on.surface_height(p);
        wet.push(dir.map(|c| c * (f64::from(WORLD_RADIUS) + top)));
    }

    println!("storage added by the map: {} B per texel ({} texels)", rd.bytes_per_texel(), total);
    for (name, points) in [("land anywhere", &land), ("at a river or lake", &wet)] {
        for level in [0usize, 4, 8] {
            let coords: Vec<BrickCoord> = points.iter().map(|&p| coord_at(p, level)).collect();
            println!("{name}, level {level} ({} bricks):", coords.len());
            let a = stats("off", &off, level, &coords);
            let b = stats("on", &on, level, &coords);
            println!("    extra: mean {:+.2} us ({:.2}x), p99 {:+.2} us", b.0 - a.0, b.0 / a.0, b.1 - a.1);
        }
    }
}
