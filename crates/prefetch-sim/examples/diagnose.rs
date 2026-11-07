//! Dumps the required bricks that are missing at the first soft-miss frames of the 2 m flight with
//! generation and intake unbounded: `cargo run --release -p prefetch-sim --example diagnose`.

use prefetch_sim::cache::{Cache, Params};
use prefetch_sim::demand::build;
use prefetch_sim::key::Key;
use prefetch_sim::model::{Model, dot, norm, sub, unit};
use prefetch_sim::path;
use prefetch_sim::report::measured;
use render::lod::brick_distance;

fn main() {
    let model = Model::real();
    let path = path::flight(&model, 2.0, 50.0, 5.0, 0.2);
    let demand = build(&model, &path);
    let params = Params {
        gen_per_core: 1.0e12,
        intake: f64::INFINITY,
        max_ahead: None,
        budget_bytes: Some(1.0e18),
        ..measured()
    };
    let mut cache = Cache::new(&model, params);
    cache.record_missing = true;
    for (i, step) in demand.steps.iter().enumerate() {
        let frame = cache.step(step, demand.dt);
        let sample = &path.samples[i.saturating_sub(1)];
        println!(
            "frame {i}: speed {:.0} Voxels/s, soft {}, holes {}, queued_new {}",
            norm(sample.vel),
            frame.soft_misses,
            frame.holes,
            frame.queued_new
        );
        if frame.soft_misses > 0 && i < 6 {
            let mut missing = cache.last_missing.clone();
            let d = |k: &Key| brick_distance(step.pos, k.level(), k.coord());
            missing.sort_by(|a, b| d(a).total_cmp(&d(b)));
            for key in missing.iter().take(6) {
                let size = (8u64 << key.level()) as f64;
                let centre = key.coord().0.map(|c| (f64::from(c) + 0.5) * size);
                let to = sub(centre, step.pos);
                let cos = dot(unit(to), unit(sample.vel));
                println!(
                    "   level {} dist {:.1} m angle to velocity {:.0} deg mask now {} (visible 1, shell 2, prefetch 4)",
                    key.level(),
                    d(key) / 10.0,
                    cos.acos().to_degrees(),
                    cache.mask_of(*key)
                );
            }
            println!("   missing in all: {}", cache.last_missing.len());
        }
    }
}
