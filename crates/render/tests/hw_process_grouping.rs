//! The BLAS size the program sets for every hardware backend (`set_process_grouping`, the
//! benchmark's `--hw-group-regions`, #236) is the size that a context made afterwards has, on every
//! way of making one. In a test binary of its own: the setting is made once per process.

use brick_store::BrickCoord;
use planet_gen::generate_region;
use render::hw::{Grouping, PRODUCTION_GROUPING, process_grouping, set_process_grouping};
use render::lod::Bands;
use render::scene::{Bound, Scene};
use render::{Offscreen, Resolution, RtBackend};

fn scene() -> Scene {
    let mut store = generate_region(7, BrickCoord([-12, 178, -12]), BrickCoord([12, 193, 12]));
    store.build_lod(5);
    let bound = Bound {
        centre: [0; 3],
        radius: 1600,
    };
    Scene::rings(&store, [-60.0, 1565.0, -60.0], Bands::with_d0(64.0), bound).unwrap()
}

#[test]
fn a_context_made_after_the_process_grouping_is_set_has_it_and_its_first_build_is_in_it() {
    // Nothing set: the program's size.
    assert_eq!(
        process_grouping(),
        Grouping::parse(PRODUCTION_GROUPING).unwrap()
    );
    let wanted = Grouping::parse("8:32@2").unwrap();
    assert_ne!(wanted, process_grouping());
    set_process_grouping(wanted);
    assert_eq!(process_grouping(), wanted);
    let scene = scene();
    for backend in [RtBackend::Hardware, RtBackend::HardwareNearest] {
        let hw = Offscreen::with_backend(Resolution::native(8, 8), &scene, backend).unwrap();
        if !hw.rt().active.is_hardware() {
            eprintln!("skipped: {:?}", hw.rt().fallback);
            return;
        }
        assert_eq!(hw.hardware_grouping(), wanted, "{backend:?}");
        // Its first build is of BLASes of that size: what the scene has under it, and not under
        // a cell a BLAS.
        let built = render::hw::blas_counts(&scene, &wanted);
        assert_ne!(built, render::hw::blas_counts(&scene, &Grouping::default()));
        assert_eq!(hw.hardware_groups(), built, "{backend:?}");
    }
}
