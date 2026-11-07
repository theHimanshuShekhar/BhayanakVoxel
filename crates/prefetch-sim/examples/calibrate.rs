use prefetch_sim::model::Model;
use prefetch_sim::report::drawn_bricks;

fn main() {
    let mut model = Model::real();
    let d0 = model.bands.d0();
    for crust in [0.0, 0.1, 0.25, 0.5, 1.0, 2.0] {
        model.crust = crust;
        let n = drawn_bricks(&model);
        println!(
            "crust {crust}: {n} bricks, {:.2} d0^2",
            n as f64 / (d0 * d0)
        );
    }
}
