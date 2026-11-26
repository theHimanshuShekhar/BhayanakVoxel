//! The experiments of #236's diagnosis of the hardware backend's GPU time, set for the whole
//! process (the benchmark's `--hw-wait`): a context made
//! afterwards builds what the scene has, and says what the GPU
//! took for the BLAS builds and for the TLAS. In a test binary of its own: they are made once per
//! process.

use brick_store::BrickCoord;
use planet_gen::generate_region;
use render::hw::{blas_counts, set_process_wait};
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
fn waiting_for_the_gpu_the_builds_make_what_the_scene_has_and_the_gpu_times_them_where_it_can() {
    set_process_wait(true);
    let scene = scene();
    let mut hw =
        Offscreen::with_backend(Resolution::native(8, 8), &scene, RtBackend::Hardware).unwrap();
    if !hw.rt().active.is_hardware() {
        eprintln!("skipped: {:?}", hw.rt().fallback);
        return;
    }
    let grouping = hw.hardware_grouping();
    assert_eq!(hw.hardware_groups(), blas_counts(&scene, &grouping));
    let log = hw.take_hardware_updates();
    let built: Vec<_> = log.iter().filter(|u| u.blas_rebuilt > 2).collect();
    assert!(!built.is_empty(), "more BLASes than a call has: {log:?}");
    if !hw.hardware_times_builds() {
        eprintln!("no timestamps in encoders on this device: {log:?}");
        assert!(
            built
                .iter()
                .all(|u| u.gpu_blas.is_zero() && u.gpu_tlas.is_zero())
        );
        return;
    }
    for u in built {
        assert!(!u.gpu_blas.is_zero() && !u.gpu_tlas.is_zero(), "{u:?}");
        // From the box pass's end to the builds' start: the wait and the recording are in it, and
        // it is before the builds' own time, which is not in it.
        assert!(!u.gpu_gap.is_zero(), "{u:?}");
        // The scratch buffer of the builds was there when the allocator was looked at, if it can be.
        assert!(
            u.scratch_bytes > 0 || hw.device_allocation_sizes().is_none(),
            "{u:?}"
        );
        // And it found room: the blocks made before the first build (and kept) took the scratch
        // buffer, so the GPU had no new memory to wait for (#236).
        assert_eq!((u.blocks_made, u.reserved_grew), (0, 0), "{u:?}");
        assert!(
            u.gpu_gap + u.gpu_blas + u.gpu_tlas
                <= u.phases.wait
                    + u.phases.wait_boxes
                    + u.phases.builds
                    + u.phases.finish
                    + u.phases.submit,
            "{u:?}"
        );
        // The GPU's time of the builds is inside the update's wait for it.
        assert!(
            u.gpu_blas + u.gpu_tlas <= u.phases.wait + u.phases.wait_boxes,
            "{u:?}"
        );
    }
}
