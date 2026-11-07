//! Prints the report: `cargo run --release -p prefetch-sim`.

use std::time::Instant;

use prefetch_sim::model::Model;
use prefetch_sim::report::{
    ALTITUDES, ablation_markdown, margin_needed, markdown, measured, paths, run_ablation,
    run_suite, shell_size,
};

fn main() {
    let started = Instant::now();
    let model = Model::real();
    let params = measured();
    let results = run_suite(&model, &params);
    let sizes: Vec<_> = ALTITUDES.iter().map(|&a| shell_size(&model, a)).collect();
    print!("{}", markdown(&model, &params, &results, &sizes));
    println!(
        "\n{}",
        ablation_markdown(&run_ablation(&model, &params), &params)
    );
    for (path, result) in paths(&model).iter().zip(&results) {
        if !result.nominal.is_clean(result.allowed_soft_seconds) {
            match margin_needed(&model, path, &params) {
                Some(m) => println!("{}: needs a radial margin of {m}", result.name),
                None => println!("{}: no radial margin up to 3 is enough", result.name),
            }
        }
    }
    println!("\nrun in {:.1?}", started.elapsed());
}
