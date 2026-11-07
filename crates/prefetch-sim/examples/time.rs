use prefetch_sim::cache::run;
use prefetch_sim::demand::build;
use prefetch_sim::model::Model;
use prefetch_sim::report::*;
use std::time::Instant;

fn main() {
    let model = Model::real();
    let params = measured();
    for path in paths(&model) {
        let t = Instant::now();
        let d = build(&model, &path);
        let tb = t.elapsed();
        let t = Instant::now();
        let s = run(&model, &d, &params);
        println!(
            "{:34} build {:6.1?} replay {:6.1?} peak {} evict {}",
            path.name,
            tb,
            t.elapsed(),
            s.peak_target_bricks,
            s.evictions
        );
    }
}
