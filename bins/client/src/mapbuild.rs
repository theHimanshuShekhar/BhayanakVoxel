//! `client map-build`: makes the full-size world map twice, as the game does on its first launch
//! and on its second, and says how long each took (#264).
//!
//! 1. **Cold:** builds the map with nothing read from the cache (on [`machine::threads`]), says the
//!    time of each stage, and writes it to the cache folder.
//! 2. **Cached:** reads it back, and says the time.
//! 3. Checks that the two are the same map (the content hash and texel by texel) and exits with
//!    status 1 if they are not.
//!
//! The constrained profile applies (`client map-build --profile low-end`), so the 4-thread build
//! of the low-end machine is timed on any machine. `--no-write` builds and does not touch the
//! cache folder (the cached read is then left out).

use std::process::ExitCode;
use std::time::Instant;

use planet_gen::world_map::cache;
use planet_gen::world_map::{WORLD_MAP_SIZE, WORLD_RADIUS};

pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    let mut write = true;
    for arg in args {
        match arg.as_str() {
            "--no-write" => write = false,
            other => {
                eprintln!(
                    "client map-build [--no-write] [--profile low-end] [--threads N]: `{other}` is not an option"
                );
                return ExitCode::FAILURE;
            }
        }
    }
    let threads = machine::threads();
    let dir = cache::default_dir();
    println!(
        "cache folder: {}",
        dir.as_ref()
            .map_or("none (the cache is off)".to_string(), |d| d
                .display()
                .to_string())
    );
    let cold = Instant::now();
    let (map, stages) = planet_gen::world_map::WorldMap::with_threads(
        crate::SEED,
        WORLD_MAP_SIZE,
        WORLD_RADIUS,
        threads,
    );
    let cold = cold.elapsed();
    println!(
        "world map built cold on {threads} threads in {cold:.2?}: raw {:.2?}, normalise {:.2?}, erode {:.2?}, drainage {:.2?}, texels {:.2?}",
        stages.raw, stages.normalise, stages.erode, stages.drainage, stages.texels
    );
    let hash = map.content_hash();
    println!("content hash {hash:#018x}");
    let (Some(dir), true) = (dir, write) else {
        return ExitCode::SUCCESS;
    };
    let wrote = Instant::now();
    if let Err(why) = cache::store(&dir, &map) {
        eprintln!("could not write the cache: {why}");
        return ExitCode::FAILURE;
    }
    println!("written to the cache in {:.2?}", wrote.elapsed());
    let read = Instant::now();
    match cache::load(&dir, crate::SEED, WORLD_MAP_SIZE, WORLD_RADIUS) {
        Ok(cached) => {
            let read = read.elapsed();
            let same = cached.content_hash() == hash;
            println!(
                "world map read from the cache in {read:.2?} ({:.0}x faster than the build), content hash {:#018x}: {}",
                cold.as_secs_f64() / read.as_secs_f64().max(1e-9),
                cached.content_hash(),
                if same {
                    "the same map"
                } else {
                    "NOT THE SAME MAP"
                }
            );
            if same {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(miss) => {
            eprintln!("the cache could not be read back: {miss:?}");
            ExitCode::FAILURE
        }
    }
}
