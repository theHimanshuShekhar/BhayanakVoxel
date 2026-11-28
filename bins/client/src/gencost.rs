//! PROTOTYPE (#267, never merged): `client gen-cost --landform [--proto-detail SET]...`, what the
//! per-brick ridge, gully and talus detail costs to generate, on one thread.
//!
//! It makes the landform world map once, picks a fixed set of bricks over the ranges, and for each
//! detail set (`none`, each layer alone and `all` unless `--proto-detail` names them) generates
//! every brick in turn on this thread, timing each one, [`REPEATS`] times over (the first is a
//! warm-up that is not counted). It prints the mean and the 99th percentile of a brick's
//! microseconds for each repeat's median, and the spread (least to greatest) of the repeats' means,
//! and the extra of each set over `none`.
//!
//! Two samples, both the same bricks whatever the set:
//!
//! - **surface**: for each of [`SITES`] texel sites of the map spread over the land above
//!   [`SITE_FROM_M`] (every n-th such texel in map order), the bricks of the column through the
//!   site's ground, 12 below and 12 above, 0.8 m apart (so 9.6 m each way, which holds the ground
//!   whichever layers move it), found with no layers: sky and rock and surface;
//! - **ring**: a ball of [`BALL`] bricks' radius round the ground of each of [`BALL_SITES`] of the
//!   steepest sites, the way the spawn ring is a ball of bricks round the player: round the ground
//!   that each set's Planet has, so the same number of bricks, and the layers' own surface in them.

use std::process::ExitCode;
use std::time::Instant;

use brick_store::{BrickCoord, VoxelCoord};
use planet_gen::planet::Planet;
use planet_gen::proto_detail::Set;
use planet_gen::world_map::{WORLD_MAP_SIZE, WorldMap};

const REPEATS: usize = 7;
const SITES: usize = 240;
const SITE_FROM_M: f64 = 300.0;
const BALL: i32 = 10;
const BALL_SITES: usize = 6;

const USAGE: &str = "usage: client gen-cost [--landform] [--proto-detail SET]... [--repeats N] [--map-size N]\n  \
SET: none, all, or a comma list of ridged, gullies, talus (default: none, ridged, gullies, talus, all)";

/// The median of `values` (the mean of the middle two for an even count).
fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

struct Run {
    mean: Vec<f64>,
    p99: Vec<f64>,
    mixed: usize,
    count: usize,
    /// Bounds calls and microseconds in them, per brick, of the last repeat.
    bounds: (f64, f64),
}

fn time(planet: &Planet, bricks: &[BrickCoord], repeats: usize) -> Run {
    let mut run = Run {
        mean: Vec::new(),
        p99: Vec::new(),
        mixed: 0,
        count: bricks.len(),
        bounds: (0.0, 0.0),
    };
    for repeat in 0..=repeats {
        planet_gen::proto_detail::reset_corner_cache();
        planet_gen::proto_detail::take_bounds_spent();
        let mut micros = Vec::with_capacity(bricks.len());
        let mut mixed = 0;
        for &coord in bricks {
            let started = Instant::now();
            let brick = planet.brick(coord);
            micros.push(started.elapsed().as_secs_f64() * 1e6);
            mixed += usize::from(!brick.is_uniform());
            std::hint::black_box(&brick);
        }
        run.mixed = mixed;
        let (calls, nanos) = planet_gen::proto_detail::take_bounds_spent();
        run.bounds = (
            calls as f64 / bricks.len() as f64,
            nanos as f64 / 1e3 / bricks.len() as f64,
        );
        if repeat == 0 {
            continue;
        }
        let mean = micros.iter().sum::<f64>() / micros.len() as f64;
        micros.sort_by(f64::total_cmp);
        run.mean.push(mean);
        run.p99
            .push(micros[(micros.len() * 99 / 100).min(micros.len() - 1)]);
    }
    run
}

pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    let (mut sets, mut repeats, mut size) = (Vec::new(), REPEATS, WORLD_MAP_SIZE);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--landform" => {}
            "--proto-detail" => match args.next().map(|v| Set::parse(&v)) {
                Some(Ok(set)) => sets.push(set),
                Some(Err(why)) => {
                    eprintln!("{why}\n{USAGE}");
                    return ExitCode::FAILURE;
                }
                None => {
                    eprintln!("--proto-detail needs a set\n{USAGE}");
                    return ExitCode::FAILURE;
                }
            },
            "--repeats" => repeats = args.next().and_then(|v| v.parse().ok()).unwrap_or(0).max(1),
            "--map-size" => {
                size = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(WORLD_MAP_SIZE);
            }
            _ => {
                eprintln!("`{arg}` is not an option\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    if sets.is_empty() {
        sets = vec![Set::NONE, Set::RIDGED, Set::GULLIES, Set::TALUS, Set::ALL];
    }
    planet_gen::world_map::use_landform_prototype(true);
    eprintln!("making the landform world map ({size} texels a face)...");
    let started = Instant::now();
    let planet = Planet::new(WorldMap::generate(crate::SEED, size));
    eprintln!("world map in {:.1} s", started.elapsed().as_secs_f64());
    let base = planet.clone().with_proto_detail(Set::NONE);

    // The sites: every n-th texel of the land above SITE_FROM_M, in map order.
    let map = base.map();
    let mut land = Vec::new();
    for face in 0..6 {
        for j in 0..map.size() {
            for i in 0..map.size() {
                let t = map.texel(face, i, j);
                if t.elevation_voxels() / brick_store::VOXELS_PER_METRE >= SITE_FROM_M {
                    land.push((face, i, j));
                }
            }
        }
    }
    let step = (land.len() / SITES).max(1);
    let sites: Vec<[f64; 3]> = land
        .iter()
        .step_by(step)
        .map(|&(f, i, j)| map.texel_direction(f, i, j))
        .collect();
    let ground = |d: [f64; 3]| {
        let radius = base.sea_radius() + base.ground_height(d.map(|c| c * base.sea_radius()));
        d.map(|c| c * radius)
    };
    let brick_of = |p: [f64; 3]| VoxelCoord(p.map(|c| c.round() as i32)).brick();
    let mut surface = Vec::new();
    for &d in &sites {
        let at = ground(d);
        let length = at.iter().map(|c| c * c).sum::<f64>().sqrt();
        for k in -12..=12 {
            let p: [f64; 3] = std::array::from_fn(|a| at[a] + at[a] / length * f64::from(k) * 16.0);
            surface.push(brick_of(p));
        }
    }
    // The steepest sites: the most elevation change to the neighbouring texel's.
    let mut steep: Vec<(f64, [f64; 3])> = sites
        .iter()
        .map(|&d| {
            let h = base.ground_height(d.map(|c| c * base.sea_radius()));
            let beside = [d[0] + 4e-5, d[1], d[2] - 4e-5];
            let n = beside.iter().map(|c| c * c).sum::<f64>().sqrt();
            let b = beside.map(|c| c / n);
            (
                (base.ground_height(b.map(|c| c * base.sea_radius())) - h).abs(),
                d,
            )
        })
        .collect();
    steep.sort_by(|a, b| b.0.total_cmp(&a.0));
    // The ring of a Planet: balls round the ground of the steepest sites, as that Planet has it.
    let steepest: Vec<[f64; 3]> = steep.iter().take(BALL_SITES).map(|s| s.1).collect();
    let ring_of = |planet: &Planet| {
        let mut ring = Vec::new();
        for &d in &steepest {
            let radius =
                planet.sea_radius() + planet.ground_height(d.map(|c| c * planet.sea_radius()));
            let centre = brick_of(d.map(|c| c * radius)).0;
            for x in -BALL..=BALL {
                for y in -BALL..=BALL {
                    for z in -BALL..=BALL {
                        if x * x + y * y + z * z <= BALL * BALL {
                            ring.push(BrickCoord([centre[0] + x, centre[1] + y, centre[2] + z]));
                        }
                    }
                }
            }
        }
        ring
    };
    println!(
        "{} sites over land above {SITE_FROM_M} m; surface sample {} bricks; ring sample {} bricks ({BALL_SITES} balls of radius {BALL}); {repeats} repeats after 1 warm-up; one thread",
        sites.len(),
        surface.len(),
        ring_of(&base).len()
    );
    for name in ["surface", "ring"] {
        println!(
            "\n## {name} sample ({})\n",
            if name == "surface" {
                "the same bricks for every set, columns through the ground with no layers"
            } else {
                "balls round the ground of each set's own Planet, as the spawn ring is round the player"
            }
        );
        println!(
            "| set | mean us/brick | spread of means | p99 us | extra mean over none | extra p99 over none | bricks not uniform | bounds calls, us per brick |"
        );
        println!("|---|---|---|---|---|---|---|---|");
        let mut none: Option<(f64, f64)> = None;
        for &set in &sets {
            let with = planet.clone().with_proto_detail(set);
            let bricks = if name == "surface" {
                surface.clone()
            } else {
                ring_of(&with)
            };
            let run = time(&with, &bricks, repeats);
            let (mean, p99) = (median(run.mean.clone()), median(run.p99.clone()));
            let least = run.mean.iter().copied().fold(f64::INFINITY, f64::min);
            let greatest = run.mean.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            if set == Set::NONE {
                none = Some((mean, p99));
            }
            let extra = |v: f64, base: Option<f64>| {
                base.map_or("-".to_string(), |b| format!("{:+.1}", v - b))
            };
            println!(
                "| {} | {mean:.1} | {least:.1} to {greatest:.1} | {p99:.0} | {} | {} | {} of {} | {:.1} calls, {:.1} us |",
                set.name(),
                extra(mean, none.map(|n| n.0)),
                extra(p99, none.map(|n| n.1)),
                run.mixed,
                run.count,
                run.bounds.0,
                run.bounds.1
            );
        }
    }
    ExitCode::SUCCESS
}
